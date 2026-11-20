"""The worker's one HTTP listener, serving both sources' snapshots.

The routes are the ones the Rust service already reads:

* ``/tracks.json`` — Amtrak's train-status documents (:mod:`worker.tracks.server`);
* ``/service-alerts-and-notices`` and ``/alert/<slug>.html`` — the notices list and its detail
  pages (:mod:`worker.notices.server`);
* ``/healthz`` — liveness. With tracks on it is the track loop's liveness: ``503`` once that loop
  stalls or keeps failing. With tracks off it is ``200``.

A source that is off answers ``404`` on its paths. Handlers only read the in-memory or on-disk
snapshot: they never wait on the browser, so a slow Amtrak page cannot stall a request.

A client that hangs up before the response is written is logged as one line through this module's
logger, never as ``socketserver``'s stderr traceback. The service's container healthcheck reads
only the status line of ``/healthz`` and closes, so that path logs at DEBUG; any other path logs a
WARNING. Any other error in a handler is logged at ERROR with its traceback.
"""

from __future__ import annotations

import logging
import sys
import time
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

from .config import WorkerConfig
from .notices import server as notices_server
from .notices.store import SnapshotStore, safe_detail_name
from .tracks import server as tracks_server
from .tracks.store import TrackStore

HEALTH_PATH = "/healthz"
TRACKS_PATH = "/tracks.json"
NOTICES_PATH = "/service-alerts-and-notices"

logger = logging.getLogger(__name__)

# The client closed its end before the response was written: a timeout, an early close, or a
# healthcheck that reads only the status line.
_DISCONNECTS = (BrokenPipeError, ConnectionResetError, ConnectionAbortedError)


@dataclass(frozen=True)
class Snapshots:
    """What the listener serves: the worker config and each enabled source's store."""

    config: WorkerConfig
    tracks: TrackStore | None = None
    notices: SnapshotStore | None = None


def route(snapshots: Snapshots, path: str, now: float) -> tuple[int, str, bytes]:
    """Decide a GET response as ``(status, content_type, body)``. Pure, for tests."""
    clean = urlsplit(path).path
    tracks = snapshots.config.tracks
    notices = snapshots.config.notices
    if clean == HEALTH_PATH:
        if tracks is not None and snapshots.tracks is not None:
            return _tracks(snapshots, HEALTH_PATH, now)
        return 200, "text/plain", b"ok"
    if clean == TRACKS_PATH:
        if tracks is None or snapshots.tracks is None:
            return 404, "text/plain", b"tracks are off"
        return _tracks(snapshots, path, now)
    if clean == NOTICES_PATH or safe_detail_name(clean) is not None:
        if notices is None or snapshots.notices is None:
            return 404, "text/plain", b"notices are off"
        return notices_server.route(
            snapshots.notices,
            NOTICES_PATH,
            notices.max_stale_secs,
            path,
            now,
            notices.detail_max_stale_secs,
        )
    return 404, "text/plain", b"not found"


def _tracks(snapshots: Snapshots, path: str, now: float) -> tuple[int, str, bytes]:
    tracks = snapshots.config.tracks
    assert tracks is not None and snapshots.tracks is not None
    return tracks_server.route(
        snapshots.tracks,
        TRACKS_PATH,
        tracks.max_stale_secs,
        tracks.document_ttl_secs,
        path,
        now,
        tracks.liveness_secs,
    )


def make_handler(snapshots: Snapshots):
    """Build a request handler bound to ``snapshots``."""

    class WorkerHandler(BaseHTTPRequestHandler):
        """Serves GETs through :func:`route`; other methods are unsupported."""

        def do_GET(self) -> None:  # noqa: N802 — http.server's required method name
            status, content_type, body = route(snapshots, self.path, time.time())
            try:
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            except _DISCONNECTS as error:
                self.close_connection = True
                # The path only: a query string is the client's, not ours to log.
                path = urlsplit(self.path).path
                level = logging.DEBUG if path == HEALTH_PATH else logging.WARNING
                logger.log(
                    level,
                    "client %s closed the connection before the %s response was written (%s)",
                    _peer(self.client_address),
                    path,
                    type(error).__name__,
                )

        def log_message(self, *args: object) -> None:  # keep the access log quiet
            return

    return WorkerHandler


def _peer(client_address: object) -> str:
    if isinstance(client_address, tuple) and len(client_address) >= 2:
        return f"{client_address[0]}:{client_address[1]}"
    return str(client_address)


class WorkerServer(ThreadingHTTPServer):
    """``ThreadingHTTPServer`` whose request errors go to this module's logger, not stderr."""

    def handle_error(self, request: object, client_address: object) -> None:
        """Log a request that failed outside :meth:`do_GET`'s own handling.

        A disconnect (for example while the request line is read) is one DEBUG line. Anything
        else is an ERROR with its traceback, so a real bug stays visible.
        """
        error = sys.exc_info()[1]
        if isinstance(error, _DISCONNECTS):
            logger.debug(
                "client %s closed the connection (%s)",
                _peer(client_address),
                type(error).__name__,
            )
            return
        logger.error(
            "unexpected error serving a request from %s", _peer(client_address), exc_info=True
        )


def make_server(snapshots: Snapshots) -> ThreadingHTTPServer:
    """A threaded server on the configured host and port. It is bound and not yet serving."""
    config = snapshots.config
    return WorkerServer((config.listen_host, config.listen_port), make_handler(snapshots))

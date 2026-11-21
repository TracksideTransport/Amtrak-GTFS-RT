"""One listener serves both sources; a source that is off answers 404."""

import http.client
import json
import logging
import socket
import struct
import threading
import time
import urllib.error
import urllib.request

import pytest

import worker.server
from worker.config import WorkerConfig
from worker.notices.store import SnapshotStore
from worker.server import (
    HEALTH_PATH,
    NOTICES_PATH,
    STATIONS_PATH,
    TRACKS_PATH,
    Snapshots,
    make_server,
    route,
)
from worker.tracks.stations import StationsStore
from worker.tracks.store import TrackStore

DETAIL = "/alert/boston-south-station-restroom-improvement-project.html"


def _snapshots(tmp_path, **flags):
    cfg = WorkerConfig.from_env({"LISTEN_PORT": "0", **flags})
    return Snapshots(
        config=cfg,
        tracks=TrackStore(started_at=1_000) if cfg.tracks is not None else None,
        stations=StationsStore(None) if cfg.tracks is not None else None,
        notices=SnapshotStore(tmp_path) if cfg.notices is not None else None,
    )


def test_both_sources_are_routed(tmp_path):
    snapshots = _snapshots(tmp_path)
    snapshots.tracks.mark_success(1_000)
    snapshots.notices.update("<html>na-service-alert</html>")
    snapshots.notices.update_detail(DETAIL, "<p>Atlantic Avenue</p>")
    _, listed = snapshots.notices.read()

    status, content_type, body = route(snapshots, TRACKS_PATH, 1_010)
    assert (status, content_type) == (200, "application/json")
    assert json.loads(body)["documents"] == []
    status, content_type, body = route(snapshots, NOTICES_PATH, listed + 1)
    assert (status, content_type, body) == (200, "text/html", b"<html>na-service-alert</html>")
    status, _, body = route(snapshots, DETAIL, listed + 1)
    assert (status, body) == (200, b"<p>Atlantic Avenue</p>")
    assert route(snapshots, "/other", 1_010)[0] == 404


def test_stations_route(tmp_path):
    off = _snapshots(tmp_path, AMTRAK_TRACKS_AMTRAK="off")
    assert route(off, STATIONS_PATH, 1_000) == (404, "text/plain", b"tracks are off")
    snapshots = _snapshots(tmp_path)
    status, _, body = route(snapshots, STATIONS_PATH, 1_000)
    assert (status, body) == (503, b"no stations yet")
    snapshots.stations.record_document(
        {"data": [{"stops": [{"station": {"code": "BBY", "name": "Boston Back Bay"}}]}]}
    )
    directory = json.dumps([{"code": "BOS", "name": "Boston", "city": "Boston"}])
    assert snapshots.stations.accept_directory(directory, 1_000.0)
    status, content_type, body = route(snapshots, STATIONS_PATH, 1_001)
    assert (status, content_type) == (200, "application/json")
    payload = json.loads(body)
    assert payload["directory"][0]["code"] == "BOS"
    assert payload["seen"]["BBY"] == {"name": "Boston Back Bay"}
    assert payload["directory_fetched_at"] == "1970-01-01T00:16:40Z"
    assert payload["seen_updated_at"] is not None


def test_a_source_that_is_off_answers_404(tmp_path):
    tracks_only = _snapshots(tmp_path, AMTRAK_ADVISORIES="off")
    assert route(tracks_only, NOTICES_PATH, 1_000) == (404, "text/plain", b"notices are off")
    assert route(tracks_only, DETAIL, 1_000)[0] == 404
    notices_only = _snapshots(tmp_path, AMTRAK_TRACKS_AMTRAK="off")
    assert route(notices_only, TRACKS_PATH, 1_000) == (404, "text/plain", b"tracks are off")


def test_health_follows_the_track_loop_and_is_ok_without_it(tmp_path):
    snapshots = _snapshots(tmp_path)
    assert route(snapshots, HEALTH_PATH, 1_100)[0] == 200
    stalled = 1_000 + snapshots.config.tracks.liveness_secs + 1
    assert route(snapshots, HEALTH_PATH, stalled) == (503, "text/plain", b"poll loop stalled")
    notices_only = _snapshots(tmp_path, AMTRAK_TRACKS="off")
    assert route(notices_only, HEALTH_PATH, stalled) == (200, "text/plain", b"ok")


def test_live_server_serves_both_paths(tmp_path):
    snapshots = _snapshots(tmp_path)
    snapshots.tracks.mark_cycle(time.time())  # the track loop is alive
    snapshots.notices.update("<html>live na-service-alert</html>")
    server = make_server(snapshots)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        host, port = server.server_address
        assert host == "127.0.0.1"
        base = f"http://127.0.0.1:{port}"
        with urllib.request.urlopen(f"{base}{HEALTH_PATH}", timeout=5) as resp:
            assert resp.status == 200
        with urllib.request.urlopen(f"{base}{NOTICES_PATH}", timeout=5) as resp:
            assert resp.status == 200 and b"na-service-alert" in resp.read()
        try:
            urllib.request.urlopen(f"{base}{TRACKS_PATH}", timeout=5)
        except urllib.error.HTTPError as error:
            assert error.code == 503  # no track poll has succeeded yet
        else:
            raise AssertionError("tracks served before any poll")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


class _Running:
    """A live worker listener on an ephemeral loopback port, shut down on exit."""

    def __init__(self, snapshots):
        self.server = make_server(snapshots)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self.server.server_address[1]

    def __exit__(self, *_exc):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def _hang_up_before_the_response(port: int, target: str, monkeypatch) -> None:
    """Send a GET, then reset the connection after the worker has read it, before it answers."""
    parsed = threading.Event()
    hung_up = threading.Event()
    real_route = worker.server.route

    def late_route(*args):
        parsed.set()
        hung_up.wait(5)
        time.sleep(0.05)  # let the reset reach the server's socket
        return real_route(*args)

    monkeypatch.setattr(worker.server, "route", late_route)
    client = socket.create_connection(("127.0.0.1", port), timeout=5)
    client.sendall(f"GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n".encode())
    assert parsed.wait(5)
    client.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
    client.close()  # linger 0: an immediate reset, like a client timeout or early close
    hung_up.set()


def _wait_for_record(caplog, level):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        found = [r for r in caplog.records if r.name == "worker.server" and r.levelno == level]
        if found:
            return found
        time.sleep(0.01)
    raise AssertionError(f"no {logging.getLevelName(level)} record from worker.server")


@pytest.mark.parametrize(
    ("target", "level", "path"),
    [
        (HEALTH_PATH, logging.DEBUG, HEALTH_PATH),
        (f"{TRACKS_PATH}?token=secret", logging.WARNING, TRACKS_PATH),
    ],
    ids=["healthcheck", "tracks"],
)
def test_a_client_that_hangs_up_is_one_log_line_not_a_traceback(
    tmp_path, monkeypatch, caplog, capsys, target, level, path
):
    snapshots = _snapshots(tmp_path, AMTRAK_ADVISORIES="off")
    snapshots.tracks.mark_cycle(time.time())
    with caplog.at_level(logging.DEBUG, logger="worker.server"), _Running(snapshots) as port:
        _hang_up_before_the_response(port, target, monkeypatch)
        records = _wait_for_record(caplog, level)
    assert len(records) == 1
    message = records[0].getMessage()
    assert records[0].exc_info is None
    assert path in message and "127.0.0.1:" in message
    assert "token" not in message and "secret" not in message
    assert message.endswith(("(BrokenPipeError)", "(ConnectionResetError)", "(ConnectionAbortedError)"))
    assert not [r for r in caplog.records if r.levelno >= logging.ERROR]
    assert "Traceback" not in capsys.readouterr().err


def test_an_unexpected_handler_error_is_logged_with_its_traceback(
    tmp_path, monkeypatch, caplog, capsys
):
    def broken_route(*_args):
        raise RuntimeError("route bug")

    monkeypatch.setattr(worker.server, "route", broken_route)
    snapshots = _snapshots(tmp_path)
    with caplog.at_level(logging.DEBUG, logger="worker.server"), _Running(snapshots) as port:
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
        connection.request("GET", HEALTH_PATH)
        with pytest.raises((http.client.RemoteDisconnected, ConnectionError)):
            connection.getresponse()
        connection.close()
        records = _wait_for_record(caplog, logging.ERROR)
    assert records[0].getMessage().startswith("unexpected error serving a request from 127.0.0.1:")
    assert records[0].exc_info is not None and records[0].exc_info[0] is RuntimeError
    assert "Exception occurred during processing" not in capsys.readouterr().err

"""Amtrak's station names, kept for the service.

Two sources feed one table the service reads at ``/stations.json``:

* the **seen** table: every ``station`` object in the status documents the worker already
  fetches (``code``, ``name``, ``displayName``, ``facility``, ``audioName``), merged by code. A
  field never erases a value already known;
* the **directory**: Amtrak's station list, fetched in the browser page about every two weeks
  and retried hourly after a failure.

With a root directory both are persisted (``stations-seen.json`` and ``stations-directory.json``)
with atomic writes. A file that cannot be read back is renamed to ``*.invalid`` and ignored.
"""

from __future__ import annotations

import json
import logging
import math
import os
import re
import tempfile
import threading
import time

from .store import iso_utc

__all__ = ["DIRECTORY_URL", "REFRESH_REQUEST_FILE", "StationsStore"]

DIRECTORY_URL = "https://www.amtrak.com/services/data.stations.json"
SEEN_FIELDS = ("name", "displayName", "facility", "audioName")
RETRY_AFTER_FAILURE_SECS = 3600
_SEEN_FILE = "stations-seen.json"
_DIRECTORY_FILE = "stations-directory.json"
REFRESH_REQUEST_FILE = "station-names-refresh-requested"
_CODE = re.compile(r"[A-Za-z]{3}")

logger = logging.getLogger(__name__)


def _text(value: object) -> str | None:
    return value.strip() if isinstance(value, str) and value.strip() else None


def _valid_directory(rows: object) -> bool:
    return (
        isinstance(rows, list)
        and bool(rows)
        and all(
            isinstance(row, dict)
            and isinstance(row.get("code"), str)
            and _CODE.fullmatch(row["code"]) is not None
            for row in rows
        )
    )


class StationsStore:
    """The seen table and the directory, in memory and optionally on disk."""

    def __init__(self, root: str | None) -> None:
        self._root = root or None
        self._lock = threading.Lock()
        self._seen: dict[str, dict[str, str]] = {}
        self._seen_updated_at: float | None = None
        self._directory: list[dict] = []
        self._directory_fetched_at: float | None = None
        self._last_failure: float | None = None
        self._handled_request: float | None = None
        if self._root is not None:
            self._load()

    def record_document(self, document: dict) -> None:
        """Merge every stop's station in ``document`` into the seen table."""
        trains = document.get("data") if isinstance(document, dict) else None
        found: dict[str, dict[str, str]] = {}
        for train in trains if isinstance(trains, list) else []:
            stops = train.get("stops") if isinstance(train, dict) else None
            for stop in stops if isinstance(stops, list) else []:
                station = stop.get("station") if isinstance(stop, dict) else None
                code = _text(station.get("code")) if isinstance(station, dict) else None
                if code is None:
                    continue
                entry = found.setdefault(code.upper(), {})
                for field in SEEN_FIELDS:
                    value = _text(station.get(field))
                    if value is not None:
                        entry[field] = value
        with self._lock:
            changed = False
            for code, fields in found.items():
                current = self._seen.setdefault(code, {})
                for field, value in fields.items():
                    if current.get(field) != value:
                        current[field] = value
                        changed = True
                if not current:
                    del self._seen[code]
            if not changed:
                return
            self._seen_updated_at = time.time()
            self._write(
                _SEEN_FILE, {"seen": self._seen, "updated_at": self._seen_updated_at}
            )

    def directory_due(self, now: float, refresh_secs: int) -> bool:
        """Whether to fetch the directory: it is missing or old, and no recent attempt failed."""
        with self._lock:
            if self._last_failure is not None:
                return now - self._last_failure >= RETRY_AFTER_FAILURE_SECS
            if self._directory_fetched_at is None or not self._directory:
                return True
            return now - self._directory_fetched_at >= refresh_secs

    def refresh_requested(self) -> float | None:
        """When an operator asked for a refresh (Unix seconds), or ``None``.

        The service's ``refresh-station-names`` command writes the time to
        ``station-names-refresh-requested`` beside the other station files. ``None`` without a
        root, with no file, or when it cannot be read. A file that does not hold a number counts
        as requested when it was last modified.
        """
        if self._root is None:
            return None
        path = self._path(REFRESH_REQUEST_FILE)
        try:
            with open(path, encoding="utf-8") as handle:
                text = handle.read().strip()
            try:
                requested = float(text)
                if math.isfinite(requested):
                    return requested
            except ValueError:
                pass
            return os.stat(path).st_mtime
        except (OSError, ValueError):
            return None

    def take_request(self) -> float | None:
        """The pending request time, marked handled so it is acted on once.

        ``None`` when there is no request, or it is not newer than the last one handled.
        """
        requested = self.refresh_requested()
        if requested is None:
            return None
        with self._lock:
            if self._handled_request is not None and requested <= self._handled_request:
                return None
            self._handled_request = requested
        return requested

    def accept_directory(self, body: str, now: float) -> bool:
        """Store a valid directory body. A bad one records the failure and keeps the old copy."""
        try:
            rows = json.loads(body)
        except (TypeError, ValueError):
            rows = None
        with self._lock:
            if not _valid_directory(rows):
                self._last_failure = now
                return False
            self._directory = rows
            self._directory_fetched_at = now
            self._last_failure = None
            self._write(_DIRECTORY_FILE, {"directory": rows, "fetched_at": now})
            return True

    def snapshot(self) -> dict | None:
        """The ``/stations.json`` payload, or ``None`` when nothing is known yet."""
        with self._lock:
            if not self._directory and not self._seen:
                return None
            return {
                "directory": list(self._directory),
                "directory_fetched_at": _iso(self._directory_fetched_at),
                "seen": {code: dict(fields) for code, fields in self._seen.items()},
                "seen_updated_at": _iso(self._seen_updated_at),
            }

    def display_name(self, code: str) -> str:
        """``displayName``, else the seen ``name``, else the directory ``name``, else ``code``."""
        key = code.strip().upper()
        with self._lock:
            seen = self._seen.get(key, {})
            for field in ("displayName", "name"):
                if field in seen:
                    return seen[field]
            for row in self._directory:
                if str(row.get("code", "")).upper() == key and _text(row.get("name")):
                    return row["name"].strip()
        return code

    def _load(self) -> None:
        seen = self._read(_SEEN_FILE)
        if isinstance(seen, dict) and isinstance(seen.get("seen"), dict):
            table = {
                code: {f: v for f, v in fields.items() if f in SEEN_FIELDS and isinstance(v, str)}
                for code, fields in seen["seen"].items()
                if isinstance(code, str) and isinstance(fields, dict)
            }
            updated = seen.get("updated_at")
            self._seen = {code: fields for code, fields in table.items() if fields}
            self._seen_updated_at = updated if isinstance(updated, (int, float)) else None
        elif seen is not None:
            self._invalidate(_SEEN_FILE)
        directory = self._read(_DIRECTORY_FILE)
        if isinstance(directory, dict):
            fetched = directory.get("fetched_at")
            if _valid_directory(directory.get("directory")) and isinstance(fetched, (int, float)):
                self._directory = directory["directory"]
                self._directory_fetched_at = float(fetched)
            else:
                self._invalidate(_DIRECTORY_FILE)
        elif directory is not None:
            self._invalidate(_DIRECTORY_FILE)

    def _path(self, name: str) -> str:
        assert self._root is not None
        return os.path.join(self._root, name)

    def _read(self, name: str) -> object | None:
        """The parsed file, or ``None`` when absent. An unparsable file is set aside."""
        if self._root is None:
            return None
        try:
            with open(self._path(name), encoding="utf-8") as handle:
                return json.load(handle)
        except FileNotFoundError:
            return None
        except (OSError, ValueError):
            self._invalidate(name)
            return None

    def _invalidate(self, name: str) -> None:
        path = self._path(name)
        try:
            os.replace(path, path + ".invalid")
            logger.warning("ignoring unreadable %s; renamed to %s.invalid", path, path)
        except OSError:
            pass

    def _write(self, name: str, payload: dict) -> None:
        if self._root is None:
            return
        tmp = None
        try:
            os.makedirs(self._root, exist_ok=True)
            fd, tmp = tempfile.mkstemp(dir=self._root, prefix=f".{name}.", suffix=".tmp")
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                json.dump(payload, handle, separators=(",", ":"))
            os.replace(tmp, self._path(name))
            tmp = None
        except OSError as error:
            logger.warning("could not persist %s: %s", name, error)
        finally:
            if tmp is not None:
                try:
                    os.remove(tmp)
                except OSError:
                    pass


def _iso(timestamp: float | None) -> str | None:
    return None if timestamp is None else iso_utc(timestamp)

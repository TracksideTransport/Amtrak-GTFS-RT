"""Choose which trains to ask Amtrak about.

Amtrak's status API often publishes a track only in the last minutes before a train reaches a
station (Providence showed track 1 about eight minutes before train 167 arrived). Some stations
publish earlier, and that value is already on the document once the train enters the window, so
polling trains with a stop near its ETA collects both kinds. A train is hot when a stop where
riders board, at a station where Amtrak publishes tracks, is inside the hot window and still has
no track in its last document. A hot train with no document yet is fetched every cycle unless
it was just tried; one that has a document waits out the cooldown like the rest, so more hot
trains than the per-cycle cap cannot starve the others. A boarding stop inside the hot window at
a station where no track has been seen yet makes a train warm: it is not hot, but it is checked
between full polls under the cooldown, so a fresh install still polls Providence or Washington
while it learns where Amtrak publishes tracks. Every other due train, including one that has
just left a stop, is rotated under that cooldown so a busy hour does not fetch every active train
every minute.
"""

from __future__ import annotations

import json
from collections.abc import Collection, Mapping
from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import NamedTuple
from zoneinfo import ZoneInfo

_ZONES = {
    "E": ZoneInfo("America/New_York"),
    "C": ZoneInfo("America/Chicago"),
    "M": ZoneInfo("America/Denver"),
    "P": ZoneInfo("America/Los_Angeles"),
}
# Arizona does not observe daylight saving time, but the train feed labels its stations "M".
_PHOENIX = ZoneInfo("America/Phoenix")
_ARIZONA_STATIONS = frozenset({"BEN", "FLG", "KNG", "MRC", "TUS", "WLO", "WMJ", "YUM"})
_TIME_FORMATS = ("%m/%d/%Y %H:%M:%S", "%m/%d/%Y %I:%M:%S %p")


class TrackStop(NamedTuple):
    """An upcoming stop whose track a rider can use: boarding is allowed and tracks are published.

    ``secs`` is the time until the stop's next future in-window event. ``hot_secs`` is that
    station's own hot window from a lead-time profile, or ``None`` for the global hot window.
    """

    code: str
    secs: float
    hot_secs: float | None = None


@dataclass(frozen=True)
class DueTrain:
    """One train-day with a stop inside the polling window.

    ``soonest_secs`` is the distance to the closest in-window event, past or future.
    ``next_secs`` is the time until the next future in-window event, or ``None`` when every
    in-window event is already past. ``upcoming`` holds the codes of the stations those future
    events belong to. ``track_stops`` are the upcoming stops that can make the train hot (see
    :func:`is_hot`): a stop where riders cannot board, or at a station not in the train list's
    ``track_stations``, is left out. ``unlisted_stops`` are the upcoming boarding stops at
    stations not in ``track_stations``; they can make the train warm (see :func:`is_warm`).
    ``next_station`` is the code of the station with the soonest of those future events (the
    first in document order on a tie), or ``None`` when no such stop has a code.
    """

    train_number: str
    service_date: str
    soonest_secs: float
    next_secs: float | None = None
    upcoming: frozenset[str] = frozenset()
    track_stops: tuple[TrackStop, ...] = ()
    unlisted_stops: tuple[TrackStop, ...] = ()
    next_station: str | None = None

    @property
    def key(self) -> tuple[str, str]:
        return (self.train_number, self.service_date)


def due_trains(
    geojson: dict,
    now: datetime,
    *,
    before: timedelta,
    after: timedelta,
    lead=None,
    hot: timedelta | None = None,
) -> list[DueTrain]:
    """Trains with a rail stop whose ETA is between ``now - after`` and ``now + before``.

    ``geojson`` is Amtrak's decrypted train feed, or the ``amtrak-fleet-trains/v1`` list the
    service writes after each generation. An estimated time replaces the scheduled time for that
    arrival or departure. Bus stops are ignored. ``soonest_secs`` is the absolute distance from
    ``now`` to the closest such event. When ``lead`` is set, each stop uses that station's
    lookahead and hot window from an ``amtrak-station-lead-time/v1`` profile, and ``hot`` is the
    global hot window those per-station values fall back to.
    """
    if now.tzinfo is None:
        raise ValueError("now must be timezone-aware")
    before_secs = before.total_seconds()
    after_secs = after.total_seconds()
    default_hot = hot.total_seconds() if hot is not None else None
    track_stations = _track_stations(geojson)
    found: dict[tuple[str, str], DueTrain] = {}
    for number, service_date, stops in _train_events(geojson):
        distances: list[float] = []
        ahead: list[float] = []
        upcoming: set[str] = set()
        track_stops: list[TrackStop] = []
        unlisted_stops: list[TrackStop] = []
        next_station: tuple[float, str] | None = None
        for code, moments, boarding in stops:
            station_before = (
                lead.before_secs(code, before_secs) if lead is not None else before_secs
            )
            stop_ahead: list[float] = []
            for moment in moments:
                delta = (moment - now).total_seconds()
                if -after_secs <= delta <= station_before:
                    distances.append(abs(delta))
                    if delta >= 0:
                        stop_ahead.append(delta)
            if not stop_ahead:
                continue
            ahead.extend(stop_ahead)
            if code is None:
                continue
            upcoming.add(code)
            soonest = min(stop_ahead)
            if next_station is None or soonest < next_station[0]:
                next_station = (soonest, code)
            if not boarding:
                continue
            station_hot = (
                lead.hot_secs(code, default_hot)
                if lead is not None and default_hot is not None
                else None
            )
            stop = TrackStop(code, min(stop_ahead), station_hot)
            if track_stations is None or code in track_stations:
                track_stops.append(stop)
            else:
                unlisted_stops.append(stop)
        if not distances:
            continue
        train = DueTrain(
            number,
            service_date,
            min(distances),
            min(ahead) if ahead else None,
            frozenset(upcoming),
            tuple(track_stops),
            tuple(unlisted_stops),
            next_station[1] if next_station is not None else None,
        )
        current = found.get(train.key)
        if current is None or train.soonest_secs < current.soonest_secs:
            found[train.key] = train
    return list(found.values())


FLEET_TRAINS_SCHEMA = "amtrak-fleet-trains/v1"

StopEvents = list[tuple[str | None, list[datetime], bool]]


def _track_stations(document: dict) -> frozenset[str] | None:
    """Stations where a track has been published, from the train list's ``track_stations``.

    ``None`` (the map feed, or a list from a service that does not write the field) means every
    station may publish one.
    """
    if document.get("schema") != FLEET_TRAINS_SCHEMA:
        return None
    stations = document.get("track_stations")
    if not isinstance(stations, list):
        return None
    return frozenset(
        code.strip().upper() for code in stations if isinstance(code, str) and code.strip()
    )


def _train_events(document: dict):
    """Yield ``(train_number, service_date, [(station_code, [instant], boarding)])`` per train-day.

    The map feed does not say where riders may board, so its stops all allow boarding.
    """
    if document.get("schema") == FLEET_TRAINS_SCHEMA:
        yield from _fleet_train_events(document)
        return
    for feature in document.get("features") or []:
        properties = feature.get("properties") or {}
        number = _train_number(properties.get("TrainNum"))
        service_date = _service_date(properties)
        if number is None or service_date is None:
            continue
        stops: StopEvents = [
            (_station_code(station), _event_times(station), True)
            for station in _stations(properties)
            if station.get("bus") is not True
        ]
        yield number, service_date, stops


def _fleet_train_events(document: dict):
    for train in document.get("trains") or []:
        if not isinstance(train, dict):
            continue
        number = _train_number(train.get("train_number"))
        service_date = train.get("service_date")
        if number is None or not isinstance(service_date, str):
            continue
        stops: StopEvents = []
        for stop in train.get("stops") or []:
            if not isinstance(stop, dict):
                continue
            code = _station_code(stop)
            times = stop.get("times_unix") or []
            moments = [
                datetime.fromtimestamp(value, tz=_ZONES["E"])
                for value in times
                if isinstance(value, int) and not isinstance(value, bool)
            ]
            # A missing field (an older service) means riders can board there.
            stops.append((code, moments, stop.get("boarding") is not False))
        yield number, service_date, stops


def is_hot(train: DueTrain, tracked: Mapping[tuple[str, str], frozenset[str]], hot_secs: float) -> bool:
    """Whether one of ``train``'s track stops is inside its hot window and has no track yet.

    ``tracked`` maps a train-day to the station codes that already show a track in its last
    document. A stop with its own window from a lead-time profile uses it; any other stop uses
    ``hot_secs``. Stops where riders cannot board, and stations where Amtrak has never published
    a track, are not track stops (see :class:`DueTrain`), so they never keep a train hot.
    """
    return _awaits_track(train.track_stops, tracked.get(train.key, frozenset()), hot_secs)


def is_warm(train: DueTrain, tracked: Mapping[tuple[str, str], frozenset[str]], hot_secs: float) -> bool:
    """Whether a boarding stop at a station with no track seen yet is inside its hot window.

    Such a station is not in the train list's ``track_stations`` (no configured or observed
    platform), so it cannot make the train hot. It still keeps the train in the checks between
    full polls, under the cooldown, until the station is observed or the stop has a track.
    """
    return _awaits_track(train.unlisted_stops, tracked.get(train.key, frozenset()), hot_secs)


def _awaits_track(stops: tuple[TrackStop, ...], known: frozenset[str], hot_secs: float) -> bool:
    return any(
        stop.code not in known
        and stop.secs <= (hot_secs if stop.hot_secs is None else stop.hot_secs)
        for stop in stops
    )


def _hot_secs(train: DueTrain, hot_secs: float) -> float:
    """Time until the train's soonest track stop, for ordering hot trains soonest first."""
    return min((stop.secs for stop in train.track_stops), default=hot_secs)


def choose_fetches(
    due: list[DueTrain],
    last_fetched: dict[tuple[str, str], float],
    now: datetime,
    *,
    hot_secs: float,
    cooldown_secs: float,
    max_fetches: int,
    tracked: Mapping[tuple[str, str], frozenset[str]] | None = None,
    hinted: Collection[tuple[str, str]] = (),
) -> list[DueTrain]:
    """Pick at most ``max_fetches`` trains, in this order.

    1. Hot trains (see :func:`is_hot`) with no stored document, soonest track stop first. One
       attempted within ``cooldown_secs`` (Amtrak answered without a usable document, or its
       in-page fetch failed) waits; a pick the browser could not run was never stamped, so it is
       taken again at once.
    2. Hot trains that have a document, once their cooldown has passed: oldest attempt first,
       then soonest track stop.
    3. Every other due train once its cooldown has passed, oldest attempt first. A warm train
       (see :func:`is_warm`) is one of these.

    ``tracked`` maps a train-day with a stored document to the station codes that already show a
    track in it. ``hinted`` train-days were named by an early-refresh hint. The service already
    spaced those by ``AMTRAK_TRACKS_MIN_GAP_SECS``, so they skip the cooldown. They go first in
    groups 2 and 3; group 1 ignores the hint and stays soonest first.
    """
    if max_fetches <= 0:
        return []
    tracked = tracked or {}
    now_ts = now.timestamp()

    def cooled(train: DueTrain) -> bool:
        previous = last_fetched.get(train.key)
        return previous is None or train.key in hinted or now_ts - previous >= cooldown_secs

    new_hot: list[DueTrain] = []
    stored_hot: list[DueTrain] = []
    ready: list[DueTrain] = []
    for train in due:
        if is_hot(train, tracked, hot_secs):
            if train.key not in tracked:
                if cooled(train):
                    new_hot.append(train)
            elif cooled(train):
                stored_hot.append(train)
        elif cooled(train):
            ready.append(train)
    new_hot.sort(
        key=lambda train: (
            _hot_secs(train, hot_secs),
            train.train_number,
            train.service_date,
        )
    )
    stored_hot.sort(
        key=lambda train: (
            train.key not in hinted,
            last_fetched.get(train.key, 0.0),
            _hot_secs(train, hot_secs),
            train.train_number,
            train.service_date,
        )
    )
    ready.sort(
        key=lambda train: (
            train.key not in hinted,
            last_fetched.get(train.key, 0.0),
            train.soonest_secs,
            train.train_number,
            train.service_date,
        )
    )
    return (new_hot + stored_hot + ready)[:max_fetches]


def _train_number(raw: object) -> str | None:
    if not isinstance(raw, str) or not raw.strip():
        return None
    text = raw.strip()
    if text[:1].upper() == "A" and text[1:].isdigit():
        text = text[1:]
    if not text.isdigit():
        return None
    return text.lstrip("0") or "0"


def origin_service_date(properties: dict) -> str | None:
    """The ``service-date`` for this train: the calendar day it left its origin.

    ``OrigSchDep`` is that instant. A stop after midnight still uses the origin day, which is
    what ``/statuses/{train}?service-date=`` expects. When the feed has no origin time, the
    first station time's calendar day is the fallback :func:`due_trains` already used.
    """
    raw = properties.get("OrigSchDep")
    if isinstance(raw, str) and len(raw) >= 10:
        parsed = _parse_date(raw[:10])
        if parsed is not None:
            return parsed
    for station in _stations(properties):
        for key in ("schdep", "scharr", "estdep", "estarr"):
            value = station.get(key)
            if isinstance(value, str) and len(value) >= 10:
                parsed = _parse_date(value[:10])
                if parsed is not None:
                    return parsed
    return None


def _service_date(properties: dict) -> str | None:
    return origin_service_date(properties)


def _parse_date(text: str) -> str | None:
    try:
        return datetime.strptime(text, "%m/%d/%Y").date().isoformat()
    except ValueError:
        return None


def _stations(properties: dict):
    numbered: list[tuple[int, object]] = []
    for key, value in properties.items():
        if isinstance(key, str) and key.startswith("Station") and key[7:].isdigit() and value:
            numbered.append((int(key[7:]), value))
    numbered.sort()
    for _, value in numbered:
        if isinstance(value, dict):
            yield value
        elif isinstance(value, str):
            try:
                parsed = json.loads(value)
            except json.JSONDecodeError:
                continue
            if isinstance(parsed, dict):
                yield parsed


def _station_code(station: dict) -> str | None:
    code = station.get("code")
    if isinstance(code, str) and code.strip():
        return code.strip().upper()
    return None


def _event_times(station: dict) -> list[datetime]:
    zone_code = station.get("tz")
    if _station_code(station) in _ARIZONA_STATIONS:
        zone = _PHOENIX
    else:
        code = zone_code.strip().upper() if isinstance(zone_code, str) else "E"
        zone = _ZONES.get(code, _ZONES["E"])
    moments: list[datetime] = []
    for estimated, scheduled in (("estarr", "scharr"), ("estdep", "schdep")):
        raw = station.get(estimated) or station.get(scheduled)
        if isinstance(raw, str) and raw.strip():
            moment = _parse_local(raw, zone)
            if moment is not None:
                moments.append(moment)
    return moments


@dataclass(frozen=True)
class FleetTrain:
    """One train-day in the maps feed, with its rail station codes in feed order.

    ``service_date`` is :func:`origin_service_date`. Bus-only features are not listed.
    """

    train_number: str
    service_date: str
    station_codes: tuple[str, ...]


def list_fleet(geojson: dict) -> tuple[list[FleetTrain], int]:
    """Rail train-days in ``geojson``, sorted by origin date then train number.

    The second value is how many features were skipped because every stop was a bus. The first
    feature wins when the same train-day appears twice. Order does not follow the feed.
    """
    found: dict[tuple[str, str], FleetTrain] = {}
    bus_only = 0
    features = geojson.get("features") if isinstance(geojson, dict) else None
    for feature in features if isinstance(features, list) else []:
        if not isinstance(feature, dict):
            continue
        properties = feature.get("properties")
        if not isinstance(properties, dict):
            continue
        number = _train_number(properties.get("TrainNum"))
        service_date = origin_service_date(properties)
        if number is None or service_date is None or number == "0":
            continue
        codes: list[str] = []
        rail = False
        bus = False
        for station in _stations(properties):
            if station.get("bus") is True:
                bus = True
                continue
            rail = True
            code = _station_code(station)
            if code and code not in codes:
                codes.append(code)
        if not rail:
            if bus:
                bus_only += 1
            continue
        key = (number, service_date)
        if key not in found:
            found[key] = FleetTrain(number, service_date, tuple(codes))
    trains = sorted(
        found.values(),
        key=lambda train: (train.service_date, int(train.train_number), train.train_number),
    )
    return trains, bus_only


def _parse_local(text: str, zone: ZoneInfo) -> datetime | None:
    naive = None
    for fmt in _TIME_FORMATS:
        try:
            naive = datetime.strptime(text.strip(), fmt)
            break
        except ValueError:
            continue
    if naive is None:
        return None
    return naive.replace(tzinfo=zone)

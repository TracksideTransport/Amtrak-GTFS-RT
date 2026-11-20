"""ETA window, hot-set priority, and the per-cycle fetch cap."""

from datetime import datetime, timedelta
from zoneinfo import ZoneInfo

from worker.tracks.lead_time import LeadProfile, StationWindow
from worker.tracks.select import choose_fetches, due_trains, is_hot

EASTERN = ZoneInfo("America/New_York")
NOW = datetime(2026, 10, 17, 17, 0, tzinfo=EASTERN)
BEFORE = timedelta(minutes=30)
AFTER = timedelta(minutes=10)


def _feature(number, origin, stations):
    properties = {"TrainNum": number, "OrigSchDep": origin}
    for index, station in enumerate(stations, start=1):
        properties[f"Station{index}"] = station
    return {"properties": properties}


def _station(code, **times):
    payload = {"code": code, "tz": "E", "bus": False}
    payload.update(times)
    return payload


def _due(features):
    return due_trains({"features": features}, NOW, before=BEFORE, after=AFTER)


def test_a_stop_inside_thirty_minutes_is_due_and_a_far_one_is_not():
    due = _due(
        [
            _feature(
                "167",
                "10/17/2026 4:25:00 PM",
                [
                    _station("PVD", scharr="10/17/2026 17:06:00", schdep="10/17/2026 17:08:00"),
                    _station("NLC", scharr="10/17/2026 18:07:00"),
                ],
            ),
            _feature(
                "99",
                "10/17/2026 12:00:00 PM",
                [_station("BOS", schdep="10/17/2026 16:00:00")],
            ),
        ]
    )
    by_number = {train.train_number: train for train in due}
    assert set(by_number) == {"167"}
    assert by_number["167"].service_date == "2026-10-17"
    assert by_number["167"].soonest_secs == 6 * 60


def test_an_estimate_replaces_the_schedule_and_bus_stops_do_not_count():
    late = _due(
        [
            _feature(
                "54",
                "10/17/2026 8:00:00 AM",
                [_station("NYP", scharr="10/17/2026 17:05:00", estarr="10/17/2026 17:40:00")],
            )
        ]
    )
    assert late == []
    early_estimate = _due(
        [
            _feature(
                "54",
                "10/17/2026 8:00:00 AM",
                [_station("NYP", scharr="10/17/2026 18:00:00", estarr="10/17/2026 17:05:00")],
            )
        ]
    )
    assert [train.train_number for train in early_estimate] == ["54"]
    bus_only = _due(
        [
            _feature(
                "3167",
                "10/17/2026 4:00:00 PM",
                [
                    {
                        "code": "BUS",
                        "tz": "E",
                        "bus": True,
                        "scharr": "10/17/2026 17:05:00",
                    }
                ],
            )
        ]
    )
    assert bus_only == []


def test_a_stop_just_after_arrival_stays_due():
    due = _due(
        [
            _feature(
                "167",
                "10/17/2026 4:25:00 PM",
                [_station("PVD", scharr="10/17/2026 16:55:00")],
            )
        ]
    )
    assert len(due) == 1
    assert due[0].soonest_secs == 5 * 60


def test_hot_trains_are_fetched_every_cycle_and_the_outer_window_waits():
    hot = _feature("167", "10/17/2026 4:25:00 PM", [_station("PVD", scharr="10/17/2026 17:06:00")])
    outer = _feature("66", "10/17/2026 3:00:00 PM", [_station("KIN", scharr="10/17/2026 17:25:00")])
    due = _due([hot, outer])
    first = choose_fetches(due, {}, NOW, hot_secs=12 * 60, cooldown_secs=90, max_fetches=24)
    assert [train.train_number for train in first] == ["167", "66"]
    just_fetched = {train.key: NOW.timestamp() for train in first}
    again = choose_fetches(
        due, just_fetched, NOW + timedelta(seconds=30), hot_secs=12 * 60, cooldown_secs=90, max_fetches=24
    )
    assert [train.train_number for train in again] == ["167"]
    cooled = choose_fetches(
        due, just_fetched, NOW + timedelta(seconds=90), hot_secs=12 * 60, cooldown_secs=90, max_fetches=24
    )
    assert [train.train_number for train in cooled] == ["167", "66"]


def test_the_cap_keeps_the_soonest_hot_trains():
    features = []
    for index, number in enumerate(range(1, 31)):
        moment = NOW + timedelta(seconds=20 * index)
        features.append(
            _feature(
                str(number),
                "10/17/2026 8:00:00 AM",
                [_station("PVD", scharr=moment.strftime("%m/%d/%Y %H:%M:%S"))],
            )
        )
    due = _due(features)
    chosen = choose_fetches(due, {}, NOW, hot_secs=12 * 60, cooldown_secs=90, max_fetches=24)
    assert len(chosen) == 24
    assert [train.train_number for train in chosen] == [str(number) for number in range(1, 25)]


def test_a_train_is_hot_only_while_an_upcoming_stop_lacks_a_track():
    train = _feature(
        "167",
        "10/17/2026 4:25:00 PM",
        [
            _station("PVD", scharr="10/17/2026 16:58:00"),
            _station("KIN", scharr="10/17/2026 17:06:00"),
        ],
    )
    due = _due([train])
    assert due[0].next_secs == 6 * 60
    assert due[0].upcoming == frozenset({"KIN"})
    key = due[0].key
    assert is_hot(due[0], {}, 12 * 60)
    assert is_hot(due[0], {key: frozenset({"PVD"})}, 12 * 60)
    assert not is_hot(due[0], {key: frozenset({"KIN"})}, 12 * 60)
    tracked = {key: frozenset({"KIN"})}
    first = choose_fetches(due, {}, NOW, hot_secs=720, cooldown_secs=90, max_fetches=24, tracked=tracked)
    assert [t.train_number for t in first] == ["167"]
    again = choose_fetches(
        due, {key: NOW.timestamp()}, NOW, hot_secs=720, cooldown_secs=90, max_fetches=24, tracked=tracked
    )
    assert again == []


def test_a_train_that_just_left_its_last_window_stop_rotates_under_the_cooldown():
    left = _feature("66", "10/17/2026 3:00:00 PM", [_station("PVD", schdep="10/17/2026 16:58:00")])
    due = _due([left])
    assert due[0].soonest_secs == 120
    assert due[0].next_secs is None
    fetched = {due[0].key: NOW.timestamp()}
    assert choose_fetches(due, fetched, NOW, hot_secs=720, cooldown_secs=90, max_fetches=24) == []
    later = NOW + timedelta(seconds=90)
    assert len(choose_fetches(due, fetched, later, hot_secs=720, cooldown_secs=90, max_fetches=24)) == 1


def test_arizona_stops_keep_standard_time_through_summer():
    summer = datetime(2026, 7, 1, 12, 0, tzinfo=ZoneInfo("UTC"))
    flagstaff = {"code": "FLG", "tz": "M", "bus": False, "scharr": "07/01/2026 05:10:00"}
    denver = {"code": "DEN", "tz": "M", "bus": False, "scharr": "07/01/2026 06:10:00"}
    feature = _feature("3", "06/30/2026 3:00:00 PM", [flagstaff, denver])
    due = due_trains({"features": [feature]}, summer, before=BEFORE, after=AFTER)
    # 05:10 MST and 06:10 MDT are both 12:10 UTC.
    assert due[0].next_secs == 10 * 60
    assert due[0].upcoming == frozenset({"FLG", "DEN"})


def test_the_fleet_train_list_is_read_like_the_map_feed():
    # The service writes amtrak-fleet-trains/v1 so the worker never polls getTrainsData.
    soon = int((NOW + timedelta(minutes=12)).timestamp())
    far = int((NOW + timedelta(hours=3)).timestamp())
    document = {
        "schema": "amtrak-fleet-trains/v1",
        "trains": [
            {"train_number": "163", "service_date": "2026-10-17",
             "stops": [{"code": "nhv", "times_unix": [soon]}, {"code": "NYP", "times_unix": [far]}]},
            {"train_number": "66", "service_date": "2026-10-17",
             "stops": [{"code": "BOS", "times_unix": [far]}]},
        ],
    }
    due = due_trains(document, NOW, before=BEFORE, after=AFTER)
    assert [(train.train_number, train.service_date) for train in due] == [("163", "2026-10-17")]
    assert due[0].upcoming == frozenset({"NHV"})
    assert due[0].next_secs == 12 * 60


def _fleet(*stops):
    return {
        "schema": "amtrak-fleet-trains/v1",
        "trains": [
            {
                "train_number": "167",
                "service_date": "2026-10-17",
                "stops": [
                    {"code": code, "times_unix": [int((NOW + timedelta(seconds=secs)).timestamp())]}
                    for code, secs in stops
                ],
            }
        ],
    }


def _due_with_profile(document):
    # PVD has a tighter 3-minute hot window; NHV and NLC keep the global 12 minutes.
    lead = LeadProfile({"PVD": StationWindow(window_before_secs=1800, hot_window_secs=180)})
    return due_trains(document, NOW, before=BEFORE, after=AFTER, lead=lead, hot=timedelta(minutes=12))


def test_a_global_station_inside_its_window_is_hot_beside_a_tighter_profile_station():
    # NHV is 400 s out (inside the global 720 s); PVD is 500 s out (outside its own 180 s).
    (train,) = _due_with_profile(_fleet(("NHV", 400), ("PVD", 500)))
    assert is_hot(train, {}, 12 * 60)


def test_a_profile_station_inside_its_own_window_is_hot_beside_a_later_global_station():
    (train,) = _due_with_profile(_fleet(("PVD", 120), ("NLC", 1500)))
    assert is_hot(train, {}, 12 * 60)


def test_a_profile_station_outside_its_own_window_is_not_hot():
    # Without the profile, PVD 300 s out would be inside the global 12-minute window.
    (train,) = _due_with_profile(_fleet(("PVD", 300), ("NLC", 1500)))
    assert not is_hot(train, {}, 12 * 60)


def test_a_hinted_train_skips_the_cooldown_and_goes_first():
    # The service already spaced train hints by AMTRAK_TRACKS_MIN_GAP_SECS, so a hint that lands
    # 60 s after the last fetch is acted on instead of being consumed and lost.
    left = _feature("66", "10/17/2026 3:00:00 PM", [_station("PVD", schdep="10/17/2026 16:58:00")])
    other = _feature("67", "10/17/2026 3:00:00 PM", [_station("PVD", schdep="10/17/2026 16:57:00")])
    due = _due([left, other])
    hinted_key = next(train.key for train in due if train.train_number == "66")
    fetched = {hinted_key: NOW.timestamp() - 60}
    picks = choose_fetches(
        due, fetched, NOW, hot_secs=720, cooldown_secs=90, max_fetches=1, hinted={hinted_key}
    )
    assert [train.train_number for train in picks] == ["66"]
    unhinted = choose_fetches(due, fetched, NOW, hot_secs=720, cooldown_secs=90, max_fetches=24)
    assert [train.train_number for train in unhinted] == ["67"]


def _fleet_trains(trains, track_stations=None):
    """An ``amtrak-fleet-trains/v1`` list. ``trains`` maps a number to ``(code, secs, boarding)``."""
    document = {
        "schema": "amtrak-fleet-trains/v1",
        "trains": [
            {
                "train_number": number,
                "service_date": "2026-10-17",
                "stops": [
                    {
                        "code": code,
                        "times_unix": [int((NOW + timedelta(seconds=secs)).timestamp())],
                        **({} if boarding else {"boarding": False}),
                    }
                    for code, secs, boarding in stops
                ],
            }
            for number, stops in trains.items()
        ],
    }
    if track_stations is not None:
        document["track_stations"] = list(track_stations)
    return document


def test_more_hot_trains_than_the_cap_still_fetch_one_without_a_document():
    # #120: 30 hot trains already hold a document, and 172 (11 minutes from Providence) has none.
    # Ordered only by next stop, 172 came 31st of 31 every cycle and never got one of 24 slots.
    trains = {str(number): [("NYP", 60 + number * 10, True)] for number in range(1, 31)}
    trains["172"] = [("PVD", 660, True)]
    due = due_trains(_fleet_trains(trains), NOW, before=BEFORE, after=AFTER)
    tracked = {train.key: frozenset() for train in due if train.train_number != "172"}
    picks = choose_fetches(
        due, {}, NOW, hot_secs=720, cooldown_secs=90, max_fetches=24, tracked=tracked
    )
    assert len(picks) == 24
    assert picks[0].train_number == "172"


def test_hot_trains_with_a_document_wait_out_the_cooldown_and_rotate():
    trains = {str(number): [("NYP", 60 + number * 10, True)] for number in range(1, 31)}
    due = due_trains(_fleet_trains(trains), NOW, before=BEFORE, after=AFTER)
    tracked = {train.key: frozenset() for train in due}
    # Trains 1-24 were fetched this cycle, 25-30 a cycle earlier.
    fetched = {
        train.key: NOW.timestamp() - (0 if int(train.train_number) <= 24 else 60) for train in due
    }
    soon = NOW + timedelta(seconds=30)
    picks = choose_fetches(
        due, fetched, soon, hot_secs=720, cooldown_secs=90, max_fetches=24, tracked=tracked
    )
    assert [train.train_number for train in picks] == [str(number) for number in range(25, 31)]
    later = NOW + timedelta(seconds=120)
    rotated = choose_fetches(
        due, fetched, later, hot_secs=720, cooldown_secs=90, max_fetches=24, tracked=tracked
    )
    # Oldest attempt first, so the six that waited go ahead of the 24 fetched after them.
    assert [train.train_number for train in rotated[:6]] == [str(n) for n in range(25, 31)]
    assert len(rotated) == 24


def test_a_hinted_train_goes_after_hot_trains_and_ahead_of_the_rotation():
    trains = {
        "1": [("PVD", 300, True)],  # hot, no document
        "2": [("PVD", 200, True)],  # hot, document, cooled down
        "3": [("PVD", 100, True)],  # hot, document, fetched 30 s ago, hinted
        "66": [("NYP", 1500, True)],  # not hot, fetched 30 s ago, hinted
        "67": [("NYP", 1400, True)],  # not hot, never fetched
    }
    due = due_trains(_fleet_trains(trains), NOW, before=BEFORE, after=AFTER)
    keys = {train.train_number: train.key for train in due}
    tracked = {keys["2"]: frozenset(), keys["3"]: frozenset()}
    fetched = {
        keys["2"]: NOW.timestamp() - 120,
        keys["3"]: NOW.timestamp() - 30,
        keys["66"]: NOW.timestamp() - 30,
    }
    picks = choose_fetches(
        due,
        fetched,
        NOW,
        hot_secs=720,
        cooldown_secs=90,
        max_fetches=24,
        tracked=tracked,
        hinted={keys["3"], keys["66"]},
    )
    assert [train.train_number for train in picks] == ["1", "3", "2", "66", "67"]
    unhinted = choose_fetches(
        due, fetched, NOW, hot_secs=720, cooldown_secs=90, max_fetches=24, tracked=tracked
    )
    assert [train.train_number for train in unhinted] == ["1", "2", "67"]


def test_a_stop_where_riders_cannot_board_does_not_make_a_train_hot():
    # Route 128 is deboard-only on Boston-bound trains, and Amtrak posts few tracks there.
    deboard = _fleet_trains({"172": [("RTE", 300, False), ("BOS", 1500, True)]})
    (train,) = due_trains(deboard, NOW, before=BEFORE, after=AFTER)
    assert train.upcoming == frozenset({"RTE", "BOS"})
    assert not is_hot(train, {}, 720)
    board = _fleet_trains({"172": [("RTE", 300, True), ("BOS", 1500, True)]})
    (train,) = due_trains(board, NOW, before=BEFORE, after=AFTER)
    assert is_hot(train, {}, 720)


def test_a_station_that_never_shows_a_track_does_not_make_a_train_hot():
    stops = {"172": [("KIN", 300, True), ("PVD", 1500, True)]}
    listed = _fleet_trains(stops, track_stations=["NYP", "PVD"])
    (train,) = due_trains(listed, NOW, before=BEFORE, after=AFTER)
    assert not is_hot(train, {}, 720)
    # A list without track_stations (an older service) treats every station as one with tracks.
    (train,) = due_trains(_fleet_trains(stops), NOW, before=BEFORE, after=AFTER)
    assert is_hot(train, {}, 720)
    soon = _fleet_trains({"172": [("PVD", 300, True)]}, track_stations=["NYP", "PVD"])
    (train,) = due_trains(soon, NOW, before=BEFORE, after=AFTER)
    assert is_hot(train, {}, 720)


def test_a_profile_station_skips_deboard_only_and_never_tracked_stops():
    # The lead-time branch: each stop is held to its own station's hot window.
    lead = LeadProfile({"PVD": StationWindow(window_before_secs=1800, hot_window_secs=180)})

    def due(document):
        (train,) = due_trains(
            document, NOW, before=BEFORE, after=AFTER, lead=lead, hot=timedelta(minutes=12)
        )
        return train

    assert is_hot(due(_fleet_trains({"172": [("PVD", 120, True)]})), {}, 720)
    assert not is_hot(due(_fleet_trains({"172": [("PVD", 120, False)]})), {}, 720)
    assert not is_hot(
        due(_fleet_trains({"172": [("PVD", 120, True)]}, track_stations=["NYP"])), {}, 720
    )
    # A later stop inside its own window still counts after a skipped one.
    later = _fleet_trains({"172": [("RTE", 100, False), ("NHV", 600, True)]})
    assert is_hot(due(later), {}, 720)


def test_a_track_on_a_later_stop_outside_the_hot_window_does_not_keep_a_train_hot():
    # PVD (5 min) has its track; NHV (20 min) is outside the 12-minute window.
    train = _fleet_trains({"172": [("PVD", 300, True), ("NHV", 1200, True)]})
    (due,) = due_trains(train, NOW, before=BEFORE, after=AFTER)
    assert not is_hot(due, {due.key: frozenset({"PVD"})}, 720)
    assert is_hot(due, {due.key: frozenset({"NHV"})}, 720)

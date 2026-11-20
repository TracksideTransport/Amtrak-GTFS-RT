"""One poll cycle with injected train data and status responses. No browser, no network."""

import asyncio
import json
import logging
import threading
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.request import Request, urlopen

import pytest

from worker.tracks.config import Config
from worker.tracks.poller import (
    TRAINS_ACCEPT,
    _default_trains,
    BrowserUnavailable,
    ResidentBrowser,
    StatusMiss,
    TrainListNotReady,
    document_from_eval,
    load_train_list_file,
    load_trains,
    poll_once,
    run_forever,
    should_abort,
    should_block,
    status_url,
    take_early_hints,
)
from worker.tracks.select import DueTrain
from worker.tracks.store import TrackStore

NOW = datetime(2026, 10, 17, 21, 0, tzinfo=timezone.utc)


def _geojson():
    return {
        "features": [
            {
                "properties": {
                    "TrainNum": "0167",
                    "OrigSchDep": "10/17/2026 4:25:00 PM",
                    "Station1": {
                        "code": "PVD",
                        "tz": "E",
                        "bus": False,
                        "scharr": "10/17/2026 17:06:00",
                    },
                }
            }
        ]
    }


def test_status_url_and_blocked_types():
    assert status_url("167", "2026-10-17") == (
        "https://www.amtrak.com/dotcom/travel-service/statuses/167?service-date=2026-10-17"
    )
    assert should_block("image") is True
    assert should_block("script") is False
    assert should_block("fetch") is False


def test_a_cycle_stores_the_document_and_a_miss_keeps_it():
    store = TrackStore()
    seen: list[DueTrain] = []

    async def trains(_cfg):
        return _geojson()

    async def status(_cfg, train: DueTrain):
        seen.append(train)
        return {"data": [{"travelService": {"number": train.train_number, "date": train.service_date}}]}

    cfg = Config(max_fetches_per_cycle=24, hot_window_secs=720, cooldown_secs=90)
    asyncio.run(
        poll_once(store, cfg, {}, now=NOW, trains_fetcher=trains, status_fetcher=status)
    )
    assert [train.train_number for train in seen] == ["167"]
    assert seen[0].service_date == "2026-10-17"
    snapshot = store.snapshot(NOW.timestamp(), 7200)
    assert snapshot is not None
    document = snapshot[1][0]
    assert document["data"][0]["travelService"]["number"] == "167"

    async def fail(_cfg, _train):
        return None

    later = NOW + timedelta(seconds=60)
    asyncio.run(
        poll_once(store, cfg, {}, now=later, trains_fetcher=trains, status_fetcher=fail)
    )
    kept = store.snapshot(later.timestamp(), 7200)
    assert kept is not None
    assert kept[1][0]["data"][0]["travelService"]["number"] == "167"


def test_a_failed_train_list_does_not_replace_the_snapshot():
    store = TrackStore()
    store.mark_success(1_000)

    async def boom(_cfg):
        raise RuntimeError("maps down")

    async def unused(_cfg, _train):
        raise AssertionError("status should not be called")

    cfg = Config()

    async def once():
        await run_forever(
            store,
            cfg,
            trains_fetcher=boom,
            status_fetcher=unused,
            sleep=_no_sleep,
            clock=lambda: NOW,
            iterations=1,
        )

    asyncio.run(once())
    assert store.snapshot(NOW.timestamp(), 7200)[0] == 1_000


async def _no_sleep(_seconds: float) -> None:
    return None


def _status_for(numbers):
    async def status(_cfg, train: DueTrain):
        numbers.append(train.train_number)
        return {"data": []}

    return status


def _two_trains():
    return {
        "features": [
            {
                "properties": {
                    "TrainNum": number,
                    "OrigSchDep": "10/17/2026 4:25:00 PM",
                    "Station1": {
                        "code": "PVD",
                        "tz": "E",
                        "bus": False,
                        "scharr": "10/17/2026 17:06:00",
                    },
                }
            }
            for number in ("167", "171")
        ]
    }


def test_a_cycle_where_every_fetch_fails_is_not_a_success():
    store = TrackStore()

    async def trains(_cfg):
        return _two_trains()

    async def rejected(_cfg, _train):
        raise StatusMiss("http 403")

    last_fetched: dict = {("9", "2026-10-16"): 1.0}
    asyncio.run(
        poll_once(store, Config(), last_fetched, now=NOW, trains_fetcher=trains, status_fetcher=rejected)
    )
    assert store.snapshot(NOW.timestamp(), 7200) is None
    # Failed attempts still start the cooldown, and trains no longer due are forgotten.
    assert set(last_fetched) == {("167", "2026-10-17"), ("171", "2026-10-17")}

    async def nothing_due(_cfg):
        return {"features": []}

    asyncio.run(
        poll_once(store, Config(), {}, now=NOW, trains_fetcher=nothing_due, status_fetcher=rejected)
    )
    assert store.snapshot(NOW.timestamp(), 7200) == (NOW.timestamp(), [])


def test_a_browser_failure_ends_the_cycle():
    store = TrackStore()
    calls: list[str] = []

    async def trains(_cfg):
        return _two_trains()

    async def down(_cfg, train: DueTrain):
        calls.append(train.train_number)
        raise BrowserUnavailable("TargetClosedError")

    asyncio.run(poll_once(store, Config(), {}, now=NOW, trains_fetcher=trains, status_fetcher=down))
    assert calls == ["167"]


def test_every_cycle_is_recorded_for_liveness_even_when_it_fails():
    store = TrackStore(started_at=0)

    async def boom(_cfg):
        raise RuntimeError("maps down")

    asyncio.run(
        run_forever(
            store,
            Config(),
            trains_fetcher=boom,
            status_fetcher=_status_for([]),
            sleep=_no_sleep,
            clock=lambda: NOW,
            iterations=1,
        )
    )
    assert store.last_cycle() == NOW.timestamp()
    assert store.consecutive_failures() == 1


def test_repeated_failed_polls_are_counted_and_a_success_clears_them():
    store = TrackStore(started_at=0)

    async def boom(_cfg):
        raise RuntimeError("maps down")

    async def trains(_cfg):
        return {"features": []}

    async def run(fetcher, iterations):
        await run_forever(
            store,
            Config(),
            trains_fetcher=fetcher,
            status_fetcher=_status_for([]),
            sleep=_no_sleep,
            clock=lambda: NOW,
            iterations=iterations,
        )

    asyncio.run(run(boom, 2))
    assert store.consecutive_failures() == 2
    asyncio.run(run(trains, 1))
    assert store.consecutive_failures() == 0


class _MapsFeed(BaseHTTPRequestHandler):
    """406 for ``Accept: application/json``; 200 ``text/plain`` JSON otherwise."""

    seen: list[str] = []

    def do_GET(self):  # noqa: N802 — http.server's method name
        accept = self.headers.get("Accept", "")
        type(self).seen.append(accept)
        if accept == "application/json":
            body = b"not acceptable"
            status = 406
        else:
            body = b'{"features":[]}'
            status = 200
        self.send_response(status)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args: object) -> None:
        return


def test_load_trains_sends_an_accept_header_the_maps_feed_allows():
    _MapsFeed.seen = []
    server = ThreadingHTTPServer(("127.0.0.1", 0), _MapsFeed)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        url = f"http://127.0.0.1:{server.server_address[1]}/trains/getTrainsData"
        rejected = Request(url, headers={"Accept": "application/json"})
        with pytest.raises(HTTPError) as error:
            urlopen(rejected, timeout=5)
        assert error.value.code == 406
        assert load_trains(Config(trains_url=url)) == {"features": []}
        assert TRAINS_ACCEPT == "*/*"
        assert _MapsFeed.seen == ["application/json", "*/*"]
    finally:
        server.shutdown()
        server.server_close()


def test_routing_blocks_page_weight_and_third_parties():
    cfg = Config()
    assert should_abort(cfg, "image", "https://www.amtrak.com/logo.png")
    assert not should_abort(cfg, "script", "https://www.amtrak.com/app.js")
    assert not should_abort(cfg, "fetch", "https://assets.amtrak.com/x")
    assert should_abort(cfg, "script", "https://tags.example.net/t.js")
    assert should_abort(cfg, "script", "https://notamtrak.com/t.js")
    open_cfg = Config(block_third_party=False, block_subresources=False)
    assert not should_abort(open_cfg, "script", "https://tags.example.net/t.js")
    assert not should_abort(open_cfg, "image", "https://www.amtrak.com/logo.png")


def test_unusable_answers_name_their_reason():
    assert document_from_eval({"status": 200, "body": '{"data": []}'}) == {"data": []}
    for raw, reason in [
        (None, "no response"),
        ({"status": 403, "body": "denied"}, "http 403"),
        ({"status": 200, "body": "<html>"}, "not json"),
        ({"status": 200, "body": "{}"}, "no data"),
    ]:
        with pytest.raises(StatusMiss, match=reason):
            document_from_eval(raw)


class _FakePage:
    def __init__(self, result):
        self.result = result

    async def evaluate(self, _script, _args):
        if isinstance(self.result, Exception):
            raise self.result
        return self.result


def test_browser_relaunch_backs_off_and_resets_after_a_success():
    clock = [1_000.0]
    cfg = Config(relaunch_backoff_max_secs=150, browser_max_age_secs=3_600)
    browser = ResidentBrowser(cfg, monotonic=lambda: clock[0])
    launches: list[float] = []
    pages: list[object] = [RuntimeError("launch"), RuntimeError("launch")]

    async def launch():
        launches.append(clock[0])
        outcome = pages.pop(0) if pages else _FakePage({"status": 200, "body": '{"data": []}'})
        if isinstance(outcome, Exception):
            raise outcome
        return outcome

    browser._launch = launch
    train = DueTrain("167", "2026-10-17", 60.0)

    async def attempt():
        return await browser.fetch(cfg, train)

    with pytest.raises(BrowserUnavailable):
        asyncio.run(attempt())
    assert browser._backoff == 60
    clock[0] += 30
    with pytest.raises(BrowserUnavailable, match="backoff"):
        asyncio.run(attempt())
    assert len(launches) == 1
    clock[0] += 31
    with pytest.raises(BrowserUnavailable):
        asyncio.run(attempt())
    assert browser._backoff == 120
    clock[0] += 121
    assert asyncio.run(attempt()) == {"data": []}
    assert browser._backoff == 0
    assert len(launches) == 3

    # A hung or failed in-page fetch closes the browser and backs off again; capped at the max.
    browser._page = _FakePage(asyncio.TimeoutError())
    with pytest.raises(BrowserUnavailable):
        asyncio.run(attempt())
    assert browser._page is None
    browser._backoff = 120
    browser._next_launch = 0
    pages.append(RuntimeError("launch"))
    with pytest.raises(BrowserUnavailable):
        asyncio.run(attempt())
    assert browser._backoff == 150


def test_the_browser_is_recycled_after_its_maximum_age():
    clock = [0.0]
    cfg = Config(browser_max_age_secs=100)
    browser = ResidentBrowser(cfg, monotonic=lambda: clock[0])
    launches: list[float] = []

    async def launch():
        launches.append(clock[0])
        return _FakePage({"status": 200, "body": '{"data": []}'})

    browser._launch = launch
    train = DueTrain("167", "2026-10-17", 60.0)
    asyncio.run(browser.fetch(cfg, train))
    clock[0] = 50
    asyncio.run(browser.fetch(cfg, train))
    clock[0] = 100
    asyncio.run(browser.fetch(cfg, train))
    assert launches == [0.0, 100]


def test_a_worker_beside_the_service_without_a_train_list_never_polls_the_map(tmp_path, monkeypatch):
    monkeypatch.setenv("AMTRAK_OUTPUT_DIR", str(tmp_path))
    monkeypatch.delenv("AMTRAK_FLEET_TRAINS_FILE", raising=False)

    def no_network(*_args, **_kwargs):
        raise AssertionError("the embedded poller must not GET getTrainsData")

    monkeypatch.setattr("urllib.request.urlopen", no_network)
    with pytest.raises(OSError):
        asyncio.run(_default_trains(Config()))


def test_one_container_poller_reads_the_service_train_list_file(tmp_path, monkeypatch):
    document = {"schema": "amtrak-fleet-trains/v1", "trains": [{"train_number": "163"}]}
    path = tmp_path / "tracks" / "fleet-trains.json"
    path.parent.mkdir()
    path.write_text(json.dumps(document), encoding="utf-8")
    monkeypatch.setenv("AMTRAK_OUTPUT_DIR", str(tmp_path))
    monkeypatch.setenv("AMTRAK_FLEET_TRAINS_FILE", str(path))

    def no_network(*_args, **_kwargs):
        raise AssertionError("the embedded poller must not GET getTrainsData")

    monkeypatch.setattr("urllib.request.urlopen", no_network)
    assert asyncio.run(_default_trains(Config())) == document


def test_missing_train_list_file_fails_the_cycle_without_polling(tmp_path, monkeypatch):
    monkeypatch.setenv("AMTRAK_FLEET_TRAINS_FILE", str(tmp_path / "absent.json"))

    def no_network(*_args, **_kwargs):
        raise AssertionError("the embedded poller must not GET getTrainsData")

    monkeypatch.setattr("urllib.request.urlopen", no_network)
    with pytest.raises(OSError):
        asyncio.run(_default_trains(Config()))


def _run_with_train_list(path, monkeypatch, caplog):
    monkeypatch.setenv("AMTRAK_OUTPUT_DIR", str(path.parent))
    monkeypatch.setenv("AMTRAK_FLEET_TRAINS_FILE", str(path))

    def no_network(*_args, **_kwargs):
        raise AssertionError("the embedded poller must not GET getTrainsData")

    monkeypatch.setattr("urllib.request.urlopen", no_network)
    store = TrackStore(started_at=0)
    with caplog.at_level(logging.INFO, logger="worker.tracks.poller"):
        asyncio.run(
            run_forever(
                store,
                Config(),
                status_fetcher=_status_for([]),
                sleep=_no_sleep,
                clock=lambda: NOW,
                iterations=1,
            )
        )
    return store, [r for r in caplog.records if r.name == "worker.tracks.poller"]


def test_a_cold_start_without_a_train_list_logs_one_info_line_and_no_traceback(
    tmp_path, monkeypatch, caplog
):
    store, records = _run_with_train_list(tmp_path / "fleet-trains.json", monkeypatch, caplog)
    assert [(r.levelno, r.getMessage()) for r in records] == [
        (logging.INFO, "waiting for the service's first train list")
    ]
    assert records[0].exc_info is None
    assert store.consecutive_failures() == 1  # not a success; the next tick retries


@pytest.mark.parametrize(
    "body",
    ["{not json", "[]", json.dumps({"schema": "amtrak-fleet-trains/v0", "trains": []})],
    ids=["not-json", "not-an-object", "wrong-schema"],
)
def test_an_existing_but_unusable_train_list_is_still_an_error(
    tmp_path, monkeypatch, caplog, body
):
    path = tmp_path / "fleet-trains.json"
    path.write_text(body, encoding="utf-8")
    _store, records = _run_with_train_list(path, monkeypatch, caplog)
    assert [r.levelno for r in records] == [logging.ERROR]
    assert records[0].getMessage() == "track poll failed; keeping the last snapshot"
    with pytest.raises(ValueError):
        load_train_list_file(str(path))


def test_an_unreadable_train_list_is_an_error_not_a_cold_start(tmp_path):
    with pytest.raises(TrainListNotReady):
        load_train_list_file(str(tmp_path / "absent.json"))
    with pytest.raises(OSError) as raised:
        load_train_list_file(str(tmp_path))  # a directory: exists, cannot be read as a file
    assert not isinstance(raised.value, TrainListNotReady)


def test_a_cycle_without_a_train_list_retries_the_full_poll_on_the_next_tick(monkeypatch):
    fulls = []

    async def fake_poll_once(store, cfg, last_fetched, *, now, trains_fetcher, status_fetcher, full):
        fulls.append(full)
        if len(fulls) == 1:
            raise FileNotFoundError("no train list yet")
        return True

    async def no_sleep(_secs):
        return None

    monkeypatch.setattr("worker.tracks.poller.poll_once", fake_poll_once)
    cfg = Config(poll_interval_secs=60, base_interval_secs=900)
    asyncio.run(
        run_forever(
            TrackStore(),
            cfg,
            trains_fetcher=lambda _cfg: None,
            status_fetcher=lambda *_args: None,
            sleep=no_sleep,
            iterations=4,
        )
    )
    # The failed first full poll is retried at once; then the base interval applies.
    assert fulls == [True, True, False, False]


def test_full_polls_run_every_base_interval_and_hot_checks_between(monkeypatch):
    fulls = []

    async def fake_poll_once(store, cfg, last_fetched, *, now, trains_fetcher, status_fetcher, full):
        fulls.append(full)
        return True

    async def no_sleep(_secs):
        return None

    monkeypatch.setattr("worker.tracks.poller.poll_once", fake_poll_once)
    cfg = Config(poll_interval_secs=60, base_interval_secs=180)
    asyncio.run(
        run_forever(
            TrackStore(),
            cfg,
            trains_fetcher=lambda _cfg: None,
            status_fetcher=lambda *_args: None,
            sleep=no_sleep,
            iterations=7,
        )
    )
    # AMTRAK_TRACKS_BASE_INTERVAL_SECS=180 with POLL_INTERVAL_SECS=60: a full poll, two
    # hot-station checks, then the next full poll.
    assert fulls == [True, False, False, True, False, False, True]


def _queue_hint(root, name, trains=(), stations=()):
    queue = root / "tracks" / "early-refresh"
    queue.mkdir(parents=True, exist_ok=True)
    (queue / name).write_text(json.dumps({"trains": list(trains), "stations": list(stations)}))


def test_early_hints_are_consumed_once(tmp_path, monkeypatch):
    monkeypatch.setenv("AMTRAK_OUTPUT_DIR", str(tmp_path))
    _queue_hint(tmp_path, "1-0.json", trains=["163"])
    _queue_hint(tmp_path, "2-1.json", trains=["66"], stations=["nyp"])
    (tmp_path / "tracks" / "early-refresh" / ".3-2.json").write_text("partial")
    assert take_early_hints() == ({"163", "66"}, {"NYP"})
    assert take_early_hints() == (set(), set())
    assert [p.name for p in (tmp_path / "tracks" / "early-refresh").iterdir()] == [".3-2.json"]


def test_station_hint_picks_due_trains_stopping_there(tmp_path, monkeypatch):
    monkeypatch.setenv("AMTRAK_OUTPUT_DIR", str(tmp_path))
    _queue_hint(tmp_path, "1-0.json", stations=["NYP"])
    soon = int((NOW + timedelta(minutes=20)).timestamp())  # due, not hot
    document = {
        "schema": "amtrak-fleet-trains/v1",
        "trains": [
            {"train_number": "163", "service_date": "2026-10-17",
             "stops": [{"code": "NYP", "times_unix": [soon]}]},
            {"train_number": "66", "service_date": "2026-10-17",
             "stops": [{"code": "BOS", "times_unix": [soon]}]},
        ],
    }
    fetched: list[str] = []

    async def trains(_cfg):
        return document

    async def status(_cfg, train):
        fetched.append(train.train_number)
        raise StatusMiss("no document in this test")

    asyncio.run(
        poll_once(TrackStore(), Config(), {}, now=NOW, trains_fetcher=trains,
                  status_fetcher=status, full=False)
    )
    assert fetched == ["163"]


def _hot_fleet(*numbers):
    """Fleet trains whose next stop, Providence, is inside the hot window, in train order."""
    return {
        "schema": "amtrak-fleet-trains/v1",
        "trains": [
            {
                "train_number": number,
                "service_date": "2026-10-17",
                "stops": [
                    {"code": "PVD", "times_unix": [int((NOW + timedelta(seconds=60 + 10 * index)).timestamp())]}
                ],
            }
            for index, number in enumerate(numbers)
        ],
    }


def test_a_browser_failure_leaves_later_picks_unstamped_and_first_next_cycle():
    # #120: 24 picked, 3 stored, then the browser failed. The other 21 were stamped as attempted,
    # so the next cycle went back to the trains that already had documents.
    store = TrackStore()
    last_fetched: dict = {}
    calls: list[str] = []
    numbers = [str(number) for number in range(1, 7)]

    async def trains(_cfg):
        return _hot_fleet(*numbers)

    async def fourth_fails(_cfg, train: DueTrain):
        calls.append(train.train_number)
        if len(calls) == 4:
            raise BrowserUnavailable("Error")
        return {"data": []}

    cfg = Config(max_fetches_per_cycle=4, hot_window_secs=720, cooldown_secs=90)
    asyncio.run(
        poll_once(store, cfg, last_fetched, now=NOW, trains_fetcher=trains, status_fetcher=fourth_fails)
    )
    assert calls == ["1", "2", "3", "4"]
    assert set(last_fetched) == {("1", "2026-10-17"), ("2", "2026-10-17"), ("3", "2026-10-17")}

    calls.clear()

    async def answers(_cfg, train: DueTrain):
        calls.append(train.train_number)
        return {"data": []}

    later = NOW + timedelta(seconds=60)
    asyncio.run(
        poll_once(store, cfg, last_fetched, now=later, trains_fetcher=trains, status_fetcher=answers)
    )
    assert calls == ["4", "5", "6"]


def test_each_cycle_logs_its_picks_with_their_outcomes(caplog):
    store = TrackStore()
    hot = _hot_fleet("172", "86", "2154", "95")
    hot["trains"].append(
        {
            "train_number": "66",
            "service_date": "2026-10-17",
            "stops": [{"code": "NYP", "times_unix": [int((NOW + timedelta(minutes=25)).timestamp())]}],
        }
    )

    async def trains(_cfg):
        return hot

    async def mixed(_cfg, train: DueTrain):
        if train.train_number == "86":
            raise StatusMiss("http 404")
        if train.train_number == "2154":
            return None
        if train.train_number == "95":
            raise BrowserUnavailable("Error")
        return {"data": []}

    caplog.set_level(logging.INFO, logger="worker.tracks.poller")
    asyncio.run(poll_once(store, Config(), {}, now=NOW, trains_fetcher=trains, status_fetcher=mixed))
    lines = [record.getMessage() for record in caplog.records]
    assert (
        "track poll picks: 172 hot stored, 86 hot rejected (http 404), 2154 hot rejected (empty), "
        "95 hot browser unavailable (Error), 66 ready skipped (browser unavailable)"
    ) in lines
    assert any(line.startswith("track poll due=5 picked=5 stored=1 ") for line in lines)


def test_a_fresh_install_still_checks_a_boarding_stop_at_an_unobserved_station():
    # track_stations holds only the seeded stations. Providence is 8 minutes out and has no
    # observed platform yet; v0.8.0 fetched such a train every minute, so a hot check between
    # full polls must still pick it once its cooldown has passed.
    document = {
        "schema": "amtrak-fleet-trains/v1",
        "track_stations": ["NHV", "NWK", "NYP"],
        "trains": [
            {
                "train_number": "172",
                "service_date": "2026-10-17",
                "stops": [{"code": "PVD", "times_unix": [int((NOW + timedelta(minutes=8)).timestamp())]}],
            },
            {
                "train_number": "174",
                "service_date": "2026-10-17",
                "stops": [
                    {"code": "RTE", "boarding": False,
                     "times_unix": [int((NOW + timedelta(minutes=8)).timestamp())]}
                ],
            },
        ],
    }
    fetched: list[str] = []

    async def trains(_cfg):
        return document

    async def status(_cfg, train):
        fetched.append(train.train_number)
        raise StatusMiss("http 404")

    last_fetched: dict = {}

    def check(at):
        fetched.clear()
        asyncio.run(
            poll_once(TrackStore(), Config(), last_fetched, now=at, trains_fetcher=trains,
                      status_fetcher=status, full=False)
        )
        return list(fetched)

    assert check(NOW) == ["172"]
    assert check(NOW + timedelta(seconds=60)) == []
    assert check(NOW + timedelta(seconds=90)) == ["172"]

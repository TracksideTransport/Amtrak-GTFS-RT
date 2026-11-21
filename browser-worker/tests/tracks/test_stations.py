"""The station-name table and directory the worker keeps for the service."""

import json
import os

from worker.tracks.stations import DIRECTORY_URL, StationsStore

DAY = 1_209_600


def _doc(code="BOS", **fields):
    return {"data": [{"stops": [{"station": {"code": code, **fields}}]}]}


def _directory(*codes):
    return json.dumps([{"code": c, "name": f"{c} Station", "city": "X"} for c in codes])


def test_directory_url():
    assert DIRECTORY_URL == "https://www.amtrak.com/services/data.stations.json"


def test_seen_merge_never_erases():
    store = StationsStore(None)
    store.record_document(_doc(name="Boston South Station", facility="South Station"))
    store.record_document(_doc(name="Boston", displayName="Boston South"))
    seen = store.snapshot()["seen"]["BOS"]
    assert seen == {
        "name": "Boston",
        "displayName": "Boston South",
        "facility": "South Station",
    }
    store.record_document(_doc(name=None, facility=""))
    assert store.snapshot()["seen"]["BOS"]["facility"] == "South Station"
    assert store.snapshot()["seen"]["BOS"]["name"] == "Boston"


def test_directory_rejects_bad_bodies():
    store = StationsStore(None)
    assert store.accept_directory(_directory("BOS", "NYP"), 100.0)
    for bad in ["[]", "{}", '[{"code": "BO"}]', "not json", '["BOS"]']:
        assert not store.accept_directory(bad, 200.0)
    snap = store.snapshot()
    assert [row["code"] for row in snap["directory"]] == ["BOS", "NYP"]
    assert snap["directory_fetched_at"] == "1970-01-01T00:01:40Z"


def test_directory_due():
    store = StationsStore(None)
    assert store.directory_due(0.0, DAY)
    t = 1_000.0
    assert store.accept_directory(_directory("BOS"), t)
    assert not store.directory_due(t + DAY - 1, DAY)
    assert store.directory_due(t + DAY, DAY)
    f = t + 10
    assert not store.accept_directory("nope", f)
    assert not store.directory_due(f + 3599, DAY)
    assert store.directory_due(f + 3600, DAY)


def test_directory_due_after_failure_without_a_directory():
    store = StationsStore(None)
    assert not store.accept_directory("nope", 500.0)
    assert not store.directory_due(500.0 + 3599, DAY)
    assert store.directory_due(500.0 + 3600, DAY)


def test_snapshot_is_none_when_empty_and_display_name_ladder():
    store = StationsStore(None)
    assert store.snapshot() is None
    assert store.display_name("BOS") == "BOS"
    store.accept_directory(_directory("BOS"), 1.0)
    assert store.display_name("BOS") == "BOS Station"
    store.record_document(_doc(name="Boston"))
    assert store.display_name("BOS") == "Boston"
    store.record_document(_doc(displayName="Boston South"))
    assert store.display_name("BOS") == "Boston South"


def test_persistence_round_trip(tmp_path):
    root = str(tmp_path / "tracks")
    store = StationsStore(root)
    store.record_document(_doc(name="Boston", audioName="Boston South Station"))
    assert store.accept_directory(_directory("BOS", "BBY"), 1_000.0)
    again = StationsStore(root)
    assert again.snapshot() == store.snapshot()
    assert again.snapshot()["directory_fetched_at"] == "1970-01-01T00:16:40Z"
    assert not again.directory_due(1_000.0 + 5, DAY)
    assert not [n for n in os.listdir(root) if n.startswith(".") or n.endswith(".tmp")]


def test_corrupt_seen_file_is_ignored(tmp_path):
    root = tmp_path / "tracks"
    root.mkdir()
    (root / "stations-seen.json").write_text("{garbage")
    (root / "stations-directory.json").write_text("[1,")
    store = StationsStore(str(root))
    assert store.snapshot() is None
    assert (root / "stations-seen.json.invalid").exists()
    assert (root / "stations-directory.json.invalid").exists()
    store.record_document(_doc(name="Boston"))
    assert StationsStore(str(root)).snapshot()["seen"]["BOS"] == {"name": "Boston"}


def test_refresh_directory_fetches_once_and_backs_off_on_failure():
    import asyncio

    from worker.tracks.config import Config
    from worker.tracks.poller import refresh_directory

    class Browser:
        def __init__(self, status, body):
            self.reply, self.calls = (status, body), []

        async def fetch_json(self, url, timeout_secs):
            self.calls.append(url)
            return self.reply

    cfg = Config()
    assert cfg.stations_refresh_secs == DAY
    store = StationsStore(None)
    bad = Browser(503, "")
    asyncio.run(refresh_directory(store, bad, cfg, 100.0))
    asyncio.run(refresh_directory(store, bad, cfg, 160.0))
    assert bad.calls == [DIRECTORY_URL]
    good = Browser(200, _directory("BOS"))
    asyncio.run(refresh_directory(store, good, cfg, 100.0 + 3600))
    asyncio.run(refresh_directory(store, good, cfg, 100.0 + 3700))
    assert good.calls == [DIRECTORY_URL]
    assert store.snapshot()["directory"][0]["code"] == "BOS"


def test_snapshot_is_detached_from_later_records():
    store = StationsStore(None)
    store.record_document(_doc(name="Boston"))
    taken = store.snapshot()
    store.record_document(_doc(facility="South Station"))
    store.record_document(_doc("NYP", name="New York"))
    assert taken["seen"] == {"BOS": {"name": "Boston"}}
    assert store.snapshot()["seen"]["BOS"]["facility"] == "South Station"


class _Browser:
    def __init__(self, status, body):
        self.reply, self.calls = (status, body), []

    async def fetch_json(self, url, timeout_secs):
        self.calls.append(url)
        return self.reply


def test_manual_request_fetches_once_despite_backoff(tmp_path):
    import asyncio

    from worker.tracks.config import Config
    from worker.tracks.poller import refresh_directory

    f = 1_000_000.0
    store = StationsStore(str(tmp_path))
    cfg = Config()
    bad = _Browser(503, "")
    asyncio.run(refresh_directory(store, bad, cfg, f))
    assert len(bad.calls) == 1
    # Not due by backoff at f+20, and no request yet: nothing is fetched.
    asyncio.run(refresh_directory(store, bad, cfg, f + 20))
    assert len(bad.calls) == 1
    (tmp_path / "station-names-refresh-requested").write_text(f"{int(f + 10)}\n")
    assert store.refresh_requested() == f + 10
    asyncio.run(refresh_directory(store, bad, cfg, f + 20))
    assert len(bad.calls) == 2
    # The same request is acted on once, even though that fetch failed.
    asyncio.run(refresh_directory(store, bad, cfg, f + 30))
    assert len(bad.calls) == 2
    # The worker never deletes the file.
    assert (tmp_path / "station-names-refresh-requested").exists()
    # A newer request fetches again, and a good answer is stored.
    good = _Browser(200, _directory("BOS"))
    (tmp_path / "station-names-refresh-requested").write_text(f"{int(f + 40)}\n")
    asyncio.run(refresh_directory(store, good, cfg, f + 50))
    assert len(good.calls) == 1
    assert store.snapshot()["directory"][0]["code"] == "BOS"


def test_refresh_requested_reads_time_mtime_or_none(tmp_path):
    assert StationsStore(None).refresh_requested() is None
    store = StationsStore(str(tmp_path))
    assert store.refresh_requested() is None
    path = tmp_path / "station-names-refresh-requested"
    path.write_text("1234\n")
    assert store.refresh_requested() == 1234.0
    path.write_text("garbage")
    os.utime(path, (5000, 5000))
    assert store.refresh_requested() == 5000.0


def test_no_request_file_behaves_as_before(tmp_path):
    import asyncio

    from worker.tracks.config import Config
    from worker.tracks.poller import refresh_directory

    store = StationsStore(str(tmp_path))
    good = _Browser(200, _directory("BOS"))
    asyncio.run(refresh_directory(store, good, Config(), 100.0))
    asyncio.run(refresh_directory(store, good, Config(), 160.0))
    assert len(good.calls) == 1

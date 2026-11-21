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

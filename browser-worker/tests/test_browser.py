"""One Chromium for both sources, driven against a fake browser (no Playwright, no network)."""

import asyncio
import dataclasses
import logging

import pytest

from worker.browser import MAX_ERROR_CHARS, SharedBrowser, describe_error
from worker.notices import poller as notices_poller
from worker.notices.config import Config as NoticesConfig
from worker.notices.store import SnapshotStore
from worker.tracks.config import Config as TracksConfig
from worker.tracks.poller import BrowserUnavailable, ResidentBrowser
from worker.tracks.select import DueTrain

NOTICES_HTML = "<html><div class='na-service-alert'>Northeast Regional</div></html>"


class FakePage:
    def __init__(self):
        self.closed = False
        self.visited: list[str] = []

    async def goto(self, url, **_kwargs):
        self.visited.append(url)

    async def content(self):
        return NOTICES_HTML

    async def query_selector(self, _selector):
        return object()

    async def evaluate(self, _script, _args):
        return {"status": 200, "body": '{"data": []}'}

    async def close(self):
        self.closed = True


class FakeContext:
    def __init__(self):
        self.closed = False
        self.pages: list[FakePage] = []

    async def route(self, _pattern, _handler):
        return None

    async def new_page(self):
        page = FakePage()
        self.pages.append(page)
        return page

    async def close(self):
        self.closed = True


class FakeBrowser:
    def __init__(self):
        self.connected = True
        self.closed = False
        self.contexts: list[FakeContext] = []

    def is_connected(self):
        return self.connected and not self.closed

    async def new_context(self, **_kwargs):
        context = FakeContext()
        self.contexts.append(context)
        return context

    async def close(self):
        self.closed = True


class FakePlaywright:
    def __init__(self):
        self.stopped = False

    async def stop(self):
        self.stopped = True


class Launcher:
    """A ``start`` for :class:`SharedBrowser` that records every Chromium it would launch."""

    def __init__(self, failures: int = 0):
        self.browsers: list[FakeBrowser] = []
        self.failures = failures

    async def __call__(self, _mode):
        if self.failures:
            self.failures -= 1
            raise RuntimeError("launch failed")
        browser = FakeBrowser()
        self.browsers.append(browser)
        return FakePlaywright(), browser


def _notices_cfg(tmp_path):
    return NoticesConfig.from_env(
        {
            "NOTICES_SNAPSHOT_DIR": str(tmp_path),
            "NOTICES_RENDER_STABLE_MS": "1",
            "NOTICES_RENDER_POLL_MS": "1",
        }
    )


def test_leases_share_one_browser_and_the_last_return_closes_it():
    launcher = Launcher()
    shared = SharedBrowser(start=launcher)

    async def scenario():
        first = await shared.acquire()
        second = await shared.acquire()
        assert first.browser is second.browser
        assert shared.launches == 1 and shared.holders == 2
        await shared.release(second)
        assert shared.running and not launcher.browsers[0].closed
        await shared.release(first)
        assert not shared.running and launcher.browsers[0].closed

    asyncio.run(scenario())


def test_a_notices_cycle_alone_launches_the_browser_and_closes_it(tmp_path):
    launcher = Launcher()
    shared = SharedBrowser(start=launcher)
    cfg = _notices_cfg(tmp_path)

    async def scenario():
        for cycle in (1, 2):
            fetched = await notices_poller._default_launcher(cfg, SnapshotStore(tmp_path), shared)
            assert fetched is not None and "na-service-alert" in fetched.list_html
            assert not shared.running, "no Chromium between notices polls"
            assert shared.launches == cycle
            browser = launcher.browsers[-1]
            assert browser.closed
            assert all(context.closed for context in browser.contexts)

    asyncio.run(scenario())


def test_tracks_keep_one_browser_resident_through_a_notices_cycle(tmp_path):
    launcher = Launcher()
    shared = SharedBrowser(start=launcher)
    tracks = ResidentBrowser(TracksConfig(), shared)
    train = DueTrain("167", "2026-10-17", 60.0)

    async def scenario():
        assert await tracks.fetch(TracksConfig(), train) == {"data": []}
        fetched = await notices_poller._default_launcher(
            _notices_cfg(tmp_path), SnapshotStore(tmp_path), shared
        )
        assert fetched is not None
        assert shared.launches == 1, "tracks and notices share one Chromium"
        browser = launcher.browsers[0]
        assert not browser.closed
        status_context, notices_context = browser.contexts
        assert notices_context.closed and not status_context.closed
        assert await tracks.fetch(TracksConfig(), train) == {"data": []}
        assert shared.launches == 1
        await tracks.close()
        assert status_context.closed and browser.closed and not shared.running

    asyncio.run(scenario())


def test_a_disconnected_browser_is_relaunched_and_its_leases_go_stale():
    launcher = Launcher()
    shared = SharedBrowser(start=launcher)

    async def scenario():
        old = await shared.acquire()
        launcher.browsers[0].connected = False
        new = await shared.acquire()
        assert shared.launches == 2 and new.browser is launcher.browsers[1]
        await shared.release(old)  # stale: must not return the new browser's lease
        assert shared.running and shared.holders == 1
        await shared.release(new)
        assert not shared.running

    asyncio.run(scenario())


def test_close_tears_the_browser_down_while_leases_are_out():
    launcher = Launcher()
    shared = SharedBrowser(start=launcher)

    async def scenario():
        held = await shared.acquire()
        await shared.close()
        assert not shared.running and launcher.browsers[0].closed
        await shared.release(held)
        assert shared.holders == 0

    asyncio.run(scenario())


def test_a_failed_launch_holds_nothing_and_the_next_acquire_retries():
    launcher = Launcher(failures=1)
    shared = SharedBrowser(start=launcher)

    async def scenario():
        with pytest.raises(RuntimeError, match="launch failed"):
            await shared.acquire()
        assert shared.holders == 0 and not shared.running
        lease = await shared.acquire()
        assert shared.launches == 1
        await shared.release(lease)

    asyncio.run(scenario())


class Error(Exception):
    """Stands in for Playwright's error class, which is named ``Error`` for every failure."""


TUNNEL_FAILURE = (
    "Page.goto: net::ERR_TUNNEL_CONNECTION_FAILED at "
    "https://user:pw@www.amtrak.com/tickets/train-status.html?session=abc#top\n"
    "Call log:\n"
    '  - navigating to "https://www.amtrak.com/tickets/train-status.html?session=abc", '
    'waiting until "domcontentloaded"\n'
)
TUNNEL_LINE = (
    "Error: Page.goto: net::ERR_TUNNEL_CONNECTION_FAILED at "
    "https://www.amtrak.com/tickets/train-status.html"
)


def test_describe_error_is_one_line_without_url_secrets():
    assert describe_error(Error(TUNNEL_FAILURE)) == TUNNEL_LINE
    assert describe_error(asyncio.TimeoutError()) == "TimeoutError"
    assert describe_error(Error("\n  Timeout   45000ms\texceeded.\n")) == (
        "Error: Timeout 45000ms exceeded."
    )
    assert describe_error(Error("at http://[::1]:8081/x?token=1")) == "Error: at http://[::1]:8081/x"
    long = describe_error(Error("x" * 1_000))
    assert len(long) == len("Error: ") + MAX_ERROR_CHARS and long.endswith("…")


def _failing_goto(error):
    async def goto(self, url, **_kwargs):
        self.visited.append(url)
        raise error

    return goto


def _warnings(caplog, logger):
    return [r.getMessage() for r in caplog.records if r.name == logger and r.levelno == logging.WARNING]


def test_a_failed_first_page_load_names_the_page_load_and_its_cause(monkeypatch, caplog):
    monkeypatch.setattr(FakePage, "goto", _failing_goto(Error(TUNNEL_FAILURE)))
    shared = SharedBrowser(start=Launcher())
    tracks = ResidentBrowser(TracksConfig(), shared)

    async def scenario():
        with pytest.raises(BrowserUnavailable):
            await tracks.fetch(TracksConfig(), DueTrain("167", "2026-10-17", 60.0))

    with caplog.at_level(logging.WARNING):
        asyncio.run(scenario())
    assert _warnings(caplog, "worker.tracks.poller") == [
        f"first status page load failed: {TUNNEL_LINE}; next browser launch in 60s"
    ]
    assert shared.holders == 0, "the failed page returns its lease"


def test_a_failed_browser_launch_is_named_as_one(caplog):
    shared = SharedBrowser(start=Launcher(failures=1))
    tracks = ResidentBrowser(TracksConfig(), shared)

    async def scenario():
        with pytest.raises(BrowserUnavailable):
            await tracks.fetch(TracksConfig(), DueTrain("167", "2026-10-17", 60.0))

    with caplog.at_level(logging.WARNING):
        asyncio.run(scenario())
    assert _warnings(caplog, "worker.tracks.poller") == [
        "browser launch failed: RuntimeError: launch failed; next browser launch in 60s"
    ]


def _one_notices_cycle(tmp_path, cfg, caplog):
    async def no_sleep(_secs):
        return None

    store = SnapshotStore(tmp_path)
    store.update("<html>previous na-service-alert</html>")
    shared = SharedBrowser(start=Launcher())
    with caplog.at_level(logging.WARNING):
        asyncio.run(
            notices_poller.run_forever(store, cfg, shared=shared, sleep=no_sleep, iterations=1)
        )
    assert store.read()[0] == b"<html>previous na-service-alert</html>"
    assert not shared.running and shared.holders == 0
    return _warnings(caplog, "worker.notices.poller")


def test_notices_log_why_the_markup_was_not_obtained(tmp_path, monkeypatch, caplog):
    monkeypatch.setattr(FakePage, "goto", _failing_goto(Error(TUNNEL_FAILURE)))
    assert _one_notices_cycle(tmp_path, _notices_cfg(tmp_path), caplog) == [
        f"advisories markup not obtained this cycle (navigation failed: {TUNNEL_LINE}); "
        "keeping last snapshot"
    ]


def test_notices_log_a_gate_that_never_appeared(tmp_path, monkeypatch, caplog):
    async def no_gate(self, _selector):
        return None

    monkeypatch.setattr(FakePage, "query_selector", no_gate)
    cfg = dataclasses.replace(_notices_cfg(tmp_path), nav_timeout_secs=1)
    assert _one_notices_cycle(tmp_path, cfg, caplog) == [
        f"advisories markup not obtained this cycle (gate {cfg.gate_selector!r} not present "
        "after 1s); keeping last snapshot"
    ]

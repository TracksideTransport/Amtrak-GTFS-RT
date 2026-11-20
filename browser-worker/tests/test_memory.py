"""What the worker keeps between cycles stays bounded (#118). No browser, no network.

Playwright keeps a ``Request``, a ``Route``, and a ``Response`` object, in its driver and in this
process, for every request a context routes, until the page that made it closes. The fakes here
model that: :class:`RetainingContext` keeps one record per routed request for as long as the
context is open.
"""

import asyncio
import re
import tracemalloc
from datetime import datetime, timedelta, timezone

from worker.browser import SharedBrowser
from worker.notices import poller as notices_poller
from worker.notices.config import Config as NoticesConfig
from worker.notices.store import SnapshotStore
from worker.tracks.config import Config as TracksConfig
from worker.tracks.poller import (
    BrowserUnavailable,
    ResidentBrowser,
    poll_once,
    should_abort,
    status_url,
)
from worker.tracks.select import DueTrain
from worker.tracks.store import TrackStore

LIST_HTML = (
    "<html><div class='na-service-alert' data-href='/alert/one.html'>NER</div>"
    "<div class='alerts-details-minimum'></div></html>"
)


def _intercepts(pattern, url: str) -> bool:
    """Playwright's matching for the two kinds of pattern the worker passes to ``route``."""
    if isinstance(pattern, re.Pattern):
        return pattern.search(url) is not None
    assert pattern == "**/*", pattern
    return True


class RetainingPage:
    def __init__(self, context, *, fail_goto=False, fail_eval=False):
        self.context = context
        self.closed = False
        self.fail_goto = fail_goto
        self.fail_eval = fail_eval

    def _request(self, url: str) -> None:
        if any(_intercepts(pattern, url) for pattern in self.context.patterns):
            self.context.routed.append(url)

    async def goto(self, url, **_kwargs):
        self._request(url)
        if self.fail_goto:
            raise TimeoutError("Timeout 45000ms exceeded.")

    async def evaluate(self, _script, args):
        url, _timeout_ms = args
        self._request(url)
        if self.fail_eval:
            raise RuntimeError("Target crashed")
        return {"status": 200, "body": '{"data": []}'}

    async def content(self):
        return LIST_HTML

    async def query_selector(self, _selector):
        return object()

    async def close(self):
        self.closed = True


class RetainingContext:
    def __init__(self, browser, *, hang_on_close=False, **page_faults):
        self.browser = browser
        self.closed = False
        self.hang_on_close = hang_on_close
        self.page_faults = page_faults
        self.patterns: list[object] = []
        self.routed: list[str] = []
        self.pages: list[RetainingPage] = []

    async def route(self, pattern, _handler):
        self.patterns.append(pattern)

    async def new_page(self):
        page = RetainingPage(self, **self.page_faults)
        self.pages.append(page)
        return page

    async def close(self):
        if self.hang_on_close and not self.browser.closed:
            await asyncio.sleep(3600)
        self.closed = True
        for page in self.pages:
            page.closed = True

    @property
    def open_pages(self) -> int:
        return sum(not page.closed for page in self.pages)


class RetainingBrowser:
    def __init__(self, **context_faults):
        self.closed = False
        self.context_faults = context_faults
        self.contexts: list[RetainingContext] = []

    def is_connected(self):
        return not self.closed

    async def new_context(self, **_kwargs):
        context = RetainingContext(self, **self.context_faults)
        self.contexts.append(context)
        return context

    async def close(self):
        self.closed = True
        for context in self.contexts:
            context.closed = True
            for page in context.pages:
                page.closed = True

    def open_contexts(self) -> list[RetainingContext]:
        return [context for context in self.contexts if not context.closed]


class _Playwright:
    async def stop(self):
        return None


def _launcher(**context_faults):
    browsers: list[RetainingBrowser] = []

    async def start(_mode):
        browser = RetainingBrowser(**context_faults)
        browsers.append(browser)
        return _Playwright(), browser

    return start, browsers


TRAIN = DueTrain("167", "2026-10-17", 60.0)


def test_status_fetches_are_not_routed_so_playwright_keeps_nothing_per_fetch():
    start, browsers = _launcher()
    cfg = TracksConfig()
    resident = ResidentBrowser(cfg, SharedBrowser(start=start))

    async def scenario():
        for _ in range(500):
            assert await resident.fetch(cfg, TRAIN) == {"data": []}

    asyncio.run(scenario())
    (context,) = browsers[0].contexts
    # Only the first page load went through the route handler; the 500 status fetches did not.
    assert context.routed == [cfg.page_url]


def test_the_status_route_still_covers_every_request_it_can_block():
    from worker.tracks.poller import intercepted_urls

    cfg = TracksConfig()
    pattern = intercepted_urls()
    for url, resource_type in [
        (cfg.page_url, "document"),
        ("https://www.amtrak.com/logo.png", "image"),
        ("https://www.amtrak.com/app.js", "script"),
        ("https://tags.example.net/t.js", "script"),
        ("https://www.amtrak.com/dotcom/travel-service/other", "fetch"),
    ]:
        assert pattern.search(url), url
    # What it leaves out is only ever allowed: a first-party fetch of a status document.
    status = status_url("167", "2026-10-17")
    assert not pattern.search(status)
    assert not should_abort(cfg, "fetch", status)
    # The source is plain enough for JavaScript's RegExp, which the driver builds from it.
    assert pattern.flags & ~re.UNICODE == 0
    assert re.fullmatch(r"\^\(\?!https://www\\\.amtrak\\\.com/[A-Za-z/\\-]+\)", pattern.pattern)


def test_a_failed_status_fetch_leaves_no_context_or_page_open():
    start, browsers = _launcher(fail_eval=True)
    cfg = TracksConfig()
    shared = SharedBrowser(start=start)
    resident = ResidentBrowser(cfg, shared)

    async def scenario():
        try:
            await resident.fetch(cfg, TRAIN)
        except BrowserUnavailable:
            pass
        else:
            raise AssertionError("expected BrowserUnavailable")

    asyncio.run(scenario())
    (context,) = browsers[0].contexts
    assert context.closed and context.open_pages == 0
    assert browsers[0].closed and not shared.running and shared.holders == 0


def test_a_failed_first_page_load_leaves_no_context_or_page_open():
    start, browsers = _launcher(fail_goto=True)
    cfg = TracksConfig()
    shared = SharedBrowser(start=start)
    resident = ResidentBrowser(cfg, shared)

    async def scenario():
        try:
            await resident.fetch(cfg, TRAIN)
        except BrowserUnavailable:
            pass
        else:
            raise AssertionError("expected BrowserUnavailable")

    asyncio.run(scenario())
    (context,) = browsers[0].contexts
    assert context.closed and context.open_pages == 0
    assert not shared.running and shared.holders == 0


def test_a_status_context_that_will_not_close_takes_the_browser_down_while_notices_hold_it():
    start, browsers = _launcher(hang_on_close=True)
    cfg = TracksConfig(fetch_timeout_secs=1, browser_max_age_secs=1)
    shared = SharedBrowser(start=start)
    clock = [0.0]
    resident = ResidentBrowser(cfg, shared, monotonic=lambda: clock[0])

    async def scenario():
        assert await resident.fetch(cfg, TRAIN) == {"data": []}
        notices = await shared.acquire()  # a notices cycle is mid-flight
        clock[0] = 5  # past the maximum age: the status page is recycled between cycles
        await resident.recycle_if_due()
        # The context that would not close does not stay open inside a browser the notices lease
        # keeps running: that browser is closed, the replacement page with it.
        first = browsers[0]
        assert first.closed and first.open_contexts() == []
        # The next fetch opens the status page in a new browser, with no relaunch backoff.
        assert await resident.fetch(cfg, TRAIN) == {"data": []}
        assert len(browsers) == 2 and len(browsers[1].open_contexts()) == 1
        await shared.release(notices)  # stale now: returning it does nothing
        assert shared.running and shared.holders == 1

    asyncio.run(scenario())


def _notices_cfg(tmp_path, **overrides):
    env = {
        "NOTICES_SNAPSHOT_DIR": str(tmp_path),
        "NOTICES_RENDER_STABLE_MS": "1",
        "NOTICES_RENDER_POLL_MS": "1",
        "NAV_TIMEOUT_SECS": "1",
        "BROWSER_CLOSE_SECS": "1",
    }
    env.update(overrides)
    return NoticesConfig.from_env(env)


def test_notices_cycles_close_every_page_and_context_even_when_close_hangs(tmp_path):
    for faults in ({}, {"hang_on_close": True}):
        start, browsers = _launcher(**faults)
        shared = SharedBrowser(start=start)
        cfg = _notices_cfg(tmp_path)

        async def scenario():
            for _ in range(3):
                await notices_poller._default_launcher(cfg, SnapshotStore(tmp_path), shared)

        asyncio.run(scenario())
        assert not shared.running and shared.holders == 0
        for browser in browsers:
            assert browser.closed
            assert all(context.open_pages == 0 for context in browser.contexts)


def _trains_at(now: datetime) -> dict:
    """A train list whose trains change every hour, like a service day rolling through."""
    hour = int(now.timestamp() // 3600)
    features = []
    for offset in range(30):
        departure = now + timedelta(minutes=offset)
        features.append(
            {
                "properties": {
                    "TrainNum": str(1000 + (hour * 30 + offset) % 9000),
                    "OrigSchDep": departure.astimezone(timezone(timedelta(hours=-5))).strftime(
                        "%m/%d/%Y %I:%M:%S %p"
                    ),
                    "Station1": {
                        "code": "PVD",
                        "tz": "E",
                        "bus": False,
                        "scharr": (departure + timedelta(minutes=5))
                        .astimezone(timezone(timedelta(hours=-5)))
                        .strftime("%m/%d/%Y %H:%M:%S"),
                    },
                }
            }
        )
    return {"features": features}


def test_poll_loop_structures_stay_bounded_over_a_day_of_distinct_train_days():
    cfg = TracksConfig(max_fetches_per_cycle=24, cooldown_secs=0)
    store = TrackStore()
    last_fetched: dict[tuple[str, str], float] = {}
    start = datetime(2026, 11, 18, 0, 0, tzinfo=timezone.utc)
    document = {"data": [{"stops": [{"station": {"code": "PVD"}, "pad": "x" * 2000}]}]}

    async def status(_cfg, _train):
        return document

    async def cycles(first: int, last: int) -> None:
        for minute in range(first, last):
            now = start + timedelta(minutes=minute)

            async def trains(_cfg, now=now):
                return _trains_at(now)

            await poll_once(
                store, cfg, last_fetched, now=now, trains_fetcher=trains, status_fetcher=status
            )

    # Nothing reads /tracks.json here, as with a worker run without the service.
    asyncio.run(cycles(0, 120))
    tracemalloc.start()
    try:
        asyncio.run(cycles(120, 240))
        warm, _ = tracemalloc.get_traced_memory()
        asyncio.run(cycles(240, 24 * 60))
        settled, _ = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    window = cfg.document_ttl_secs // 3600 + 1
    assert len(store._documents) <= 30 * (window + 1)
    assert len(store._tracked) == len(store._documents)
    assert len(last_fetched) <= 30
    # A day of distinct train-days after warm-up adds no lasting allocations.
    assert settled - warm < 256 * 1024, settled - warm

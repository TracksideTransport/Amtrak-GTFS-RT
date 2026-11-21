"""Poll loop: decrypt the maps feed, then fetch status JSON for trains near a stop.

The maps feed (``maps.amtrak.com``) answers a plain HTTP client. ``www.amtrak.com`` does not, so
status documents are fetched in a resident page of the worker's one headless browser
(:mod:`worker.browser`): launching a browser on every 60-second cycle would thrash a Raspberry Pi. Image, font, stylesheet, and media requests are
blocked, and so is every host outside ``amtrak.com`` unless ``BLOCK_THIRD_PARTY`` is off.

A failed status fetch keeps the last good document. A browser failure ends the cycle's fetches
and the relaunch backs off, so a blocked or crashing browser is not relaunched once per train.
The trains it left unfetched are not counted as attempted: a hot one is retried as soon as the
browser is back, and a ready one waits for the next full poll unless it is warm or hinted. A pick
whose own in-page fetch failed counts as an attempt and takes the cooldown. A cycle counts as a
success only when it had nothing to fetch or stored a document, so a worker Amtrak has stopped
answering lets ``/tracks.json`` go stale instead of looking current.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import re
import time
import urllib.request
from collections import Counter
from collections.abc import Awaitable, Callable
from datetime import datetime, timedelta, timezone
from urllib.parse import urlsplit

from ..browser import LAUNCH_ARGS, Lease, SharedBrowser, describe_error
from .config import Config
from .decrypt import DecryptError, decrypt_payload
from .lead_time import LeadProfile
from .select import FLEET_TRAINS_SCHEMA, DueTrain, choose_fetches, due_trains, is_hot, is_warm
from .stations import DIRECTORY_URL, StationsStore
from .store import TrackStore

logger = logging.getLogger(__name__)

# Kept under this name for the one-shot tools (``dump`` and ``coverage``), which launch their own.
_LAUNCH_ARGS = LAUNCH_ARGS
_BLOCKED_RESOURCE_TYPES = frozenset({"image", "font", "media", "stylesheet"})
_FIRST_PARTY = "amtrak.com"
# maps.amtrak.com rejects ``Accept: application/json`` with HTTP 406 and serves the ciphertext
# as ``text/plain``. ``*/*`` is what that endpoint accepts. The www.amtrak.com status document
# is JSON, so the in-page fetch below still asks for ``application/json``.
TRAINS_ACCEPT = "*/*"
STATUS_PREFIX = "https://www.amtrak.com/dotcom/travel-service/statuses/"
_RELAUNCH_BACKOFF_MIN_SECS = 60
_FETCH_JS = """async ([url, timeoutMs]) => {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetch(url, {
      headers: { "Accept": "application/json", "Content-Type": "application/json" },
      signal: controller.signal,
    });
    return { status: response.status, body: await response.text() };
  } finally {
    clearTimeout(timer);
  }
}"""


class BrowserUnavailable(Exception):
    """The browser could not run a fetch. The rest of the cycle is skipped.

    ``attempted`` is true when the in-page status fetch for the pick actually ran and failed (a
    timed-out or refused request, or a renderer that died during it). Such a failure may be
    specific to that train, so the pick is stamped like any other attempt.
    """

    def __init__(self, reason: str, *, attempted: bool = False) -> None:
        super().__init__(reason)
        self.attempted = attempted


class StatusMiss(Exception):
    """Amtrak answered, but not with a usable status document. The message is the reason."""


class _FirstLoadFailed(Exception):
    """The browser and its page are up, but the first navigation to the status page failed."""

    def __init__(self, error: Exception) -> None:
        super().__init__(type(error).__name__)
        self.error = error


TrainsFetcher = Callable[[Config], Awaitable[dict]]
StatusFetcher = Callable[[Config, DueTrain], Awaitable[dict | None]]
Clock = Callable[[], datetime]
Monotonic = Callable[[], float]


def should_block(resource_type: str) -> bool:
    """Abort page weight the status fetch does not need. Script and fetch stay allowed."""
    return resource_type in _BLOCKED_RESOURCE_TYPES


def is_first_party(url: str) -> bool:
    """Whether ``url`` is on ``amtrak.com`` or one of its subdomains."""
    host = (urlsplit(url).hostname or "").lower()
    return host == _FIRST_PARTY or host.endswith("." + _FIRST_PARTY)


def should_abort(cfg: Config, resource_type: str, url: str) -> bool:
    """Routing decision for one browser request."""
    if cfg.block_subresources and should_block(resource_type):
        return True
    return cfg.block_third_party and not is_first_party(url)


def status_url(train_number: str, service_date: str) -> str:
    """Amtrak train-status URL for one train-day."""
    return f"{STATUS_PREFIX}{train_number}?service-date={service_date}"


def intercepted_urls() -> re.Pattern[str]:
    """The status context's route pattern: every URL except the status documents themselves.

    Playwright keeps a ``Request``, a ``Route``, and a ``Response`` object, in its driver and in
    this process, for every request a context routes, until the page that made it closes (past
    10,000 of one kind it drops the oldest). The resident page makes up to
    ``MAX_FETCHES_PER_CYCLE`` status fetches a minute and lives for hours, so routing them grew the
    worker by tens of MiB an hour (#118). A status fetch is a first-party ``fetch``, which
    :func:`should_abort` never aborts, so leaving it unrouted changes no routing decision. The
    driver rebuilds the pattern as a JavaScript ``RegExp``, so it stays a plain lookahead.
    """
    return re.compile("^(?!" + re.escape(STATUS_PREFIX) + ")")


def load_trains(cfg: Config) -> dict:
    """GET the maps train feed and return its GeoJSON. A JSON body is used as-is."""
    request = urllib.request.Request(
        cfg.trains_url,
        headers={"Accept": TRAINS_ACCEPT, "User-Agent": "amtrak-gtfs-rt-track-fetcher"},
    )
    with urllib.request.urlopen(request, timeout=cfg.nav_timeout_secs) as response:
        text = response.read().decode("utf-8", "replace").strip()
    if text.startswith("{"):
        parsed = json.loads(text)
    else:
        parsed = json.loads(decrypt_payload(text))
    if not isinstance(parsed, dict):
        raise DecryptError("train feed is not an object")
    return parsed


class TrainListNotReady(FileNotFoundError):
    """The service has not written its first train list yet. Expected on a cold start."""


def load_train_list_file(path: str) -> dict:
    """The ``amtrak-fleet-trains/v1`` list the one-container service writes after each generation.

    Raises :class:`TrainListNotReady` until the service has published its first generation; the
    cycle is retried on the next tick. A file that exists but cannot be read raises ``OSError``,
    one that is not JSON or not an ``amtrak-fleet-trains/v1`` object raises ``ValueError``.
    """
    try:
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
    except FileNotFoundError:
        raise TrainListNotReady("the service has not written its first train list yet") from None
    parsed = json.loads(text)
    if not isinstance(parsed, dict):
        raise ValueError("fleet train list is not an object")
    if parsed.get("schema") != FLEET_TRAINS_SCHEMA:
        raise ValueError(f"fleet train list schema is not {FLEET_TRAINS_SCHEMA}")
    return parsed


async def _default_trains(cfg: Config) -> dict:
    """Read the service's train list. The service is the only map poller.

    Beside the service (``AMTRAK_OUTPUT_DIR`` set), the list is ``AMTRAK_FLEET_TRAINS_FILE``; without
    it the cycle fails rather than polling ``getTrainsData`` a second time. Only a worker run on
    its own, with neither variable, polls the map itself.
    """
    train_list = os.environ.get("AMTRAK_FLEET_TRAINS_FILE", "")
    if train_list:
        return await asyncio.to_thread(load_train_list_file, train_list)
    if os.environ.get("AMTRAK_OUTPUT_DIR", ""):
        raise FileNotFoundError(
            "AMTRAK_FLEET_TRAINS_FILE is not set beside the service; the service is the only map poller"
        )
    return await asyncio.to_thread(load_trains, cfg)


class ResidentBrowser:
    """One status page in the shared browser, reused across polls.

    The page lives in its own browser context and holds a lease on the worker's one Chromium for
    as long as it is up, which keeps that browser resident. A failed fetch closes the page and
    returns the lease (the browser closes too unless the notices source is mid-cycle), and the
    next launch waits an exponential backoff from one minute up to ``RELAUNCH_BACKOFF_MAX_SECS``,
    reset by the next successful fetch.

    After ``BROWSER_MAX_AGE_SECS`` the page is recycled, to bound the renderer's memory, by
    :meth:`recycle_if_due` between poll cycles, never in the middle of one. The new context and page
    are loaded first and the old ones closed only after that succeeds. A reload that fails keeps
    the old page, starts no backoff, and is tried again a minute or more later.
    """

    def __init__(
        self,
        cfg: Config,
        shared: SharedBrowser | None = None,
        *,
        monotonic: Monotonic = time.monotonic,
    ) -> None:
        self._cfg = cfg
        self._shared = shared if shared is not None else SharedBrowser(cfg.browser_mode)
        self._monotonic = monotonic
        self._lease: Lease | None = None
        self._context = None
        self._page = None
        self._launched_at = 0.0
        self._backoff = 0.0
        self._next_launch = 0.0
        self._next_recycle = 0.0

    async def fetch(self, cfg: Config, train: DueTrain) -> dict:
        """Return one status document.

        Raises :class:`StatusMiss` when Amtrak's answer is unusable and
        :class:`BrowserUnavailable` when the browser cannot run the request.
        """
        url = status_url(train.train_number, train.service_date)
        status, body = await self.fetch_json(url, cfg.fetch_timeout_secs)
        return document_from_eval({"status": status, "body": body})

    async def fetch_json(self, url: str, timeout_secs: int) -> tuple[int, str]:
        """Fetch ``url`` in the page with ``Accept: application/json``: ``(status, body)``.

        Raises :class:`BrowserUnavailable` when the browser cannot run the request.
        """
        page = await self._ensure_page()
        try:
            raw = await asyncio.wait_for(
                page.evaluate(_FETCH_JS, [url, timeout_secs * 1000]), timeout_secs + 5
            )
        except Exception as error:  # noqa: BLE001 — a dead renderer or hung request
            await self._failed(f"status fetch failed: {describe_error(error)}")
            raise BrowserUnavailable(type(error).__name__, attempted=True) from None
        self._backoff = 0.0
        if not isinstance(raw, dict):
            return 0, ""
        status, body = raw.get("status"), raw.get("body")
        return (status if isinstance(status, int) else 0), (body if isinstance(body, str) else "")

    async def recycle_if_due(self) -> None:
        """Swap the status page for a fresh one once it is ``BROWSER_MAX_AGE_SECS`` old.

        Called between poll cycles. The new page is opened and loaded before the old one is
        closed; if that fails, the old page stays, the reason is logged, and no relaunch backoff
        starts. Nothing happens while no page is up: the next fetch opens one.
        """
        now = self._monotonic()
        if self._page is None or now - self._launched_at < self._cfg.browser_max_age_secs:
            return
        if now < self._next_recycle:
            return
        logger.info("recycling the status page after %ds", self._cfg.browser_max_age_secs)
        try:
            lease, context, page = await self._open()
        except Exception as error:  # noqa: BLE001 — the browser, its context, or the page load
            failure = error.error if isinstance(error, _FirstLoadFailed) else error
            self._next_recycle = now + _RELAUNCH_BACKOFF_MIN_SECS
            logger.warning(
                "status page recycle failed (%s); keeping the current page", describe_error(failure)
            )
            return
        old_lease, old_context = self._lease, self._context
        self._lease, self._context, self._page = lease, context, page
        self._launched_at = now
        if await self._close_parts(old_lease, old_context):
            # The old context would not close, so its browser was closed, and the new page with
            # it. The next fetch opens a page in a fresh browser, without a backoff.
            self._lease = self._context = self._page = None

    async def close(self) -> None:
        """Close the status page and return the browser lease. The next fetch opens a new one."""
        context, lease = self._context, self._lease
        self._page = None
        self._context = None
        self._lease = None
        await self._close_parts(lease, context)

    async def _close_parts(self, lease: Lease | None, context) -> bool:
        """Close ``context`` and return ``lease``. Returns whether the browser had to be closed.

        A context that does not close in time takes its browser down with it, even while the
        notices source holds that browser: left open, its page and renderer memory would last as
        long as the browser does.
        """
        if context is not None:
            try:
                await asyncio.wait_for(context.close(), self._cfg.fetch_timeout_secs)
            except Exception as error:  # noqa: BLE001 — a hung close, or the browser is gone
                if lease is not None and await self._shared.discard(lease):
                    logger.warning(
                        "status context did not close (%s); closed the browser",
                        describe_error(error),
                    )
                    return True
                logger.debug("status context close failed", exc_info=True)
        if lease is not None:
            await self._shared.release(lease)
        return False

    async def _failed(self, reason: str) -> None:
        await self.close()
        self._backoff = min(
            max(self._backoff * 2, _RELAUNCH_BACKOFF_MIN_SECS),
            self._cfg.relaunch_backoff_max_secs,
        )
        self._next_launch = self._monotonic() + self._backoff
        logger.warning("%s; next browser launch in %ds", reason, self._backoff)

    async def _ensure_page(self):
        if self._page is not None:
            return self._page
        now = self._monotonic()
        if now < self._next_launch:
            raise BrowserUnavailable("relaunch backoff")
        try:
            lease, context, page = await self._open()
        except _FirstLoadFailed as failure:
            # The browser launched; the status page did not load (blocked, offline, timed out).
            await self._failed(f"first status page load failed: {describe_error(failure.error)}")
            raise BrowserUnavailable(type(failure.error).__name__) from None
        except Exception as error:  # noqa: BLE001 — the browser, its context, or its page
            await self._failed(f"browser launch failed: {describe_error(error)}")
            raise BrowserUnavailable(type(error).__name__) from None
        self._lease, self._context, self._page = lease, context, page
        self._launched_at = now
        self._next_recycle = 0.0
        return page

    async def _open(self):
        """Borrow the browser and load the status page in a new context: ``(lease, context, page)``.

        On any failure the context is closed and the lease returned before the error propagates;
        a failed page load raises :class:`_FirstLoadFailed`.
        """
        cfg = self._cfg
        lease = await self._shared.acquire()
        context = None
        try:
            context = await lease.browser.new_context()
            if cfg.block_subresources or cfg.block_third_party:

                async def _route(route):
                    request = route.request
                    if should_abort(cfg, request.resource_type, request.url):
                        await route.abort()
                    else:
                        await route.continue_()

                await context.route(intercepted_urls(), _route)
            page = await context.new_page()
            # domcontentloaded lets the site's first-party scripts set their cookies before the
            # first status call; "commit" returned before they ran.
            try:
                await page.goto(
                    cfg.page_url,
                    wait_until="domcontentloaded",
                    timeout=cfg.nav_timeout_secs * 1000,
                )
            except Exception as error:  # noqa: BLE001 — reported apart from a launch failure
                raise _FirstLoadFailed(error) from error
        except BaseException:
            await self._close_parts(lease, context)
            raise
        return lease, context, page


def document_from_eval(raw: object) -> dict:
    """The status document in an in-page fetch result, or :class:`StatusMiss` with the reason."""
    if not isinstance(raw, dict):
        raise StatusMiss("no response")
    status = raw.get("status")
    if status != 200:
        raise StatusMiss(f"http {status}")
    body = raw.get("body")
    if not isinstance(body, str):
        raise StatusMiss("no body")
    try:
        document = json.loads(body)
    except json.JSONDecodeError:
        raise StatusMiss("not json") from None
    if not isinstance(document, dict) or "data" not in document:
        raise StatusMiss("no data")
    return document


async def poll_once(
    store: TrackStore,
    cfg: Config,
    last_fetched: dict[tuple[str, str], float],
    *,
    now: datetime,
    trains_fetcher: TrainsFetcher,
    status_fetcher: StatusFetcher,
    full: bool = True,
    stations: StationsStore | None = None,
) -> bool:
    """One cycle. Raises when the train list itself cannot be read; status misses are skipped.

    Returns whether the cycle succeeded: it had nothing to fetch, or it stored a document. Every
    attempt that reached Amtrak, stored or rejected, starts the train's cooldown, and so does a
    pick whose own in-page fetch ran and failed (:class:`BrowserUnavailable` with ``attempted``).
    A pick the browser could not run at all (a failed launch or first page load, or the relaunch
    backoff) and every pick after a browser failure are not stamped. A hot one is retried as soon
    as the browser is back; a ready one is left out of the cycles between full polls unless it is
    warm or hinted, so it waits for the next full poll. ``last_fetched`` is pruned to the trains
    still due, and ``store`` to documents younger than ``DOCUMENT_TTL_SECS``. One INFO line names
    each pick, whether it was hot or ready, and its outcome; a second gives the cycle's totals.
    """
    geojson = await trains_fetcher(cfg)
    profile = LeadProfile.load(
        cfg.lead_time_file or None,
        default_before=cfg.window_before_secs,
        default_hot=cfg.hot_window_secs,
    )
    due = due_trains(
        geojson,
        now,
        before=timedelta(seconds=cfg.window_before_secs),
        after=timedelta(seconds=cfg.window_after_secs),
        lead=profile,
        hot=timedelta(seconds=cfg.hot_window_secs),
    )
    early, early_stations = take_early_hints()
    if not full:
        # Between full polls: hinted trains, hot trains, and warm ones (a boarding stop at a
        # station with no track seen yet), which then take the cooldown like any ready train.
        known = store.tracked()
        due = [
            train
            for train in due
            if train.train_number in early
            or train.upcoming & early_stations
            or is_hot(train, known, cfg.hot_window_secs)
            or is_warm(train, known, cfg.hot_window_secs)
        ]
    due_keys = {train.key for train in due}
    for key in [key for key in last_fetched if key not in due_keys]:
        del last_fetched[key]
    tracked = store.tracked()
    picks = choose_fetches(
        due,
        last_fetched,
        now,
        hot_secs=cfg.hot_window_secs,
        cooldown_secs=cfg.cooldown_secs,
        max_fetches=cfg.max_fetches_per_cycle,
        tracked=tracked,
        hinted={train.key for train in due if train.train_number in early},
    )
    stored = 0
    rejected: Counter[str] = Counter()
    browser_down = None
    outcomes: list[str] = []
    for train in picks:
        kind = "hot" if is_hot(train, tracked, cfg.hot_window_secs) else "ready"
        label = f"{train.train_number} {kind}"
        suffix = ""
        if train.next_station:
            shown = (
                stations.display_name(train.next_station)
                if stations is not None
                else train.next_station
            )
            suffix = f" next={train.next_station} ({shown})"
        if browser_down is not None:
            outcomes.append(f"{label} skipped (browser unavailable){suffix}")
            continue
        try:
            document = await status_fetcher(cfg, train)
        except BrowserUnavailable as error:
            browser_down = str(error)
            if error.attempted:
                # The pick's own fetch ran and failed. It takes the cooldown, so a failure
                # specific to this train cannot put it first after every relaunch.
                last_fetched[train.key] = now.timestamp()
            outcomes.append(f"{label} browser unavailable ({browser_down}){suffix}")
            continue
        except StatusMiss as error:
            last_fetched[train.key] = now.timestamp()
            rejected[str(error)] += 1
            outcomes.append(f"{label} rejected ({error}){suffix}")
            continue
        last_fetched[train.key] = now.timestamp()
        if document is None:
            rejected["empty"] += 1
            outcomes.append(f"{label} rejected (empty){suffix}")
            continue
        store.put(train.train_number, train.service_date, document, now.timestamp())
        if stations is not None:
            stations.record_document(document)
        stored += 1
        outcomes.append(f"{label} stored{suffix}")
    store.prune(now.timestamp(), cfg.document_ttl_secs)
    succeeded = not picks or stored > 0
    if succeeded:
        store.mark_success(now.timestamp())
    if outcomes:
        logger.info("track poll picks: %s", ", ".join(outcomes))
    logger.info(
        "track poll due=%s picked=%s stored=%s rejected=%s%s",
        len(due),
        len(picks),
        stored,
        dict(rejected) or 0,
        f" browser_unavailable={browser_down}" if browser_down else "",
    )
    return succeeded


def take_early_hints() -> tuple[set[str], set[str]]:
    """Consume queued early-refresh hints: ``(train numbers, station codes)``.

    The service renames each debounced, capped flush into ``tracks/early-refresh/``. Every complete file
    is read and deleted, so a hint is acted on once; dot-prefixed files are still being
    written and are left alone. No output directory means no hints.
    """
    root = os.environ.get("AMTRAK_OUTPUT_DIR", "")
    trains: set[str] = set()
    stations: set[str] = set()
    if not root:
        return trains, stations
    queue = os.path.join(root, "tracks", "early-refresh")
    try:
        names = sorted(name for name in os.listdir(queue) if not name.startswith("."))
    except OSError:
        return trains, stations
    for name in names:
        path = os.path.join(queue, name)
        try:
            with open(path, encoding="utf-8") as handle:
                body = json.loads(handle.read())
        except (OSError, json.JSONDecodeError):
            body = None
        try:
            os.remove(path)
        except OSError:
            pass
        if not isinstance(body, dict):
            continue
        for train in body.get("trains") or []:
            if str(train).isdigit():
                trains.add(str(train))
        for station in body.get("stations") or []:
            if isinstance(station, str) and station.strip():
                stations.add(station.strip().upper())
    return trains, stations


async def refresh_directory(
    stations: StationsStore, browser: ResidentBrowser, cfg: Config, now: float
) -> None:
    """Fetch Amtrak's station directory when it is due. A failure is logged, never raised."""
    if not stations.directory_due(now, cfg.stations_refresh_secs):
        return
    try:
        status, body = await browser.fetch_json(DIRECTORY_URL, cfg.fetch_timeout_secs)
    except BrowserUnavailable as error:
        stations.accept_directory("", now)
        logger.warning("station directory not fetched: browser unavailable (%s)", error)
        return
    except Exception:  # noqa: BLE001 — the directory must never stop the track loop
        stations.accept_directory("", now)
        logger.exception("station directory fetch failed")
        return
    if status == 200 and stations.accept_directory(body, now):
        rows = (stations.snapshot() or {}).get("directory") or []
        logger.info("station directory fetched: %d stations", len(rows))
        return
    if status == 200:
        logger.warning("station directory rejected: not a usable station list")
    else:
        stations.accept_directory("", now)
        logger.warning("station directory not fetched: http %s", status)


async def run_forever(
    store: TrackStore,
    cfg: Config,
    *,
    shared: SharedBrowser | None = None,
    browser: ResidentBrowser | None = None,
    trains_fetcher: TrainsFetcher | None = None,
    status_fetcher: StatusFetcher | None = None,
    sleep: Callable[[float], Awaitable[None]] = asyncio.sleep,
    clock: Clock | None = None,
    iterations: int | None = None,
    stations: StationsStore | None = None,
) -> None:
    """Poll until cancelled, or ``iterations`` times in tests.

    The default status fetcher keeps one :class:`ResidentBrowser` (``browser``, built on
    ``shared`` unless a test injects one) for the life of the loop, and recycles its page between
    cycles.
    """
    trains = trains_fetcher or _default_trains
    browser = browser if browser is not None else ResidentBrowser(cfg, shared)
    status = status_fetcher or browser.fetch
    now_of = clock or (lambda: datetime.now(timezone.utc))
    last_fetched: dict[tuple[str, str], float] = {}
    count = 0
    elapsed = 0
    try:
        while iterations is None or count < iterations:
            full = elapsed == 0 or elapsed >= cfg.base_interval_secs
            raised = False
            await browser.recycle_if_due()
            try:
                succeeded = await poll_once(
                    store,
                    cfg,
                    last_fetched,
                    now=now_of(),
                    trains_fetcher=trains,
                    status_fetcher=status,
                    full=full,
                    **({} if stations is None else {"stations": stations}),
                )
            except TrainListNotReady:
                # A cold start: the service publishes its first generation shortly. Not a failure.
                logger.info("waiting for the service's first train list")
                succeeded = False
                raised = True
            except Exception:  # noqa: BLE001 — a failed cycle must not crash the worker
                logger.exception("track poll failed; keeping the last snapshot")
                succeeded = False
                raised = True
            if stations is not None:
                await refresh_directory(stations, browser, cfg, now_of().timestamp())
            store.record_poll(succeeded)
            store.mark_cycle(now_of().timestamp())
            count += 1
            if iterations is not None and count >= iterations:
                break
            await sleep(cfg.poll_interval_secs)
            if full and raised:
                # The cycle never got a train list (the service has not published its first
                # generation yet, or the list was unreadable), so no train was polled. Retry the
                # full poll on the next tick instead of a base interval later. A cycle that reached
                # Amtrak and was refused keeps the normal schedule.
                elapsed = 0
            else:
                elapsed = cfg.poll_interval_secs if full else elapsed + cfg.poll_interval_secs
    finally:
        await browser.close()

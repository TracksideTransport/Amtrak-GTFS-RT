# Station names in the published static feed (#179)

## Why

Riders see `stop_name` in every app that uses this feed. Today it is Amtrak's static name, used as
is:

- **17 names are shared by 38 stops.** `BOS`, `BBY` and `BON` are all "Boston"; Springfield MA and
  IL are both "Springfield".
- **193 names say nothing about the station.**
- **The naming is inconsistent.** `NYP` is "Ny Moynihan Train Hall At Penn Station".

The goal is names riders recognise, which means the ones on Amtrak's departure boards, that are
never ambiguous within the feed.

## Goals

- **`stop_name` is the departure-board name** (`displayName`) where Amtrak has one, made unique by
  a fixed ladder when two stops would collide.
- **`stop_desc` carries the other details:** Amtrak's station name, the facility, and the municipality
  when it differs. For example, "Route 128, MA (in Westwood, MA)" and "New York, NY – Moynihan Train
  Hall at Penn Station".
- **`tts_stop_name` is Amtrak's announcement name** (`audioName`) where there is one. For example,
  `NWK` is "Newark New Jersey".
- **The browser worker can see station names** (owner requirement). It fetches the directory, and
  its own logs and dump output name stations, not just codes.
- **Platform stops are named from the new name:** "New Haven (State Street) Track 2".

## Non-goals

- No change to any `stop_id`, `parent_station`, platform id or `platform_code`.
- No change to realtime feeds. Names live only in `stops.txt`.
- No names copied into the repository. They are learned at runtime from Amtrak.
- Getting Amtrak to improve the names in its own static GTFS is a separate effort for another
  day. Static names remain this feature's last fallback, so better upstream names help
  automatically.

## Anti-goals

- **Names must not make the static feed churn.** Every change to `static.zip` gives a new static
  version and makes every consumer reload the whole schedule. So names change at most once every
  14 days (the owner's cadence, instead of daily polling), and never because of a single poll.
- **No duplicate `stop_name` in the published feed.**
- **No name that is worse than today's.** When the sources have nothing for a stop, it keeps its
  static name.

## Sources (verified 2026-11-21)

| Source | How it is reached | Coverage | Fields used |
|---|---|---|---|
| Status documents (`/dotcom/travel-service/statuses/{train}`), `data[0].stops[i].station` | Already fetched by the worker for tracks. They are in `/tracks.json` today | Stations on trains the worker has fetched: 433 of 645 stops after a day | `code`, `name` ("City, ST"), `displayName` (196), `facility` (79), `audioName` (8) |
| Station directory, `https://www.amtrak.com/services/data.stations.json` | Not fetched today. Plain HTTP gets `403` (bot block). **Verified 2026-11-21:** headless Chromium with the worker's launch arguments loads `train-status.html`, and an in-page `fetch()` then returns `200 application/json`, 190,257 bytes, 1,031 rows | 1,031 stations; 636 of the 645 static stops | `code`, `name` ("City, ST"), `city`, `state`, `facilityName64` (235; unabbreviated, e.g. "Moynihan Train Hall at Penn Station"), `facilityName32`, `autoFillName`, `stationAlias` |

**The directory has no `displayName` or `audioName`.** Board and announcement names exist only in
the status documents, so they accumulate as trains are seen.

The 9 static stops the directory lacks are `BWE`, `CBN`, `EDG`, `HAE`, `MSA`, `OTN`, `PRV`, `SEB`
and `WBL`. They keep their names unless a status document names them.

## Approaches considered

1. **The worker fetches the directory every 14 days and keeps board names from the status documents. The
   service names stops at static-build time (chosen).** Full coverage from the first day, board
   names wherever Amtrak has them, and the worker sees names, as the owner asked.
2. **Status documents only.** No new fetch, but 212 stops have no source at all, and coverage only
   grows as trains are seen.
3. **A directory snapshot committed to the repo.** Predictable, but names go stale, and every
   operator's names depend on this project's release cadence.

The owner chose, on 2026-11-21:

- departure-board names (`displayName`) for `stop_name`;
- the other details (station name, facility, municipality) for `stop_desc`;
- `audioName` for `tts_stop_name`.

## Design

### Naming rule (deterministic, per static build)

For each plain stop in the official `stops.txt` (not a generated parent or platform):

1. **Base:** the first available of:
   - `displayName` from the seen table;
   - the status document's `name` ("Route 128, MA");
   - the directory's `name`;
   - today's static `stop_name`.

   The status document comes before the directory because it is Amtrak's name for the station
   itself. The directory gives the municipality ("Westwood, MA"); 30 of 433 seen stations differ.
2. **City collision:**
   - **What counts as a collision:** stops whose base names share a city key. The city key is the
     directory `city` (or the part of `name` before the comma), lowercased.
   - **When it applies:** only when at least two of those stops' current names don't already
     differ beyond the city, i.e. a name equal to the bare city or to "City, ST". Example:
     `SPG` "Springfield" and `SPI` "Springfield, IL" collide; `BOS` "Boston" and `BBY` "Boston
     Back Bay" don't.
   - **What changes:** only the colliding stops whose name is bare, meaning the city alone or
     "City, ST". Each takes its "City, ST" name: the status document's, else the directory's.
     For example, `SPG` and `SPI` become "Springfield, MA" and "Springfield, IL".
   - **Board names that are already distinct stay.** `EWR` keeps "Newark Airport", while `NWK`
     becomes "Newark, NJ". *(Builder-check call: this follows the owner's rule of board names,
     added to only when ambiguous.)*
3. **Exact collision:** stops whose names are still equal take
   `name + " – " + facilityName64` when the directory has a facility. Example: `MYA` becomes
   "Monterey, CA – Monterey Bay Aquarium".
4. **Still equal:** append ` (CODE)`, as Amtrak's own `autoFillName` does. On the 2026-11-21 data
   this applies only to `NRG` and `WES` ("Los Angeles, CA").

Steps 2–4 run until nothing changes. On the 2026-11-21 data the result has **0 duplicate names
across 645 stops**, and about 590 names differ from today's. The builder check counted 594; the
exact figure depends on which stations the worker has seen, so tests must not pin it.

- **`stop_desc`:** built from three parts.
  - **The station name:** the status document's `name`, else the directory's.
  - **The facility:** ` – ` plus `facilityName64`, else the status document's `facility`, when
    present.
  - **The municipality:** ` (in City, ST)`, from the directory's `name`, when its city differs from
    the station name's.

  Examples: RTE "Route 128, MA (in Westwood, MA)"; NYP "New York, NY – Moynihan Train Hall at Penn
  Station"; BBY "Boston, MA – Back Bay Station". `stop_desc` is left empty when it would equal
  `stop_name`, because the MobilityData validator warns when they repeat each other.
- **`tts_stop_name`:** `audioName` when present, otherwise empty.
- **Missing columns:** `stop_desc` and `tts_stop_name` are added to `stops.txt` when absent, as
  `location_type` is today.
- **Generated stops:**
  - the parent `{stop}:station` takes the stop's final `stop_name` and `stop_desc`;
  - each platform `{stop}:track:{label}` takes `"{final name} {Track …}"`, using the existing
    `platform_name`.

### Worker (Python, `browser-worker/worker/`)

- **New `stations` module.** It fetches the directory from the same page context the status fetches
  use: after the first successful page load, then every 14 days (owner's cadence).
  - **Interval:** `STATIONS_REFRESH_SECS`, default `1209600`.
  - **Validation:** a non-empty JSON array of objects, each with a 3-letter `code`. Anything else is
    rejected, and the previous copy is kept; a failed fetch is retried hourly.
  - **Persistence:** the copy and its fetch time are saved under the worker's data directory, so a
    restart neither refetches it nor resets the 14 days.
- **Board names from status documents.** On every stored status document, it records each
  `data[0].stops[i].station` (`code`, `name`, `displayName`, `facility`, `audioName`) into a
  persistent per-code table.
  - **Merging:** later documents update fields; an absent field never erases a known one.
  - **Storage:** written atomically, as `observed-platforms.txt` is.
- **New route `GET /stations.json`:**
  `{"fetched_at", "directory": [...], "directory_fetched_at", "seen": {code: {...}}}`. Before
  anything is known it returns `503`, and it returns `404` when the Amtrak status source is off, as
  `/tracks.json` does.
- **Names in the worker's own output** (owner requirement):
  - `worker.tracks.dump` rows show `displayName` (falling back to `name`) beside each code;
  - the per-cycle "track poll picks" log names the picked trains' next stations by board name.

### Service (Rust)

- **Fetching.** The service reads `/stations.json` from the worker only when a renaming is due
  (below), on the tracks refresher's client and allowlist, retrying hourly until it gets one.
- **Applying names** (new module `src/static_names.rs`):
  - `fn station_names(directory, seen) -> StationNames`, the per-code inputs;
  - `fn apply_names(stops_csv, &StationNames) -> Result<Vec<u8>, AugmentError>`, the naming rule
    above.
  - It runs inside the static build, after `repair_static` and before platform augmentation, so the
    generated stops inherit the new names.
- **Versioning.** `feed_info.feed_version` and the published static version gain
  `+names.{digest}`.
  - **What the digest covers:** the first 8 hex digits of the SHA-256 of the **resolved output only**.
    That is `stop_id`, `stop_name`, `stop_desc` and `tts_stop_name` for the feed's stops, in
    `stop_id` order. It doesn't cover the raw directory or seen table, so a change to a field this
    feature doesn't use never changes the static version. *(Builder-check call.)*
  - **Ordering:** the suffix follows `+tracks.{digest}` when both apply, so the realtime headers still
    match `feed_info`.
  - **Restored snapshots:** `matches_platforms`, the check that a restored snapshot fits the current
    configuration, has to accept the extra suffix and compare it with the *applied* names.
- **Cadence: every 14 days** (owner's cadence; `AMTRAK_STOP_NAMES_REFRESH_SECS`, default
  `1209600`).
  - **Applied names:** the inputs the last renaming used are persisted, with their time, to
    `{AMTRAK_OUTPUT_DIR}/tracks/station-names-applied.json`. Every static build uses them: the daily
    scheduled refresh, track-triggered rebuilds (30 s debounce, 10 min gap), restarts, and the
    restored-snapshot check.
  - **Board names persist** (owner decision, 2026-11-21): the read is merged over the applied inputs
    before resolving. A present field wins and an absent field keeps the applied value; stations
    missing from the read keep their entry; an empty directory keeps the applied directory.
  - **When a renaming is due**, the service reads `/stations.json` and resolves the names. If the
    resolved digest differs from the applied one, it persists the new inputs as applied and rebuilds
    static, even when Amtrak's GTFS answers `304`, skipping the conditional request the way a new
    preferred platform table already does. If the digest is unchanged, it only records the time.
  - **So names change at most once every 14 days,** and never because of a poll, a new track, a
    restart or the daily static refresh.
  - **First renaming** *(builder-check call)*: 24 hours after the worker's first successful directory
    fetch, so the seen table already holds a day of board names (433 stations after one day on
    2026-11-21). Until then, stop names are unchanged and there is no `+names` suffix. This avoids
    a directory-only first round followed by a board-name round two weeks later.
- **Manual refresh** (owner decision, 2026-11-21, option A of three: a subcommand, a signal, or an
  HTTP endpoint):
  - **Command:** `amtrak-gtfs-rt-service refresh-station-names`, run in the container, for example
    `docker compose exec amtrak-gtfs-rt amtrak-gtfs-rt-service refresh-station-names` (the release Compose file's service name).
    - It writes the request time (Unix seconds) atomically to
      `{AMTRAK_OUTPUT_DIR}/tracks/station-names-refresh-requested`, prints that the refresh was
      requested, and exits `0`.
    - It exits `2` with a message, and writes nothing, when names are off (`AMTRAK_STOP_NAMES`,
      `AMTRAK_TRACKS` or `AMTRAK_TRACKS_AMTRAK`) or `AMTRAK_OUTPUT_DIR` is unusable.
    - It starts no service, opens no port and needs no network.
  - **Worker:** each poll cycle, it compares the request time with the last request it acted on. A
    newer request fetches the directory at once, ignoring the 14-day interval and the hourly failure
    backoff. It is acted on once whether the fetch succeeds or fails, and the worker never deletes
    the file.
  - **Service:** the names refresher wakes every minute, and keeps the hourly cadence check. When
    the request file exists, it reads `/stations.json` and renames immediately, skipping the 14-day
    and first-24-hour waits. That happens once the directory was fetched at or after the request, or
    10 minutes after the request, whichever comes first. The merge and every guard still apply.
    - It deletes the file after a renaming is applied.
    - A failed read or a guarded empty read keeps the file and retries the next minute.
    - An hour after the request, the service gives up, deletes the file and logs a warning.
    - A manual renaming resets the 14-day clock.
  - **Result:** fresh names published within about two minutes. There is no new route or port, and
    it works with the read-only root filesystem, because the request file is on the data volume.
- **Failure.** If renaming fails, or the renamed feed fails parsing or validation, the build logs a
  warning and publishes without names. That is the same fallback augmentation already uses.
- **Configuration.** `AMTRAK_STOP_NAMES` (`on` | `off`, default `on`) takes effect only when the
  worker's Amtrak status source is on (`AMTRAK_TRACKS` and `AMTRAK_TRACKS_AMTRAK`). With it off,
  `stops.txt` names are unchanged, and the version has no `+names` suffix.
- **Add-on.** `platform-addon.zip` already carries a changed `stops.txt`, so it carries the names
  too. `official-static.zip` stays byte-for-byte Amtrak's.

### Documentation

- **README:** a "Station names" section with the rule, the sources, the cadence and
  `AMTRAK_STOP_NAMES`, plus the worker's new route and setting.
- **AGENTS.md:** the route, the module, and the `+names` suffix.
- **CHANGELOG `Unreleased`:**
  - most stop names change, so consumers that match stops by name must re-match, by `stop_id`;
  - the feed version gains `+names.{digest}`.

## Testing

- **Rust unit tests in `static_names`,** using the cases from the 2026-11-21 data:
  - `BOS`/`BBY`/`BON` keep distinct board names;
  - `NHV` "New Haven" and `STS` "New Haven (State Street)";
  - `SPG`/`SPI` both get states, through the city collision;
  - Lakeland's two stops resolve through the facility;
  - `NRG`/`WES` get codes;
  - a stop missing from every source keeps its static name;
  - `stop_desc` is empty when it equals `stop_name`, and `tts_stop_name` comes from `audioName`;
  - the output has no duplicate `stop_name`;
  - the same input always gives the same bytes and digest.
- **Static pipeline tests:**
  - with names on, the version is `…+tracks.X+names.Y` and the validator passes;
  - with names off, the output is unchanged;
  - a restored snapshot with a stale names digest is rebuilt.
- **Worker tests (pytest):**
  - directory validation (an empty array, a non-array or a missing `code` keeps the previous copy);
  - merging the seen table never erases fields;
  - `/stations.json` answers `503` before data, `404` when off and `200` after;
  - dump rows include the name.
- **Fixture validators:** `validate-feeds.sh --fixture` still reports zero ERROR notices with names
  applied, so the fixture needs a small directory and seen table.

## Left to the builder

- Module boundaries inside the worker (a new `stations/` package or inside `tracks/`), and exact
  JSON field names on `/stations.json`.
- Whether the service reads `/stations.json` from the tracks refresher or from its own task.
- Test helper layout.

# Station Names Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish Amtrak's departure-board names as `stop_name`, the other details as `stop_desc`,
and announcement names as `tts_stop_name`. Names change at most every 14 days, and the browser
worker supplies and shows them (#179).

**Architecture:**
- **The worker:**
  - keeps every `station` object it sees in status documents;
  - fetches Amtrak's station directory every 14 days through its resident browser page;
  - persists both, and serves them at `/stations.json`.
- **A new Rust module, `src/static_names.rs`,** resolves names with a pure, deterministic ladder.
- **The static pipeline** applies the *applied* names inside augmentation and versions the result
  with `+names.{digest}`.
- **A new Rust task** renames at most every 14 days, persists what it applied, and wakes the static
  refresher for one non-conditional rebuild.

**Tech Stack:** Rust (axum, tokio, reqwest, csv, serde_json, sha2); Python 3 worker (Playwright,
pytest).

**Spec:** `docs/superpowers/specs/2026-11-21-station-names-design.md`

## Global Constraints

- **No change** to any `stop_id`, `parent_station`, platform id or `platform_code`, to the realtime
  feeds, or to `official-static.zip`.
- **`stop_name`** comes from a ladder. The base is the first available of:
  1. `displayName` (seen);
  2. the status document's `name` (seen);
  3. the directory's `name`;
  4. today's static name.

  Disambiguation then runs in order: (a) a city collision changes only bare names to their "City,
  ST" form; (b) an exact collision adds ` – ` + `facilityName64`; (c) anything still equal gets
  ` (CODE)`. It repeats until nothing changes, and the result has no duplicate `stop_name`.
- **`stop_desc`** is the station name (seen `name`, else the directory's `name`), plus ` – ` +
  facility (`facilityName64`, else the seen `facility`), plus ` (in {directory name})` when the
  directory's city differs from the station name's city. It is left empty when it equals `stop_name`.
- **`tts_stop_name`** is the seen `audioName`, else empty.
- **The digest** is the first 8 hex digits of the SHA-256 over the resolved output only: `stop_id`,
  `stop_name`, `stop_desc` and `tts_stop_name` for the feed's plain stops, sorted by `stop_id`. The
  suffix `+names.{digest}` follows `+tracks.{digest}`.
- **Cadence:**
  - the worker refetches the directory every `STATIONS_REFRESH_SECS` (default `1209600`), retrying
    hourly after a failed fetch;
  - the service renames every `AMTRAK_STOP_NAMES_REFRESH_SECS` (default `1209600`);
  - the first renaming comes 24 hours (`86400` s) after the directory's first successful fetch.
- **Configuration:** `AMTRAK_STOP_NAMES` is `on` (default) or `off`, and takes effect only when
  `AMTRAK_TRACKS` and `AMTRAK_TRACKS_AMTRAK` are on. When it's off, `stops.txt` is unchanged and
  the version has no `+names`.
- **Files:**
  - worker: `{AMTRAK_OUTPUT_DIR}/tracks/stations-directory.json` and
    `{AMTRAK_OUTPUT_DIR}/tracks/stations-seen.json`;
  - service: `{AMTRAK_OUTPUT_DIR}/tracks/station-names-applied.json`;
  - every write is atomic (temp file plus rename).
- **Directory URL:** `https://www.amtrak.com/services/data.stations.json`, fetched in the page with
  `Accept: application/json`.
- **Commits:** subjects are imperative sentences ending with a period, with the trailer
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. The PR says `Closes #179`.

## Review Focus

- **A station Amtrak renames or drops between two renamings** must not leave a duplicate
  `stop_name`. The ladder runs on every build. Test: `ladder_never_leaves_duplicates_for_any_input`
  in Task 1.
- **A corrupt or truncated persisted file** must be ignored (renamed to `*.invalid`), not fail
  startup or the worker. Tests: `corrupt_applied_file_is_ignored` (Task 2) and
  `corrupt_seen_file_is_ignored` (Task 3).
- **A `/stations.json` read that fails at the due time** must keep the applied names and retry
  hourly, never apply an empty set. Test: `failed_read_keeps_applied_names` (Task 5).
- **Amtrak's static GTFS adding a new stop between renamings** must get today's name (no input for
  it) and still be unique. Test: `stop_without_any_source_keeps_static_name` (Task 1).
- **Generated parent and platform stops** must take the final name after renaming, not the old one.
  Test: `platforms_inherit_the_new_name` (Task 2).

---

### Task 1: The naming ladder (`src/static_names.rs`)

**Files:**
- Create: `src/static_names.rs` (module doc, types, functions, unit tests)
- Modify: `src/lib.rs` (`pub mod static_names;`)

**Interfaces:**
- Produces (`src/static_names.rs`):
  - `pub struct SeenStation { pub name: Option<String>, pub display_name: Option<String>, pub facility: Option<String>, pub audio_name: Option<String> }`
  - `pub struct DirectoryStation { pub name: String, pub city: String, pub facility: Option<String> }`
    - `name` is "City, ST" and `facility` is `facilityName64`.
  - `pub struct StationNames { pub directory: BTreeMap<String, DirectoryStation>, pub seen: BTreeMap<String, SeenStation> }`
    - derives `Clone, Debug, Default, PartialEq, Serialize, Deserialize`.
  - `pub struct ResolvedName { pub stop_name: String, pub stop_desc: String, pub tts_stop_name: String }`
  - `pub fn resolve_names(static_names: &BTreeMap<String, String>, inputs: &StationNames) -> BTreeMap<String, ResolvedName>`
  - `pub fn names_digest(resolved: &BTreeMap<String, ResolvedName>) -> String`, 8 lowercase hex
    characters.
  - `pub fn apply_names(stops_csv: &[u8], inputs: &StationNames) -> Result<(Vec<u8>, String), crate::static_augment::AugmentError>`
    - renames plain stops (`location_type` empty or `0`, no `parent_station`) and returns the bytes
      and the digest;
    - adds the `stop_desc` and `tts_stop_name` columns when absent;
    - keeps the UTF-8 BOM handling and the `\n` terminator of `augment_stops`.
  - `pub const NAMES_VERSION_MARKER: &str = "+names.";`
  - `pub fn names_version_suffix(digest: &str) -> String`, returning `+names.{digest}`.

- [ ] **Step 1: Write the failing tests** in the module's `tests`. Use a helper,
  `fn inputs(seen: &[(&str, SeenStation)], dir: &[(&str, DirectoryStation)]) -> StationNames`.
  The expected values, from the 2026-11-21 data:
  - `board_names_kept_when_distinct`: BOS, BBY and BON with display names "Boston", "Boston Back
    Bay" and "Boston North Station" keep them.
  - `city_collision_adds_state_to_bare_names`:
    - SPG has display "Springfield" and directory "Springfield, MA" (city "Springfield");
    - SPI has directory "Springfield, IL" and no display name;
    - both have static name "Springfield";
    - → "Springfield, MA" and "Springfield, IL".
  - `distinct_board_name_survives_city_collision`:
    - EWR has display "Newark Airport", directory "Newark, NJ";
    - NWK has display "Newark, NJ", directory "Newark, NJ";
    - NRK has display "Newark, DE", directory "Newark, DE";
    - → EWR "Newark Airport", NWK "Newark, NJ", NRK "Newark, DE".
  - `exact_collision_adds_facility`: LAK and LKL both have display "Lakeland" and directory
    "Lakeland, FL", with facilities "To/from points north" and "To/from points south" → "Lakeland,
    FL – To/from points north" and "Lakeland, FL – To/from points south".
  - `last_resort_adds_code`: NRG and WES have no seen station and directory "Los Angeles, CA" with
    no facility → "Los Angeles, CA (NRG)" and "Los Angeles, CA (WES)".
  - `stop_without_any_source_keeps_static_name`: CBN has static "Canadian Border New York" and no
    inputs → unchanged.
  - `stop_desc_details`:
    - RTE: seen name "Route 128, MA", display "Route 128"; directory "Westwood, MA" (city
      "Westwood") → `stop_desc` "Route 128, MA (in Westwood, MA)";
    - NYP: seen name "New York, NY"; directory "New York, NY" with facility "Moynihan Train Hall at
      Penn Station" → "New York, NY – Moynihan Train Hall at Penn Station";
    - SPI: `stop_desc` empty, because it would equal its `stop_name`.
  - `tts_from_audio_name`: NWK with `audio_name` "Newark New Jersey" → `tts_stop_name` "Newark New
    Jersey"; BOS → "".
  - `ladder_never_leaves_duplicates_for_any_input`: 50 stops whose seen display names, directory
    names and static names are all identical → 50 distinct `stop_name`s.
  - `digest_is_deterministic_and_output_only`: the same inputs give the same digest. Adding a
    directory entry for a code not in `static_names` doesn't change the digest.
  - `apply_names_adds_columns_and_skips_generated_rows`:
    - the input `stops.txt` has no `stop_desc`/`tts_stop_name` columns and includes a
      `location_type=1` row;
    - → the columns are present, the plain row is renamed, and the station row is unchanged.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --locked --lib static_names::`
Expected: compile errors (the module doesn't exist), then FAIL until it's implemented.

- [ ] **Step 3: Implement the module.**
  - The ladder follows Global Constraints.
  - **City key:** the lowercased directory `city`, else the lowercased part of the base before
    `,`.
  - **Bare name:** a name whose lowercase equals the city key, or equals the stop's "City, ST" name
    (seen `name`, else the directory's).
  - Iterate steps (a)–(c) until there are no duplicates. Step (c) always terminates, because codes
    are unique.

- [ ] **Step 4: Run them to see them pass**

Run: `cargo test --locked --lib static_names:: && cargo fmt --check`
Expected: all pass, and fmt is clean.

- [ ] **Step 5: Commit.** Subject: "Resolve station names with a fixed, deterministic ladder."

### Task 2: Apply names in the static build, and version them

**Files:**
- Modify: `src/static_augment.rs` (`augment_static` and the feed_version suffix)
- Modify: `src/static_gtfs.rs`:
  - `build_static` and `augmented_snapshot`, which pass names through;
  - `matches_platforms`, which becomes `matches_static_config`;
  - `refresh_snapshot_once` and `run_snapshot_refresh(_with)`, which take the names handle and
    wake on its change;
  - `bootstrap_static` and the restored-snapshot check.
- Modify: `src/static_names.rs` (the `SharedNames` handle)
- Modify: `src/lib.rs` (pass `Option<SharedNames>` where `SharedPlatforms` is passed)

**Interfaces:**
- Consumes: `apply_names`, `names_version_suffix`, `NAMES_VERSION_MARKER` and `StationNames`
  (Task 1).
- Produces:
  - `pub struct AppliedNames { pub inputs: StationNames, pub applied_at_unix: u64 }`, serde.
  - `pub struct SharedNames`, in `src/static_names.rs`:
    - `pub fn load(path: PathBuf) -> SharedNames`: a missing file means nothing applied; a corrupt
      file is renamed to `*.invalid` and ignored;
    - `pub fn applied(&self) -> Option<Arc<AppliedNames>>`;
    - `pub fn set_applied(&self, names: AppliedNames) -> std::io::Result<()>`: persists atomically,
      then notifies;
    - `pub fn changed(&self) -> Arc<tokio::sync::Notify>`.
  - `augment_static(upstream: &[u8], table: &PlatformTable, names: Option<&StationNames>) -> Result<Vec<u8>, AugmentError>`
    - with `names`, it renames `stops.txt` *before* platform generation, so parents and platforms
      inherit the new names;
    - the feed_version suffix becomes `tracks_version_suffix(table) + names_version_suffix(digest)`.
  - `pub fn matches_static_config(version: &str, tables: &[PlatformTable], names_applied: bool) -> bool`
    - the tracks rule is unchanged;
    - the version contains `NAMES_VERSION_MARKER` exactly when `names_applied`;
    - it replaces every `matches_platforms` call.
  - `run_snapshot_refresh(..., platforms: Option<SharedPlatforms>, names: Option<SharedNames>)`:
    - a names change wakes the loop immediately, with no debounce;
    - that refresh is non-conditional (validators are not sent).

- [ ] **Step 1: Write the failing tests:**
  - `static_augment`: `platforms_inherit_the_new_name`. A covered stop `NHV`, renamed through the
    inputs (display "New Haven"), with tracks `1` → the parent is "New Haven" and the platform is
    "New Haven Track 1".
  - `static_gtfs`:
    - `names_applied_version_has_both_suffixes`: the version ends with `+tracks.{t}+names.{n}`, and
      the validator is called;
    - `names_off_output_unchanged`: `None` → the bytes are identical to today's augmentation;
    - `restored_snapshot_without_names_is_rebuilt`: `matches_static_config(v, tables, true)` is
      false for a `v` without `+names.`;
    - `names_change_forces_full_download`: after `set_applied`, the next refresh sends no
      `If-None-Match` (assert at a mock server);
    - `corrupt_applied_file_is_ignored`: a garbage file → `applied()` is `None`, and the file is
      renamed to `*.invalid`.
    - `renaming_failure_keeps_tracks`: `apply_names` fails (a `stops.txt` with no `stop_name`
      column) → the output is still augmented with platforms, its version has `+tracks.` and no
      `+names.`, and a warning is logged. This is the spec's "publishes without names", not
      "without augmentation".

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --locked --lib static_augment:: static_gtfs:: static_names::`
Expected: FAIL (the signatures change, and the new behaviour is missing).

- [ ] **Step 3: Implement.** Inside `augment_static`, an `apply_names` error is logged with
  `tracing::warn!(stage = "static", ...)`, and augmentation continues on the unrenamed `stops.txt`
  with no `+names` suffix. Thread `Option<&StationNames>` through `build_static` and
  `augmented_snapshot` from `SharedNames::applied()`. Update every caller and test in
  `static_gtfs.rs`, `orchestrator.rs` and `lib.rs`: callers without names pass `None`, and the
  old `matches_platforms(v, t)` cases become `matches_static_config(v, t, false)`.

- [ ] **Step 4: Run the whole suite**

Run: `cargo test --locked --all-targets --all-features && cargo fmt --check`
Expected: all pass.

- [ ] **Step 5: Commit.** Subject: "Apply the applied station names in the static build and version them."

### Task 3: The worker keeps station names and the directory, and serves them

**Files:**
- Create: `browser-worker/worker/tracks/stations.py`
- Modify:
  - `browser-worker/worker/tracks/poller.py`:
    - `ResidentBrowser.fetch_json`;
    - record the seen stations after `store.put` in `poll_once`;
    - in `run_forever`, fetch the directory once per due window.
  - `browser-worker/worker/tracks/config.py`: `stations_refresh_secs` from `STATIONS_REFRESH_SECS`
    (default `1209600`), with `pos_int`.
  - `browser-worker/worker/server.py`: `STATIONS_PATH = "/stations.json"`, routed when tracks are on.
  - `browser-worker/worker/__main__.py`: build the `StationsStore` and put it in `Snapshots`.
- Test: `browser-worker/tests/tracks/test_stations.py`, plus additions to
  `browser-worker/tests/tracks/test_server.py`

**Interfaces:**
- Produces (`worker/tracks/stations.py`):
  - `DIRECTORY_URL = "https://www.amtrak.com/services/data.stations.json"`
  - `class StationsStore`:
    - `__init__(self, root: str | None)`: `root` is `{AMTRAK_OUTPUT_DIR}/tracks`, or `None` for
      in-memory only;
    - `record_document(self, document: dict) -> None`: merges every `data[i].stops[j].station`
      into the seen table by `code` (fields `name`, `displayName`, `facility`, `audioName`). A
      field never erases a known value. It persists when something changed.
    - `directory_due(self, now: float, refresh_secs: int) -> bool`: true when the directory is
      missing, older than `refresh_secs`, or the last attempt failed more than 3600 s ago.
    - `accept_directory(self, body: str, now: float) -> bool`: validates a non-empty JSON array of
      objects, each with a 3-letter `code`. It stores and persists on success; on failure it records
      the failure time and keeps the old copy.
    - `snapshot(self) -> dict | None`: returns `{"directory": [...], "directory_fetched_at": iso,
      "seen": {...}, "seen_updated_at": iso}`, or `None` when both the directory and the seen
      table are empty.
    - `display_name(self, code: str) -> str`: `displayName`, else the seen `name`, else the
      directory `name`, else `code`. Task 4 uses it.
  - `ResidentBrowser.fetch_json(self, url: str, timeout_secs: int) -> tuple[int, str]`: the
    in-page fetch that `fetch` already does, made generic. `fetch` calls it.
- The server answers `/stations.json`:
  - `200` with `snapshot()` as JSON;
  - `503` when it is `None`;
  - `404` when tracks are off.

- [ ] **Step 1: Write the failing pytest tests:**
  - `test_seen_merge_never_erases`: a document whose BOS has `facility` "South Station", then one
    without `facility` → it stays "South Station".
  - `test_directory_rejects_bad_bodies`: `"[]"`, `"{}"`, `"[{\"code\": \"BO\"}]"` and `"not json"`
    are each rejected, keeping the previous copy.
  - `test_directory_due`:
    - missing → due;
    - fetched at `t` → not due at `t+1209599`, due at `t+1209600`;
    - a failure at `f` → not due at `f+3599`, due at `f+3600`.
  - `test_persistence_round_trip`: write, then a new `StationsStore(root)` sees the same seen
    table, the same directory and the same `directory_fetched_at`.
  - `test_corrupt_seen_file_is_ignored`: garbage in `stations-seen.json` → an empty seen table,
    with the file renamed to `*.invalid`.
  - In `test_server.py`, `test_stations_route`: `404` with tracks off; `503` empty; `200` with the
    directory's `BOS` row and the seen `BBY` after records.

- [ ] **Step 2: Run them to see them fail**

Run: `cd browser-worker && python -m pytest tests/tracks/test_stations.py tests/tracks/test_server.py -q`
Expected: FAIL (the module is missing).

- [ ] **Step 3: Implement** the module and the wiring above. In `run_forever`, after each cycle:
  if `stations.directory_due(now, cfg.stations_refresh_secs)`, call `browser.fetch_json(DIRECTORY_URL, cfg.fetch_timeout_secs)`.
  Then `accept_directory` when the status is `200`, or record a failure. Log one INFO line with
  the row count, or a WARNING with the reason.

- [ ] **Step 4: Run the worker suite**

Run: `cd browser-worker && python -m pytest -q`
Expected: all pass.

- [ ] **Step 5: Commit.** Subject: "Keep Amtrak's station names and directory in the worker and serve them at /stations.json."

### Task 4: Station names in the worker's own output

**Files:**
- Modify:
  - `browser-worker/worker/tracks/dump.py` (a `name` column);
  - `browser-worker/worker/tracks/status_parse.py` (`posted_track_rows` adds `station_display`);
  - `browser-worker/worker/tracks/poller.py` (the "track poll picks" line).
- Test: `browser-worker/tests/tracks/test_dump.py`, `browser-worker/tests/tracks/test_poller.py`

**Interfaces:**
- Consumes: `StationsStore.display_name(code)` (Task 3).
- Produces:
  - `posted_track_rows` rows gain `"station_display"`: `displayName`, else `name`;
  - the picks line becomes `"{train} {hot|ready} {outcome} next={code} ({display})"` per pick;
    `next` is the pick's next station.

- [ ] **Step 1: Write the failing tests:**
  - `test_dump_rows_name_station`: a fixture document whose BBY has `displayName` "Boston Back
    Bay" → the row's `station_display` is "Boston Back Bay";
  - `test_picks_line_names_next_station`: a caplog line contains `next=PVD (Providence)` for a
    pick whose next stop is PVD, with seen display "Providence".

- [ ] **Step 2: Run them to see them fail**

Run: `cd browser-worker && python -m pytest tests/tracks/test_dump.py tests/tracks/test_poller.py -q`
Expected: FAIL.

- [ ] **Step 3: Implement** the column and the log format.

- [ ] **Step 4: Run the worker suite**

Run: `cd browser-worker && python -m pytest -q`
Expected: all pass.

- [ ] **Step 5: Commit.** Subject: "Name stations in the worker's dump rows and poll log."

### Task 5: The service renames every 14 days

**Files:**
- Create: `src/sources/tracks/names.rs`
- Modify:
  - `src/sources/tracks/mod.rs` (`pub mod names;`);
  - `src/config.rs`: `TrackConfig` gains `stop_names: bool` (`AMTRAK_STOP_NAMES`, default on),
    `stop_names_refresh: Duration` (`AMTRAK_STOP_NAMES_REFRESH_SECS`, default `1209600`) and
    `stations_url: Option<String>`. `stations_url` is derived from `amtrak_url` by replacing a
    trailing `/tracks.json` with `/stations.json`, and is `None` when that's impossible.
  - `src/lib.rs`: build `SharedNames::load({output}/tracks/station-names-applied.json)` when
    `stop_names && tracks && amtrak`, pass it to the static bootstrap and refresh (Task 2), and spawn
    `run_names_refresher`.
- Test: in-module tests, with an axum mock worker as in `refresher.rs`

**Interfaces:**
- Consumes: `SharedNames`, `AppliedNames` and `StationNames` (Tasks 1–2); the board client from
  `refresher::board_client`.
- Produces:
  - `pub fn parse_stations(body: &str) -> Result<(StationNames, Option<SystemTime>), String>`:
    converts the worker's JSON (directory rows' `code`, `name`, `city`, `facilityName64`; seen
    `name`, `displayName`, `facility`, `audioName`) and returns `directory_fetched_at`.
  - `pub fn renaming_due(applied: Option<&AppliedNames>, directory_fetched_at: Option<SystemTime>, now: SystemTime, every: Duration) -> bool`
    - with nothing applied: due once `directory_fetched_at + 86400 s <= now`;
    - otherwise: due once `applied_at + every <= now`.
  - `pub async fn run_names_refresher(http: reqwest::Client, url: String, names: SharedNames, every: Duration)`
    - checks hourly;
    - when due, it reads, parses and calls `set_applied`;
    - a failed read keeps the applied names.

- [ ] **Step 1: Write the failing tests:**
  - `parse_stations_reads_worker_json`
  - `first_renaming_waits_a_day`: the directory was fetched at `t` → not due at `t+86399`, due at
    `t+86400`; with no directory → never due.
  - `later_renamings_every_fourteen_days`: applied at `a` → due at `a+1209600`, not at
    `a+1209599`.
  - `due_read_applies_and_notifies`: the mock serves `/stations.json` → the applied file is written
    and `changed()` fires.
  - `failed_read_keeps_applied_names`: the mock answers `503` → `applied()` is unchanged and
    nothing is notified.
  - `stations_url_derivation`: `http://127.0.0.1:8081/tracks.json` →
    `http://127.0.0.1:8081/stations.json`; `http://x/other` → `None`.
  - Config: `AMTRAK_STOP_NAMES=off` → no `SharedNames`, and static stays unrenamed.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --locked --lib sources::tracks::names:: config::`
Expected: FAIL.

- [ ] **Step 3: Implement** the module, the configuration and the wiring.

- [ ] **Step 4: Run the whole suite**

Run: `cargo test --locked --all-targets --all-features && cargo fmt --check && RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps`
Expected: all pass.

- [ ] **Step 5: Commit.** Subject: "Rename stops every 14 days from the worker's station names."

### Task 6: Fixture validators and documentation

**Files:**
- Modify: `examples/validation_fixture.rs`. Build the fixture's static feed with a small
  `StationNames`, covering two stops with a board name and one with `audioName`, so the
  MobilityData validators check the renamed `stops.txt`, `stop_desc` and `tts_stop_name`.
- Modify:
  - `README.md`: a "Station names" section with the rule, sources, cadence, `AMTRAK_STOP_NAMES`
    and `AMTRAK_STOP_NAMES_REFRESH_SECS`; the worker's `/stations.json` and `STATIONS_REFRESH_SECS`
    in the worker section; the `+names` suffix where `+tracks` is described.
  - `browser-worker/README.md`: the route, the setting and the files.
  - `AGENTS.md`: the module table (`src/static_names.rs`, `src/sources/tracks/names.rs`), the
    route, and the `+names` suffix rule.
  - `CHANGELOG.md` `Unreleased`:
    - **Added:** station names;
    - **Changed:** most `stop_name`s change, so consumers that match stops by name must re-match by
      `stop_id`, and the feed version gains `+names.{digest}`.
- Add: the plan and the spec are already committed.

- [ ] **Step 1: Run the fixture validators before the change**

Run: `./scripts/validate-feeds.sh --fixture`
Expected: zero ERROR notices. This is the baseline.

- [ ] **Step 2: Update the fixture and run it**

Run: `./scripts/validate-feeds.sh --fixture`
Expected: zero ERROR notices. `stops.txt` in the output carries the new names, `stop_desc` and
`tts_stop_name`; check with `unzip -p`.

- [ ] **Step 3: Update the docs and check that they name what shipped**

Run: `grep -c 'stations.json' README.md browser-worker/README.md AGENTS.md && grep -c 'AMTRAK_STOP_NAMES' README.md AGENTS.md CHANGELOG.md && grep -c '+names' README.md AGENTS.md CHANGELOG.md`
Expected: every count is 1 or more.

- [ ] **Step 4: Run the whole suite**

Run: `cargo test --locked --all-targets --all-features && (cd browser-worker && python -m pytest -q) && bash scripts/test-release-controls.sh`
Expected: all pass.

- [ ] **Step 5: Commit.** Subject: "Validate renamed stops in the fixture and document station names."

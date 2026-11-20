# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **A hot train with no status document gets fetched.** The track poller fetched up to 24 hot
  trains every cycle, soonest stop first, with no cooldown, so with more than 24 a train 8 to 13
  minutes out could go without one: train 172 had no document until about 6 minutes before
  Providence, and after a restart none again until Route 128 was about 6 minutes away. A train
  whose remaining stops are deboard-only (Route 128 and Back Bay toward Boston) or at stations
  that never show a track also stayed hot until its last stop, taking a slot every cycle. Hot
  trains with no document now go first; every train, hot or not, waits out `TRACK_COOLDOWN_SECS`
  after Amtrak answers for it (even with a 404 or an empty answer), oldest fetch first. A train is
  now hot only for a stop inside the hot window where riders can board and Amtrak has published a
  track: `fleet-trains.json` marks a stop with static `pickup_type` 1 `"boarding": false`, and
  lists the stations with a configured or observed platform as `track_stations`. A boarding stop
  inside the hot window at a station not yet listed (on a fresh install, everything but the seeded
  New York, Newark, and New Haven) still keeps the train checked between full polls, under the
  cooldown. Both fields are optional and the schema stays `amtrak-fleet-trains/v1`; a list without
  them counts every stop, as before. A browser failure mid-cycle no longer marks the trains it left unfetched as attempted,
  so they go first on the next cycle. Each cycle also logs one INFO line naming every pick,
  whether it was hot or ready, and its outcome, for example `track poll picks: 172 hot stored, 86
  hot rejected (http 404), 95 hot browser unavailable (Error), 66 ready skipped (browser
  unavailable)` (#120).
- **Observed tracks reach `static.zip` while the service runs.** A static snapshot built
  from the configured platforms alone no longer counts as current once a track is observed, so the
  rebuild downloads `GTFS.zip` in full instead of accepting a `304`. Before, observed platforms were
  published only after a fresh start or a new upstream feed. A scheduled rebuild now logs
  `new track observed; static rebuild scheduled` (#112).
- **`static.zip`'s `feed_info.feed_version` matches the realtime headers.** It now carries the
  published static version, `{feed_version}+tracks.{digest}`, as the GTFS-Realtime reference
  requires. Before, it kept Amtrak's version, and two `static.zip` files with
  different platform stops shared one `feed_version`. `official-static.zip` is unchanged, and
  `platform-addon.zip` now lists `feed_info.txt` among its files. Consumers that read
  `feed_info.feed_version` to compare with Amtrak's own version should strip the `+tracks.` suffix
  or read `official-static.zip` (#108).
- **A stalled `GTFS.zip` download times out.** The download had no timeout, so a connection that
  stalled could hang startup or the daily refresh until the operating system gave up. It now
  allows 30 seconds to connect and 5 minutes in all, and a timeout is reported like any other
  download failure (#106).
- **A fatal startup error says what failed.** It is logged at `ERROR` through tracing with its
  stage (`configuration`, `static GTFS bootstrap`, `HTTP listener bind`, ...) and the full cause
  chain, for example `static GTFS download failed: error sending request: client error
  (Connect): tcp connect error: Connection refused`, in place of a bare
  `Error: StaticError("static fetch failed")`. A failed static download now names the timeout,
  DNS, connect, TLS, or HTTP status cause in scheduled refresh warnings too. URLs are never
  logged. The process exits with status 1, as before: the `exit=0` in the report is what
  `docker inspect` shows for a container that a restart policy has already started again (#106).
- **Train-status alerts have a `header_text`.** The per-train alerts in `alerts.pb` (ids such as
  `20261117-98`) had only `description_text`, although GTFS-Realtime requires a header. The header
  is now the description's leading label after the train number, such as `Train 98: Delay
  Notification`, with `Train 98 service alert` or `Service alert` as the fallback. An alert whose
  notices are all delay notifications gets `effect: SIGNIFICANT_DELAYS`; other alerts keep no
  effect. Alerts that already had a header are unchanged (#107).
- **Falling back to `transitdocs-asm` says why.** Each cycle in which the Amtrak source errors or
  returns nothing logs `WARN primary realtime source failed; falling back` with the source, the
  fallback, and the error (timeout, connect, HTTP status, decode), with URLs removed. A fallback
  that also fails, or a chain with no fallback, is warned too. When the Amtrak source succeeds
  again, one `INFO` line says it recovered; a healthy source logs nothing extra. The bare
  `ASM data successfully downloaded` line each cycle is a `println!` in the pinned
  `amtrak-gtfs-rt` crate and still appears (#110).
- **An Amtrak answer with no trains falls back.** An answer with no trip updates and no vehicle
  positions still counted as a batch when it carried alerts: the Amtrak crate adds its Pacific
  Surfliner advisories even when it returns no trains, and with notices on the cached station and
  passenger notices were added too. The chain did not fall back to `transitdocs-asm`, nothing was
  warned, and the generation published alerts and no trains. Such an answer now counts as empty,
  and notices are added only to a batch that has something in it, so the fallback takes over and
  publishes the notices. When the fallback is also empty or switched off, no new generation is
  published: the last one keeps serving, and `/readyz` returns `503` once it is 300 s old (#110).
- **Notices survive a fallback generation.** A generation built from `transitdocs-asm` published
  no station or passenger notices, so they vanished from `alerts.pb` for that cycle. The fallback
  now shares the Amtrak source's notices cache, so it publishes the same notices without another
  page fetch. Train-status alerts still come only from the Amtrak source (#110).
- **The feed set served right after a restart has `match_rate`.** Each generation now stores its
  match rate in `match-rate.txt` beside `manifest.txt`, and recovery reads it, so the manifest
  served from disk matches the live one. `manifest.txt` keeps its exact shape, so 0.8.0 can still
  recover these generations after a rollback, and a generation written by 0.8.0 still loads, with
  no `match_rate` (#116).
- **Generation ids count up by one.** The `-<sequence>` suffix advanced by 2 per published
  generation because the `current` marker's temp file name took a number from the same counter.
  The marker has its own counter now, so the suffix reads `-0`, `-1`, `-2`, and a failed publish
  does not use a number up. It still starts at 0 after a restart; the timestamp prefix keeps ids
  unique, and an id already on disk is skipped. Ids remain opaque: fetch `/v1/feed-set.json`
  rather than predicting them (#117).
- **A stalled notices request no longer holds up the feeds.** The service's request to the
  notices page had no timeout, and it runs while the generation is assembled, so a worker that
  accepted the connection and then stopped answering held up every feed. It now gives up after 5
  seconds to connect or 30 seconds in all, and the generation publishes with the last good notices
  (#109).
- **A cold start no longer logs a traceback.** Until the service writes its first train list
  (`{AMTRAK_OUTPUT_DIR}/tracks/fleet-trains.json`), the worker's track poll logs one INFO line,
  `waiting for the service's first train list`, and retries on the next tick. A train list that
  exists but cannot be read, is not JSON, or is not `amtrak-fleet-trains/v1` still logs at ERROR
  (#103).
- **A client that hangs up no longer prints a traceback.** The container healthcheck reads only
  the status line of the worker's `/healthz` and closes, so the worker's body write could fail
  with `ConnectionResetError` or `BrokenPipeError`, and `socketserver` printed the traceback to
  stderr. The worker now logs one line with the peer and path (no query string): DEBUG for
  `/healthz`, WARNING for any other path. Any other handler error is logged at ERROR, with its
  traceback, through the worker's logger. No feed data was lost: the service's track and notices
  requests read whole bodies (#109).
- **Browser failures in the worker's log name their cause.** The track poller logged
  `browser launch failed: Error` when the browser had launched and only the first load of the
  status page had failed. It now logs `first status page load failed:` or `browser launch failed:`
  followed by the error's type and the first line of its message, for example
  `Error: Page.goto: net::ERR_TUNNEL_CONNECTION_FAILED at https://www.amtrak.com/tickets/train-status.html`.
  A failed status fetch carries the same detail. The notices poller's
  `advisories markup not obtained this cycle` line now gives the reason: the navigation error, a
  gate that never appeared, or a page still changing at the timeout. A failed detail page also
  logs its reason. A notices cycle in which the browser itself fails (for example, it closed
  mid-cycle) logs one `WARNING`, `poll cycle failed (...)`, instead of a traceback whose Playwright
  message carried the browser's recent output, page URLs and query strings included; any other
  error still logs its traceback. Any URL in these lines keeps only its scheme, host, port, and
  path (#115).
- **Validation reports carry only their own run.** `validation-reports/` held two tracked
  records from August 2026, so every `scripts/validate-feeds.sh` run overwrote the tracked
  `release-evidence.json` and the `validation-reports` and `offline-validation-reports`
  artifacts paired fresh evidence with a stale "block" `release-decision.json`. The two records
  now live in `.security/release-gate/2026-08-14/`, `validation-reports/` is fully ignored, and
  `scripts/test-release-controls.sh` fails if a file under it is tracked (#102).

## [0.8.0] - 2026-11-18

The first release of the one image: the service, its browser worker with one Chromium, and the
GTFS validator in `ghcr.io/sohampatwardhan/amtrak-gtfs-rt`, installable with Docker alone.

### Upgrading from 0.7.1

- **Drop the two sidecars.** Remove the `track-fetcher` and `advisory-fetcher` services and any
  `AMTRAK_TRACKS_AMTRAK_URL` or `AMTRAK_ADVISORIES_URL` that points at them. The worker inside the
  image replaces both. The Compose service keeps its name, `amtrak-gtfs-rt`, and its published
  port, `127.0.0.1:8090`.
- **Notices are on by default.** Set `AMTRAK_ADVISORIES=off` to keep them off (#70).
- **The fallback position source is on by default.** Set `AMTRAK_FALLBACK_ASM=off` to keep
  publishing nothing while the Amtrak source is down (#62).
- **The image binds `0.0.0.0:8080` and admits `gateway`.** The `-p` binding decides who can reach
  the feed. With `--network host`, set `AMTRAK_BIND_ADDR=127.0.0.1:8080` (and you may clear
  `AMTRAK_ALLOWED_PEER_IPS`) to keep the old loopback-only posture (#71).
- **Notices-only settings carry `NOTICES_`.** `NOTICES_POLL_INTERVAL_SECS`,
  `NOTICES_MAX_STALE_SECS`, `NOTICES_SOURCE_URL`, `NOTICES_SNAPSHOT_DIR`,
  `NOTICES_GATE_SELECTOR`, `NOTICES_DETAIL_TTL_SECS`, `NOTICES_DETAIL_MAX_STALE_SECS`,
  `NOTICES_RENDER_STABLE_MS`, and `NOTICES_RENDER_POLL_MS`, because `POLL_INTERVAL_SECS` and
  `MAX_STALE_SECS` belong to the track source. `SERVE_PATH` is gone (#73).
- **`AMTRAK_ROLE` is rejected.** Any value other than unset, empty, or `combined` refuses to
  start. This only affects a deployment of `main` between releases; the README's "Upgrading from
  the three-container layout" says what to change and how to delete the old `fleet/` and
  `contrib/` directories (#72).

### Added

- **Install with Docker alone.** Each GitHub Release attaches `docker-compose.yml`, which pulls
  and runs the release it names from an empty directory, with no checkout, toolchain, or registry
  login. The README opens with the three commands, and the Raspberry Pi steps no longer clone the
  repository. `docker run -p 127.0.0.1:8090:8080 -v amtrak-data:/data
  ghcr.io/sohampatwardhan/amtrak-gtfs-rt:0.8.0` serves the full feed with no other settings.
- **One image with every feed and helper.** The root `Dockerfile` builds the image from
  `docs/one-container.md` on `python:3.12-slim-bookworm` (digest-pinned): `tini` as PID 1, the
  static musl service, the browser worker with Playwright's `chromium-headless-shell`, and a
  `jlink`'d glibc Java 17 runtime (Temurin 17.0.21) with the same digest-checked validator JAR.
  It is no longer `scratch`, and it is about 1 GB uncompressed on linux/arm64. pip is removed from
  the image. `BROWSER_BUNDLE=full` adds full Chromium and Xvfb (#71).
- **The service supervises the browser worker.** It starts `python3 -m worker` on
  `127.0.0.1:8081` only when `AMTRAK_TRACKS_AMTRAK` or `AMTRAK_ADVISORIES` is on, restarts it with
  backoff (2 s doubling to 60 s), and exits non-zero after 5 exits in a row so the restart policy
  takes over; a run of 5 minutes resets the count. With both off, no Python or Chromium process
  runs. The worker runs in its own process group, which is killed after every exit. On `SIGTERM`
  the service stops the worker (`SIGTERM`, then `SIGKILL` after 5 s), then itself, and exits `0`
  inside Docker's 10-second grace period. `--healthcheck` also fails while the worker's listener
  is not answering. The worker does not inherit `NJT_RAILDATA_USERNAME` or
  `NJT_RAILDATA_PASSWORD` (#71, #52).
- **`gateway` in `AMTRAK_ALLOWED_PEER_IPS`.** The keyword resolves at startup to the IPv4 default
  gateway from `/proc/net/route`, which inside a container is where requests through a published
  port arrive. With no default route it admits nobody. A host binary is unchanged: a non-loopback
  bind with no allowlist still refuses to start (#71).
- **An on/off flag for every optional source.** `AMTRAK_TRACKS_AMTRAK` and
  `AMTRAK_TRACKS_HARTFORD` (both default on) turn Amtrak's own track documents and the Hartford
  board off without blanking a URL. `AMTRAK_ADVISORIES_URL` and `AMTRAK_TRACKS_AMTRAK_URL` default
  to the built-in worker and remain overrides; an explicitly empty URL still skips its source.
  `AMTRAK_ADVISORIES` and `AMTRAK_FILTER_CAPITAL_CORRIDOR` accept the same words as
  `AMTRAK_TRACKS` (`on`/`true`/`1`/`yes`, `off`/`false`/`0`/`no`), and any other value is a
  startup error instead of being read as off. The startup banner names each source, says why one
  is off, and shows the Capital Corridor filter; it never prints a source URL (#70).
- **A fallback position source.** `AMTRAK_FALLBACK_ASM` (default on) and
  `AMTRAK_FALLBACK_ASM_URL` add TransitDocs' ASM train map after the Amtrak source. It runs only
  when the Amtrak source errors or returns an empty batch, matches each train to a static trip by
  train number and service calendar, and publishes only trains whose trip and stop ids resolve,
  with delays in seconds and positions converted from mph. The manifest's `source` reads
  `transitdocs-asm` while it supplies a generation; its trips carry no track or gate stamps. Live
  check: 82 of 93 Amtrak trains resolved. The README credits TransitDocs and notes that no terms
  of use were found for the endpoint (#62).
- **The feed set reports a match rate.** `/v1/feed-set.json` adds `match_rate` (`trip_updates`
  and `vehicle_positions`, each `emitted` and `matched`). The same numbers are logged every cycle,
  and a warning fires when fewer than 80% of at least ten trip updates match, the earliest sign of
  a static and realtime mismatch. A generation recovered from disk has none until the next one is
  built, so the on-disk manifest format is unchanged and a rollback can still read it (#61).
- **Official static GTFS is served untouched.** `official-static.zip` is Amtrak's download.
  `platform-addon.zip` (`amtrak-platform-addon/v1`, plus a digest when it changes a file) holds the
  dangling-stop repair and platform stops. `static.zip` is still the combined feed.
  `feed-set.json` names all three and `platform_addon_version`.
- **Optional Caddy proxy.** `docker compose --profile proxy up` adds gzip, a per-IP rate limit,
  access logs, and TLS when `AMTRAK_PROXY_TLS` is set. It is off by default, builds from the
  repository's `proxy/` directory, and `amtrak-gtfs-rt` still serves `127.0.0.1:8090`.
- **Track schedule and station lead times.** Full track polls default to every 15 minutes
  (`AMTRAK_TRACKS_BASE_INTERVAL_SECS`), with hot checks between them. `AMTRAK_TRACKS_LEAD_TIME_FILE`
  loads an `amtrak-station-lead-time/v1` profile; `observed` stations marked `wider` use their
  suggested windows and other stations keep the global window. `python -m
  worker.tracks.coverage` writes that file (`tracks` queries named trains, `survey` walks the
  active fleet, `merge` folds snapshots).
- **Early track refreshes.** A `train` or `station` query on trip updates queues a debounced
  background refresh (one file per flush under `tracks/early-refresh/`) and still returns the
  cached protobuf; a `station` query refreshes the due trains that stop there.
  `AMTRAK_TRACKS_DEBOUNCE_SECS`, `AMTRAK_TRACKS_MIN_GAP_SECS`, and `AMTRAK_TRACKS_MAX_EARLY` are
  parsed with the rest of the configuration, so a non-numeric value is a startup error that names
  the variable; a blank value counts as unset (#72).
- **Pull requests run the validators on a deterministic fixture.** `scripts/validate-feeds.sh
  --fixture` validates a feed set that `cargo run --example validation_fixture` builds without
  any network, through the real generation builder. Both MobilityData validators must report zero
  ERROR notices. The `Offline GTFS and GTFS-RT validators` CI job runs it when feed-shaping paths
  change; it is not a required check yet. The live daily gate and `validation/baseline.json` are
  unchanged (#88).

### Changed

- **One browser worker with one Chromium replaces the track and advisory fetchers.**
  `browser-worker/` (`python -m worker`) runs Amtrak's train-status documents and the Service
  Alerts & Notices pages in one process, serves `/tracks.json`, `/service-alerts-and-notices`, and
  `/alert/...` from one listener on `127.0.0.1:8081`, and lends one Chromium to both: each source
  opens its own browser context, the track page keeps the browser resident, and with tracks off a
  notices cycle launches the browser and closes it afterward. It reads the service's own flags.
  The one-shot tools are `python -m worker.tracks.dump` and `python -m worker.tracks.coverage`
  (#73).
- **One `getTrainsData` poll feeds every product.** The service writes
  `{AMTRAK_OUTPUT_DIR}/tracks/fleet-trains.json` (`amtrak-fleet-trains/v1`) after each published
  generation and passes it to the worker as `AMTRAK_FLEET_TRAINS_FILE`, so the worker never polls
  the map. A track cycle with no train list yet retries on the next `POLL_INTERVAL_SECS` tick, so
  tracks arrive about two minutes after a cold start instead of fifteen. A test pins one upstream
  poll per generation cycle (#71, #55).
- **One track scheduler per concern.** The worker's poller is the only implementation of the base
  and hot schedule and the lead-time profile; the service is the only implementation of request
  debouncing, the minimum gap, and the 24-per-flush cap. With a lead-time profile, a train is hot
  when any upcoming stop is inside its own station's hot window, and a train named by an
  early-refresh hint skips the worker's 90-second cooldown. A lead-time file that is missing,
  unreadable, or has the wrong schema logs a warning. `AMTRAK_TRACKS_BASE_INTERVAL_SECS` is now
  checked by the worker alone (#66).
- **One hardened Compose service.** `docker-compose.yml` runs `amtrak-gtfs-rt` from the released
  image on `127.0.0.1:8090` with a read-only root filesystem, tmpfs `/tmp` and `/home/amtrak`, all
  capabilities dropped, `no-new-privileges`, a 1.5 GB memory limit, a 512-process limit, and a
  64 MB `/dev/shm` (#71).
- **The daily `GTFS.zip` refresh is conditional.** Later refreshes send `If-None-Match` and
  `If-Modified-Since`, and a `304` keeps the current snapshot without downloading or validating
  again. A restart's first refresh, and a rebuild for new platform tables, always download (#60).
- **Artifacts are hashed once and served without a copy; conditional requests follow RFC 9110.**
  Warm reads and `304` revalidations of the roughly 19 MB `static.zip` no longer re-hash and
  re-copy it on every request. `If-Modified-Since` is ignored when `If-None-Match` is present;
  `*` and weak tags match. ETag values, headers, and URLs are unchanged (#56).
- **`scripts/validate-feeds.sh` also validates `official-static.zip`** on its own, with its own
  report directory and a zero-ERROR gate, so a notice can be pinned on Amtrak's data or on the
  repair and platform stops (#55). Its network steps retry Maven Central's HTTP 429 with backoff
  (`VALIDATION_NETWORK_ATTEMPTS`, `VALIDATION_NETWORK_BASE_DELAY`), and its caches hash only the
  validator pins (#89).
- **The validator's JSON report is the static gate.** OGNL 3.4.13 removes `DefaultMemberAccess`,
  so Thymeleaf's HTML export exits non-zero after `report.json` is written. A report with no
  `ERROR` notices is accepted.
- **The image's vulnerability gate is "no fixable Critical or High".** The Debian, Python, and
  Chromium base always carries Grype matches with no fix (479 on linux/arm64 at this change,
  including 4 Critical that Debian marks won't-fix), so the `scratch` image's zero-match policy
  cannot hold. The full Grype report remains a release asset (#71).
- **License notices are checked package by package.** `/licenses/README` indexes the notices for
  everything in the image: the validator, Playwright's Chromium and ffmpeg downloads, the Java
  runtime, every Debian package, CPython and every Python package, and the Node.js bundled with
  Playwright, plus the offer of source for the AGPL service. `scripts/verify-release-image.sh`
  checks them on both architectures in every pull request. `THIRD_PARTY_LICENSES.html` is
  regenerated against the current `Cargo.lock`, and the release metadata check compares it with
  a fresh `cargo about` run (#74).
- **CI builds the one image on both architectures.** The required `One image (amd64)` and
  `One image (arm64)` checks build both browser bundles natively and run
  `scripts/test-image-offline.sh`. Heavy CI jobs skip their steps when a pull request changes only
  documentation (#72, #96).
- **gtfs-mcp and Trackside are the named consumers.** Public URLs, content types, manifest
  fields, and protobuf header meanings stay; new names are additions. A contract test pins the URL
  list and the artifact cache headers.
- **Docs match the one-container layout.** The README's container, Compose, Raspberry Pi,
  advisories, and track-source passages describe the one image, and its configuration table lists
  every new variable (#57).

### Deprecated

- **`amtrak-gtfs-rt-track-fetcher` and `amtrak-gtfs-rt-advisory-fetcher`** (last published at
  v0.7.1) get no new versions. Unlinking and deleting them waits for the owner's confirmation
  after this release (#74).

### Removed

- **The three-container roles that `main` briefly ran.** `src/roles.rs`, `src/cohort.rs`, the
  fleet and contribution snapshots on the shared volume, `trip-updates.Dockerfile`,
  `alerts.Dockerfile`, and their bake targets are gone, which retires #49, #51, and #54; new
  platforms are staged by an in-process notifier after a 30-second debounce, at most once per 10
  minutes (#72).
- **The fetcher images.** The release no longer builds, scans, attests, or publishes them, and
  `image-release.txt` lists only the one image. `browser-worker/Dockerfile`,
  `scripts/test-fetcher-image.sh`, `track-fetcher/`, `advisory-fetcher/`, and the fetcher bake
  targets are removed (#73, #74).

### Security

- **Patched Rust crates.** The lockfile moves h2 to 0.4.16, rustls to 0.23.46, rustls-webpki to
  0.103.16, tokio to 1.53.3, webpki-root-certs to 1.0.10, `rustix` to 1.1.6 (RUSTSEC-2026-0486),
  and `quinn-proto` to 0.11.19 and `quinn-udp` to 0.5.16 (RUSTSEC-2026-0338, -0523, -0524, and
  -0538). The 2026-11 Cargo risk acceptance is approved and replaces the v0.2.0 record (#58).
- **Playwright 1.64.1 with Node 24.22.1.** 1.64.0 bundled Node 24.21.0, which has fixable High
  advisories CVE-2026-105482, CVE-2026-105483, CVE-2026-106069, and CVE-2026-111497.
- **The validator JAR forces Guava 33.7.2 and OGNL 3.4.13**, the fixed releases for
  GHSA-xxph-c9ww-hj94 and GHSA-crwc-vw2r-qr6m. The Java runtime moves to Temurin 17.0.21, which
  fixes CVE-2026-41254 and CVE-2026-47063 (#71).

## [0.7.1] - 2026-10-21

### Fixed

- **Publishing a new image retries the manifest read.** Tagging a GHCR package that did
  not exist yet can make the next `imagetools inspect` exit 255 with no message. The
  publish job retries that read. A rerun verifies a version tag whose platform digests
  already match, and still refuses a tag that points at something else.
- **The track sidecar can read Amtrak's train list again.** `maps.amtrak.com`'s
  `getTrainsData` now answers `Accept: application/json` with HTTP 406 and serves the
  ciphertext as `text/plain`. The poll asks for `*/*`, which that endpoint accepts. The
  Rust service does not send this header: positions still come from the `amtrak-gtfs-rt`
  crate's default client. `/healthz` stays up through one failed poll and returns 503
  after three in a row, so a loop that keeps running while every poll fails is restarted.

## [0.7.0] - 2026-10-20

### Added

- **Amtrak's boarding gates in GTFS-Realtime.** Where Amtrak posts a gate beside the track
  (Washington's `24` with gate `H`, for example), the stop time's `assigned_stop_id` names a
  platform stop for that track and gate, `WAS:track:24:gate:H`, whose `platform_code` is
  `24 (Gate H)`. GTFS-Realtime has no gate field, so the gate travels in the platform stop that
  consumers already resolve for the track. Gated platforms are observed and added to the static
  feed like new tracks, and `AMTRAK_TRACKS_PLATFORMS` accepts `track@gate` (`WAS=24@H`).
- **One-shot Amtrak track dump.** `python -m fetcher.dump 66 2150 --date 2026-10-19`, run from
  `track-fetcher/` or inside its image, prints every track Amtrak has posted for those trains:
  station, arrival or departure, track, gate, scheduled time, and when Amtrak posted it. It loads
  amtrak.com once in headless Chromium, like the sidecar, then makes one paced status request per
  train-day, capped at 40 by default.
- **The service prints where its feed is.** Once listening it prints the feed-set, generation,
  readiness, and liveness URLs, who may read them, and whether advisories and each track source
  (Amtrak status, NJ Transit RailData, Hartford Line board) are on, then prints the
  vehicle-positions, trip-updates, alerts, and static URLs of the first generation it publishes.
  `AMTRAK_PUBLIC_URL` sets the base URL shown for a published port; Compose sets
  `http://127.0.0.1:8090`. `cargo run` now starts the service without `--bin`.

- **Releases publish the browser sidecars.** Tagged releases push
  `ghcr.io/sohampatwardhan/amtrak-gtfs-rt-track-fetcher` and
  `ghcr.io/sohampatwardhan/amtrak-gtfs-rt-advisory-fetcher` for `linux/amd64` and `linux/arm64`,
  with the service image's tags (version, minor, `latest`) and OCI labels, so a Raspberry Pi pulls
  all three. Each platform digest is built on a native runner, smoke-tested offline (Chromium
  renders a page under the Compose confinement, `/healthz` answers, `SIGTERM` stops it), scanned,
  and SBOM-attested before any tag is created. SIGTERM kills a Playwright navigation that would
  otherwise ignore cancellation, so `docker stop` returns during a poll. The sidecars fail a
  release on a fixable Critical or High Grype match; the service keeps its zero-match policy.
  `image-release.txt` records the sidecars' manifest and platform digests after the service's.

### Changed

- **Image gates run on pull requests.** A pull request that changes the service image, and the
  weekly rescan, runs the container smoke test and the zero-match Grype policy before a tag is
  pushed. Sidecar changes run the sidecar smoke test and the fixable Critical/High policy on
  both architectures.
- **Sidecar images clear fixable High advisories.** Playwright is pinned to 1.64.0, whose driver is
  Node 24.21.0 (1.61.0 shipped Node 24.17.0), and track-fetcher's `cryptography` is pinned to
  50.0.2. Both Dockerfiles `apt-get upgrade` before installing packages, so a release build contains
  Debian security updates. The sidecar gate still fails on any remaining fixable Critical or High
  Grype match.
- **Compose and bake name the released GHCR images.** The root `docker-compose.yml` runs
  `ghcr.io/sohampatwardhan/amtrak-gtfs-rt{,-track-fetcher,-advisory-fetcher}` at
  `${AMTRAK_IMAGE_TAG:-0.7.0}`, so `docker compose pull && docker compose up -d` needs no build.
  The `build:` sections stay; `AMTRAK_IMAGE_TAG=local docker compose up -d --build` builds from
  the checkout. Bake tags each target with the same name as well as `<name>:local`. The release
  metadata check requires both defaults to match the release version.
- **Tracks are on by default.** `AMTRAK_TRACKS` now defaults to on; set `off`, `false`, `0`, or
  `no` to publish Amtrak's feeds without platform stops. Any other value is a startup error
  instead of silently meaning off. This changes the published static feed for deployments that did
  not set it: covered stations gain a parent station and platform stops, and the static version
  gains a `+tracks.{digest}` suffix.
- **The Compose stack runs the track sidecar and reads Amtrak's tracks and gates.** `track-fetcher`
  is no longer behind the `tracks` profile, and `AMTRAK_TRACKS_AMTRAK_URL` points at it by default,
  so `docker compose up` publishes Amtrak's tracks, including at stations outside
  `AMTRAK_TRACKS_PLATFORMS`. `AMTRAK_TRACKS=off docker compose up -d --scale track-fetcher=0` runs
  without tracks or the sidecar's Chromium. Run outside Compose, the Amtrak URL is still empty by
  default, and the service warns at startup that Amtrak's own tracks are off.

## [0.6.2] - 2026-10-19

### Fixed

- **The container starts with the rebuilt validator.** The service checks the bundled GTFS
  validator against a list of approved SHA-256 digests, and that list still named the jar from
  before the jsoup 1.23.2 rebuild, so the `v0.6.1` image refused to start
  (`AMTRAK_GTFS_VALIDATOR_JAR must match an approved MobilityData 8.0.1 CLI SHA-256`). The
  approved repository-hardened digest is now the rebuilt jar's.

### Notes

- `v0.6.0` and `v0.6.1` never completed publication; their changes ship in this release.

## [0.6.1] - 2026-10-19

### Security

- **jsoup 1.23.2 in the bundled GTFS validator.** The release scan blocked `v0.6.0` on
  GHSA-65r4-943x-97jj (High): jsoup 1.23.1's XML parser could exhaust memory on deeply nested
  namespace declarations. The validator's dependency override now pins the fixed 1.23.2.

## [0.6.0] - 2026-10-19

### Added

- **RailData coverage for every station Amtrak and NJ Transit both serve.** The default poll is
  New York Penn, Newark Penn, Newark Airport, Metropark, New Brunswick, Princeton Junction,
  Trenton, and Philadelphia 30th Street (`NY`, `NP`, `NA`, `MP`, `NB`, `PJ`, `TR`, `PH` mapped to
  `NYP`, `NWK`, `EWR`, `MET`, `NBK`, `PJC`, `TRE`, `PHL`). A track from that board is published
  even when it is not in the seeded New York, Newark, and New Haven platform lists. All eight
  stations share one cached token.
- **Amtrak's own track numbers** (still default-off). Set `AMTRAK_TRACKS_AMTRAK_URL` to the
  `track-fetcher` sidecar and `AMTRAK_TRACKS=on`. The sidecar reads
  `GET /dotcom/travel-service/statuses/{train}?service-date={YYYY-MM-DD}` — the document the
  Amtrak app uses for `departure.track.number`, falling back to `arrival.track.number` — for
  trains with a stop inside a 30-minute lookahead and a 10-minute lookback. A train whose next
  stop is within about 12 minutes and still has no track is fetched every minute; the rest of
  that window rotates under a 90-second cooldown with at most 24 fetches a cycle, using one
  resident headless browser. A track stays published until the stop's latest time plus
  `AMTRAK_TRACKS_RETAIN_GRACE_SECS` (default 900), including when a later document omits it.
  Amtrak wins over NJ Transit and the Hartford board while its document is fresh; an older
  Amtrak track yields to a live board reading. An Amtrak track that is not already in
  `AMTRAK_TRACKS_PLATFORMS`, at a station in the static feed, is recorded in
  `observed-platforms.txt` (observed labels only, at most 1000) and becomes a platform stop on a
  static rebuild at most every 10 minutes; if the validator rejects it, static is rebuilt with
  the configured platforms alone. The sidecar backs off browser relaunches, times out each
  request, blocks hosts outside `amtrak.com`, serves `503` when every fetch fails, and fails
  `/healthz` when its poll loop stalls; in Compose it runs read-only with no capabilities and a
  768 MB memory limit. Unset the URL to leave this source off. `docker buildx bake` does not build
  the sidecar; CI builds it on amd64 and arm64.
- **Time- and count-bounded generation retention.** A superseded generation is deleted once it
  has been superseded for `AMTRAK_GENERATION_RETAIN_SECS` (default 600), measured from the newer
  generation's manifest mtime so the window holds across restarts. `AMTRAK_GENERATION_MAX`
  (default 20, current included) caps how many are kept on disk and in memory. The current feed
  set is never deleted.
- **Byte-capped generation retention.** `AMTRAK_GENERATION_BUDGET_BYTES` (default 256 MiB, `0` to
  disable) deletes the oldest immutable generations once unique on-disk bytes exceed the cap.
  The generation referenced by the current feed set is never deleted. Unchanged static GTFS is
  stored once under `blobs/<sha256>` and hard-linked from each `generations/<id>/static.zip`, so
  served URLs stay `/v1/generations/{id}/static.zip`.
- **Static validator cache.** `AMTRAK_VALIDATOR_CACHE` (default on) skips the Java GTFS validator
  when a ZIP's SHA-256 already has an accept or standards-reject result. Tool failures and
  timeouts are not cached.
- **Advisory render limits.** The fetcher re-renders the notices list on its existing 15-minute
  poll, stops each page once the HTML size is stable instead of waiting on page timers, and
  reuses detail pages for `DETAIL_TTL_SECS` (default 24 hours).
- **`amtrak-status` delay deltas.** Train and station output now annotate each predicted time
  with how it compares to the static schedule — `(on time)`, `(12 min late)`, `(3 min early)` —
  and the train-level `delay:` line uses the same label. The delay prefers the feed's own
  GTFS-Realtime `delay` when present and otherwise derives it as predicted time minus scheduled
  time, with the scheduled instant rebuilt in the stop's timezone and aligned to the service
  day nearest the prediction. Amtrak's feed publishes predicted times without `delay` values,
  so the derivation is what normally produces the number; previously the `delay:` line could
  only appear when a feed supplied `delay` explicitly, and per-stop times carried no delay
  information at all.

### Changed

- A static GTFS feed that does not parse now reports why, for example
  `static parse failed: The id OSM is not known` for a stop time that names a stop missing from
  `stops.txt`, instead of a bare `static parse failed`. A rejected daily static refresh logs the
  same reason. Parse errors come from in-memory bytes and never include a URL or credential.
- The README is reorganized: GitHub callouts for warnings and requirements, diagrams for the
  architecture, the manifest-first request flow, the Compose stack, platform stops, and the static
  snapshot lifecycle, a table of the stations where Amtrak's own status posted a track, and a
  section for the `amtrak-status` CLI.
- The container Java runtime's `jlink` set now includes `java.sql` (Gson loads `java.sql.Time`
  while parsing GTFS dates; without it the CLI does not produce a report) and strips debug
  symbols. `java.desktop` remains because notice introspection loads `java.beans`. The validator
  process is started with `-Djava.awt.headless=true`.
- The advisory-fetcher image installs Playwright's Debian 12 Chromium libraries and
  `chromium-headless-shell` directly, instead of `playwright install --with-deps`, which also
  pulled xvfb and the large font packs. Compose sizes `/dev/shm` at 64 MiB because the browser
  is launched with `--disable-dev-shm-usage`.

### Fixed

- **Amtrak's static schedule is accepted again when it names an undefined stop.** In Amtrak's current feed
  (20261018) the Mardi Gras Service calls at Ocean Springs (`OSM`), which `stops.txt` does not
  define, so every static refresh failed with `The id OSM is not known` and the service kept the
  last accepted schedule. With tracks on, this also meant no track was ever stamped. Stop times
  naming a stop that `stops.txt` lacks are now dropped before parsing and validation, with a
  warning naming the stop; the rest of each trip is kept.
- **RailData login errors no longer spend the daily token budget.** NJ Transit answers failed
  logins with HTTP 500 and a JSON `errorMessage` (for example `Missing user account.`); these were
  treated as transport errors, so the service requested a new token for every station and used all
  10 of the day's requests within a minute. Such answers are now classified as rejected credentials
  (or as NJ Transit's daily limit when the message says so), and any failed token request pauses
  NJ Transit requests for an hour.
- **Turning tracks on or off takes effect at startup.** A restart recovered the static feed saved
  by the previous run, so platform stops appeared (or disappeared) only at the next daily static
  refresh and every track was dropped until then. When the recovered static was built for a
  different track configuration, the first static refresh now runs immediately.
- **`BROWSER_MODE=xvfb-headful` works in both browser sidecars.** Neither image had full
  Chromium or Xvfb, and nothing started an X server, so the documented fallback failed at launch.
  Build with `--build-arg BROWSER_BUNDLE=full` (or `FETCHER_BROWSER_BUNDLE=full` for Compose and
  bake) to add both; the fetcher then starts Xvfb itself. The default image is unchanged, and
  selecting headful mode on it stops at startup with a message naming the build argument. CI
  builds both bundles of both images on amd64 and arm64.
- Startup no longer reads every generation on the volume into memory. It loads the current
  generation, prunes the rest by directory metadata, and then loads only the survivors. A volume
  with hundreds of generations left by an older release previously pushed the process past
  5 GB RSS within a minute of starting.

## [0.5.0] - 2026-09-28

### Added

- **Live platform and track assignments** (default-off). With `AMTRAK_TRACKS=on`, the published
  static feed gains a parent station and one platform stop per configured track (`{stop}:track:{label}`
  with `platform_code`) at each covered station, and trip updates point
  `stop_time_properties.assigned_stop_id` at the platform stop with `stop_sequence` set and
  `stop_id` omitted, as the GTFS-Realtime reference requires. Tracks come from NJ Transit's
  registered RailData API (New York Penn, Newark Penn, Newark Airport, Metropark, New Brunswick,
  Princeton Junction, Trenton) and the CTrail Hartford
  Line board (New Haven Union). Defaults cover New York Penn, Newark Penn, and New Haven Union;
  other tracks are added with `AMTRAK_TRACKS_PLATFORMS`. Boards refresh in the background,
  readings expire after five minutes, and the RailData token is cached so the service stays within
  NJ Transit's 10 token requests a day. The static version gains a `+tracks.{digest}` suffix while
  tracks are on.

## [0.4.0] - 2026-09-27

### Added

- **Advisory detail text in `alerts.pb`.** The advisory-fetcher now snapshots each
  advisory's detail page, and the service copies its paragraphs, effective date, and
  PSN into the alert's `description_text`. A missing detail page keeps the title-only
  description.
- **`amtrak-status` consumer** for station departures and train status, reading
  either the local feed set or Amtrak directly.
- **Root-level packaging.** `docker compose up -d --build` at the repository root
  builds and runs the scratch service and the advisory-fetcher together with
  advisories on, published on host loopback port 8090. `docker buildx bake` builds
  both images without starting them. Chromium stays out of the service image.

### Changed

- CI now also runs the advisory-fetcher's offline tests, the release-control
  fixtures, and a Compose/Bake config check, and release scan failures upload the
  reports that explain them.

### Notes

- `v0.3.0` never completed publication; its changes ship in this release.

## [0.3.0] - 2026-08-18

### Added

- **Service advisories as scoped GTFS-RT alerts** (default-off). When enabled, the
  service turns Amtrak's Service Alerts & Notices into alert entities: station
  advisories become stop-scoped (`stop_id`) and passenger advisories become
  route-scoped (`route_id`). It is fail-open — an unreachable source, unexpected
  markup, or an unmappable station never fails feed generation — and is configured
  through `AMTRAK_ADVISORIES` and `AMTRAK_ADVISORIES_URL`.
- **`advisory-fetcher` sidecar** (`advisory-fetcher/`): a standalone Python +
  Playwright container that runs a headless Chromium to defeat Amtrak's Akamai bot
  gate (which blocks plain requests) and serves the advisories HTML over HTTP for
  the service to consume. It is memory-efficient (launch-per-poll, a single browser
  per cycle with none resident between polls, subresource-blocking) and fail-open,
  and ships as its own image so the service keeps its scratch/musl,
  no-vulnerabilities posture. Verified end-to-end on a Raspberry Pi 5.

## [0.2.0] - 2026-08-14

### Added

- **Immutable feed generations** containing one static GTFS snapshot, separated
  TripUpdates, VehiclePositions, Alerts, and a manifest that identifies their
  shared generation and static version.
- **Manifest-first API** at `/v1/feed-set.json` with generation-pinned artifact
  URLs, plus independent `/livez` and freshness-aware `/readyz` health routes.
- **Durable recovery** through an atomically replaced current marker and validated
  generation scan, preserving the last-good feed set across process restarts and
  upstream outages.
- **Internal peer access policy** with loopback-only defaults, exact-IP allowlists,
  fail-closed transport-peer identity, and no trust in forwarding headers.
- **Feed validation gate** running content-pinned MobilityData `gtfs-validator`
  8.0.1 and a commit-pinned `gtfs-realtime-validator` against exactly one
  manifest-selected feed set. It independently decodes all three realtime feeds
  and rejects malformed, expired, or unapproved validator exceptions.
- **CI workflows**: `ci.yml` (build and offline tests on every push and pull
  request) and `validate-feeds.yml` (the validation gate, nightly plus on
  pipeline changes and on demand — kept off pull requests so an Amtrak outage
  cannot block unrelated work).
- **Spec-driven delivery record** under `.specs/amtrak-gtfs-rt-service`, including
  approved requirements, design, task execution evidence, and rollout decision.
- **Public multi-platform container release** for `linux/amd64` and `linux/arm64`
  through GitHub Container Registry, with immutable-digest installation support.
- **Release licensing evidence** containing the project AGPL text, third-party
  notices, per-platform SPDX SBOMs, vulnerability reports, and provenance.

### Changed

- Replaced the Debian/JRE container with a package-manager-free, non-root scratch
  runtime and a source-built MobilityData validator whose Gradle distribution,
  dependency graph, source archive, and final JAR are checksum-verified. The
  exact final image has a retained SPDX SBOM and zero-match Grype report.
- Replaced mutable top-level feed files with immutable generation routes.
- Split mixed upstream entities into type-correct GTFS-Realtime products, removed
  invalid FULL_DATASET deletions, normalized headers, and filtered unresolved
  static references.
- Static GTFS replacements now pass the same pinned standards validator at
  runtime before they can participate in a committed generation.
- Long-lived task supervision now cancels and drains sibling activities when the
  poller, static refresher, or HTTP server exits or fails.

### Known issues

- The complete Cargo audit retains nine inherited advisory warnings with no
  confirmed CISA KEV match. They are not treated as clean, although none blocks
  this container-only remediation.

### Planned

- `TransitDocsSource` / `RailRatSource` positional fallback implementations (the
  `RtSource` trait is already in place for them).
- ETag-conditional static refresh (currently a full re-download each cycle).

## [0.1.0] - 2026-07-29

First working release. Serves live Amtrak GTFS-Realtime feeds, verified end-to-end
against Amtrak's production endpoints (145 trip updates, 145 vehicle positions,
31 alerts in a live poll).

### Added

- **GTFS-Realtime feeds** served over HTTP: `/trip-updates.pb`,
  `/vehicle-positions.pb`, and `/alerts.pb` as `application/protobuf`.
- **Static GTFS** ingest from Amtrak's official
  `https://content.amtrak.com/content/gtfs/GTFS.zip`, re-served at `/static.zip`
  and refreshed on an interval (default daily).
- **`/health`** liveness endpoint.
- **`RtSource` trait** normalizing any provider's data into an `RtBatch`, so
  fallback sources can be added without touching the orchestrator. `AmtrakSource`
  is the sole v1 implementation, delegating decryption, per-station arrival
  parsing, trip matching, and multi-day date-offset handling to
  [`catenarytransit/amtrak-gtfs-rt`](https://github.com/catenarytransit/amtrak-gtfs-rt).
- **Source fallback chain**: sources are tried in order and the first fresh,
  non-empty batch wins; empty and failing sources are logged and skipped.
- **`feed_version` stamping**: every RT feed header carries the active static
  feed's version so consumers can confirm the two match.
- **Atomic writes**: the three `.pb` files are written to temp files and renamed
  as a group, so a failure partway through can never leave a mix of fresh and
  stale feeds — and consumers never read a partial file.
- **Last-good serving**: a poll cycle with no fresh data leaves the previous
  files untouched rather than serving empty or malformed feeds. A failed static
  refresh likewise keeps the last-good schedule.
- **Non-zero exit** when a long-lived task dies, so process supervisors using
  `Restart=on-failure` actually restart the service.
- **Configuration** via environment variables: `AMTRAK_STATIC_URL`,
  `AMTRAK_OUTPUT_DIR`, `AMTRAK_POLL_SECS`, `AMTRAK_STATIC_REFRESH_SECS`,
  `AMTRAK_FILTER_CAPITAL_CORRIDOR`, `AMTRAK_BIND_ADDR`.
- **Optional Capital Corridor filter** (route 84), which has a better dedicated
  feed via 511.org.
- 21 tests, including live integration tests against Amtrak's real endpoints
  (run with `cargo test -- --include-ignored`).

[Unreleased]: https://github.com/sohampatwardhan/Amtrak-GTFS-RT/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/sohampatwardhan/Amtrak-GTFS-RT/compare/v0.7.1...v0.8.0
[0.7.1]: https://github.com/sohampatwardhan/Amtrak-GTFS-RT/releases/tag/v0.7.1
[0.7.0]: https://github.com/sohampatwardhan/Amtrak-GTFS-RT/compare/v0.6.2...v0.7.0
[0.6.2]: https://github.com/sohampatwardhan/Amtrak-GTFS-RT/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/sohampatwardhan/Amtrak-GTFS-RT/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/sohampatwardhan/Amtrak-GTFS-RT/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/sohampatwardhan/Amtrak-GTFS-RT/releases/tag/v0.5.0
[0.4.0]: https://github.com/sohampatwardhan/Amtrak-GTFS-RT/releases/tag/v0.4.0
[0.3.0]: https://github.com/sohampatwardhan/Amtrak-GTFS-RT/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/sohampatwardhan/Amtrak-GTFS-RT/releases/tag/v0.2.0
[0.1.0]: https://github.com/sohampatwardhan/Amtrak-GTFS-RT/releases/tag/v0.1.0

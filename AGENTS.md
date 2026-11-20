# AGENTS.md

Instructions for coding agents working in this repository.

## Project

Amtrak GTFS-RT (AGPL-3.0-only) publishes live GTFS-Realtime for Amtrak — TripUpdates, VehiclePositions, and Alerts — plus the static GTFS those feeds bind to. Consumers start at `GET /v1/feed-set.json` and then fetch only the four immutable URLs in that manifest.

The target is **one container**, specified in [docs/one-container.md](docs/one-container.md). One image, `ghcr.io/sohampatwardhan/amtrak-gtfs-rt`, serves the manifest, the static ZIPs, and all three GTFS-Realtime feeds. It includes the helpers they need: one browser worker and the Java validator. Each optional source has an on/off environment flag. That document supersedes the three-container design in `docs/rearchitecture.md`. The three-container roles are gone: combined mode (`run_combined`) is the only mode, and a leftover `AMTRAK_ROLE` other than unset or `combined` refuses to start. Do not reintroduce roles, shared-volume fleet or contribution snapshots, or per-feed images. New work follows the implementation plan in `docs/one-container.md`. Do not add a reverse proxy beyond the optional Caddy profile unless the owner asks for one.

Decrypt, trip matching, and protobuf encoding stay in the git dependency [`catenarytransit/amtrak-gtfs-rt`](https://github.com/catenarytransit/amtrak-gtfs-rt), pinned in `Cargo.lock`, called from the shared library. A neutral `RtSource` trait wraps that source so another provider can be added without changing the orchestrator. Do not copy code from Piero Maddaleni's repositories. If an approach of his is reused, credit him in `README.md` and add the license notice that `docs/rearchitecture.md` describes. Repositories of his with no `LICENSE` file are not copied.

Both optional features fail open. A notices failure or a track failure never stops the other feeds from publishing.

- **Alerts.** TransitDocs ASM and Pacific Surfliner advisories come from the one fleet poll. Amtrak station and passenger notices (`AMTRAK_ADVISORIES`, default on) are merged in. `www.amtrak.com` blocks plain HTTP (Akamai). The notices browser runs in the background. Do not spoof a browser fingerprint, copy Akamai cookies, or use curl-impersonate.
- **TripUpdates.** Platform, track, and gate assignments (`AMTRAK_TRACKS`, default on) stamp `assigned_stop_id` and `platform_code`. The browser worker is embedded when `/app/worker` exists. Full polls run every `AMTRAK_TRACKS_BASE_INTERVAL_SECS` (default 900). Hot stations, including a profile such as PVD, are still checked on `POLL_INTERVAL_SECS` between those runs. A train is hot only for a stop where riders can board (`fleet-trains.json` marks static `pickup_type` 1 as `"boarding": false`) at a station in its `track_stations` (configured or observed platforms). A boarding stop at a station not yet in `track_stations` makes the train warm: checked between full polls, but in the ready group. Hot trains with no status document go first; every train takes the `TRACK_COOLDOWN_SECS` cooldown after Amtrak answers for it (`worker/tracks/select.py`). `AMTRAK_TRACKS_LEAD_TIME_FILE` is an `amtrak-station-lead-time/v1` JSON file. Only `confidence=observed` and `compared_to_default=wider` change that station's lookahead and hot window. `sparse`, `no_track`, `none`, `covered`, and missing stations keep the global window. A `train` or `station` query on trip updates is debounced (`AMTRAK_TRACKS_DEBOUNCE_SECS`, default 5), coalesced per train, and capped (`AMTRAK_TRACKS_MAX_EARLY`, default 24) with `AMTRAK_TRACKS_MIN_GAP_SECS` (default 90). The HTTP response does not wait for that refresh. Each concern has one implementation: the worker's poller owns the base and hot schedule and the lead-time profile (`worker/tracks/poller.py`, `worker/tracks/lead_time.py`), and the service owns request debouncing, the minimum gap, and the cap (`src/schedule.rs`). Do not add a second copy of either.
- **VehiclePositions.** One `getTrainsData` poll per cycle feeds all three products. Trains are matched to Amtrak's official static GTFS, and the same process serves the public HTTP API.

`cargo run` starts the current single service (`default-run` is `amtrak-gtfs-rt-service`). The `amtrak-status` station and train CLI is a second binary behind the `status` feature and is not in the released container. HTTP handlers serve the last good generation from memory. They must not wait on Chromium or on an upstream fetch. Responses carry `Last-Modified` and `ETag`, and honor `If-None-Match` / `If-Modified-Since` with `304`.

## Architecture

Target image, from `docs/one-container.md`:

| Process | Runs when | Does |
| --- | --- | --- |
| `tini` | always | PID 1. Reaps zombies and forwards signals. |
| `amtrak-gtfs-rt-service`, combined mode | always | One `getTrainsData` poll per cycle (including ASM and Surfliner), the daily `GTFS.zip` download, NJ Transit RailData, the Hartford board, generation assembly, and the public HTTP API on `:8080` |
| Browser worker (Python, one headless Chromium) | `AMTRAK_TRACKS_AMTRAK` or `AMTRAK_ADVISORIES` is on | Amtrak status documents and the notices HTML, on `127.0.0.1` only. Started, restarted, and stopped by the service. |
| Java validator | on each new static ZIP | MobilityData validator 8.0.1 (hardened JAR) on a glibc Java 17 runtime |

`feed-set.json` stays on `127.0.0.1:8090` in Compose. A generation that is missing any of the four files is not current. The previous complete generation keeps being served.

Checked-in paths, until the split replaces them:

| Path | Role |
| --- | --- |
| `src/main.rs` | Wires config, sources, static bootstrap, and supervised tasks for the service |
| `src/config.rs` | Environment configuration, peer allowlist, validator JAR check |
| `src/sources/mod.rs` | `RtSource` and the `RtBatch` model |
| `src/sources/amtrak.rs` | Primary source; wraps the catenary crate |
| `src/sources/identity.rs` | One vehicle identity for both sources: `vehicle.id` the per-train-day id (`20261119-172`, the entity id), `vehicle.label` the train number, on the trip update and the vehicle position |
| `src/sources/asm.rs` | Fallback source (TransitDocs ASM map), used only when the primary errors or returns nothing. Original code; do not copy another project's. |
| `src/sources/advisories.rs` | Optional notices scraper |
| `src/sources/tracks/` | Track and gate stamp, RailData, Hartford, Amtrak status, refresher |
| `src/static_augment.rs` | Adds platform stops to the published static ZIP when tracks are on |
| `src/static_repair.rs` | Drops stop times whose `stop_id` is missing from `stops.txt` |
| `src/static_gtfs.rs` | Exact-byte static validation and the pending/active lifecycle |
| `src/orchestrator.rs` | Source selection, one coherent generation, polling |
| `src/schedule_relationship.rs` | Writes `SCHEDULED` wherever a trip descriptor (trip update, vehicle position, alert informed entity) or stop time update leaves `schedule_relationship` unset. The generation builder calls it on each product after every source and rewrite; values a source set are kept |
| `src/alert_header.rs` | Gives an alert that arrives without `header_text` (Amtrak's per-train status alerts) a title from its description |
| `src/writer.rs` | Immutable generations on disk |
| `src/serve.rs` | axum HTTP API, peer check, and readiness |
| `src/worker.rs` | Starts the built-in browser worker when a browser source needs it, restarts it with backoff, gives up after 5 exits in a row, and stops its process group on `SIGTERM` |
| `src/schedule.rs` | Debounced early track refreshes from `train`/`station` queries, queued as one file per flush under `tracks/early-refresh/` for the worker |
| `src/fleet_trains.rs` | The `amtrak-fleet-trains/v1` train list; combined mode writes it to `{AMTRAK_OUTPUT_DIR}/tracks/fleet-trains.json` after each generation so the worker never polls the map. New fields stay optional (`boarding` per stop, `track_stations`) so the schema stays v1 |
| `src/bin/amtrak_status.rs`, `src/bin/status/` | `amtrak-status` CLI (`--features status`) |
| `browser-worker/` | The one Python/Playwright browser worker (`python -m worker`): the track source (`worker/tracks/`) and the notices source (`worker/notices/`) share one Chromium (`worker/browser.py`) and one listener on `127.0.0.1:8081`. Notices-only settings carry `NOTICES_`. It is built into the root `Dockerfile`; there is no standalone worker image. |
| `scripts/` | Feed validation, container smoke, release controls |
| `Dockerfile`, `docker-compose.yml` | The one image (`tini`, the service, the browser worker and Chromium, the glibc Java runtime and validator, on `python:3.12-slim-bookworm`) and its one hardened Compose service, `amtrak-gtfs-rt`. |
| `docs/one-container.md` | The one-container design and the PR plan. Follow it. |
| `docs/rearchitecture.md` | Superseded three-container design. Kept for its research sections. |

There is no checked-in protobuf codegen step. The `gtfs-realtime` crate's build script compiles its `.proto` files with `protoc` at `cargo build` time.

HTTP contract. Do not add other feed URLs.

| Path | Access | Body |
| --- | --- | --- |
| `/livez` | public | Process liveness, independent of feed readiness |
| `/readyz` | controlled | `200` only while the current generation is younger than 300 seconds |
| `/v1/feed-set.json` | controlled | Generation id, timestamp, static version, platform add-on version, entity counts, artifact URLs |
| `/v1/generations/{id}/static.zip` | controlled | Combined static GTFS for exactly that generation |
| `/v1/generations/{id}/official-static.zip` | controlled | Amtrak's static GTFS, byte for byte |
| `/v1/generations/{id}/platform-addon.zip` | controlled | Overlay (`addon.txt` plus changed GTFS files) that rebuilds `static.zip` |
| `/v1/generations/{id}/{trip-updates,vehicle-positions,alerts}.pb` | controlled | One GTFS-Realtime product, `application/x-protobuf` |

`static.zip` remains the single file existing consumers load. It is the official download with the dangling-stop repair and platform stops applied. Consumers who want the pieces unzip `official-static.zip` and replace each name on the add-on's `files=` line. The Java validator is in the one image, and the `amtrak-gtfs-rt` service serves the public URLs. Each GitHub Release attaches `docker-compose.yml`, which runs the released image from an empty directory, so keep that file working without a checkout: only `--build` and the `proxy` profile may need the repository. An optional Caddy proxy is the Compose `proxy` profile and is off by default. Do not assume nginx or Tailscale Serve is in front.

Controlled routes authorize the direct socket peer. An empty allowlist admits loopback only. `Forwarded` and `X-Forwarded-For` are ignored. A non-loopback bind with no `AMTRAK_ALLOWED_PEER_IPS` refuses to start. `403` is an unauthorized peer, `404` is an unknown generation or artifact, `503` is no current or fresh generation.

Host defaults are bind `127.0.0.1:8080`, output `./out`, and validator JAR `./tools/gtfs-validator-v8.0.1-cli.jar`. The container defaults differ: `/data`, the hardened JAR, bind `0.0.0.0:8080`, and `AMTRAK_ALLOWED_PEER_IPS=gateway`, a keyword that resolves at startup to the container's IPv4 default gateway (the published-port path). With no default route it admits nobody. The full variable table is in the README section "Configuration (environment variables)".

## Setup

- **Rust** stable, 1.85 or newer. The catenary crate uses edition 2024. This repo is developed against 1.96; CI uses `stable`; the release image pins `rust:1.96-alpine`.
- **`protoc`** (`protobuf-compiler` on Debian/Ubuntu).
- **`libssl-dev` and `pkg-config`**. `gtfs-structures` 0.46.1 pulls `reqwest` 0.12 with native TLS, so the build compiles `openssl-sys`. The service's own `reqwest` 0.13 uses rustls.
- **Python 3.12** for the browser worker's unit tests. Pins are `browser-worker/requirements.txt` (pytest, playwright, cryptography). Playwright is imported lazily; unit tests do not download or launch a browser.
- **Java 17+** and the official MobilityData GTFS validator CLI 8.0.1 to run the service. Startup checks SHA-256 `19293ddd9b6f954f216d4f12054bd8a3232921751c4484339e339764a91000e2`. The container's repository-hardened JAR is a different digest and is not a substitute on the host. Unit tests do not start the JVM.
- **`jq`** for `scripts/check-release-metadata.sh`.

```bash
sudo apt-get update
sudo apt-get install -y protobuf-compiler libssl-dev pkg-config python3 python-is-python3 python3-pip jq
rustup toolchain install stable --profile minimal --component rustfmt,clippy
rustup default stable
python3 -m pip install --break-system-packages \
  --requirement browser-worker/requirements.txt
```

Download the official JAR from the MobilityData v8.0.1 release to `tools/gtfs-validator-v8.0.1-cli.jar` and check the digest above before `cargo run`. That path is gitignored.

## Build, test, and lint

CI (`.github/workflows/ci.yml`) builds and tests the Rust crate without ignored tests, runs the browser worker suite, builds the one image natively on amd64 and arm64 and runs `scripts/test-image-offline.sh` on it, runs `scripts/test-release-controls.sh`, and checks Compose/Bake config. Release-quality Rust changes also pass fmt and clippy. The heavy jobs (Rust build and tests, the browser worker tests, and the two image builds) run only when a pull request changes implementation or logic: `scripts/ci-scope.sh` skips their steps when every changed file is documentation or a record (`*.md`, `docs/`, `.security/`, `.kiro/`, `LICENSE`), and any other path counts as logic, so an unrecognised file still runs CI. The skip is per step inside each job, so every required check still reports; do not move it to a workflow-level `paths:` filter or a job-level `if:`, which would leave required checks pending.

```bash
cargo build --locked
cargo test --locked                         # offline unit and integration tests
cargo test --locked --features status       # also the amtrak-status CLI
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings   # see the note below

(cd browser-worker && python -m pytest tests -q)
scripts/test-release-controls.sh
```

Run the service on loopback (it fetches live Amtrak data once it is up):

```bash
cargo run
```

Station and train CLI, which reads a local generation by default:

```bash
cargo run --features status --bin amtrak-status -- station NYP --limit 10
cargo run --features status --bin amtrak-status -- train 167
```

Compose builds and runs one service, `amtrak-gtfs-rt`, with every source on, the hardening defaults, and `127.0.0.1:8090` published:

```bash
docker compose up -d --build
```

The image alone is `docker build --tag amtrak-gtfs-rt:local .` and `docker run -d -p 127.0.0.1:8090:8080 -v amtrak-data:/data amtrak-gtfs-rt:local`. Flags such as `AMTRAK_TRACKS=off` and `AMTRAK_ADVISORIES=off` turn sources off inside it; with `AMTRAK_TRACKS_AMTRAK=off` and `AMTRAK_ADVISORIES=off` no Python or Chromium process runs.

Do not start Compose or the HTTP server in order to run the unit tests. They are offline.

`cargo fmt --all -- --check` is clean. Clippy with `-D warnings` is the bar described in the design notes, and CI does not run it. On Rust 1.99 it currently fails on pre-existing lints in `src/writer.rs`, `src/orchestrator.rs`, `src/sources/mod.rs`, `src/static_gtfs.rs`, `src/bin/status/source.rs`, and `examples/station_departures.rs` (`needless_return`, `useless_conversion`, `double_must_use`, `needless_borrows_for_generic_args`, `unnecessary_sort_by`, `print_literal`). Do not drive-by fix those unless the task is a clippy cleanup. The release image still builds with Rust 1.96.

The MobilityData gate is separate and, by default, fetches live feeds:

```bash
./scripts/validate-feeds.sh
```

It needs Java 17 and Maven, or Docker (automatic fallback). The live run is not part of pull-request CI; `./scripts/validate-feeds.sh --fixture` is (see "Data sources, feeds, and releases").

## Coding conventions

Rust in this crate is edition 2021. Format with rustfmt defaults; there is no `rustfmt.toml` or `clippy.toml`.

- Keep the existing error style: small `Display` types (`ConfigError`, `StoreError`, `RefreshError`) that name the field or stage. Do not introduce `anyhow` or `thiserror` for config and publish errors.
- `TrackConfig`'s `Debug` reports only whether RailData credentials are set. Log lines and error strings must not contain URLs, headers, tokens, or passwords.
- Parse environment through `from_map` so tests inject a map instead of mutating process environment.
- Document public items, including `# Errors` on fallible public functions. Non-trivial modules open with a `//!` comment that states the invariant.
- Silence a lint with `#[expect(clippy::..., reason = "...")]`, not a blanket `allow`.
- The `status` feature stays off the default build. `cargo build --locked --release` must not compile `amtrak-status`.
- Browser worker Python uses `from __future__ import annotations`, the stdlib HTTP server, and pytest functions named `test_*`. There is no ruff or black config; match the surrounding module. Keep Playwright imports inside the functions that launch a browser.

## Testing expectations

- New behavior needs an offline test. Config goes through `from_map`. HTTP tests drive the axum router with an injected peer and clock. Source tests use `sources::mock`.
- Tests that hit the network stay `#[ignore]` with a reason. Run them with `cargo test -- --ignored` or `--include-ignored`. Do not un-ignore them to make CI call Amtrak.
- Ignored today: `sources::amtrak::tests::live_fetch_returns_batch`, `orchestrator::tests::live_candidate_builds_and_passes_semantic_validation`, and `sources::asm::tests::live_map_resolves_against_the_static_feed` (these three run in the scheduled live job, `scripts/test-live-amtrak.sh`); `sources::advisories_live_tests::advisories_live_resolve_scoped` (needs the browser worker, so it stays out of that job); `status::live_tests` (the `status` feature, not in the `--lib` list); and the manual `static_augment::tests::augment_file` (needs `AUGMENT_IN`).
- Browser worker tests stay offline: no browser, no Amtrak, no RailData. `tests/test_browser.py` drives the shared browser against fakes.
- `scripts/test-release-controls.sh` is the deterministic release-control suite. It uses local git fixtures only.
- A feed change that can produce a new MobilityData ERROR code needs a real fix or a `validation/baseline.json` entry that records the cause. Do not gate on occurrence counts; those follow how many trains are running. Amtrak's static GTFS is gated at zero ERROR notices.

## Data sources, feeds, and releases

Sources. One caller per upstream. Do not triplicate `getTrainsData`.

- Static GTFS: `https://content.amtrak.com/content/gtfs/GTFS.zip` (`AMTRAK_STATIC_URL`). One downloader per deployment: the service process.
- Realtime trains: exactly one process fetches and decrypts `getTrainsData` on `maps.amtrak.com`, through the catenary crate: the service process. Track selection reads that same poll through `tracks/fleet-trains.json`; a worker beside the service never GETs the map feed.
- Notices and track and gate documents: the background browser worker. It is a single worker supervised by the service. `AMTRAK_ADVISORIES_URL` and `AMTRAK_TRACKS_AMTRAK_URL` point at it. An empty Amtrak track URL skips that source and logs a warning when tracks are on. A plain HTTP client to `www.amtrak.com` is reset by Akamai. Do not bypass that. No fingerprint spoofing, no curl-impersonate, no copied cookies, no login.
- Hartford Line board: `AMTRAK_TRACKS_HARTFORD_URL` (default the public connecting-train page). Set it empty to skip.
- NJ Transit RailData (`AMTRAK_TRACKS_RAILDATA_BASE`, default `https://raildata.njtransit.com/api/TrainData`) needs `NJT_RAILDATA_USERNAME` and `NJT_RAILDATA_PASSWORD` set together. Without them, NJ stations are skipped. One token is cached at `{AMTRAK_OUTPUT_DIR}/tracks/raildata-token.json` (mode `0600`, never served). A token is requested only when that cache is empty or older than 23 hours, and never more than 10 times in 24 hours. Any failed token request pauses NJ Transit for an hour. Do not point exploratory runs, including `scripts/validate-feeds.sh`, at production RailData credentials: a cold cache spends that daily budget. The `--tracks` flag of the validation script deliberately omits those credentials.

Published feed shape. Consumers already depend on this.

- One generation is one static ZIP plus three protobuf products. Each realtime header carries that generation's static version and timestamp. Do not mix artifacts across generations, and do not build generation URLs by hand when a manifest is available.
- Publication is atomic. Readers see a complete old generation or a complete new one. A source, conversion, validation, or disk failure leaves the current generation in place. Do not rewrite a published generation directory.
- Tracks on (the default) changes the published static feed. For each covered station the service adds a parent `{stop}:station`, re-parents the original stop under it, and adds `{stop}:track:{label}` with `platform_code`. A boarding gate is its own stop, `{stop}:track:{track}:gate:{gate}`, with `platform_code` like `24 (Gate H)`. GTFS-Realtime has no gate field, so the gate travels only through `assigned_stop_id`. `stop_sequence` is set whenever `assigned_stop_id` is. The static version gains a `+tracks.{digest}` suffix, where `digest` is the first 8 hex digits of the platform table. `feed_info.txt`'s non-empty `feed_version` gains the same suffix, so it matches the realtime headers' `feed_version`. Every other ZIP entry is copied byte-for-byte. `AMTRAK_TRACKS=off` publishes the upstream static feed with no platform stops and no suffix.
- Before validation, stop times that name a stop missing from `stops.txt` are dropped and a warning is logged (`stop times name a stop missing from stops.txt; dropped`). Amtrak's 20261018 feed calls at Ocean Springs (`OSM`) without defining it. A feed with no such rows is unchanged.
- The mutable routes `/trip-updates.pb`, `/vehicle-positions.pb`, `/alerts.pb`, `/static.zip`, and `/health` are removed. Do not add them back.

Release workflow:

- User-visible feed, config, or image changes go under `## [Unreleased]` in `CHANGELOG.md` (Keep a Changelog, semver).
- `Cargo.toml` `version` is the released version. A release commit moves Unreleased into `## [X.Y.Z] - YYYY-MM-DD` and must pass `scripts/check-release-metadata.sh` (version, tag form `vX.Y.Z`, changelog heading, `AGPL-3.0-only`, and `THIRD_PARTY_LICENSES.html`).
- Pushing a `v*.*.*` tag starts `.github/workflows/release.yml`. It publishes the one image, `linux/amd64` and `linux/arm64`, to `ghcr.io/sohampatwardhan/amtrak-gtfs-rt`, which is the only package. `amtrak-gtfs-rt-track-fetcher` and `amtrak-gtfs-rt-advisory-fetcher` (last published at v0.7.1) are deprecated and get no releases.
- A release is complete only when the GitHub Release carries `image-release.txt` with the tag, revisions, manifest digest, and both platform digests. Until then the previous complete release stays the stable image. Current complete release is `v0.9.0` (2026-11-20, manifest `sha256:bb6c230f0d929e6e2bab91b5abffcb66e76defc860420a3fee487e005c3bbed9`). `v0.8.0` (2026-11-18) was the first release of the one image. Tags `v0.3.0`, `v0.6.0`, and `v0.6.1` never finished publication; do not deploy them or their partial images.
- Pull requests that touch image inputs (`Dockerfile`, `.dockerignore`, `container/`, any file in `src/`, `Cargo.toml`, `Cargo.lock`, or the smoke, scanner, and policy scripts) run `.github/workflows/release-gates.yml`: a linux/amd64 image build, `scripts/test-container.sh` (live Amtrak fetch), and the pinned Grype policy (no Critical or High match that has a fix; the zero-match policy ended with the scratch image, #71). arm64 is still checked only by the release itself.
- `./scripts/validate-feeds.sh` runs on a schedule and on pushes that touch the pipeline, not on every pull request, so an Amtrak outage does not block unrelated work. `./scripts/validate-feeds.sh --fixture` is the pull-request counterpart: `cargo run --example validation_fixture` builds a deterministic feed set (a small static GTFS with platform and gate stops, plus a mock realtime batch shaped as the catenary crate encodes it, through the primary source's conversion and the real builder), both MobilityData validators must report zero ERROR notices, the realtime validator must not report W002 (`vehicle_id` not populated) or W009 (`schedule_relationship` not populated), and no Amtrak call is made. The `Offline GTFS and GTFS-RT validators` job in `ci.yml` runs it when `src/`, `examples/`, `Cargo.*`, `validation/`, or the validation scripts change, and is not a required check. Network steps in the script retry with backoff (`scripts/lib-retry.sh`), because Maven Central answers HTTP 429 to a cold cache; validator cache keys hash the pins, not the script.

## What agents must not do

- Do not commit secrets, `.env` files, RailData tokens, or `raildata-token.json`. Pass `NJT_RAILDATA_USERNAME` and `NJT_RAILDATA_PASSWORD` only through the environment or Cursor's Secrets tab.
- Do not commit `tools/*.jar`, `/out`, `/target`, or anything under `validation-reports/`. That directory is run output only and is ignored; `scripts/test-release-controls.sh` fails if a file there is tracked. The August 2026 release decision and feed evidence are kept as history in `.security/release-gate/2026-08-14/`.
- Do not change the generation API, platform-stop ids, `platform_code` text (`24 (Gate H)`), or the `+tracks.{digest}` suffix without an explicit consumer-migration note in the README.
- Do not serve the generation directory with a generic file server. That bypasses peer checks, readiness, and manifest-first discovery.
- Do not treat `X-Forwarded-For` or `Forwarded` as the client.
- Do not make a missing notices browser, a RailData outage, or a Hartford failure fail the generation. VehiclePositions still publishes the last complete generation.
- Do not add container images or GHCR packages. `amtrak-gtfs-rt` is the one package. Its browser worker and Chromium start only when `AMTRAK_TRACKS_AMTRAK` or `AMTRAK_ADVISORIES` is on, and they listen on `127.0.0.1` only.
- Do not run more than one Chromium in the container. Notices open a browser context in the worker's resident browser.
- Do not let an HTTP handler wait on a browser, a map poll, or any other upstream call. Serve the warm cache. A cold browser launch is a scheduler concern, not a request-path concern.
- Do not advance `/v1/feed-set.json` until `static.zip`, `trip-updates.pb`, `vehicle-positions.pb`, and `alerts.pb` for that generation id are all present and the three protobuf headers share its timestamp and static version.
- Do not poll `getTrainsData` from more than one process.
- Do not push tags, publish to GHCR, or run the release workflow unless the user explicitly asks.
- Do not spend NJ Transit's 10-token daily budget on exploratory logins.
- Do not weaken the validator digest check, the baseline gate, or the container vulnerability policy to make a build pass.

## Pull requests and commits

- Branch from `main`. Commit subjects are imperative sentences with a period, specific enough to stand alone (`Publish Amtrak's boarding gates and turn tracks on by default.`).
- `main` is merged with merge commits (`Merge pull request #N`). Do not rewrite published history.
- `main` and the `v*.*.*` release tags are protected by repository rulesets, versioned in `.github/rulesets/`. Every change to `main` goes through a pull request that passes every required `ci.yml` check (seven, listed in `.github/rulesets/main.json`), has its review threads resolved, and is merged with a merge commit. Admins can bypass only inside a pull request. Release tags cannot be deleted or moved. After you rename, add, or remove a `ci.yml` job, update `.github/rulesets/main.json` in the same PR. `scripts/test-release-controls.sh` fails when a required check is not a job. After that PR merges, an admin runs `scripts/apply-rulesets.sh`, and `scripts/apply-rulesets.sh --check` reports drift. Do not edit rulesets in the GitHub UI without updating these files.
- Update `CHANGELOG.md` for user-visible changes. Leave the `Cargo.toml` version and git tags to an intentional release.
- Run the offline commands in "Build, test, and lint" before pushing. Say which ignored live tests you did not run.
- A PR that changes image inputs should expect the release-gates workflow, which needs network access to Amtrak and can fail when Amtrak is down.

## GitHub issues

When you create or triage an issue, fill in every field GitHub offers where it fits. Leave a field empty rather than invent a value.

- Assignee: `sohampatwardhan`, unless told otherwise.
- Labels: use the existing ones (`bug`, `enhancement`, `documentation`, `question`). Do not create new labels. Dependabot's pull requests carry its own labels, `dependencies` and the ecosystem (`rust`, `python`, `github_actions`, `docker`); those are allowed.
- Type: always set exactly one. **Task** is a specific piece of work (chores, renewals, refactors, CI, docs). **Bug** is an unexpected problem or behavior. **Feature** is a request, idea, or new functionality.
- Projects and milestone: the repo has no milestones and no repo-level project. Do not add issues to the owner's user-level projects, which belong to other work.
- Relationships: read the other open issues first. Use a parent or sub-issue for real containment, blocked by or blocking for real ordering, and relates to for issues that share an area (for example a release-gate issue and a dependency-audit issue). Set them through the GraphQL mutations `addSubIssue`, `addBlockedBy`, and `addRelatesTo`.
- Development: link the pull request with a closing keyword (`Closes #N`) in its body.
- Security alert: Dependabot alerts and security updates are enabled on this repo. When an issue is about a vulnerability, link the matching Dependabot alert.

Dependabot (`.github/dependabot.yml`) opens version-update pull requests weekly for Cargo and the browser worker's pip requirements and monthly for GitHub Actions and the Docker base images, with minor and patch updates grouped per ecosystem and major updates grouped separately (at most two open version-update pull requests per ecosystem), a cooldown before a new release is proposed (Cargo and pip: patch 3 days, minor 7, major 30; Actions and Docker: 7 days). Docker base images get only patch tags and digest refreshes for Python and Rust, and none for `eclipse-temurin`: the validator stage's JDK is fixed by the reproducible JAR digest, so update that stage, and the JRE stage with it, by hand. Treat them like any other pull request: the required checks must pass. A pull request that changes `Cargo.lock` is a dependency change, so run the dependency audit on it, and regenerate `THIRD_PARTY_LICENSES.html` before the next release (`scripts/check-release-metadata.sh` checks it at release time). A Playwright update in `browser-worker/requirements.txt` also changes the Chromium build and the Node runtime inside the image, so let the release-gates image build and vulnerability gate run on it.

## Cursor Cloud specific instructions

Cloud Agent setup is `bash .cursor/install.sh`, from `.cursor/environment.json`. The script is idempotent and uses Cursor's default image. There is no `.cursor/Dockerfile`, and there is no `start` or `terminals` entry: unit tests do not need the HTTP server, the browser worker, or Docker.

The script installs `protoc`, `libssl-dev`, `pkg-config`, `python`, `jq`, and CA certificates when they are missing; installs Rust stable with `rustfmt` and `clippy` (the image's preinstalled 1.83 cannot compile the edition 2024 dependency); installs the browser worker's requirement file; runs `cargo fetch --locked`; and downloads the official GTFS validator JAR to `tools/gtfs-validator-v8.0.1-cli.jar` after the SHA-256 check. It installs OpenJDK 17 only when `java` is missing or older than 17. The default image already has a newer JDK, which satisfies the service's Java 17 minimum.

After install, the commands in "Build, test, and lint" are the verification set. `cargo test --locked` does not run the ignored live tests.

`cargo run` listens on `127.0.0.1:8080` and needs outbound HTTPS to `content.amtrak.com` and `maps.amtrak.com`. With tracks left on and the Hartford URL left at its default, it also calls `hartfordline.com`. It does not call `www.amtrak.com` unless an advisory or Amtrak-track URL points there, and that call will fail closed at Akamai. By default the notices and Amtrak-track sources call the built-in browser worker on `127.0.0.1:8081`. On a host with no worker those calls fail open and log a warning each cycle. Set `AMTRAK_ADVISORIES=off AMTRAK_TRACKS_AMTRAK=off` to silence them. That single process is the combined mode that the one container in `docs/one-container.md` builds on.

Secrets, only for NJ Transit tracks. Add them in Cursor's Secrets tab. Do not put them in the repo, Compose, or this file.

- `NJT_RAILDATA_USERNAME`
- `NJT_RAILDATA_PASSWORD`

`cargo build`, `cargo test`, and `cargo run` all work with those unset; NJ stations are skipped. Ignored tests and `./scripts/validate-feeds.sh` call live Amtrak endpoints and can fail when Amtrak is unreachable. They are not part of install verification. Do not export RailData credentials for that script.

The release workflow needs GitHub Actions and GHCR credentials that this environment does not have. Do not push a tag to obtain them.

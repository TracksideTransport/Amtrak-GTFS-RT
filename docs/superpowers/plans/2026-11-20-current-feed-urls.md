# Fixed "Current" Feed URLs: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve every artifact at a stable URL, `/v1/current/{artifact}`, that always returns the generation current when the request arrives (#111).

**Architecture:** Add one axum route in `src/serve.rs`. The existing generation handler is split: the part after a generation is chosen becomes a shared function, and both handlers call it. The shared path adds an `X-Generation-Id` header. Nothing else changes: the store, writer, manifest and proxy stay as they are.

**Tech Stack:** Rust, axum, tokio, tower (`ServiceExt::oneshot` in tests).

**Spec:** `docs/superpowers/specs/2026-11-20-current-feed-urls-design.md`

## Global Constraints

- Route: `GET /v1/current/{artifact}`. The artifact set is exactly `Artifact::from_str`'s: `static.zip`, `official-static.zip`, `platform-addon.zip`, `trip-updates.pb`, `vehicle-positions.pb`, `alerts.pb`.
- Header name `X-Generation-Id`, value `generation.id.0` (`GenerationId(pub String)` in `src/orchestrator.rs:110`), on `200` and `304`. The generation route gets it too, because the path is shared.
- Same semantics as `/v1/generations/{id}/{artifact}`:
  - controlled peer (`denied_peer` with route label `"/v1/current/{artifact}"`): `403`;
  - no current generation: `503`;
  - unknown or empty artifact: `404`;
  - `ETag` from the generation's cached `ArtifactEtags`, `Last-Modified` = `manifest.generated_at_unix`, and `Cache-Control: private, no-cache`;
  - `304` per `not_modified`;
  - the trip-interest hook for `trip-updates.pb` requests whose query names trains or stations (parsed by the existing `interest_from_query`; the existing test uses `?train=167`).
- Freshness is not checked: a stale current generation is served. `/readyz` stays the health signal.
- Handlers read only the in-memory store, with no new awaits on I/O (AGENTS.md: "HTTP handlers serve the last good generation from memory").
- No change to `feed-set.json`, `/v1/generations/...` URLs, the writer, or `proxy/`.
- Commit subjects are imperative sentences ending with a period, with the trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. The PR body says `Closes #111` and ends with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.

## Review Focus

- **A current generation older than the `/readyz` limit** must still be served (`200`), not `503`. Test: `current_alias_serves_a_stale_generation`.
- **The alias must follow a new publish** instead of caching the first generation it saw. Test: `current_alias_follows_a_new_generation`.
- **Trackside's train/station interest query on the alias** must reach the trip-interest hook, without delaying the response. Test: `current_alias_feeds_the_trip_interest_hook`.
- **Path tricks** (`%2e%2e`, an empty artifact, a trailing slash) must give `404`, never fall through to another route. Test: `current_alias_rejects_unknown_artifacts`.
- **A conditional request with the alias's `ETag`** must get `304` and still carry `X-Generation-Id`. Test: `current_alias_answers_304_with_its_generation_id`.

---

### Task 1: `/v1/current/{artifact}` with a shared artifact response

**Files:**
- Modify: `src/serve.rs`:
  - `router` (lines 118-125);
  - `artifact` (lines 227-292): split it;
  - `cached_bytes` (lines 341-372): add the header;
  - the `tests` module.

**Interfaces:**
- Produces, in `src/serve.rs`:
  - `pub async fn current_artifact(State(state): State<AppState>, peer: Peer, Path(artifact): Path<String>, RawQuery(raw_query): RawQuery, headers: HeaderMap) -> Response`, the new handler.
  - `async fn generation_artifact(state: &AppState, generation: Arc<PublishedGeneration>, artifact: Artifact, raw_query: Option<String>, headers: &HeaderMap) -> Response`, everything `artifact` does after the generation is chosen: empty check, ETag, trip-interest hook, `cached_bytes`.
  - `const GENERATION_ID_HEADER: &str = "x-generation-id";`
  - `cached_bytes` gains a final parameter `generation_id: &str`.
- Consumes: the existing `GenerationStore::current`, `Artifact`, `denied_peer`, and test helpers `fixture_state`, `request`, `body_json`.

- [ ] **Step 1: Write the failing tests** in `src/serve.rs`'s `tests` module. Use loopback `MockConnectInfo` unless the test is about peers. Read the alias's `X-Generation-Id` with `response.headers()["x-generation-id"]`.

  - `current_aliases_serve_the_current_generation_with_its_etag`. Published at 100, now 100. For each `(key, url)` in `manifest["urls"]` from `/v1/feed-set.json`, with `file = url.rsplit('/').next()`, fetch both `url` and `/v1/current/{file}`. Assert:
    - both are `200`;
    - the bodies are equal;
    - the `ETag`, `Content-Type`, `Last-Modified` and `Cache-Control` headers are equal;
    - the alias's `x-generation-id` equals `manifest["generation_id"]`.
  - `current_alias_answers_304_with_its_generation_id`. Take the `ETag` from `/v1/current/trip-updates.pb`, then send it back as `If-None-Match`. Assert `304`, the same `ETag`, and `x-generation-id` present and equal to the manifest's id.
  - `current_alias_is_unavailable_before_first_generation`. `fixture_state(.., 100, None)`. `/v1/current/trip-updates.pb` returns `503`.
  - `current_alias_requires_an_allowed_peer`:
    - with no `MockConnectInfo`, `/v1/current/static.zip` returns `403`;
    - with peer `10.0.0.1` and an empty allowlist, it returns `403`.
  - `current_alias_rejects_unknown_artifacts`. Each of these returns `404`:
    - `/v1/current/missing.pb`
    - `/v1/current/%2e%2e`
    - `/v1/current/trip-updates.pb.bak`
    - `/v1/current/`
    - `/v1/current`
  - `current_alias_serves_a_stale_generation`. Published at 100, now 10_000. `/readyz` returns `503`, but `/v1/current/trip-updates.pb` returns `200`.
  - `current_alias_follows_a_new_generation`. Published at 100. Record the alias's `x-generation-id`. Publish a second generation at 200 against the same store, following `a_new_commit_recovers_readiness_after_staleness` (around line 826). Assert that the alias's `x-generation-id` changed and equals the new `feed-set.json` `generation_id`.
  - `current_alias_feeds_the_trip_interest_hook`. `state.with_trip_interest` with a hook that sets an `AtomicBool`. Request `/v1/current/trip-updates.pb?train=167`. Assert `200` within 100 ms, then poll the flag for up to 1 s and assert it became true.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --locked --lib serve::tests::current_alias`
Expected: FAIL. The alias routes return `404`, so `current_alias_is_unavailable_before_first_generation`, the `200` checks and the header checks fail.

- [ ] **Step 3: Split `artifact` and add `current_artifact`.**
  - `artifact` keeps its peer check (label unchanged), id and artifact parsing, the `503` and `404` lookups, and then returns `generation_artifact(...)`.
  - `current_artifact`, in this order, matching `artifact`:
    1. `denied_peer` with label `"/v1/current/{artifact}"` (`403`);
    2. `Artifact::from_str` (`404` on error);
    3. `state.store.current().await` (`503` on `None`);
    4. return `generation_artifact(...)`.
  - `cached_bytes` adds `(HeaderName::from_static(GENERATION_ID_HEADER), generation_id)` to both its `200` and `304` header arrays.
  - `router` adds `.route("/v1/current/{artifact}", get(current_artifact))`.
  - Update `router`'s doc comment ("versioned immutable API") to mention the current aliases.

- [ ] **Step 4: Run the new tests, then the whole suite**

Run: `cargo test --locked --lib serve::tests::current_alias && cargo test --locked --all-targets --all-features`
Expected: all pass, and the existing `public_contract_pins_urls_content_types_and_cache_headers` and `closed_routes_reject_unknown_traversal_and_removed_legacy_paths` still pass.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt
git add src/serve.rs
git commit  # subject: "Serve every artifact at a fixed /v1/current URL that follows the current generation."
```

### Task 2: Document the fixed URLs

**Files:**
- Modify: `README.md`:
  - the endpoint table (lines 176-182);
  - a new subsection right after the table, titled `### Fixed URLs`.
- Modify: `AGENTS.md`:
  - line 7 ("Consumers start at `GET /v1/feed-set.json` …");
  - the route table (lines 70-76).
- Modify: `CHANGELOG.md`: an `### Added` heading under `## [Unreleased]`, placed before the existing `### Changed`.
- Add: `docs/superpowers/specs/2026-11-20-current-feed-urls-design.md` (already written) and this plan.

**Interfaces:** consumes Task 1's route and header names exactly.

- [ ] **Step 1: README table row** after the `{trip-updates,vehicle-positions,alerts}.pb` row:
  `| /v1/current/{static.zip,official-static.zip,platform-addon.zip,trip-updates.pb,vehicle-positions.pb,alerts.pb} | controlled | as the generation file | The same file from whichever generation is current when the request arrives; carries X-Generation-Id |` (with the README's code-span formatting).

- [ ] **Step 2: README `### Fixed URLs`.** Say that apps and routing backends configured with fixed URLs use `/v1/current/...`. Give the four realtime and static URLs on `http://127.0.0.1:8090`. Say that `If-None-Match` polling gets `304`. State the trade-off: separate requests can straddle a new generation. Compare `X-Generation-Id`, or each realtime header's `feed_version` and `timestamp`, or read `feed-set.json` for one consistent set.

- [ ] **Step 3: AGENTS.md.**
  - Line 7: consumers start at `feed-set.json` for one consistent generation, or use the fixed `/v1/current/{artifact}` URLs.
  - Add a route-table row matching the README's.

- [ ] **Step 4: CHANGELOG.** Under `## [Unreleased]`, add `### Added` with one bullet: "**Fixed feed URLs.**" `/v1/current/{artifact}` serves each file from the current generation, with the same `ETag`/`304` behaviour and an `X-Generation-Id` header, for apps and routing backends configured with fixed URLs (#111).

- [ ] **Step 5: Check that the docs name the shipped routes**

Run: `grep -c '/v1/current/' README.md AGENTS.md CHANGELOG.md && grep -c 'X-Generation-Id' README.md AGENTS.md CHANGELOG.md`
Expected: every count is 1 or more.

- [ ] **Step 6: Commit**

```bash
git add README.md AGENTS.md CHANGELOG.md docs/superpowers/specs/2026-11-20-current-feed-urls-design.md docs/superpowers/plans/2026-11-20-current-feed-urls.md
git commit  # subject: "Document the fixed /v1/current feed URLs and their trade-off."
```

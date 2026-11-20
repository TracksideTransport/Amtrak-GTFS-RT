# Fixed "current" feed URLs (#111)

## Why

Most GTFS-Realtime consumers are configured once with fixed URLs: transit apps, routing backends
(OpenTripPlanner, MOTIS), validators and the gtfs-mcp connector. Today every artifact URL carries a
generation id (`/v1/generations/{id}/trip-updates.pb`), and a generation is deleted about 11 minutes
after it is superseded. A saved URL then returns `404`, so anyone hosting this service for an app
has to run a relay that reads `feed-set.json` on every request. This is the step that lets other
operators point apps at the feed directly.

## Goals

- One stable URL per artifact that always serves the generation that is current when the request
  arrives.
- Conditional polling keeps working: an unchanged file still answers `304`.
- A careful client can tell which generation it got.

## Non-goals

- No change to `/v1/generations/{id}/...` or to `feed-set.json`'s schema. Manifest readers keep
  exact, consistent generations.
- No routing-backend setup docs (OpenTripPlanner, MOTIS). That's a separate roadmap item.
- No proxy change. The Caddy proxy forwards every path.

## Anti-goals

- The aliases must not wait on anything the generation route doesn't. They serve from memory and
  never block on the browser or an upstream fetch.
- They must not be looser than the generation route: same peer allowlist, same `503` before first
  publication.

## Approaches

1. **Serve the bytes directly (chosen).** Works with every client, including ones that don't follow
   redirects. The response carries the same `ETag` and `Last-Modified` as that generation's file,
   so `If-None-Match` polling gets `304`. An `X-Generation-Id` header names the generation.
2. **`302` to the generation URL.** Always exact-generation bytes, but some consumers don't follow
   redirects, and a client that saves the redirect target is back to an expiring URL.
3. **A relay or proxy rule outside the service.** This is what the test box does today. It moves
   the problem to every operator instead of solving it once.

The owner picked 1 on 2026-11-20.

## Design

- **Routes:** `GET /v1/current/{artifact}` for the same closed set the generation route serves:
  `static.zip`, `official-static.zip`, `platform-addon.zip`, `trip-updates.pb`,
  `vehicle-positions.pb`, `alerts.pb`. HEAD works as it does for the generation route.
- **Behaviour matches `/v1/generations/{id}/{artifact}` for the current generation.** The two
  share one code path after the generation is chosen:
  - controlled peer check, with route label `/v1/current/{artifact}`;
  - `503` before the first publication, and `404` for an unknown or empty artifact;
  - the same cached `ETag`, `Last-Modified`, `Cache-Control: private, no-cache` and `304`
    handling;
  - the same trip-interest hook for `trip-updates.pb?trains=…&stations=…` (Trackside uses it).
- **Extra header:** `X-Generation-Id: {id}` on `200` and `304`.
- **Freshness:** serves the current generation even when it is older than the `/readyz` limit,
  as `feed-set.json` does. `/readyz` remains the health signal.
- **Consistency caveat (documented):** four requests to `/v1/current/...` can straddle a new
  generation. `static.zip` changes about daily, and each realtime file's header carries
  `feed_version` and `timestamp`. A client that needs one consistent set reads `feed-set.json`
  instead, or compares `X-Generation-Id`.
- **Docs:**
  - README endpoint table and a short "Fixed URLs" section with the trade-off;
  - AGENTS.md: the route table, and the line saying consumers start at `feed-set.json`;
  - CHANGELOG `Unreleased`.
- **Tests** (in `src/serve.rs`'s existing test module):
  - an alias returns the same bytes and `ETag` as the generation URL;
  - it answers `304` with that `ETag` and carries `X-Generation-Id`;
  - `503` before the first publication;
  - `403` for a denied peer;
  - `404` for an unknown artifact;
  - it follows the newest generation after a publish.

## Left to the builder

- How to factor the shared code path (for example, a function taking the chosen generation).
- Test helper reuse.

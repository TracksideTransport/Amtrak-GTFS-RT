# CI always rebuilds this crate from the checkout (#142)

## Why

On #129 the "Offline GTFS and GTFS-RT validators" job validated a fixture built from code without
that PR's changes. A re-run of the same commit passed. A CI check that can test old code can pass a
broken change, or fail a good one, as it did there.

## What we know

- Both Rust jobs ("Build and test" and "Offline GTFS and GTFS-RT validators") restore
  `Swatinem/rust-cache` v2 before building.
- rust-cache's documentation says it does **not** cache workspace crates, only dependencies. So the
  "a cached `target/` held the old example binary" theory in #142 is unproven, and possibly
  wrong. The cause is still open.

## Goals

- Neither job can ever build or run this crate, or its examples, from anything but the checked-out
  source, whatever the cause.
- Each job's log shows this crate being compiled.

## Non-goals

- Proving the root cause. That would need a reproduction, and the failure happened once in many
  runs.
- Changing which dependencies are cached.

## Approaches

1. **Delete this crate's build output after the cache restore (chosen).** Add a step after
   rust-cache in both jobs: `cargo clean --locked --package amtrak-gtfs-rt-service`. This removes
   only this crate's artifacts (library, binaries, examples, fingerprints) and keeps the cached
   dependencies, so it costs one compile of this crate. That compile is already expected, since
   rust-cache doesn't cache it.
2. **Drop rust-cache from these jobs.** This removes any cache question, but every run rebuilds
   all dependencies, adding several minutes to required checks.
3. **Have the fixture record the commit it was built from and check it.** That would catch a stale
   fixture, but it needs a build script and only covers the validator job.

Approach 1 is the sensible one. 2 costs minutes on every PR, and 3 detects the problem instead of
preventing it.

## Design

- **Both jobs:** a step right after `Swatinem/rust-cache`, with the same `if:` condition as its
  neighbours, running `cargo clean --locked --package amtrak-gtfs-rt-service`, with a comment
  pointing to #142.
- **Validator job:** before `./scripts/validate-feeds.sh --fixture`, run
  `cargo build --locked --example validation_fixture`, so the log shows
  `Compiling amtrak-gtfs-rt-service` before the fixture runs.
- **Verification:** this PR touches `.github/workflows/ci.yml`, so both jobs run on it. Their logs
  must show the clean step and `Compiling amtrak-gtfs-rt-service`.
- **AGENTS.md:** the line describing the validator job gains a short note on the clean step.

## Left to the builder

- Exact step names and comment wording.

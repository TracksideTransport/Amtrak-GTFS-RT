# Release audit notes: v0.10.0 (2026-11-21)

- **Audit:** `release-2026-11-21.json` and `release-2026-11-21.md`, release mode, completed
  2026-11-21T18:38:49Z.
- **Revision:** `main` at `b68ca43` (merge of #174) plus the v0.10.0 release metadata. The release
  commit changes only the root package's version line in `Cargo.lock`.
- **Inputs:**
  - `--reachability .security/risk-acceptance/evidence/2026-11-cargo-reachability.json`
  - `--acceptance .security/risk-acceptance/2026-11-cargo-dependencies.json`
- **Inventory:** complete, 401 crates.io packages, fingerprint `a9dc220fbfb0…`. That is the same
  fingerprint as the change audit for #172, so nothing moved since then except this crate's own
  version.
- **Result:** **WARNINGS**, exit 0. 0 blocked, 43 warnings. KEV: none.

## Against the v0.9.0 release audit (2026-11-20)

1. **The 43 records are the same advisories, and all 43 are accepted.**
   - 16 records were covered by the original 2026-11 acceptance.
   - 27 `heapless` records were added to it on 2026-11-20 as `locked-not-built` (#158, #170).
   - Every decision carries an acceptance. The acceptance runs through 2027-02-12.
2. **New packages since v0.9.0 bring no findings:**
   - `jiff` 1.0.1, `jiff-core`, `jiff-static` and `jiff-tzdb`, plus `portable-atomic` and
     `portable-atomic-util`, through the catenary crate 0.10.1 (#172);
   - the Cargo minor and patch updates (#146);
   - `scraper` 0.29 (#151).
3. **Some acceptance paths changed, the records did not.** `chrono` and `chrono-tz` are no longer
   reached through the catenary crate; this crate links them directly. `fxhash` is reached only
   through the catenary crate's `scraper` 0.22. The acceptance record says so.

`THIRD_PARTY_LICENSES.html` is regenerated for this release with `scripts/generate-third-party-licenses.sh`. `cargo about --fail` passed, so every license is on `about.toml`'s accepted list.

# Release audit notes: v0.9.0 (2026-11-20)

- **Audit:** `release-2026-11-20.json` and `release-2026-11-20.md`, release mode, completed
  2026-11-20T04:36:17Z.
- **Revision:** `main` at `1f122ac` (merge of #141) plus the v0.9.0 release metadata. The release
  commit changes only the root package's version line in `Cargo.lock`.
- **Inputs:** `--reachability .security/risk-acceptance/evidence/2026-11-cargo-reachability.json`
  and `--acceptance .security/risk-acceptance/2026-11-cargo-dependencies.json`, as in the
  2026-11-18 release audit. Inventory complete: 375 crates.io packages, the same set of exact
  package URLs as 2026-11-18.
- **Result:** **WARNINGS**, exit 0. 0 blocked, 43 warnings. KEV: none.

## Against the 2026-11-18 release audit

1. **The 16 accepted records are unchanged.** The same package, version, and advisory pairs
   (rust-crypto, rustc-serialize, time 0.1, gcc, zip 6.0.0, atomic-polyfill, derivative, fxhash,
   chrono, chrono-tz) appear again, each still covered by the approved 2026-11 acceptance, which
   runs through 2027-02-12.
2. **27 records are new, and none is a new dependency.** RustSec published nine `heapless`
   advisories on 2026-11-19 (RUSTSEC-2026-0574 to -0582: races, `IndexMap::truncate`, drops that
   mishandle unwinding, `SortedLinkedList`'s `Zeroize`, `Deque::try_from` and
   `Deque::make_contiguous`, and a full-map index overflow; all fixed in 0.9.4). Each
   matches the three `heapless` versions in `Cargo.lock` (0.6.1, 0.7.17, 0.8.0), so 27 records.
   Their severity is unknown, so the gate classifies them as non-blocking warnings
   (`non_blocking_severity`). The acceptance does not cover them.
   - **Not built.** `heapless` is a `Cargo.lock` entry reached only through the `rstar` versions
     that `geo-types` can select through optional features. No target or feature combination
     builds it: `cargo tree --offline --locked -i heapless@{0.6.1,0.7.17,0.8.0} --target all
     --all-features -e all` and the same for every locked `rstar` (0.8.4, 0.9.3, 0.10.0, 0.11.0,
     0.12.2) print nothing, and the full `--target all --all-features -e all` tree has no
     `heapless` line. This is the same position as `atomic-polyfill` (`locked-not-built` in
     `2026-11-cargo-reachability.txt`).
   - **Not remediated here.** 0.9.4 is outside the ranges `rstar` asks for, and nothing builds the
     crate. The owner decides whether to add these records to the reachability evidence and the
     acceptance; this release does not change either file.
3. **`chrono-tz` and `chrono` are now direct, normal dependencies (#127).** Until #127 they were
   optional, behind the `status` feature, and the service linked them only through
   `amtrak-gtfs-rt`. The fallback source now dates trains with them, so `Cargo.toml` declares both
   unconditionally. The resolved graph is unchanged (same versions, no added crate), but the
   inventory now records both as `direct: true`, `scope: runtime`, which is why its fingerprint
   changed (`ecd7910b…` to `e88fb9eb…`). Their unmaintained notices (RUSTSEC-2026-0543 and
   -0555) remain accepted. The acceptance's mitigation text still says they are linked "through
   the amtrak-gtfs-rt (catenary) crate"; that remains true, and they are now also direct
   dependencies of this service. Update that sentence at the next review of the acceptance.

## Sources

`cargo-audit`, OSV, GitHub advisories, CISA KEV, and the reachability file were `ok`. Two NVD
lookups were `partial` (`NVD CVSS container version is invalid`), as in the 2026-11-18 audit.
The inventory is the Cargo graph only: `pip-audit` reports `not_applicable`, so the browser
worker's Python pins and the image's OS and Node packages are not in this audit. The image's
Grype policy in `release-gates.yml` and `release.yml` covers them.

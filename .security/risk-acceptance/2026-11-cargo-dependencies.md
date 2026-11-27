# 2026-11 Cargo Dependency Risk Acceptance

| Field | Value |
|---|---|
| Status | **Approved** 2026-11-18 (rewritten that day for the one-container layout) |
| Supersedes | [v0.2.0 Cargo dependency risk acceptance](v0.2.0-cargo-dependencies.md) (expires 2026-11-12) |
| Prepared | 2026-11-07; rewritten 2026-11-18 |
| Audit revision | `ac52a48c24e80a7559a2a8e0e3cb289bbd42d86e` (`main` after #86, plus the lockfile update in #87). The 2026-11-07 audit was at `088104119f86c09b0073674791fd531115316ab3`; `Cargo.lock` is unchanged between them except for #87. |
| Inventory | 375 packages, complete (fresh `main` audit, 2026-11-18, revision above). |
| Inventory fingerprint | `ecd7910b29d7f844d75129c7756e56bf1d0888b41bae621a997cbbe376ffa6f7` (2026-11-18 `main` audit). The 2026-11-07 value, `613f04a586eb…`, belonged to the lockfile before #87. Regenerate it if `Cargo.lock` changes again. |
| Audits | [main 2026-11-07](../dependency-audit/main-2026-11-07.md) (WARNINGS, 0 blocking); [release 2026-11-07](../dependency-audit/release-2026-11-07.md) (UNAVAILABLE until this record is approved); [reassessment 2026-11-18](../dependency-audit/reassessment-2026-11-18.md) (`cargo audit` against the 2026-11-17 database) |
| Reachability evidence | [2026-11-cargo-reachability.txt](evidence/2026-11-cargo-reachability.txt), [.json](evidence/2026-11-cargo-reachability.json) (2026-11-07), extended by the 2026-11-18 `cargo tree` and source greps in the reassessment |
| Scope | `main` and the next release cut from it; re-review on any dependency change |
| Expiry | 2027-02-12 |

## Decision requested

This record asks to accept the exact 16 warning records below through 2027-02-12. It is not a
clean audit. The 2026-11-07 version of this record listed 16 rows; the 2026-11-18 reassessment found
two more (`chrono`, `chrono-tz`, both unmaintained notices) and cleared three (`quinn-proto`,
`quinn-udp`, `rustix`) with a lockfile-only update. KEV result: none of the records was in the
CISA Known Exploited Vulnerabilities catalog (version 2026.11.06, 1,769 entries) when checked on
2026-11-07; it was **not** re-queried on 2026-11-18. None of them has an authoritative fixed
version that can be reached without upstream changes. A yanked `chacha20 0.10.1` is a lockfile
entry that no target builds and carries no advisory; it is noted here, not accepted.

### What changed since v0.2.0

- All nine v0.2.0 warnings are still present. No fix was published for any of them, and
  `catenarytransit/amtrak-gtfs-rt` upstream (`b33dc280`, 2026-11-07, crate 0.10.0) still
  declares `rust-crypto = "0.2"`.
- The 2026-11-18 reassessment found that three more `quinn-proto`/`quinn-udp` records and one
  `rustix` record were published after the 2026-11-07 audit. All four were cleared by a
  lockfile-only update (`quinn-proto` 0.11.19, `quinn-udp` 0.5.16, `rustix` 1.1.6). It also found
  `chrono 0.4.45` (RUSTSEC-2026-0543) and `chrono-tz 0.10.4` (RUSTSEC-2026-0555), unmaintained
  notices pulled in by the catenary crate, which are added below. The RUSTSEC ids RUSTSEC-2026-0545,
  -0547, and -0548 are aliases of GHSA-8HXF, -9HJ8, and -M665 already listed, not new records.
- The fresh audit at `4c328d1` found 16 more records. Commit `0881041` cleared nine of them,
  including the blocking high-severity `quinn-proto` RUSTSEC-2026-0338, with a lockfile-only
  update to each advisory's fixed version: h2 0.4.16, quinn-proto 0.11.18, rustls 0.23.46,
  rustls-webpki 0.103.16 (needs aws-lc-rs 1.19.0 / aws-lc-sys 0.47.0), tokio 1.53.3, and
  webpki-root-certs 1.0.10. `Cargo.toml` is unchanged. `cargo build --locked` and
  `cargo test --locked --all-targets --all-features` pass (188 passed, 6 live or manual tests
  ignored).
- The other seven new records are against `zip 6.0.0`. Their fixes need zip 9.x (a major
  version, and a `Cargo.toml` change). The newest `gtfs-structures` (0.52.0) still requires
  zip `^8.6`, so bumping our direct requirement alone would leave an affected zip in the graph.
  Five of the seven were blocking until the reachability evidence below was supplied.

### Remaining findings

| Exact package | Advisory | Severity | Fix status | Dependency path | Reachability (evidence anchor) |
|---|---|---:|---|---|---|
| `atomic-polyfill 1.0.3` | `RUSTSEC-2023-0089` | Unknown | None; unmaintained | lockfile only: `geo-types → rstar (old) → heapless 0.7.17 → atomic-polyfill` | Not built in any target or feature set (`locked-not-built`) |
| `derivative 2.2.0` | `RUSTSEC-2024-0388` | Unknown | None; unmaintained | `gtfs-structures → derivative` (proc-macro) | Compile-time only (`build-time-only`) |
| `chrono 0.4.45` | `RUSTSEC-2026-0543` | Unknown | None; unmaintained | `amtrak-gtfs-rt → chrono` (also used by the optional `amtrak-status` CLI behind the `status` feature) | Linked into the service through the catenary crate for time arithmetic; an unmaintained notice with no vulnerability claim (`chrono-linked`) |
| `chrono-tz 0.10.4` | `RUSTSEC-2026-0555` | Unknown | None; unmaintained | `amtrak-gtfs-rt → chrono-tz` | Linked through the catenary crate for time-zone tables; no vulnerability claim (`chrono-linked`) |
| `fxhash 0.2.1` | `RUSTSEC-2025-0057` | Unknown | None; unmaintained | `amtrak-gtfs-rt` / service `→ scraper → selectors → fxhash` | **Reachable** from HTML parsers (`fxhash-reachable`) |
| `gcc 0.3.55` | `RUSTSEC-2025-0121` | Unknown | None; deprecated | `amtrak-gtfs-rt → rust-crypto → gcc` (build-dependency) | Build script only (`build-time-only`) |
| `rust-crypto 0.2.36` | `RUSTSEC-2016-0005` | Unknown | No released safe version | `amtrak-gtfs-rt → rust-crypto` | Never referenced; 0 symbols in release binary (`rust-crypto-unreferenced`) |
| `rust-crypto 0.2.36` | `RUSTSEC-2022-0011` / `GHSA-JP3W-3Q88-34CF` | Critical | None published | `amtrak-gtfs-rt → rust-crypto` | Same as above |
| `rustc-serialize 0.3.25` | `RUSTSEC-2022-0004` / `GHSA-2226-4V3C-CFF8` | Medium | None published | `amtrak-gtfs-rt → rust-crypto → rustc-serialize` | Same as above |
| `rustc-serialize 0.3.25` | `RUSTSEC-2025-0025` | Unknown | None; unmaintained | `amtrak-gtfs-rt → rust-crypto → rustc-serialize` | Same as above |
| `time 0.1.45` | `RUSTSEC-2020-0071` / `CVE-2020-26235` | Medium | Needs rust-crypto removed upstream | `amtrak-gtfs-rt → rust-crypto → time` | Same as above; service never calls `set_var` |
| `zip 6.0.0` | `RUSTSEC-2026-0545` (folds `GHSA-8HXF-QR8P-62X4` / `CVE-2026-115906`, `GHSA-9HJ8-73FH-9963` / `CVE-2026-115907` as `RUSTSEC-2026-0547`, and `GHSA-M665-V7PM-7GRJ` / `CVE-2026-115905` as `RUSTSEC-2026-0548`) | Critical | 9.0.0 (9.0.5 for the header-name flaw); not reachable via gtfs-structures | service direct; `gtfs-structures → zip` | `aes-crypto` and `legacy-zip` off; no `extract()`; no `ZipStreamReader` (`enabled-zip-features`, `zip-api-call-sites`); see residual |
| `zip 6.0.0` | `RUSTSEC-2026-0367` / `GHSA-VV5M-JXXH-4627` | High | 9.0.0; not reachable via gtfs-structures | as above | No `extract()` (`zip-api-call-sites`) |
| `zip 6.0.0` | `RUSTSEC-2026-0368` / `GHSA-4V9H-P7M9-RH26` | Unknown | 9.0.0; not reachable via gtfs-structures | as above | No `extract()` (`zip-api-call-sites`) |
| `zip 6.0.0` | `RUSTSEC-2026-0369` / `GHSA-4684-H2Q9-2VCP` | Critical | 9.0.0; not reachable via gtfs-structures | as above | No `ZipStreamReader::extract` (`zip-api-call-sites`) |
| `zip 6.0.0` | `RUSTSEC-2026-0429` / `GHSA-Q7V9-X93C-XQV3` | High | 9.0.0; not reachable via gtfs-structures | as above | No `extract()` (`zip-api-call-sites`) |

The reachability claims come from a manual review: `cargo tree`, source grep across this crate,
`gtfs-structures 0.46.1`, and `amtrak-gtfs-rt f3a83db0`, and `nm` on a host
(aarch64-apple-darwin) release build. The musl container binaries were not symbol-inspected.
The owner should review the claims before approving.

### Residual risk this record does not exclude

- `official-static.zip` is served byte for byte. If `content.amtrak.com` served a crafted
  archive, a consumer that stream-reads it could see different entry names than the service's
  central-directory view (GHSA-M665). The download uses HTTPS, and the exact bytes must pass
  the MobilityData validator before they are published.
- `ZipArchive` still parses upstream-controlled headers. None of the seven zip records is a
  `ZipArchive`-only read flaw. A future one would be.
- The rust-crypto exclusion holds only while the catenary crate never calls `crypto::`.

## Mitigations and controls (one-container layout)

- One image, `ghcr.io/tracksidetransport/amtrak-gtfs-rt`, runs one service. Compose publishes only
  `127.0.0.1:8090`. The browser worker listens on `127.0.0.1:8081` inside the container and is
  never published.
- The image is `python:3.12-slim-bookworm` with Chromium, a glibc Java 17 runtime, and the service
  binary: not the `scratch` image that earlier versions of this record described. It runs as UID/GID
  `10001` under tini. Compose sets a read-only root filesystem, tmpfs `/tmp` and home, all
  capabilities dropped, `no-new-privileges`, and memory and PID limits.
- Controlled routes authorize the direct socket peer. The image defaults to
  `AMTRAK_ALLOWED_PEER_IPS=gateway` (the container's default gateway, where a published port
  arrives from), so the `-p` binding decides who can reach it; a host binary that binds a
  non-loopback address with no allowlist refuses to start. `Forwarded` and `X-Forwarded-For` are
  ignored.
- The static ZIP comes only from `content.amtrak.com` over HTTPS and is validated with the
  digest-pinned MobilityData validator before publication. A failure keeps the previous
  generation. The refresh is conditional (`ETag`/`Last-Modified`), so an unchanged file is not
  re-downloaded.
- The `release-gates` workflow and the release run the container smoke test and apply the pinned
  Grype policy: **no Critical or High match that has a fix**. The previous zero-match policy ended
  with the `scratch` image, because the Debian and Chromium base always carries unfixed matches.
  Grype does not see Cargo advisories, so a clean scan does not clear the findings above.
- CI fails when an approved acceptance is past its expiry (`scripts/check-risk-acceptance-expiry.py`).

## Review triggers

Revoke and re-audit on any of these:

- any `Cargo.lock` or direct-dependency change, including a move of the `amtrak-gtfs-rt` git
  revision (re-check the rust-crypto exclusion);
- a `gtfs-structures` release that accepts zip ≥ 9.0.5;
- any new zip advisory that affects the `ZipArchive` read path;
- a new CISA KEV match;
- a newly published fix;
- evidence of reachability or exploitation;
- a failed release gate;
- a change of the `amtrak-gtfs-rt` git revision, at which point the `rust-crypto` exclusion must
  be re-checked on a **musl** release binary (only an aarch64 macOS build was symbol-inspected);
- expiry on 2027-02-12.

## Approval record

**Approved 2026-11-18.** The repository owner replied "approve" in conversation to the proposed
sentence below, for this exact set of sixteen records through 2027-02-12:

> I explicitly accept the documented sixteen-warning Cargo dependency risk in
> .security/risk-acceptance/2026-11-cargo-dependencies.md through 2027-02-12.

Evidence at approval: the 2026-11-18 `main` audit (WARNINGS, 0 blocked, 16 findings: 7 proven
unreachable, 9 with no authoritative fix; inventory fingerprint in the table above) and a CISA KEV
re-query (catalog 2026.11.17, 1,782 entries; no match, with the limitation that only 5 of the
records carry a CVE id). Known gaps accepted with it: the NVD source returned *partial* severity
data, and the `rust-crypto` "never referenced" claim was symbol-checked on an aarch64 macOS build
only, not on the musl container binary (a review trigger above).

The same text is in each `approval` field of
[2026-11-cargo-dependencies.json](2026-11-cargo-dependencies.json), so `--acceptance` accepts the
file. The acceptance expires 2027-02-12; `scripts/check-risk-acceptance-expiry.py` fails CI after
that date.

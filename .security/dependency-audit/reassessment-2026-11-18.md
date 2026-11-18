# Risk assessment for #58: 2026-11 Cargo risk acceptance

- **Date:** 2026-11-18
- **Revision assessed:** `main` at the merge of #86 (`Cargo.lock` unchanged since the record's audit revision `088104119f86`; `Cargo.toml` differs only by tokio's `process` feature, which adds no package)
- **Tool:** `cargo audit` 2026-11-17 database (1,527 advisories), plus `cargo tree` reachability checks and a grep of this crate's zip and rustix use
- **Verdict:** **Do not approve `2026-11-cargo-dependencies.md` as written.** The remaining risk is low and acceptable, but the record is stale and cannot be approved as "the exact set".

## 1. Why the record cannot be approved as written

1. **It is no longer the exact set.** It accepts "the exact 16 warning records". A fresh audit shows these records that it does not list (the zip RUSTSEC-2026-0545, -0547, and -0548 are aliases of GHSA-8HXF, -9HJ8, and -M665, which it already lists, so they are not new):

   | Advisory | Package | Patched | Built into the service? |
   |---|---|---|---|
   | RUSTSEC-2026-0523, -0524 | `quinn-proto 0.11.18` | 0.11.19 | No. `cargo tree --target all -e all` finds no path. Lockfile only. |
   | RUSTSEC-2026-0538 | `quinn-udp 0.5.15` | 0.5.16 | No. Lockfile only. |
   | RUSTSEC-2026-0486 (unsound) | `rustix 1.1.4` | 1.1.6 | **Yes**, a direct dependency (statvfs, file opens, `kill_process_group`). The advisory is about the xattr functions, which this repo never calls. |
   | RUSTSEC-2026-0543, -0555 (unmaintained) | `chrono 0.4.45`, `chrono-tz 0.10.4` | none | Yes, through the catenary crate. |
   | yanked | `chacha20 0.10.1` | n/a | No. Lockfile only. |

   The record says commit `0881041` moved `quinn-proto` to 0.11.18 as the fixed version. 0.11.18 is itself now affected.
2. **Its mitigation section describes a layout that no longer exists.** It names three Compose services selected by `AMTRAK_ROLE` and a `FROM scratch` image. As of #77 and #78 there is one container on `python:3.12-slim-bookworm` with Chromium and a Java runtime, and the vulnerability gate is "no fixable Critical or High" instead of zero matches. The claim "a larger surface than the scratch image" is now true of the only image.
3. **Three of the new records have a fix that needs no code change:** `rustix`, `quinn-proto`, and `quinn-udp` update in range (`cargo update -p rustix -p quinn-proto -p quinn-udp`, verified with `--dry-run`). They should be fixed, not accepted.

## 2. Risk by group (likelihood x impact in this deployment)

| Group | Advisories | Exposure here | Residual risk |
|---|---|---|---|
| `rust-crypto`, `rustc-serialize`, `time 0.1`, `gcc` | RUSTSEC-2022-0011 (critical, AES miscomputation), -0004, -2020-0071, and unmaintained notices | Pulled only by the catenary crate. 0 symbols in the aarch64 macOS release binary; the musl release binary was **not** inspected. The service never calls `crypto::`. Amtrak's payload is decrypted by catenary's own `amtk`, not by `rust-crypto`. | **Low.** Would become real only if catenary starts calling it. |
| `zip 6.0.0` (7 advisories; 3 also appear under a second id) | extract and `ZipStreamReader` symlink, permission, and header-name flaws; AES authentication; legacy Shrink/Reduce/Implode preallocation | The service uses `ZipArchive::new`, `by_index`, `by_name`, and `ZipWriter` only (`static_addon.rs`, `static_augment.rs`); no `extract()`, no `ZipStreamReader`. Features are `deflate` only: no `aes-crypto`, no `legacy-zip`. Input is Amtrak's GTFS over HTTPS and must pass the pinned MobilityData validator before publication. | **Low, with one residual.** `official-static.zip` is served byte for byte, so a crafted upstream archive could confuse a *consumer's* streaming reader (RUSTSEC-2026-0548). That needs Amtrak's HTTPS endpoint to be compromised. |
| `quinn-*`, `chacha20` | 3 advisories, 1 yank | Not in any built target | **Negligible**, and fixable by a lockfile update. |
| `rustix 1.1.4` | RUSTSEC-2026-0486 | Built in, but the vulnerable xattr API is unused | **Negligible**, and fixable by a lockfile update. |
| `derivative`, `fxhash`, `atomic-polyfill`, `chrono`, `chrono-tz` | unmaintained notices | `fxhash` is reachable through the `scraper` HTML parser (hash function quality, not memory safety); the rest are build-time or time arithmetic. | **Low.** Advisory-only; no known exploit. |

None of these is in the CISA KEV catalog per the record; I did not re-query KEV today.

## 3. What each assumption rests on

- **zip API use:** `grep -rn 'ZipArchive|extract(|ZipStreamReader|read_zipfile' src` finds only `new`, `by_index`, `by_name` and the writer. Checked today.
- **zip features:** the resolved feature tree shows `deflate` and its flate2 backend only.
- **`gtfs-structures` 0.46.1's own zip use** is not independently re-reviewed here; the record's review of it stands.
- **The rust-crypto exclusion** was symbol-checked on a host build only.

## 4. Recommendation

1. **Fix the three in-range ones** (`rustix`, `quinn-proto`, `quinn-udp`) with a lockfile-only update, and rebuild and test. **Done in the PR that carries this file:** the audit goes from 13 vulnerabilities to 10 (`zip` 7, `rust-crypto`, `rustc-serialize`, `time` 1 each), and `cargo test --locked` passes (190).
2. **Rewrite the record** for the one-container layout (replace the mitigation section) and list the current exact set: the 16 existing records plus `chrono` (RUSTSEC-2026-0543) and `chrono-tz` (RUSTSEC-2026-0555), with the yanked `chacha20 0.10.1` noted. A draft is in this PR.
3. **Then approve with an expiry no later than 2027-02-12.** Keep the review triggers as written, and add one: any `amtrak-gtfs-rt` revision change (re-check the `rust-crypto` exclusion on a musl binary).
4. **Optional hardening:** run `nm` on the musl release binary to close the one unverified claim; no CI check currently fails on an expired record beyond the existing "approved acceptances" step.

Approval is the owner's call. This document does not approve anything.

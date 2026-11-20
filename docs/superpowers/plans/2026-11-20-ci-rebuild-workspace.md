# CI Rebuilds This Crate From the Checkout: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Both Rust CI jobs always compile `amtrak-gtfs-rt-service` and its examples from the checked-out source, whatever a cache restore left behind (#142).

**Architecture:** After `Swatinem/rust-cache` restores, delete this crate's own build output with `cargo clean --package`. Cached dependencies stay. The validator job also builds the fixture example explicitly, so its log shows the compile.

**Tech Stack:** GitHub Actions, cargo, actionlint.

**Spec:** `docs/superpowers/specs/2026-11-20-ci-rebuild-workspace-design.md`

## Global Constraints

- The clean command is exactly `cargo clean --locked --package amtrak-gtfs-rt-service`.
- The new steps use the same `if:` condition as their neighbours: `steps.scope.outputs.logic == 'true'` in "Build and test", and `steps.paths.outputs.run == 'true'` in the validator job.
- Don't change which dependencies are cached, the rust-cache pin, or any other job.
- Commit subjects are imperative sentences ending with a period, with the trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. The PR body says `Closes #142` and ends with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.

## Review Focus

- **A step added without its job's `if:` guard** runs on docs-only PRs and fails or wastes time. Check each new step's `if:`.
- **`cargo clean` before the toolchain step** would use the runner's default cargo. The clean must come after `dtolnay/rust-toolchain` and rust-cache.
- **A misspelled package name** makes `cargo clean -p` fail the job. The dry run in Step 2 pins the exact spelling.

---

### Task 1: Clean this crate after the cache restore, and build the fixture explicitly

**Files:**
- Modify: `.github/workflows/ci.yml`: after the rust-cache step in "Build and test" (around line 52), and after the rust-cache step in "Offline GTFS and GTFS-RT validators" (around line 278), plus a build step before "Validate the fixture feed set (zero ERROR notices)".
- Modify: `AGENTS.md:198`, the `./scripts/validate-feeds.sh --fixture` paragraph.
- Add: `docs/superpowers/specs/2026-11-20-ci-rebuild-workspace-design.md` (already written) and this plan.

**Interfaces:** none (CI configuration only).

- [ ] **Step 1: Add the clean step to both jobs**

Right after each `uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2` step:

```yaml
      # rust-cache restores dependencies only, but #142 saw a fixture built from stale code after a
      # restore. Delete this crate's own artifacts so it always compiles from the checkout.
      - name: Rebuild this crate from the checkout
        if: <the job's existing condition>
        run: cargo clean --locked --package amtrak-gtfs-rt-service
```

- [ ] **Step 2: Add the explicit fixture build to the validator job**

Immediately before the step named `Validate the fixture feed set (zero ERROR notices)`:

```yaml
      - name: Build the validation fixture
        if: steps.paths.outputs.run == 'true'
        run: cargo build --locked --example validation_fixture
```

- [ ] **Step 3: Check the commands locally**

Run: `cargo clean --locked --package amtrak-gtfs-rt-service --dry-run && cargo build --locked --example validation_fixture 2>&1 | grep -c 'Compiling amtrak-gtfs-rt-service'`
Expected: the dry run exits 0, and the count is `1` or more.

- [ ] **Step 4: Lint the workflow and run the repository's workflow tests**

Run: `actionlint .github/workflows/ci.yml && bash scripts/test-ci-scope.sh && bash scripts/test-release-controls.sh`
Expected: no actionlint output, and both scripts exit 0.

- [ ] **Step 5: Update AGENTS.md**

In the `./scripts/validate-feeds.sh --fixture` paragraph (line 198), after the sentence naming the `Offline GTFS and GTFS-RT validators` job, add: "Both Rust jobs run `cargo clean --package amtrak-gtfs-rt-service` after the cache restore, so this crate and its examples always compile from the checkout (#142)."

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/ci.yml AGENTS.md docs/superpowers/specs/2026-11-20-ci-rebuild-workspace-design.md docs/superpowers/plans/2026-11-20-ci-rebuild-workspace.md
git commit  # subject: "Rebuild this crate from the checkout after the CI cache restore."
```

- [ ] **Step 7: Verify in the PR's own runs**

The PR touches `.github/workflows/ci.yml`, so both jobs run. In each job's log, check that `Rebuild this crate from the checkout` ran and that `Compiling amtrak-gtfs-rt-service` appears after it. Record the two job links in the PR body. That is #142's "done when".

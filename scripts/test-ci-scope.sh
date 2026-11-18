#!/usr/bin/env bash
# Offline test for scripts/ci-scope.sh: which changed files count as implementation or logic.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

check() { # expected description file...
  local expected="$1" description="$2" actual
  shift 2
  actual="$(printf '%s\n' "$@" | scripts/ci-scope.sh --classify)"
  [ "$actual" = "logic=$expected" ] || {
    printf 'FAIL: %s: expected logic=%s, got %s\n' "$description" "$expected" "$actual" >&2
    exit 1
  }
}

check false "one markdown file" AGENTS.md
check false "nested markdown and docs" README.md docs/one-container.md browser-worker/README.md CHANGELOG.md
check false "records and kiro specs" .security/risk-acceptance/x.json .kiro/specs/a/design.md LICENSE
check true "Rust source" src/lib.rs
check true "markdown beside source" AGENTS.md src/lib.rs
check true "the license file the image copies" THIRD_PARTY_LICENSES.html
check true "a workflow" .github/workflows/ci.yml
check true "a script" scripts/validate-feeds.sh
check true "worker code" browser-worker/worker/__main__.py
check true "the Dockerfile" Dockerfile
check true "a lockfile" Cargo.lock
check true "an unrecognised path fails safe" something/new.file
check true "a markdown-looking name that is not markdown" notes.markdown.txt
echo "ci-scope tests passed"

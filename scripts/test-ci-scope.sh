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
check false "root and docs markdown" README.md docs/one-container.md docs/a/b/deep.md browser-worker/README.md CHANGELOG.md
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
check true "markdown under src is not documentation" src/notes.md
check true "markdown under scripts" scripts/run.md
check true "markdown under the workflows" .github/workflows/readme.md
check true "markdown nested outside docs" a/b/c.md
check true "a worker markdown file below the top level" browser-worker/worker/x.md
check true "a markdown-looking name that is not markdown" notes.markdown.txt

# The pull-request path, against a real repository: what a rename or a type change looks like.
repo="$(mktemp -d)"
trap 'rm -rf "$repo"' EXIT
git init -q "$repo"
git -C "$repo" config user.email t@example.invalid
git -C "$repo" config user.name test
mkdir -p "$repo/src" "$repo/docs"
printf 'fn main() {}\n' >"$repo/src/lib.rs"
printf '# readme\n' >"$repo/README.md"
printf '# doc\n' >"$repo/docs/a.md"
git -C "$repo" add -A && git -C "$repo" commit -q -m base
base="$(git -C "$repo" rev-parse HEAD)"

pr() { # prints logic=... for the change currently committed on top of $base
  (cd "$repo" && GITHUB_EVENT_NAME=pull_request BASE_SHA="$base" GITHUB_OUTPUT=/dev/null "$OLDPWD/scripts/ci-scope.sh" | head -1)
}
expect() { [ "$1" = "$2" ] || { printf 'FAIL: %s: expected %s, got %s\n' "$3" "$2" "$1" >&2; exit 1; }; }

printf '\nmore\n' >>"$repo/README.md"
git -C "$repo" commit -q -am docs
expect "$(pr)" "logic=false" "a markdown-only change"

git -C "$repo" reset -q --hard "$base"
git -C "$repo" mv src/lib.rs src/lib.md
git -C "$repo" commit -q -m "rename code to markdown"
expect "$(pr)" "logic=true" "renaming src/lib.rs to src/lib.md hides a code deletion"

git -C "$repo" reset -q --hard "$base"
rm "$repo/README.md" && ln -s src/lib.rs "$repo/README.md"
git -C "$repo" add -A && git -C "$repo" commit -q -m "markdown becomes a symlink"
expect "$(pr)" "logic=true" "a markdown file turned into a symlink"

git -C "$repo" reset -q --hard "$base"
git -C "$repo" rm -q docs/a.md
git -C "$repo" commit -q -m "delete a doc"
expect "$(pr)" "logic=false" "deleting documentation"

git -C "$repo" reset -q --hard "$base"
expect "$(pr)" "logic=true" "an empty diff runs CI"

echo "ci-scope tests passed"

#!/usr/bin/env bash
# Decide whether a pull request changes implementation or logic, so CI can skip its heavy jobs
# (Rust build and tests, the browser worker tests, the two image builds) for changes that cannot
# alter how the program runs.
#
#   scripts/ci-scope.sh                 in a workflow: writes logic=true|false to $GITHUB_OUTPUT
#   scripts/ci-scope.sh --classify      reads file names on stdin and prints logic=true|false
#
# The rule fails safe: only paths listed as NON_LOGIC below are ignored, so an unrecognised file
# counts as logic and CI runs. A push (not a pull request) always counts as logic.
#
# NON_LOGIC is documentation and records in known locations only: root-level *.md, the top level
# of browser-worker/ (its README), docs/, .security/, .kiro/, LICENSE, .gitignore. A markdown file
# anywhere else (src/, scripts/, .github/, ...) counts as logic, because code there could read it.
# Deliberately not listed: THIRD_PARTY_LICENSES.html (copied into the image and compared with a
# fresh `cargo about` run) and everything under src/, examples/, container/, validation/, proxy/.
#
# The pull request diff is taken with rename detection off, so a rename shows as a deletion of the
# old path plus an addition of the new one: renaming src/lib.rs to src/lib.md lists src/lib.rs and
# counts as logic. A change of file type (a file becoming a symlink, or the reverse) is logic
# whatever the path.
set -euo pipefail

NON_LOGIC='^([^/]+\.md|browser-worker/[^/]+\.md|docs/.+|\.security/.+|\.kiro/.+|LICENSE|\.gitignore)$'

classify() {
  local file seen=0
  while IFS= read -r file; do
    [ -n "$file" ] || continue
    seen=1
    if ! printf '%s\n' "$file" | grep -Eq "$NON_LOGIC"; then
      echo "logic=true"
      return
    fi
  done
  # No files at all is not evidence of a docs-only change; run CI.
  if [ "$seen" -eq 1 ]; then echo "logic=false"; else echo "logic=true"; fi
}

if [ "${1:-}" = "--classify" ]; then
  classify
  exit 0
fi

if [ "${GITHUB_EVENT_NAME:-}" != "pull_request" ]; then
  result="logic=true"
else
  base="${BASE_SHA:?BASE_SHA is required for a pull request}"
  # NUL-separated `status<NUL>path<NUL>` records, no rename detection (see above).
  changed="$(mktemp)"
  trap 'rm -f "$changed"' EXIT
  git diff --no-renames --name-status -z "$base"...HEAD >"$changed"
  result="logic=false"
  seen=0
  while IFS= read -r -d '' status && IFS= read -r -d '' file; do
    seen=1
    case "$status" in
      T*) result="logic=true"; break ;;
    esac
    if [ "$(printf '%s\n' "$file" | classify)" = "logic=true" ]; then
      result="logic=true"
      break
    fi
  done <"$changed"
  # An empty diff is not evidence of a docs-only change; run CI.
  [ "$seen" -eq 1 ] || result="logic=true"
fi
echo "$result"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "$result" >>"$GITHUB_OUTPUT"
fi
if [ "$result" = "logic=false" ]; then
  echo "Only documentation and records changed; the heavy CI jobs are skipped."
fi

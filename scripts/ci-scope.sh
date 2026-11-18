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
# NON_LOGIC is documentation and records only. Deliberately not listed: THIRD_PARTY_LICENSES.html
# (copied into the image and compared with a fresh `cargo about` run), scripts/, .github/, and
# anything under src/, examples/, browser-worker/, container/, validation/, or proxy/.
set -euo pipefail

NON_LOGIC='^(.*\.md|docs/.*|\.security/.*|\.kiro/.*|LICENSE|\.gitignore)$'

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
  result="$(git diff --name-only "$base"...HEAD | classify)"
fi
echo "$result"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "$result" >>"$GITHUB_OUTPUT"
fi
if [ "$result" = "logic=false" ]; then
  echo "Only documentation and records changed; the heavy CI jobs are skipped."
fi

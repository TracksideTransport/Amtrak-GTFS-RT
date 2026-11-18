#!/usr/bin/env bash
# Offline test for scripts/lib-retry.sh. No network, no sleeping (the base delay is 0).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
# shellcheck source=scripts/lib-retry.sh
. scripts/lib-retry.sh

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }

# A command that fails twice and then succeeds is retried until it passes, and no further.
count_file="$tmp/count"
: >"$count_file"
flaky() { echo x >>"$count_file"; [ "$(wc -l <"$count_file")" -ge 3 ]; }
retry_with_backoff 5 0 flaky 2>/dev/null || fail "a flaky command should succeed on its third attempt"
[ "$(wc -l <"$count_file")" -eq 3 ] || fail "expected exactly 3 attempts, got $(wc -l <"$count_file")"

# A command that always fails is attempted exactly ATTEMPTS times and its status is returned.
: >"$count_file"
always() { echo x >>"$count_file"; return 7; }
status=0
retry_with_backoff 4 0 always 2>/dev/null || status=$?
[ "$status" -eq 7 ] || fail "expected the last exit status 7, got $status"
[ "$(wc -l <"$count_file")" -eq 4 ] || fail "expected exactly 4 attempts, got $(wc -l <"$count_file")"

# A command that succeeds at once runs once.
: >"$count_file"
once() { echo x >>"$count_file"; }
retry_with_backoff 3 0 once
[ "$(wc -l <"$count_file")" -eq 1 ] || fail "a passing command must not be repeated"

# The backoff is linear: 20, then 40, then 60 seconds, checked with sleep stubbed out.
schedule="$(bash -c '. scripts/lib-retry.sh; sleep() { echo "sleep $1"; }; retry_with_backoff 4 20 false' 2>/dev/null | tr "\n" " " || true)"
[ "$schedule" = "sleep 20 sleep 40 sleep 60 " ] || fail "unexpected backoff schedule: $schedule"

echo "retry helper tests passed"

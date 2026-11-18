#!/usr/bin/env bash
# Retry helper for steps that depend on a public registry that rate-limits (Maven Central answers
# HTTP 429 to a cold CI cache). Sourced by scripts/validate-feeds.sh; no side effects on source.
#
#   retry_with_backoff ATTEMPTS BASE_DELAY_SECS command [args...]
#
# Runs the command up to ATTEMPTS times. After failed attempt N it sleeps BASE_DELAY_SECS * N
# seconds (linear backoff: 20, 40, 60 with a base of 20), prints each failure to stderr, and
# returns the last exit status when every attempt fails. A command that succeeds is never repeated.
retry_with_backoff() {
  local attempts="$1" delay="$2" attempt=1 status=0
  shift 2
  while :; do
    "$@" && return 0
    status=$?
    if [ "$attempt" -ge "$attempts" ]; then
      printf 'giving up after %s attempt(s): %s (exit %s)\n' "$attempts" "$*" "$status" >&2
      return "$status"
    fi
    printf 'attempt %s/%s failed (exit %s); retrying in %ss: %s\n' \
      "$attempt" "$attempts" "$status" "$((delay * attempt))" "$1" >&2
    sleep "$((delay * attempt))"
    attempt=$((attempt + 1))
  done
}

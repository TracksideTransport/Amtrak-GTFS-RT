#!/usr/bin/env bash

set -euo pipefail

# Offline smoke test for the one amtrak-gtfs-rt image. It needs no network, so CI runs it natively
# on linux/amd64 and linux/arm64 for every pull request: nothing contacts Amtrak, and a blocked
# runner cannot fail it. scripts/test-container.sh is the live test (release gates).
#
#   scripts/test-image-offline.sh IMAGE [FULL_IMAGE]
#
# IMAGE is the default (headless-shell) build. FULL_IMAGE, when given, is the BROWSER_BUNDLE=full
# build; its Xvfb is started once.
USAGE='usage: scripts/test-image-offline.sh IMAGE [FULL_IMAGE]'
IMAGE="${1:?$USAGE}"
FULL_IMAGE="${2:-}"
RUN_ID="image-offline-$$-$(date +%s)"
WORKER="${RUN_ID}-worker"
GUARD="${RUN_ID}-guard"
VOLUME="${RUN_ID}-data"

cleanup() {
  docker rm -f "$WORKER" "$GUARD" >/dev/null 2>&1 || true
  docker volume rm "$VOLUME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1" >&2; exit 1; }

# The Compose service's confinement.
HARDENING=(
  --read-only
  --tmpfs /tmp:size=256m,mode=1777
  --tmpfs /home/amtrak:size=256m,uid=10001,gid=10001,mode=0700
  --cap-drop ALL --security-opt no-new-privileges:true
  --memory 1536m --pids-limit 512 --shm-size 64m
)

inspect() { docker image inspect "$IMAGE" -f "$1"; }

# Image contract.
[ "$(inspect '{{.Config.User}}')" = "10001:10001" ] || fail "image user is not 10001:10001"
[ "$(inspect '{{json .Config.Entrypoint}}')" = '["/usr/bin/tini","--","/usr/local/bin/amtrak-gtfs-rt-service"]' ] \
  || fail "entrypoint is not tini -- amtrak-gtfs-rt-service"
[ "$(inspect '{{json .Config.Healthcheck.Test}}')" = '["CMD","/usr/local/bin/amtrak-gtfs-rt-service","--healthcheck"]' ] \
  || fail "healthcheck is not amtrak-gtfs-rt-service --healthcheck"
[ "$(inspect '{{index .Config.Labels "org.opencontainers.image.licenses"}}')" = "AGPL-3.0-only" ] \
  || fail "license label"
inspect '{{range .Config.Env}}{{println .}}{{end}}' | grep -qx 'AMTRAK_ALLOWED_PEER_IPS=gateway' \
  || fail "AMTRAK_ALLOWED_PEER_IPS does not default to gateway"
pass "image contract (user, tini entrypoint, healthcheck, labels, gateway default)"

# Java runs on the Debian base (the validator's runtime).
docker run --rm --network none "${HARDENING[@]}" --entrypoint /opt/java/bin/java "$IMAGE" -version \
  >/dev/null 2>&1 || fail "java -version failed in the image"
pass "Java runtime starts"

# Chromium (headless shell) launches under the confinement and renders a page.
docker run --rm --network none "${HARDENING[@]}" --entrypoint python3 "$IMAGE" -c '
import asyncio
from playwright.async_api import async_playwright

async def main():
    async with async_playwright() as p:
        browser = await p.chromium.launch(args=["--disable-dev-shm-usage"])
        page = await browser.new_page()
        await page.set_content("<p id=x>smoke</p>")
        assert await page.text_content("#x") == "smoke"
        await browser.close()

asyncio.run(main())
' || fail "Chromium did not render a page under the confinement"
pass "Chromium renders under the Compose confinement"

# With no upstream and no recoverable generation, the service fails closed instead of serving.
docker volume create "$VOLUME" >/dev/null
docker run -d --name "$GUARD" --network none "${HARDENING[@]}" -v "$VOLUME":/data "$IMAGE" >/dev/null
for _ in $(seq 1 60); do
  [ "$(docker inspect -f '{{.State.Running}}' "$GUARD")" = true ] || break
  sleep 1
done
[ "$(docker inspect -f '{{.State.Running}}' "$GUARD")" = false ] \
  || fail "service kept running with no generation and no upstream"
[ "$(docker inspect -f '{{.State.ExitCode}}' "$GUARD")" != 0 ] \
  || fail "service exited 0 with no generation and no upstream"
pass "service fails closed offline with no generation"

# A leftover three-container role refuses to start.
docker rm -f "$GUARD" >/dev/null
role_log="$(mktemp)"
if docker run --rm --network none -e AMTRAK_ROLE=trip-updates "$IMAGE" >"$role_log" 2>&1; then
  fail "AMTRAK_ROLE=trip-updates started"
fi
grep -q 'AMTRAK_ROLE' "$role_log" || { cat "$role_log" >&2; fail "no AMTRAK_ROLE message"; }
rm -f "$role_log"
pass "AMTRAK_ROLE=trip-updates refuses to start"

# The worker the service supervises serves /healthz on loopback and stops promptly on SIGTERM.
docker run -d --name "$WORKER" --network none "${HARDENING[@]}" --workdir /app \
  -e LISTEN_HOST=127.0.0.1 -e POLL_INTERVAL_SECS=3600 -e NOTICES_POLL_INTERVAL_SECS=3600 \
  -e AMTRAK_FLEET_TRAINS_FILE=/tmp/absent.json -e AMTRAK_OUTPUT_DIR=/tmp \
  --entrypoint python3 "$IMAGE" -m worker >/dev/null
healthy=""
for _ in $(seq 1 60); do
  [ "$(docker inspect -f '{{.State.Running}}' "$WORKER")" = true ] \
    || { docker logs "$WORKER" >&2 || true; fail "worker exited during startup"; }
  code="$(docker exec "$WORKER" python3 -I -c '
import urllib.error, urllib.request
try:
    print(urllib.request.urlopen("http://127.0.0.1:8081/healthz", timeout=5).status)
except urllib.error.HTTPError as error:
    print(error.code)
' 2>/dev/null || true)"
  if [ "$code" = 200 ]; then healthy=yes; break; fi
  sleep 1
done
[ -n "$healthy" ] || { docker logs "$WORKER" >&2 || true; fail "worker /healthz did not answer 200 within 60s"; }
start="$(date +%s)"
docker stop --time 20 "$WORKER" >/dev/null
elapsed=$(( $(date +%s) - start ))
[ "$elapsed" -lt 15 ] || fail "worker took ${elapsed}s to stop on SIGTERM"
pass "worker serves /healthz on loopback and stops in ${elapsed}s"

if [ -n "$FULL_IMAGE" ]; then
  docker run --rm --network none --entrypoint python3 --workdir /app "$FULL_IMAGE" -c \
    "from worker.display import start_virtual_display, stop_virtual_display; stop_virtual_display(start_virtual_display('xvfb-headful', {}))" \
    || fail "Xvfb did not start in the full bundle"
  pass "full bundle starts Xvfb"
fi

printf 'One-image offline smoke test passed: %s\n' "$IMAGE"

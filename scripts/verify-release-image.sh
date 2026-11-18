#!/usr/bin/env bash

set -euo pipefail

IMAGE="${1:?usage: scripts/verify-release-image.sh IMAGE VERSION REVISION ARCH}"
VERSION="${2:?usage: scripts/verify-release-image.sh IMAGE VERSION REVISION ARCH}"
REVISION="${3:?usage: scripts/verify-release-image.sh IMAGE VERSION REVISION ARCH}"
ARCH="${4:?usage: scripts/verify-release-image.sh IMAGE VERSION REVISION ARCH}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_ID="release-image-verify-$$-$(date +%s)"
CONTAINER="${RUN_ID}-container"
WORKDIR="$(mktemp -d)"

cleanup() {
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

test "$(docker image inspect "$IMAGE" -f '{{.Config.User}}')" = "10001:10001"
test "$(docker image inspect "$IMAGE" -f '{{index .Config.Labels "org.opencontainers.image.licenses"}}')" = "AGPL-3.0-only"
test "$(docker image inspect "$IMAGE" -f '{{index .Config.Labels "org.opencontainers.image.source"}}')" = "https://github.com/sohampatwardhan/Amtrak-GTFS-RT"
test "$(docker image inspect "$IMAGE" -f '{{index .Config.Labels "org.opencontainers.image.version"}}')" = "$VERSION"
test "$(docker image inspect "$IMAGE" -f '{{index .Config.Labels "org.opencontainers.image.revision"}}')" = "$REVISION"
test "$(docker image inspect "$IMAGE" -f '{{.Architecture}}')" = "$ARCH"
test "$(docker image inspect "$IMAGE" -f '{{json .Config.Healthcheck.Test}}')" = '["CMD","/usr/local/bin/amtrak-gtfs-rt-service","--healthcheck"]'
test "$(docker image inspect "$IMAGE" -f '{{json .Config.Entrypoint}}')" = '["/usr/bin/tini","--","/usr/local/bin/amtrak-gtfs-rt-service"]'
test "$(docker image inspect "$IMAGE" -f '{{index .Config.Labels "org.opencontainers.image.title"}}')" = "Amtrak GTFS-RT"

docker create --name "$CONTAINER" "$IMAGE" >/dev/null

# Notices for everything in the image (container/licenses/NOTICES.md says where each one lives).
# A release with a missing notice fails here, before it can be published. The image has Python, so
# the check runs inside it.
docker run --rm --network none --entrypoint python3 "$IMAGE" -I -c '
import glob, sys
required = {
    "/licenses/AGPL-3.0-only.txt": "service license",
    "/licenses/THIRD_PARTY_LICENSES.html": "Rust crate licenses",
    "/licenses/NOTICES.md": "notice index",
    "/licenses/gtfs-validator-LICENSE": "validator license",
    "/usr/share/doc/libc6/copyright": "Debian package copyright files",
    "/usr/local/lib/python3.12/LICENSE.txt": "Python license",
}
patterns = {
    "/ms-playwright/chromium_headless_shell-*/*/LICENSE.headless_shell": "Chromium license",
    "/ms-playwright/ffmpeg-*/COPYING.LGPLv2.1": "FFmpeg license",
    "/opt/java/legal/java.base/*": "Java runtime legal notices",
    "/usr/local/lib/python3.12/site-packages/playwright-*.dist-info": "Playwright metadata",
    "/usr/local/lib/python3.12/site-packages/cryptography-*.dist-info": "cryptography metadata",
}
missing = []
for path, what in required.items():
    try:
        open(path, "rb").close()
    except OSError:
        missing.append(f"{what}: {path}")
for pattern, what in patterns.items():
    if not glob.glob(pattern):
        missing.append(f"{what}: {pattern}")
if missing:
    print("missing license notices:\n  " + "\n  ".join(missing), file=sys.stderr)
    sys.exit(1)
'

docker cp "$CONTAINER:/licenses/AGPL-3.0-only.txt" "$WORKDIR/AGPL-3.0-only.txt"
docker cp "$CONTAINER:/licenses/THIRD_PARTY_LICENSES.html" "$WORKDIR/THIRD_PARTY_LICENSES.html"

cmp -s "$ROOT/LICENSE" "$WORKDIR/AGPL-3.0-only.txt"
cmp -s "$ROOT/THIRD_PARTY_LICENSES.html" "$WORKDIR/THIRD_PARTY_LICENSES.html"
test -s "$WORKDIR/THIRD_PARTY_LICENSES.html"

printf 'Release image contract verified: %s (%s, %s, %s)\n' \
  "$IMAGE" "$VERSION" "$REVISION" "$ARCH"

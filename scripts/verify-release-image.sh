#!/usr/bin/env bash

# Release contract of the one amtrak-gtfs-rt image: OCI labels, runtime config, architecture, and
# the license notices for everything in it (container/licenses/README is the index). Offline:
# the image runs only a shell, with --network none.
#
#   scripts/verify-release-image.sh IMAGE VERSION REVISION ARCH
#
# CI runs it on every pull request with VERSION=dev and REVISION=unknown (the Dockerfile's
# defaults); the release runs it on each exact platform digest.

set -euo pipefail

USAGE='usage: scripts/verify-release-image.sh IMAGE VERSION REVISION ARCH'
IMAGE="${1:?$USAGE}"
VERSION="${2:?$USAGE}"
REVISION="${3:?$USAGE}"
ARCH="${4:?$USAGE}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_ID="release-image-verify-$$-$(date +%s)"
CONTAINER="${RUN_ID}-container"
WORKDIR="$(mktemp -d)"

cleanup() {
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

fail() { printf 'FAIL  %s\n' "$1" >&2; exit 1; }
inspect() { docker image inspect "$IMAGE" -f "$1"; }
label() { inspect "{{index .Config.Labels \"org.opencontainers.image.$1\"}}"; }
expect() { [ "$2" = "$3" ] || fail "$1: expected '$3', got '$2'"; }

# OCI labels.
expect title "$(label title)" "Amtrak GTFS-RT"
[ -n "$(label description)" ] || fail "description label is empty"
expect source "$(label source)" "https://github.com/TracksideTransport/Amtrak-GTFS-RT"
expect documentation "$(label documentation)" "https://github.com/TracksideTransport/Amtrak-GTFS-RT#container"
expect licenses "$(label licenses)" "AGPL-3.0-only"
expect base.name "$(label base.name)" "docker.io/library/python:3.12-slim-bookworm"
expect version "$(label version)" "$VERSION"
expect revision "$(label revision)" "$REVISION"

# Runtime config.
expect architecture "$(inspect '{{.Architecture}}')" "$ARCH"
expect user "$(inspect '{{.Config.User}}')" "10001:10001"
expect entrypoint "$(inspect '{{json .Config.Entrypoint}}')" \
  '["/usr/bin/tini","--","/usr/local/bin/amtrak-gtfs-rt-service"]'
expect healthcheck "$(inspect '{{json .Config.Healthcheck.Test}}')" \
  '["CMD","/usr/local/bin/amtrak-gtfs-rt-service","--healthcheck"]'
expect ports "$(inspect '{{json .Config.ExposedPorts}}')" '{"8080/tcp":{}}'
expect volumes "$(inspect '{{json .Config.Volumes}}')" '{"/data":{}}'

# Notices copied from this repository are byte-identical to it.
docker create --name "$CONTAINER" "$IMAGE" >/dev/null
for pair in \
  "LICENSE:AGPL-3.0-only.txt" \
  "THIRD_PARTY_LICENSES.html:THIRD_PARTY_LICENSES.html" \
  "container/licenses/README:README"; do
  repo_file="${pair%%:*}"
  image_file="${pair#*:}"
  docker cp "$CONTAINER:/licenses/$image_file" "$WORKDIR/$image_file" >/dev/null \
    || fail "/licenses/$image_file is missing"
  cmp -s "$ROOT/$repo_file" "$WORKDIR/$image_file" \
    || fail "/licenses/$image_file differs from $repo_file"
done

# Third-party notices, checked inside the image. Each failure names the component.
# shellcheck disable=SC2016 # the script is expanded by the image's shell, not this one
notices='
set -eu
bad=0
miss() { echo "missing notice: $1" >&2; bad=1; }

grep -q "Apache License" /licenses/gtfs-validator/LICENSE 2>/dev/null \
  || miss "MobilityData validator (/licenses/gtfs-validator/LICENSE)"

# Debian: every installed package has its copyright file.
packages=0
for pkg in $(dpkg-query -W -f "\${Package}\n"); do
  packages=$((packages + 1))
  [ -s "/usr/share/doc/${pkg%%:*}/copyright" ] || miss "Debian package $pkg"
done
[ "$packages" -gt 0 ] || miss "dpkg database"

# Python: the interpreter and every installed distribution.
[ -s /usr/local/lib/python3.12/LICENSE.txt ] || miss "CPython"
dists=0
for dist in /usr/local/lib/python3.12/site-packages/*.dist-info; do
  [ -d "$dist" ] || continue
  dists=$((dists + 1))
  [ -n "$(find "$dist" -type f \( -path "*/licenses/*" -o -name "LICENSE*" -o -name "COPYING*" \) -size +0)" ] \
    || miss "Python distribution $(basename "$dist")"
done
[ "$dists" -gt 0 ] || miss "Python site-packages"
driver=/usr/local/lib/python3.12/site-packages/playwright/driver
[ -s "$driver/LICENSE" ] || miss "Node.js bundled with Playwright"
[ -s "$driver/package/ThirdPartyNotices.txt" ] || miss "Playwright driver third-party notices"

# Playwright browsers: every download has its notices under /licenses.
browsers=0
for dir in /ms-playwright/*/; do
  browsers=$((browsers + 1))
  name="$(basename "$dir")"
  dest="/licenses/playwright-browsers/${name%-*}"
  ls "$dest" 2>/dev/null | grep -Eq "^(LICENSE|COPYING)" || miss "Playwright browser $name"
done
[ "$browsers" -gt 0 ] || miss "Playwright browsers"
[ -s /licenses/playwright-browsers/chromium_headless_shell/LICENSE.headless_shell ] \
  || miss "Chromium headless shell"

# Java: the runtime license and a legal directory for each module.
[ -s /opt/java/legal/java.base/LICENSE ] || miss "Java runtime"
modules="$(sed -n "s/^MODULES=\"\(.*\)\"$/\1/p" /opt/java/release)"
[ -n "$modules" ] || miss "Java module list"
for module in $modules; do
  [ -d "/opt/java/legal/$module" ] || miss "Java module $module"
done

[ "$bad" -eq 0 ]
echo "$packages Debian packages, $dists Python distributions, $browsers Playwright browsers"
'
counts="$(docker run --rm --network none --entrypoint /bin/sh "$IMAGE" -c "$notices")" \
  || fail "third-party license notices are incomplete"

printf 'Release image contract verified: %s (%s, %s, %s); notices for %s\n' \
  "$IMAGE" "$VERSION" "$REVISION" "$ARCH" "$counts"

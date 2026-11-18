#!/usr/bin/env bash

set -euo pipefail

VERSION="${1:?usage: scripts/check-release-metadata.sh VERSION [TAG]}"
TAG="${2:-v${VERSION}}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

actual="$(
  cargo metadata --manifest-path "$ROOT/Cargo.toml" --locked --offline --no-deps \
    --format-version 1 \
    | jq -r '.packages[] | select(.name == "amtrak-gtfs-rt-service") | .version'
)"

[ "$actual" = "$VERSION" ]
[ "$TAG" = "v${VERSION}" ]
grep -Eqx "## \[${VERSION//./\\.}\] - [0-9]{4}-[0-9]{2}-[0-9]{2}" "$ROOT/CHANGELOG.md"
grep -Fqx 'license = "AGPL-3.0-only"' "$ROOT/Cargo.toml"
test -s "$ROOT/THIRD_PARTY_LICENSES.html"

# The root compose file and bake default to this release's one GHCR image, and name no other
# ghcr.io package (#74: amtrak-gtfs-rt is the only one).
image_line="    image: ghcr.io/sohampatwardhan/amtrak-gtfs-rt:\${AMTRAK_IMAGE_TAG:-${VERSION}}"
grep -Fqx "$image_line" "$ROOT/docker-compose.yml"
if grep -E '^ +image: ghcr\.io/' "$ROOT/docker-compose.yml" | grep -Fvx "$image_line"; then
  printf 'docker-compose.yml names a GHCR image other than amtrak-gtfs-rt\n' >&2
  exit 1
fi
grep -A1 -x 'variable "AMTRAK_IMAGE_TAG" {' "$ROOT/docker-bake.hcl" \
  | grep -Fqx "  default = \"${VERSION}\""
if grep -E 'amtrak-gtfs-rt-[a-z]' "$ROOT/docker-bake.hcl"; then
  printf 'docker-bake.hcl names a package other than amtrak-gtfs-rt\n' >&2
  exit 1
fi

if command -v cargo-about >/dev/null 2>&1; then
  generated="$(mktemp)"
  trap 'rm -f "$generated"' EXIT
  "$ROOT/scripts/generate-third-party-licenses.sh" "$generated"
  cmp -s "$ROOT/THIRD_PARTY_LICENSES.html" "$generated"
fi

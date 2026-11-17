# Builds the one image this repository ships, ghcr.io/sohampatwardhan/amtrak-gtfs-rt
# (docs/one-container.md): the service, its browser worker with one headless Chromium, and the
# validator's Java runtime.
#
#   docker buildx bake                 # the one image (the `service` target)
#   docker buildx bake service         # the same
#
# The target is the `runtime` stage of the root Dockerfile. It is tagged `amtrak-gtfs-rt:local`
# and with the GHCR name the root compose file runs, at AMTRAK_IMAGE_TAG (the release version by
# default), so `docker compose up` uses what bake built. Tag local builds apart from the pulled
# release with `AMTRAK_IMAGE_TAG=local docker buildx bake`. Releases are built and pushed by
# .github/workflows/release.yml, not from here.
#
# FETCHER_BROWSER_BUNDLE=full adds full Chromium and Xvfb, for BROWSER_MODE=xvfb-headful (the same
# variable docker-compose.yml reads):  FETCHER_BROWSER_BUNDLE=full docker buildx bake

variable "FETCHER_BROWSER_BUNDLE" {
  default = "headless-shell"
}

# Keep in step with Cargo.toml and docker-compose.yml; scripts/check-release-metadata.sh checks it.
variable "AMTRAK_IMAGE_TAG" {
  default = "0.7.1"
}

variable "REGISTRY" {
  default = "ghcr.io/sohampatwardhan"
}

group "default" {
  targets = ["service"]
}

target "service" {
  context    = "."
  dockerfile = "Dockerfile"
  target     = "runtime"
  tags       = ["amtrak-gtfs-rt:local", "${REGISTRY}/amtrak-gtfs-rt:${AMTRAK_IMAGE_TAG}"]
  args       = { BROWSER_BUNDLE = FETCHER_BROWSER_BUNDLE }
}

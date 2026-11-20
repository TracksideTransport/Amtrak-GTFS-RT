# syntax=docker/dockerfile:1
#
# The one Amtrak GTFS-RT image: the service, the browser worker with one headless Chromium, and the
# validator's Java runtime (docs/one-container.md). linux/amd64 and linux/arm64.
#
# Build tools, package caches, and the source tree stay in disposable stages. The final stage is a
# slim Debian Python base, because headless Chromium needs glibc and its shared libraries. It holds:
#
#   tini                      PID 1. Reaps zombies and forwards signals.
#   amtrak-gtfs-rt-service    static musl binary, combined mode, HTTP API on :8080. It starts the
#                             browser worker only when AMTRAK_TRACKS_AMTRAK or AMTRAK_ADVISORIES is
#                             on, restarts it with backoff, and stops it on SIGTERM.
#   python -m worker          browser-worker/worker, on 127.0.0.1:8081 only.
#   /opt/java                 a jlink'd glibc Java 17 runtime for the validator.
#   the validator JAR         rebuilt from pinned MobilityData v8.0.1 source, digest-checked.
#   /licenses                 license notices for all of the above; /licenses/README is the index.
#
# It is the repository's only package, ghcr.io/sohampatwardhan/amtrak-gtfs-rt (#74).
#
# `docker build .` builds it; the last stage is `runtime`. Run it with:
#
#   docker run -d -p 127.0.0.1:8090:8080 -v amtrak-data:/data amtrak-gtfs-rt:local
#
# The image binds 0.0.0.0:8080 and admits the container's default gateway (the published-port
# path), so the -p binding decides who can reach it. See README "Container".

ARG OCI_VERSION=dev
ARG OCI_REVISION=unknown

# -------------------------------------------------------------------------------------------------
# Stage 1 — locked Rust release builder
# -------------------------------------------------------------------------------------------------
FROM rust:1.96-alpine@sha256:a41f7740f8b45d45795624eec13a8b42263cc700f19f7e4e86e04d3dda08a479 AS builder

RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add \
        build-base \
        ca-certificates \
        git \
        openssl-dev \
        openssl-libs-static \
        pkgconf \
        protobuf-dev

WORKDIR /build

# Build against musl and statically link the inherited native-tls/OpenSSL path.
# The Rust musl target links musl statically too, so the binary needs no loader and runs on the
# glibc base below; the runtime stage checks that.
RUN --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=Cargo.lock \
    --mount=type=bind,source=src,target=src \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,id=amtrak-target-musl-static-openssl,target=/build/target \
    OPENSSL_STATIC=1 \
    OPENSSL_LIB_DIR="$(pkg-config --variable=libdir openssl)" \
    OPENSSL_INCLUDE_DIR="$(pkg-config --variable=includedir openssl)" \
    cargo build --locked --release && \
    mkdir -p /out && \
    cp /build/target/release/amtrak-gtfs-rt-service /out/amtrak-gtfs-rt-service && \
    ! ldd /out/amtrak-gtfs-rt-service | grep -E 'lib(ssl|crypto|gcc_s|stdc\+\+)\.so'

# -------------------------------------------------------------------------------------------------
# Stage 2 — validator source, test, reproducibility, and digest gate
# -------------------------------------------------------------------------------------------------
FROM eclipse-temurin:17-jdk@sha256:f9b295135b39ed8c650c713c6116600dd4c39ac5f3883f566d96fdec917ce3b2 AS validator

RUN apt-get update && \
    apt-get install --yes --no-install-recommends git && \
    rm -rf /var/lib/apt/lists/*

# The commit is the v8.0.1 release. The archive and the rebuilt CLI JAR are both immutable gates;
# overriding the URL cannot change accepted bytes.
ARG VALIDATOR_SOURCE_URL=https://github.com/MobilityData/gtfs-validator/archive/d74d7177f9f7c6bc7adc69508bb939362f2cf770.tar.gz

COPY container/validator-dependency-overrides.gradle /opt/amtrak/validator-dependency-overrides.gradle
COPY container/validator-verification-metadata.xml /opt/amtrak/validator-verification-metadata.xml

WORKDIR /build
RUN --mount=type=cache,target=/root/.gradle \
    curl --fail --location --silent --show-error \
      --output /tmp/validator-source.tar.gz "${VALIDATOR_SOURCE_URL}" && \
    echo "651872b1a7abbde5b999d7261f875532eebaee22a9d7ce4946b8f764cdf7b8a3  /tmp/validator-source.tar.gz" \
      | sha256sum --check --strict && \
    mkdir -p /build/validator && \
    tar --extract --gzip --file /tmp/validator-source.tar.gz \
      --directory /build/validator --strip-components=1 && \
    cd /build/validator && \
    printf '\ndistributionSha256Sum=8cc27038d5dbd815759851ba53e70cf62e481b87494cc97cfd97982ada5ba634\n' \
      >> gradle/wrapper/gradle-wrapper.properties && \
    cp /opt/amtrak/validator-verification-metadata.xml gradle/verification-metadata.xml && \
    git init --quiet && \
    git config user.name container-build && \
    git config user.email container-build@invalid && \
    git add --all && \
    git commit --quiet --message 'pinned MobilityData v8.0.1 source' && \
    git tag v8.0.1 && \
    # Maven Central and the Gradle plugin portal sometimes answer a cold cache with errors that
    # Gradle reports as "Could not find". Retry the build up to three times; later attempts
    # refresh dependencies so a cached miss is not reused. Strict verification and the JAR digest
    # below still decide which bytes are accepted.
    attempt=1 && \
    until ./gradlew --no-daemon \
        --dependency-verification=strict \
        --init-script /opt/amtrak/validator-dependency-overrides.gradle \
        $( [ "$attempt" -gt 1 ] && echo --refresh-dependencies ) \
        test :cli:shadowJar; do \
      if [ "$attempt" -ge 3 ]; then echo "validator build failed after $attempt attempts" >&2; exit 1; fi; \
      echo "validator build attempt $attempt failed; retrying in $((attempt * 30))s" >&2; \
      sleep $((attempt * 30)); \
      attempt=$((attempt + 1)); \
    done && \
    echo "30de704a8fbd0a81e119f3afa45eaa158fd224275659bc4d3685b3156c443b64  cli/build/libs/gtfs-validator-8.0.1-cli.jar" \
      | sha256sum --check --strict && \
    mkdir -p /out && \
    cp cli/build/libs/gtfs-validator-8.0.1-cli.jar \
      /out/gtfs-validator-v8.0.1-amtrak-hardened.1-cli.jar

# The validator's own license (Apache-2.0), from the same digest-checked source, for /licenses.
# A separate step, so it does not invalidate the cached build and test above. The JAR's bundled
# dependencies keep their notices in its META-INF.
RUN grep -q 'Apache License' /build/validator/LICENSE && \
    mkdir -p /out/licenses/gtfs-validator && \
    install -m 0444 /build/validator/LICENSE /out/licenses/gtfs-validator/LICENSE

# -------------------------------------------------------------------------------------------------
# Stage 3 — glibc Java 17 runtime for the validator
# -------------------------------------------------------------------------------------------------
# Temurin's Linux binaries are built for an old glibc baseline, so the jlink'd runtime runs on the
# Debian bookworm base below. The modules are the ones the validator CLI loads:
#
# java.sql is required: Gson initializes java.sql.Time while parsing GTFS dates, and without that
# class the CLI's worker pool never writes report.json (measured on the official 8.0.1 jar).
# java.desktop stays because notice introspection loads java.beans. Debug symbols are stripped,
# which needs objcopy from binutils; binutils stays in this stage. java.desktop's X11 display
# libraries are removed: the validator runs headless, and they would pull X libraries into the
# image only to fail the ldd check below.
# Pinned separately from the validator stage: this JDK's runtime ships in the image, so it tracks
# Temurin's security releases (17.0.21 here; 17.0.19 has fixable High advisories), while the
# validator stage keeps the JDK its reproducible JAR digest was measured with.
FROM eclipse-temurin:17-jdk@sha256:5465815640e10433422217335e15a523a3e57021a1f2ae0fc3849380d237979f AS jre

RUN apt-get update && \
    apt-get install --yes --no-install-recommends binutils && \
    rm -rf /var/lib/apt/lists/*

RUN "${JAVA_HOME}/bin/jlink" \
      --module-path "${JAVA_HOME}/jmods" \
      --add-modules java.base,java.desktop,java.logging,java.sql,java.xml \
      --no-man-pages \
      --no-header-files \
      --strip-debug \
      --compress=2 \
      --output /out/java && \
    rm -f /out/java/lib/libawt_xawt.so /out/java/lib/libjawt.so /out/java/lib/libsplashscreen.so

# -------------------------------------------------------------------------------------------------
# Stage 4 — the one runtime image
# -------------------------------------------------------------------------------------------------
FROM python:3.12-slim-bookworm@sha256:935c3c5b12357a7e10caa9c1e6ca5692344d97cf229832ef584be2cec9fc03df AS runtime

ARG OCI_VERSION
ARG OCI_REVISION
# 1.65.2 bundles Node 26.13.1. 1.64.1's Node 24.22.1 has fixable High advisories (CVE-2026-123863,
# CVE-2026-123864, CVE-2026-123865, CVE-2026-123866, CVE-2026-123872, CVE-2026-123874,
# CVE-2026-126051), and 1.65.0's Node 26.13.0 is not the fixed release.
ARG PLAYWRIGHT_VERSION=1.65.2
ARG CRYPTOGRAPHY_VERSION=50.0.2
# `headless-shell` (default) installs only chromium-headless-shell. `full` also installs full
# Chromium and Xvfb for BROWSER_MODE=xvfb-headful, the fallback if Amtrak blocks the headless
# shell. It makes the image larger, so build it only when that mode is needed.
ARG BROWSER_BUNDLE=headless-shell

ENV PLAYWRIGHT_BROWSERS_PATH=/ms-playwright \
    PIP_NO_CACHE_DIR=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1

# Debian 12 names from Playwright's chromium nativeDeps, a small Latin font set, CA certificates,
# and tini. `playwright install --with-deps` would also pull Xvfb and large font packs that
# headless-shell capture does not need. Upgrade first: the slim tag can lag Debian security fixes.
RUN apt-get update \
    && apt-get upgrade --yes --no-install-recommends \
    && apt-get install --yes --no-install-recommends \
        ca-certificates \
        fonts-liberation \
        libasound2 \
        libatk-bridge2.0-0 \
        libatk1.0-0 \
        libatspi2.0-0 \
        libcairo2 \
        libcups2 \
        libdbus-1-3 \
        libdrm2 \
        libfontconfig1 \
        libfreetype6 \
        libgbm1 \
        libglib2.0-0 \
        libnspr4 \
        libnss3 \
        libpango-1.0-0 \
        libx11-6 \
        libxcb1 \
        libxcomposite1 \
        libxdamage1 \
        libxext6 \
        libxfixes3 \
        libxkbcommon0 \
        libxrandr2 \
        tini \
    && if [ "${BROWSER_BUNDLE}" = "full" ]; then \
        apt-get install --yes --no-install-recommends xvfb; \
    elif [ "${BROWSER_BUNDLE}" != "headless-shell" ]; then \
        echo "BROWSER_BUNDLE must be headless-shell or full" >&2; exit 1; \
    fi \
    && rm -rf /var/lib/apt/lists/*

# `--only-shell` installs chromium-headless-shell and skips full Chromium; the `full` bundle
# installs both and checks full Chromium's libraries too. pip is removed afterwards: nothing
# installs packages at runtime.
RUN pip install --no-cache-dir \
        "playwright==${PLAYWRIGHT_VERSION}" \
        "cryptography==${CRYPTOGRAPHY_VERSION}" \
    && if [ "${BROWSER_BUNDLE}" = "full" ]; then \
        python -m playwright install chromium \
        && full="$(find /ms-playwright -type f -name chrome -path '*chrome-linux*' -print -quit)" \
        && test -n "${full}" \
        && ! ldd "${full}" | grep 'not found' \
        && command -v Xvfb; \
    else \
        python -m playwright install --only-shell chromium; \
    fi \
    && shell="$(find /ms-playwright -type f \( -name chrome-headless-shell -o -name headless_shell \) -print -quit)" \
    && test -n "${shell}" \
    && ! ldd "${shell}" | grep 'not found' \
    && chmod -R a+rX /ms-playwright \
    && python -m pip uninstall --yes pip \
    && mkdir -p /licenses/playwright-browsers \
    && for dir in /ms-playwright/*/; do \
        name="$(basename "${dir}")"; \
        dest="/licenses/playwright-browsers/${name%-*}"; \
        mkdir -p "${dest}"; \
        find "${dir}" -type f \( -name 'LICENSE*' -o -name 'COPYING*' -o -name ABOUT \) \
            -exec cp {} "${dest}/" \; ; \
        ls "${dest}" | grep -Eq '^(LICENSE|COPYING)' \
            || { echo "no license notice under ${dir}" >&2; exit 1; }; \
    done \
    && chmod -R a+rX,go-w /licenses

# One unprivileged user. Its home holds Chromium's profile and caches (a tmpfs under Compose's
# read-only root filesystem). /data is the only volume; owning the mount point keeps a
# first-mounted empty volume writable.
RUN groupadd --gid 10001 amtrak \
    && useradd --uid 10001 --gid 10001 --create-home --home-dir /home/amtrak \
        --shell /usr/sbin/nologin amtrak \
    && mkdir -p /data /opt/amtrak \
    && chown 10001:10001 /data \
    && chmod 0555 /opt/amtrak

COPY --from=jre /out/java /opt/java
# libjvm.so sits in lib/server and the JDK finds it through its own RPATH; ldd needs the hint.
RUN ! LD_LIBRARY_PATH=/opt/java/lib/server ldd /opt/java/bin/java /opt/java/lib/*.so \
        /opt/java/lib/server/*.so 2>/dev/null | grep 'not found' \
    && /opt/java/bin/java -version

COPY --from=builder --chmod=0555 /out/amtrak-gtfs-rt-service /usr/local/bin/amtrak-gtfs-rt-service
RUN ldd /usr/local/bin/amtrak-gtfs-rt-service 2>&1 | grep -Eq 'statically linked|not a dynamic executable'
COPY --from=validator --chmod=0444 \
    /out/gtfs-validator-v8.0.1-amtrak-hardened.1-cli.jar \
    /opt/amtrak/gtfs-validator-v8.0.1-amtrak-hardened.1-cli.jar
# License notices for everything in the image. /licenses/README is the index: it lists these files
# and the notices that already ship in place (Debian copyright files, Python dist-info, the Java
# runtime's legal/ directory, Playwright's Node.js notices). scripts/verify-release-image.sh checks
# them all.
COPY --chmod=0444 container/licenses/README /licenses/README
COPY --chmod=0444 container/licenses/AGPL-3.0-only.txt /licenses/AGPL-3.0-only.txt
COPY --chmod=0444 THIRD_PARTY_LICENSES.html /licenses/THIRD_PARTY_LICENSES.html
COPY --from=validator /out/licenses/ /licenses/
COPY --chmod=u=rwX,go=rX browser-worker/worker/ /app/worker/

# `licenses` is the project's own license. Third-party components keep theirs; see /licenses/README.
LABEL org.opencontainers.image.title="Amtrak GTFS-RT" \
      org.opencontainers.image.description="Validated static GTFS and GTFS-Realtime feeds for Amtrak, with the browser worker and GTFS validator they use" \
      org.opencontainers.image.source="https://github.com/sohampatwardhan/Amtrak-GTFS-RT" \
      org.opencontainers.image.documentation="https://github.com/sohampatwardhan/Amtrak-GTFS-RT#container" \
      org.opencontainers.image.licenses="AGPL-3.0-only" \
      org.opencontainers.image.base.name="docker.io/library/python:3.12-slim-bookworm" \
      org.opencontainers.image.version="$OCI_VERSION" \
      org.opencontainers.image.revision="$OCI_REVISION"

# D1 (docs/one-container.md): bind every interface inside the container and admit the default
# gateway, which is where requests through a published port arrive from. The `-p` binding decides
# who can reach the service. A host binary keeps the loopback default.
ENV JAVA_HOME=/opt/java \
    PATH=/opt/java/bin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    HOME=/home/amtrak \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt \
    AMTRAK_OUTPUT_DIR=/data \
    AMTRAK_GTFS_VALIDATOR_JAR=/opt/amtrak/gtfs-validator-v8.0.1-amtrak-hardened.1-cli.jar \
    AMTRAK_BIND_ADDR=0.0.0.0:8080 \
    AMTRAK_ALLOWED_PEER_IPS=gateway

VOLUME ["/data"]
EXPOSE 8080/tcp

USER 10001:10001
WORKDIR /data

# /livez must answer 200, and when the service runs the browser worker, the worker's listener must
# answer. A worker past its restart budget has already made the service exit non-zero.
HEALTHCHECK --interval=30s --timeout=10s --start-period=120s --retries=3 \
    CMD ["/usr/local/bin/amtrak-gtfs-rt-service", "--healthcheck"]

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/amtrak-gtfs-rt-service"]

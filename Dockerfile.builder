# Reusable build environment. Keeps the toolchain and system libraries out of every
# throwaway container: ONNX Runtime links libstdc++, and sqlx/reqwest link OpenSSL.
FROM rust:1-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates g++ curl xz-utils \
 && rm -rf /var/lib/apt/lists/*

# watchexec drives the `dev` service in docker-compose.yml: it watches the bind-mounted sources and
# restarts the debug binary when one changes. Rust has no hot reload of its own, and cargo has no
# watch mode, so a running server picks up an edit by being killed and started again.
#
# A prebuilt musl binary rather than `cargo install watchexec-cli`, which compiles a second
# dependency tree from source every time this image is rebuilt. The checksum is verified because
# this is a release artifact fetched over the network into a toolchain image. The published
# .sha256 holds a bare hash with no filename, so the two-space form sha256sum -c wants is built here.
#
# `uname -m` rather than the TARGETARCH build arg: TARGETARCH is only populated under BuildKit, and
# this image is built by hand as often as by compose. uname reports aarch64 and x86_64, which is
# already the spelling the release assets use.
ARG WATCHEXEC_VERSION=2.5.1
RUN arch="$(uname -m)" \
 && base="watchexec-${WATCHEXEC_VERSION}-${arch}-unknown-linux-musl" \
 && url="https://github.com/watchexec/watchexec/releases/download/v${WATCHEXEC_VERSION}/${base}.tar.xz" \
 && curl -fsSL -o /tmp/we.tar.xz "$url" \
 && curl -fsSL -o /tmp/we.sha256 "${url}.sha256" \
 && echo "$(cat /tmp/we.sha256)  /tmp/we.tar.xz" | sha256sum -c - \
 && tar -xJf /tmp/we.tar.xz -C /tmp \
 && install -m 0755 "/tmp/${base}/watchexec" /usr/local/bin/watchexec \
 && rm -rf /tmp/we.tar.xz /tmp/we.sha256 "/tmp/${base}" \
 && watchexec --version

WORKDIR /app

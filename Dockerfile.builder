# Reusable build environment. Keeps the toolchain and system libraries out of every
# throwaway container: ONNX Runtime links libstdc++, and sqlx/reqwest link OpenSSL.
FROM rust:1-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates g++ curl \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app

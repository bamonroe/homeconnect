# syntax=docker/dockerfile:1
# Production image: build the SPA + release binary in a toolchain stage, then
# ship a slim runtime with just ffmpeg + the binary + built assets. Migrations
# are embedded in the binary at compile time (sqlx::migrate!), so the runtime
# needs neither the migrations dir nor a DB toolchain.

FROM rust:1-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
        capnproto nodejs npm pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
# Build the SPA (Vite → web/dist).
RUN cd web && npm ci && npm run build
# Build the release binary; cache cargo registry + target across builds and copy
# the artifact out of the (ephemeral) cache mount.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --bin homeconnect && \
    cp /build/target/release/homeconnect /usr/local/bin/homeconnect

FROM debian:bookworm-slim AS runtime
# ffmpeg + VAAPI drivers for both GPUs so either is selectable at runtime:
#   mesa-va-drivers          → radeonsi (AMD)
#   intel-media-va-driver    → iHD (Intel Quick Sync)
# (vainfo is handy for debugging GPU access from inside the container.)
RUN apt-get update && apt-get install -y --no-install-recommends \
        ffmpeg ca-certificates libva2 vainfo \
        mesa-va-drivers intel-media-va-driver \
        openssh-client \
    && rm -rf /var/lib/apt/lists/*
# The container runs as uid 1000 (see compose). ssh-keygen/ssh call getpwuid(),
# which fails ("No user exists for uid 1000") without a passwd entry — so create
# one. HOME points at a writable dir for OpenSSH's bookkeeping.
RUN useradd -u 1000 -m -d /home/app -s /usr/sbin/nologin app
ENV HOME=/home/app
WORKDIR /app
COPY --from=builder /usr/local/bin/homeconnect /usr/local/bin/homeconnect
COPY --from=builder /build/web/dist /app/web/dist
ENV HC_WEB_DIR=/app/web/dist \
    HC_DATA_DIR=/data \
    HC_BIND=0.0.0.0:8099
EXPOSE 8099
ENTRYPOINT ["homeconnect"]

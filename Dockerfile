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
RUN apt-get update && apt-get install -y --no-install-recommends \
        ffmpeg ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /usr/local/bin/homeconnect /usr/local/bin/homeconnect
COPY --from=builder /build/web/dist /app/web/dist
ENV HC_WEB_DIR=/app/web/dist \
    HC_DATA_DIR=/data \
    HC_BIND=0.0.0.0:8099
EXPOSE 8099
ENTRYPOINT ["homeconnect"]

# syntax=docker/dockerfile:1
#
# NOTE: this Dockerfile has not been executed/verified in the environment
# this project was built in (no Docker daemon available there) — see
# docs/ROADMAP.md. It follows the standard Rust-on-musl pattern; treat it
# as a strong starting point and verify it once in CI/locally before
# relying on it.

FROM rust:1.85-slim AS builder
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl wasm32-wasip2

WORKDIR /build
COPY . .

# Build the example plugin first (independent nested workspace) so it can
# be baked into the image as a ready-to-use plugin, then the main binary.
RUN cd plugins/example-renderer && cargo build --target wasm32-wasip2 --release
RUN cargo build --release --target x86_64-unknown-linux-musl -p morph-cli

FROM scratch
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/morph /morph
COPY --from=builder /build/plugins/example-renderer/target/wasm32-wasip2/release/example_renderer.wasm /plugins/example_renderer.wasm
COPY --from=builder /build/morph.example.toml /morph.toml

EXPOSE 8080
ENTRYPOINT ["/morph", "--config", "/morph.toml"]

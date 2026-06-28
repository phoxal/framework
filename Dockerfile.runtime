# syntax=docker/dockerfile:1.6

# Shared multi-arch Dockerfile for every phoxal platform runtime.
# Built per runtime via `scripts/build-images.sh` with --build-arg RUNTIME_NAME=<name>.
#
# Stages:
#   chef    - base image with cargo-chef installed
#   planner - produce recipe.json from the workspace
#   builder - cook deps (cached), then build the requested runtime bin
#   final   - distroless image carrying only the binary

ARG RUST_VERSION=1.94

FROM rust:${RUST_VERSION}-slim-bookworm AS chef
WORKDIR /workspace
RUN cargo install --version 0.1.71 --locked cargo-chef

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
ARG RUNTIME_NAME

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev cmake clang \
    && rm -rf /var/lib/apt/lists/*

COPY --from=planner /workspace/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json \
    --bin phoxal-runtime-${RUNTIME_NAME}

COPY . .
RUN cargo build --release --bin phoxal-runtime-${RUNTIME_NAME}

FROM gcr.io/distroless/cc-debian12:nonroot
ARG RUNTIME_NAME
COPY --from=builder /workspace/target/release/phoxal-runtime-${RUNTIME_NAME} \
                    /usr/local/bin/runtime
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/runtime"]

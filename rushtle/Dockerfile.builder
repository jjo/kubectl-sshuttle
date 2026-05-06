# Builder image used by `scripts/build-rushtle-prebuilt.sh`.
#
# We deliberately skip the usual "warm up dep cache with a stub main()" trick
# — Docker COPY normalizes mtimes, so cargo's incremental check sees the
# stub-built target as fresh and skips rebuilding when the real src is
# COPYd in. Result was a 330KB `fn main(){}` binary that exits 0 silently.
# Single COPY + single build avoids the trap.

ARG RUST_VERSION=1.91
ARG DEBIAN_RELEASE=bookworm

FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS builder
ARG GIT_REV=unknown
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config build-essential ca-certificates && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /src

COPY Cargo.toml Cargo.lock* build.rs ./
COPY src ./src
RUN RUSHTLE_GIT_REV_OVERRIDE="${GIT_REV}" cargo build --release && \
    strip target/release/rushtle && \
    cp target/release/rushtle /rushtle

FROM scratch AS bin
COPY --from=builder /rushtle /rushtle

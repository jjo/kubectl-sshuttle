# Build a static rushtle binary with musl, package it in a small runtime
# image. The same binary is used both for `rushtle server` (in-pod ssnet
# server) and as a `python -c` shim for stock sshuttle clients
# (`--python=/usr/local/bin/rushtle`).
#
# Multi-arch: TARGETARCH is supplied by buildx for linux/amd64 +
# linux/arm64; we map it to the matching musl rust target. Hardcoding a
# single triple here would silently produce a wrong-arch binary when the
# image is built via `buildx --platform linux/arm64`.
#
# We deliberately avoid the "warm dep cache with a stub main()" trick —
# Docker normalizes COPY mtimes, so cargo's incremental check sees the
# stub-built target as fresh and skips rebuilding when the real src is
# COPYd in. That trap shipped a 330KB `fn main(){}` binary. Single COPY
# + single build avoids it.

ARG RUST_VERSION=1.91
ARG ALPINE_VERSION=3.20

# No `--platform=$BUILDPLATFORM` pin: buildx runs the builder under qemu
# for the target platform, so cargo compiles natively (alpine rust image
# on arm64 → aarch64 toolchain → native musl link). Pinning to BUILDPLATFORM
# would require a cross-linker (musl-cross-aarch64) we don't bundle.
FROM rust:${RUST_VERSION}-alpine${ALPINE_VERSION} AS builder
ARG TARGETARCH
# Short git sha for the version string. Defaults to `unknown` so the build
# still succeeds when invoked without `--build-arg GIT_REV=...`. See
# build.rs for how the override is consumed.
ARG GIT_REV=unknown
RUN apk add --no-cache musl-dev
WORKDIR /src

COPY Cargo.toml Cargo.lock* build.rs ./
COPY src ./src
# Alpine rust image's default host target is already `<arch>-unknown-linux-musl`,
# so plain `cargo build --release` produces a static musl binary for whichever
# arch we're emulating. No --target / rustup-target-add needed.
RUN RUSHTLE_GIT_REV_OVERRIDE="${GIT_REV}" cargo build --release && \
    strip /src/target/release/rushtle && \
    cp /src/target/release/rushtle /rushtle

FROM alpine:${ALPINE_VERSION}
# tini for clean PID 1 signal handling; ca-certificates for TLS targets.
# No iptables in this image — the in-pod rushtle server doesn't need it
# (NAT redirect happens on the local side). Pod runs as uid=1001.
RUN apk add --no-cache ca-certificates tini && \
    addgroup -g 1001 rushtle && adduser -D -u 1001 -G rushtle rushtle
COPY --from=builder /rushtle /usr/local/bin/rushtle
RUN chmod 0755 /usr/local/bin/rushtle
USER rushtle
WORKDIR /home/rushtle

# Default: idle so kubectl exec can attach. The actual `rushtle server`
# (or `-c PYSCRIPT` python-shim mode) is launched per-session by the
# kubectl-sshuttle client.
ENTRYPOINT ["/sbin/tini", "--"]
CMD ["sh", "-c", "touch /tmp/ready && exec sleep infinity"]

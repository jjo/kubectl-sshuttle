# Build a static rushtle binary with musl, package it in a small runtime
# image that has iptables+iproute2 (for debugging the pod) and a
# `rushtle-shim` script that lets stock sshuttle drive rushtle as if it were
# python (`--python=/usr/local/bin/rushtle-shim`).

ARG RUST_VERSION=1.91
ARG ALPINE_VERSION=3.20

FROM rust:${RUST_VERSION}-alpine${ALPINE_VERSION} AS builder
RUN apk add --no-cache musl-dev
WORKDIR /src

COPY Cargo.toml Cargo.lock* ./
RUN mkdir -p src && echo "fn main(){}" > src/main.rs && \
    cargo build --release --target x86_64-unknown-linux-musl 2>/dev/null || true
RUN rm -rf src target/x86_64-unknown-linux-musl/release/deps/rushtle*

COPY src ./src
RUN cargo build --release --target x86_64-unknown-linux-musl && \
    strip /src/target/x86_64-unknown-linux-musl/release/rushtle

FROM alpine:${ALPINE_VERSION}
# tini for clean PID 1 signal handling; ca-certificates for TLS targets.
# No iptables in this image — the in-pod rushtle server doesn't need it
# (NAT redirect happens on the local side). Pod runs as uid=1001.
RUN apk add --no-cache ca-certificates tini && \
    addgroup -g 1001 rushtle && adduser -D -u 1001 -G rushtle rushtle
COPY --from=builder /src/target/x86_64-unknown-linux-musl/release/rushtle /usr/local/bin/rushtle
RUN chmod 0755 /usr/local/bin/rushtle
USER rushtle
WORKDIR /home/rushtle

# Default: idle so kubectl exec can attach. The actual `rushtle server`
# (or `-c PYSCRIPT` python-shim mode) is launched per-session by the
# kubectl-sshuttle client.
ENTRYPOINT ["/sbin/tini", "--"]
CMD ["sh", "-c", "touch /tmp/ready && exec sleep infinity"]

#!/usr/bin/env bash
# Cross-build rushtle as a static glibc binary for goreleaser archives.
#
#   prebuilt/rushtle_<os>_<arch>/rushtle
#
# Outside dist/ because goreleaser nukes dist/ at the start of every run.
#
# Local builds (this script): linux/amd64 only — fast, no QEMU, native arch.
# CI: rebuilds linux/arm64 + darwin/{amd64,arm64} on the right runners and
#     overwrites the placeholders this script writes.
#
# Set BUILD_ALL_LINUX=1 to also build linux/arm64 locally (needs Docker
# buildx + QEMU on amd64 hosts).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUSHTLE_DIR="$ROOT/rushtle"
DIST="$ROOT/prebuilt"
mkdir -p "$DIST"

# Short git sha embedded into rushtle's --version output. Computed here
# rather than via build.rs because the Docker build context excludes
# `.git/` so build.rs can't introspect the repo on its own.
GIT_REV="${GIT_REV:-$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)}"
export GIT_REV
echo "==> rushtle git rev: $GIT_REV"

build_linux_native_amd64() {
    local out="$DIST/rushtle_linux_amd64"
    mkdir -p "$out"
    echo "==> linux/amd64: docker buildx (native amd64) → ${out}/rushtle"
    docker buildx build \
        --platform linux/amd64 \
        --no-cache \
        --build-arg "GIT_REV=${GIT_REV}" \
        -f "$RUSHTLE_DIR/Dockerfile.builder" \
        --target bin \
        --output "type=local,dest=${out}" \
        "$RUSHTLE_DIR"
    if [ ! -f "$out/rushtle" ]; then
        echo "ERROR: ${out}/rushtle missing after buildx" >&2
        exit 1
    fi
    chmod 0755 "$out/rushtle"
}

build_linux_qemu_arm64() {
    local out="$DIST/rushtle_linux_arm64"
    mkdir -p "$out"
    echo "==> linux/arm64: docker buildx (QEMU emulation) → ${out}/rushtle"
    docker buildx build \
        --platform linux/arm64 \
        --no-cache \
        --build-arg "GIT_REV=${GIT_REV}" \
        -f "$RUSHTLE_DIR/Dockerfile.builder" \
        --target bin \
        --output "type=local,dest=${out}" \
        "$RUSHTLE_DIR"
    chmod 0755 "$out/rushtle"
}

placeholder() {
    local triple="$1"  # e.g. linux_arm64
    local out="$DIST/rushtle_${triple}"
    mkdir -p "$out"
    cat > "$out/rushtle" <<EOF
#!/bin/sh
echo "rushtle: placeholder binary for ${triple}." >&2
echo "rushtle: real binary is built in CI on the matching runner." >&2
exit 1
EOF
    chmod 0755 "$out/rushtle"
}

# Always build the host platform (linux/amd64). Matches local dev — fast,
# usable, single-arch output good enough for `make krew-install-local`.
build_linux_native_amd64

# Optional local arm64 (slow QEMU build). CI flips this on via env.
if [ "${BUILD_ALL_LINUX:-0}" = "1" ]; then
    build_linux_qemu_arm64
else
    echo "==> linux/arm64: writing placeholder (set BUILD_ALL_LINUX=1 to build locally; CI fills it in)"
    placeholder linux_arm64
fi

# Darwin: only when host is macOS. CI uses a darwin runner.
if [ "$(uname -s)" = "Darwin" ]; then
    for arch in amd64 arm64; do
        triple="darwin_${arch}"
        case "$arch" in
            amd64) rust_target=x86_64-apple-darwin ;;
            arm64) rust_target=aarch64-apple-darwin ;;
        esac
        out="$DIST/rushtle_${triple}"
        mkdir -p "$out"
        echo "==> darwin/${arch}: cargo --target=${rust_target} → ${out}/rushtle"
        ( cd "$RUSHTLE_DIR" && \
            rustup target add "$rust_target" >/dev/null && \
            cargo build --release --target "$rust_target" && \
            cp "target/${rust_target}/release/rushtle" "$out/rushtle" && \
            chmod 0755 "$out/rushtle" )
    done
else
    echo "==> darwin: writing placeholders (host is $(uname -s); CI fills these in)"
    placeholder darwin_amd64
    placeholder darwin_arm64
fi

echo "==> done. artifacts:"
ls -la "$DIST"/rushtle_*/rushtle 2>&1 || true

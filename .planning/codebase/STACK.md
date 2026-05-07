# Technology Stack

**Analysis Date:** 2026-05-07

## Languages

**Primary:**
- Go 1.25.0 — `kubectl sshuttle` CLI plugin (the user-facing binary). Declared in `go.mod` line 3.
- Rust 2021 edition — `rushtle`, the in-pod TCP/UDP tunnel and bootstrapped python-shim. Declared in `rushtle/Cargo.toml`.

**Toolchain pins:**
- Rust `1.91` (pinned twice: `rushtle/Dockerfile` `ARG RUST_VERSION=1.91`, `rushtle/Dockerfile.builder` `ARG RUST_VERSION=1.91`).
- No `rust-toolchain` / `rust-toolchain.toml` file in `rushtle/` — host builds use whatever stable rustc is installed, container builds use the pinned `rust:1.91-*` image.

**Auxiliary:**
- Python 3 (host-side) — used only for the in-tree smoke harnesses under `scripts/rushtle-*-smoketest.py` (driven by `make rushtle-test`). The released plugin does not depend on Python on the client side; the legacy proxy pod ships Python 3.12.
- Bash — release packaging (`scripts/build-rushtle-prebuilt.sh`).

## Runtime

**Go binary (`kubectl-sshuttle`):**
- Static-ish Go binary (no cgo dependencies declared); ships in the krew tarball next to `rushtle`.
- Discovers `kubectl` and `sshuttle` via `exec.LookPath`; never imports the Kubernetes Go client — it shells out to `kubectl` (`cmd/sshproxy.go:51`, `cmd/connect.go:66-68`).

**Rushtle binary (`rushtle`):**
- Tokio multi-threaded async runtime built explicitly in `rushtle/src/main.rs:143-146` and again at `:153-155` (one for `-c PYSCRIPT` shim path, one for normal CLI path).
- Static musl Linux build via Alpine (`rushtle/Dockerfile`) for the in-pod image; Debian-bookworm glibc build via `Dockerfile.builder` for the goreleaser archives.
- Release profile is size-tuned: `strip = true`, `lto = "thin"`, `codegen-units = 1`, `opt-level = "z"`, `panic = "abort"` (`rushtle/Cargo.toml` lines 22-27).

**Package Manager:**
- Go modules (`go.mod` + `go.sum`).
- Cargo (`rushtle/Cargo.toml` + `rushtle/Cargo.lock` — committed).

## Frameworks

**CLI:**
- Go: `github.com/spf13/cobra v1.10.2` (`go.mod:5`). Indirect: `pflag v1.0.9`, `mousetrap v1.1.0`. No viper; flags live on a single `Config` struct in `cmd/root.go:13-25`.
- Rust: `clap` 4 with `derive` feature (`rushtle/Cargo.toml:15`). All subcommands defined as a single `enum Cmd` in `rushtle/src/main.rs:27-92`.

**Async runtime / networking (Rust):**
- `tokio` 1, features: `rt-multi-thread`, `net`, `io-util`, `io-std`, `macros`, `process`, `signal`, `sync`, `time` (`rushtle/Cargo.toml:14`).
- `bytes` 1 — frame buffer reuse in `rushtle/src/ssnet.rs`.

**Tracing / logging:**
- Rust: `tracing` 0.1 + `tracing-subscriber` 0.3 with `env-filter` (`rushtle/Cargo.toml:19-20`). Initialised in `rushtle/src/main.rs:109-121`. Verbosity from `-v/-vv` or `RUST_LOG` env.
- Go: stdlib `fmt.Fprintf(os.Stderr, ...)` only — no structured logger.

**Error handling (Rust):**
- `anyhow` 1 throughout (`Result<T>` aliasing `anyhow::Result`, `Context` for chain-of-causes).

**System / FFI (Rust):**
- `libc` 0.2 — used for `SO_ORIGINAL_DST` ioctls (Linux) and `DIOCNATLOOK` ioctl plumbing (macOS) in `rushtle/src/firewall/{linux,macos}.rs`.

**Testing:**
- Go: stdlib `testing`. Test files: `cmd/sshproxy_test.go`, `proxy/deployment_test.go`. Run via `make test` → `go test ./... -v`.
- Rust: in-tree Python smoke tests under `scripts/rushtle-*-smoketest.py` (six harnesses) driven by `make rushtle-test`. No `cargo test` suite yet.

## Build Tools

**Makefile** (`Makefile`, root) — primary entry point. Key targets:
- `build` — `go build -ldflags "-X .../cmd.version=<git-describe>+<sha>"`.
- `test` — `go test ./... -v`.
- `rushtle` — `cd rushtle && cargo build --release`.
- `rushtle-image`, `rushtle-image-multiarch`, `rushtle-push` — single-arch and `linux/amd64,linux/arm64` buildx for `xjjo/rushtle`.
- `sshuttle-image`, `sshuttle-push` — same flow for `xjjo/sshuttle` (`proxy/Dockerfile`).
- `images-push` — both above.
- `rushtle-prebuilt` — calls `scripts/build-rushtle-prebuilt.sh` to populate `prebuilt/rushtle_<os>_<arch>/rushtle` for goreleaser.
- `release-snapshot` — local goreleaser dry-run (`goreleaser release --snapshot --clean --skip=publish,sign`).
- `krew-install-local` / `krew-uninstall-local` — install the just-built snapshot tarball through the real `kubectl krew` for end-to-end testing.
- `rushtle-test` — runs all six Python smoketest harnesses.

**goreleaser** (`.goreleaser.yml`, version 2):
- Builds `kubectl-sshuttle` for `linux,darwin × amd64,arm64` (4 archives total).
- Archive name template: `kubectl-sshuttle_v{{.Version}}_{{.Os}}_{{.Arch}}.tar.gz` (the leading `v` matters for the krew-release-bot template).
- `before.hooks` runs `bash scripts/build-rushtle-prebuilt.sh` to populate `prebuilt/rushtle_<os>_<arch>/rushtle` before archives are assembled (per-platform `rushtle` is dropped into each archive at `dst: rushtle`, mode `0755`).
- CI (see release.yml below) overrides this with `--skip=before` and instead downloads the per-platform rushtle artifacts uploaded by the matrix prebuild jobs.
- ldflags inject version + short commit into `cmd.version`.

**cargo** — drives the Rust build:
- `cargo build --release` on host (Linux/macOS).
- Inside Alpine container: `cargo build --release` produces a static `<arch>-unknown-linux-musl` binary (no explicit `--target` needed; alpine rust image's host target already is musl).
- `rushtle/build.rs` resolves the short git sha (via `RUSHTLE_GIT_REV_OVERRIDE` env or `git rev-parse --short HEAD`) and exposes it as `RUSHTLE_GIT_REV` to the crate.

## Container Images

**`xjjo/sshuttle`** (`proxy/Dockerfile`):
- Base: `python:3.12-slim`.
- `pip install --no-cache-dir sshuttle` baked in (no apt-get, no pip at pod startup).
- Non-root user: `uid=1001` `gid=1001` (`sshuttle:sshuttle`), `WORKDIR /home/sshuttle`.
- CMD: `sh -c "touch /tmp/ready && exec sleep infinity"` (idle pod; the kubectl-exec path drives the actual sshuttle server per session).

**`xjjo/rushtle`** (`rushtle/Dockerfile`):
- Builder: `rust:${RUST_VERSION}-alpine${ALPINE_VERSION}` (defaults `RUST_VERSION=1.91`, `ALPINE_VERSION=3.20`) + `apk add musl-dev`.
- Multi-arch via buildx: `TARGETARCH` from the buildx target, native-emulated build under qemu for cross-arch.
- Runtime: `alpine:3.20` + `tini` (PID 1) + `ca-certificates`. Non-root user `rushtle:rushtle` (uid=1001).
- ENTRYPOINT `/sbin/tini --`; CMD `sh -c "touch /tmp/ready && exec sleep infinity"`.
- `GIT_REV` build-arg fed into the binary via `RUSHTLE_GIT_REV_OVERRIDE` (consumed by `rushtle/build.rs`).

**Builder-only image** (`rushtle/Dockerfile.builder`):
- Base: `rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE}` (defaults `RUST_VERSION=1.91`, `DEBIAN_RELEASE=bookworm`) + `pkg-config build-essential ca-certificates`.
- Final stage: `FROM scratch AS bin` containing only `/rushtle`.
- Used by `scripts/build-rushtle-prebuilt.sh` to extract the prebuilt binary (`docker buildx build ... --target bin --output type=local,dest=...`) — produces the per-platform `rushtle` baked into the goreleaser tarballs (glibc, not musl, so it matches the `kubectl-sshuttle` Go binary's libc).

## Krew Plugin Manifest

**Production template:** `.krew.yaml` — used by goreleaser/krew-release-bot to generate the krew-index manifest at release time.
- Plugin name: `sshuttle` (so users run `kubectl krew install sshuttle`).
- 4 platforms declared: `linux/amd64`, `linux/arm64`, `darwin/amd64`, `darwin/arm64`.
- Each archive includes `LICENSE`, `kubectl-sshuttle` (the Go binary, set as `bin:`), and `rushtle` (the Rust binary, dropped beside the Go binary so `cmd/connect.go:resolveRushtleBin()` finds it via `os.Executable() + /rushtle`).
- `addURIAndSha` template feeds the per-tag GitHub Releases tarball URL.

**Legacy / committed snapshot:** `krew-index-plugins-sshuttle.yaml` is a v0.1.0 manifest (pre-rushtle) preserved for reference; `PR_krew-index.md` (untracked) is the krew-index PR body.

## CI

**Workflow:** `.github/workflows/release.yml`.
- Trigger: `push tags v*.*.*`.
- Permissions: `contents: write`.
- 3 jobs:
  1. **`build-rushtle-linux`** — `runs-on: ubuntu-latest`. Sets `BUILD_ALL_LINUX=1` and runs `scripts/build-rushtle-prebuilt.sh`, which produces `linux/amd64` natively and `linux/arm64` via buildx + qemu. Uploads each as a separate `actions/upload-artifact@v4` artifact (`rushtle_linux_amd64`, `rushtle_linux_arm64`).
  2. **`build-rushtle-darwin`** — `runs-on: macos-latest` (Apple Silicon). Uses `dtolnay/rust-toolchain@stable` with both `x86_64-apple-darwin` and `aarch64-apple-darwin` targets, builds each natively, uploads `rushtle_darwin_amd64` and `rushtle_darwin_arm64` artifacts.
  3. **`goreleaser`** — `needs: [build-rushtle-linux, build-rushtle-darwin]`. Downloads all `rushtle_*` artifacts into `prebuilt/`, runs `goreleaser/goreleaser-action@v6` with `args: release --clean --skip=before` (so the `before.hooks` script doesn't overwrite the matrix-built binaries with placeholders), then runs `rajatjindal/krew-release-bot@v0.0.50` to open the krew-index PR.

## Configuration

**Build-time:**
- `GIT_REV` env var → `RUSHTLE_GIT_REV_OVERRIDE` → embedded in `rushtle --version`.
- `-X github.com/jjo/kubectl-sshuttle/cmd.version=<git-describe>+<sha>` → embedded in `kubectl sshuttle --version`.

**Runtime env vars (read by the binaries themselves):**
- `KUBECTL_SSHUTTLE_CONTEXT`, `KUBECTL_SSHUTTLE_NAMESPACE`, `KUBECTL_SSHUTTLE_NAME` — passed from `connect` to the hidden `ssh-proxy` subcommand (`cmd/sshproxy.go:19-29`).
- `KUBECTL_SSHUTTLE_CHUNK_BYTES`, `KUBECTL_SSHUTTLE_CHUNK_DELAY_US` — opt-in stdin-chunking workaround for kubectl-exec >1KB truncation on tailscale-fronted clusters (`cmd/sshproxy.go:28-30`).
- `RUSHTLE_FRAME_DELAY_US` — runtime per-frame microsecond delay knob in rushtle ssnet (`rushtle/src/ssnet.rs:53-59`).
- `RUSHTLE_BIN` — override path to the local rushtle binary (`cmd/connect.go:166`).
- `SUDO_USER` / `USER` — used to derive the per-user proxy deployment name (`<user>-sshuttle-proxy` or `<user>-rushtle-proxy`) in `cmd/root.go:107-117`.

**Defaults:**
- Image: `xjjo/sshuttle` (default), `xjjo/rushtle` (`--rushtle` / `--rushtle-server`).
- Namespace: `default`.
- Name: `<user>-sshuttle-proxy` (or `-rushtle-proxy` in rushtle modes).
- Listen ports: TCP `12300`, DNS UDP `12353` (`rushtle/src/client.rs:19-20`).

## Platform Requirements

**Development:**
- Go ≥ 1.25 (`go.mod`).
- Rust stable (Cargo) — Rust 1.91 used in CI/containers.
- Docker + `docker buildx` for image and prebuilt-binary builds.
- `goreleaser` v2 for snapshot/release.
- `kubectl` + `kubectl krew` for `make krew-install-local`.
- `sshuttle` (pip) on the client host for default and `--rushtle-server` modes (not for `--rushtle`).
- Local `iptables`/`ip6tables` (Linux) or `pfctl` (macOS) when running `--rushtle` mode (requires root via sudo).

**Production (end users):**
- Linux or macOS, amd64 or arm64 (the 4 krew platforms).
- Kubernetes cluster reachable via `kubectl`; no in-cluster Go client / RBAC manifests — relies on whatever `kubectl` already has.

---

*Stack analysis: 2026-05-07*

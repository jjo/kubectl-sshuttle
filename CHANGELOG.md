# Changelog

## v0.2.6 — 2026-05-07

Fixes a hard failure mode in `--rushtle` mode against burst-kill
apiserver middleware (observed on tailscale-fronted clusters): the
unchunked link-probe burst tore down the entire kubectl-exec
websocket session (`close 1006 abnormal closure`), and `ensure_link`'s
in-pipe retry then hit `Broken pipe` because the pipe was already
dead. Workaround was to manually pre-set `RUSHTLE_FRAME_DELAY_US=2000`
or pass `--chunk-bytes 768`.

### Fixed

- **`rushtle/src/client.rs`**: extracted `link_setup` which wraps the
  child spawn + sync-header read + `ensure_link` probe. On dead-pipe
  failure (heuristic match against `Broken pipe`, `os error 32`, EOF,
  `UnexpectedEof`) it kills the old child, sets `FRAME_DELAY_US =
  fallback_us`, respawns a fresh kubectl-exec session, and probes
  again with chunking applied from the first frame. Healthy clusters
  pay zero cost — the fast-path probe at zero delay is unchanged.
  Burst-kill clusters now auto-recover with a one-time ~3 s startup
  penalty instead of bailing.

## v0.2.5 — 2026-05-06

Re-release of v0.2.4. Fixes the macOS half of the new matrix release
workflow, which failed because `scripts/build-rushtle-prebuilt.sh`
unconditionally invoked `docker buildx` to build the linux/amd64
variant — and macos-latest runners have no Docker preinstalled.

### Fixed

- **`scripts/build-rushtle-prebuilt.sh`**: branched the linux build
  block on `uname -s`. On a darwin host the linux variants are written
  as placeholders (the linux builder job in the workflow uploads the
  real linux/amd64 + linux/arm64 binaries separately). The darwin
  builder job now runs cleanly without needing Docker.

## v0.2.4 — 2026-05-06

Re-release of v0.2.3. Fixes the per-platform rushtle binary in the
release tarballs: v0.2.3 shipped real `rushtle` only for linux/amd64;
linux/arm64 + darwin/{amd64,arm64} got placeholder shell stubs.

### Fixed

- **`.github/workflows/release.yml`**: split into per-platform builder
  jobs that upload artifacts, plus a goreleaser job that downloads
  them. linux/{amd64,arm64} builds on `ubuntu-latest` via buildx
  (qemu for arm64). darwin/{amd64,arm64} builds on `macos-latest` via
  cargo with both `*-apple-darwin` rustup targets installed.
  goreleaser now runs with `--skip=before` so the local prebuild
  script doesn't overwrite the matrix-built binaries with placeholders.

## v0.2.3 — 2026-05-06

Re-release of v0.2.2 with the krew publish path fixed. The v0.2.2
release artifacts are present on GitHub Releases but never made it
into the krew index because the release workflow's krew-release-bot
step failed on a template error (see below). v0.2.3 ships the same
binary changes plus the publish fix.

### Fixed

- **`.krew.yaml`**: replaced `{{- range .Platforms }}` with explicit
  per-platform `addURIAndSha` blocks. krew-release-bot v0.0.50's
  `ReleaseRequest` does not expose a `.Platforms` field, so the loop
  failed at template evaluation with `can't evaluate field Platforms
  in type *source.ReleaseRequest`.
- **`.goreleaser.yml`**: archive `name_template` now embeds the
  leading `v` (`kubectl-sshuttle_v{{ .Version }}_{{ .Os }}_{{ .Arch }}`)
  so the filename matches the URL pattern used by the krew template
  (`{{ .TagName }}` = `v0.2.3`). Without this, the bot's
  `addURIAndSha` would 404 trying to fetch the archive.

## v0.2.2 — 2026-05-06

Hardening pass on `--rushtle` mode based on field testing against
tailscale-fronted apiservers. Adds an automatic link probe so users
no longer need to know about `--chunk-bytes` for the common cases,
plus enough debug surface to diagnose the rest.

### Added

- **Link health probe** at `--rushtle` startup. Sends a 16-frame burst
  of small PINGs immediately after the sync header; if any PONG goes
  missing within 3 s, automatically enables `RUSHTLE_FRAME_DELAY_US`
  and retries. Catches the rate-driven coalescing failure mode where
  back-to-back ssnet frames are merged into a >1 KB websocket message
  and dropped by middleware.
- **`--probe-fallback-us` / `RUSHTLE_FRAME_DELAY_US`** runtime knobs.
  Default 2000 (2 ms inter-frame delay on probe miss). `0` disables
  the probe entirely. The plugin's `--chunk-delay-us` is plumbed
  through as the rushtle-side fallback so the same UX flag works in
  both `--rushtle-server` and `--rushtle` modes.
- **`--version`** for both `kubectl-sshuttle` and `rushtle`. Output
  format `<semver>+<git-short-sha>` (e.g. `0.2.2+ab12cd3`). Go side
  via `-ldflags -X cmd.version=...`; Rust side via `build.rs` running
  `git rev-parse --short HEAD`. Docker images accept `--build-arg
  GIT_REV=` since `.dockerignore` excludes `.git/`.
- **Symmetric `tx`/`rx` debug logging** on both client and server
  writer tasks. Every framed message is logged once at debug level
  with channel, command name, and length — single grep to map any
  symptom to a wire-level cause. See `README.md::Debugging`.
- **`README.md::Debugging` section**: `RUST_LOG=rushtle=debug` recipe,
  log-format reference, symptom→cause table, `--probe-fallback-us 0`
  to disable the probe, manual stale-iptables-chain cleanup recipe.
- **`AGENTS.md`**: operating notes for AI coding agents (architecture,
  conventions, the truncation workaround, Docker traps, hard "never
  do" list, frame-ordering invariants).

### Fixed

- **iptables cleanup `Device or resource busy`**: drain ALL `-D OUTPUT
  -j chain` jumps in a loop (a stale jump from a prior crash plus the
  fresh install left two refs; one `-D` cleared one of them, leaving
  `-X` busy). Retry `-X` once after 100 ms for the nf_tables rule-GC
  race.
- **iptables cleanup multi-second hang**: `-w 5` on every iptables
  call bounds xtables-lock contention with firewalld /
  NetworkManager. Single-call timing logged at `>500ms` for future
  diagnosis.
- **iptables expected-stderr noise**: `iptables_cmd_quiet` swallows
  the harmless `Bad rule (does a matching rule exist in that chain?)`
  emitted by the drain-loop terminator and the `Chain already exists`
  emitted when reusing a chain on `iptables_create_chain`.
- **iptables `-N` failure handling**: treats existing chain as
  non-fatal and reuses (sshuttle parity); previously a stale chain
  from a prior crash would block install forever.
- **`SIGTERM` left iptables chain stranded**: client signal handler
  now catches `SIGTERM` in addition to `SIGINT` so `kill <pid>` /
  systemd-stop runs the cleanup path.
- **Chunked stdin pump (`cmd/sshproxy.go`) silently swallowed write
  errors**: now cancels the kubectl-exec context on write/read
  failure so `cmd.Wait()` unblocks with an exit error sshuttle can
  see, rather than wedging.
- **`--rushtle` / `--rushtle-server` mutex** moved to root command's
  `PersistentPreRunE`. Now applies uniformly to `create` (which
  previously silently picked one mode and proceeded) and
  `connect`.
- **`effectiveName()`** detects explicit `--name` via
  `Flags().Changed("name")` instead of comparing the value against
  the default. Passing the literal default value as `--name` no
  longer flips into the rushtle-suffix branch.
- **macOS `PfiocNatlook` struct size**: was 76 bytes (with `u16`
  ports), `DIOCNATLOOK` ioctl writes 96 bytes — kernel UB. Now uses
  4-byte `pf_state_xport` unions + tail pad to reach 96, with a
  compile-time `assert!(size_of == 96)`.
- **macOS pf direction probe**: matches sshuttle's `pf.py` order —
  `PF_OUT` then fall back to `PF_IN`.

### Changed

- **`rushtle/Dockerfile`** — multiarch via `TARGETARCH` + buildx
  qemu emulation; no hardcoded `x86_64-unknown-linux-musl` triple.
  Stub-build-then-real trick removed (Docker mtime normalization
  caused the stub to be shipped instead of the real binary; same
  trap that `Dockerfile.builder` was already fixed for).
- **`Makefile`** — `KREW_TARBALL` uses recursive (`=`) expansion so
  the `wildcard` is evaluated when the variable is referenced, not
  at parse time (`dist/` doesn't exist until `release-snapshot`
  runs). Also adds `--build-arg GIT_REV=$(GIT_REV)` to the docker
  image targets.
- **`.goreleaser.yml`** — embeds `{{ .Version }}+{{ .ShortCommit }}`
  in the build ldflags so released binaries also report a
  meaningful `--version`.

## v0.2.0 — 2026-04-30

### Added

- **`rushtle`** — sshuttle-compatible TCP/UDP/DNS tunnel in Rust, bundled
  in the krew tarball alongside `kubectl-sshuttle`. Speaks sshuttle's
  `ssnet` wire protocol byte-exact.
- **`--rushtle`** mode: rushtle on both ends. No python, no sshuttle
  bootstrap. Required for tailscale-fronted apiservers that drop
  `kubectl exec` stdin writes >1 KB.
- **`--rushtle-server`** mode: sshuttle locally, rushtle in pod (acts as a
  `python -c` shim that consumes sshuttle's bootstrap then enters ssnet).
- **`--chunk-bytes` / `--chunk-delay-us`** flags: chunk sshuttle's stdin
  into ≤1 KB writes inside `kubectl-sshuttle ssh-proxy`. Workaround for
  the same `kubectl exec` truncation in default/`--rushtle-server` modes.
- **DNS forwarding (`-- --dns`)**: matches sshuttle exactly. Reads
  nameservers from `/etc/resolv.conf` and `/run/systemd/resolve/resolv.conf`,
  installs per-IP UDP/53 REDIRECT rules, leaves `/etc/resolv.conf`
  untouched. systemd-resolved's `127.0.0.53` stub is captured
  automatically.
- **`xjjo/rushtle`** docker image (alpine + rushtle, ~16 MB, runs as
  uid 1001 with no capabilities — no apt/pip at startup).
- **`xjjo/sshuttle`** docker image (python:3.12-slim + sshuttle pre-baked,
  runs as uid 1001 — replaces the legacy `apt-get + pip install`
  startup flow that required root in the pod).
- **Image-name heuristic**: deployment template auto-selects the prebaked
  variant (no install commands, `runAsNonRoot: true`, dropped caps) when
  `--image` matches `*/sshuttle*` or `*/rushtle*`. Other images fall
  back to the legacy install-at-startup flow.
- **macOS scaffold** for rushtle client: `pf` rdr-anchor + `DIOCNATLOOK`
  ioctl on `/dev/pf`. cfg-gated, cargo-checks clean for
  `aarch64-apple-darwin`. Needs hardware verification.
- **Smoke tests** (no kubernetes required): drives `rushtle server` with
  Python harnesses to verify TCP CONNECT/DATA echo, DNS_REQ → upstream,
  UDP_OPEN/UDP_DATA bidi, HOST_REQ → HOST_LIST, `--compat-bootstrap`,
  and `-c PYSCRIPT` python-shim mode.

### Changed

- Default deploy name uses `<user>-rushtle-proxy` (vs.
  `<user>-sshuttle-proxy`) when in either rushtle mode, so the python
  and rust deploys can coexist in the same namespace.
- iptables rule order matches sshuttle's `methods/nat.py`:
  per-nameserver UDP/53 REDIRECT first, then per-subnet TCP REDIRECT,
  then `addrtype LOCAL -j RETURN` at the chain tail.
- Image pull policy auto-detected: `Always` for `:latest`/untagged,
  `IfNotPresent` for pinned tags or digest references.
- rushtle binary self-`sudo`s its own iptables calls (matches sshuttle's
  UX — no need to wrap `kubectl-sshuttle` under sudo).
- Default rushtle log level: `info` (was `warn`); use `-v`/`-vv` for
  more.

### Fixed

- `kubectl-sshuttle ssh-proxy` no longer hardcodes `python3` as the
  remote interpreter. `--python=` is propagated based on mode so the
  rushtle binary can stand in.
- `defaultDeployName()` prefers `SUDO_USER` over `USER` so a sudo'd
  invocation targets the same deploy as a plain run.
- Synchronization header (`\0\0SSHUTTLE0001`) emitted by rushtle server
  matches sshuttle's protocol exactly.
- Server emits an empty `CMD_ROUTES` frame after the initial PING so
  sshuttle's client unblocks `fw.start()` and installs iptables.

### Build / release

- `Dockerfile.builder` for cross-arch rushtle builds via Docker buildx.
- `.goreleaser.yml`: `archives.files` template embeds per-platform
  rushtle binary into each tarball.
- `.krew.yaml`: lists `rushtle` for all 4 platforms.
- `make krew-install-local`: builds + installs the snapshot tarball via
  `kubectl krew install --manifest --archive` for end-to-end testing
  before tagging.

## v0.1.0 — 2026-04-12

Initial release. Basic `create` / `connect` / `delete` flow with
python+sshuttle on both ends.

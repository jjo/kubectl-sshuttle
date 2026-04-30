# Changelog

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

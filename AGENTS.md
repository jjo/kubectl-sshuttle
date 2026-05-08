# AGENTS.md — Operating Guide for AI Coding Agents

Working notes distilled from prior development sessions on this repo. Read
this **before** touching code; it captures non-obvious context that's hard
to derive from `grep` alone.

---

## What this project is

`kubectl-sshuttle` is a kubectl plugin that creates a transient proxy pod
in a Kubernetes cluster and tunnels traffic through it via either:

1. **`sshuttle`** (Python) on both ends — default, legacy.
2. **`rushtle`** (Rust port, in `rushtle/`) on either end:
   - `--rushtle-server`: sshuttle local + rushtle in pod.
   - `--rushtle`: rushtle on both ends, no Python anywhere.

The Rust port exists primarily to work around a real-world bug:
**tailscale-fronted apiservers truncate kubectl-exec stdin writes above
~1 KB**. Python sshuttle's bootstrap is ~80 KB; that bootstrap silently
truncates and the tunnel never establishes. rushtle ships as a static
musl binary — no bootstrap — so the truncation window only matters for
runtime frame writes, which are capped at 768 bytes (`ssnet::CHUNK`).

---

## Architecture in one diagram

```
                                local                        in-pod
  ┌────────┐  CIDR redirect  ┌──────────┐   kubectl exec   ┌──────────┐
  │ apps   │ ──── iptables ─▶│ rushtle  │ ──── stdio ─────▶│ rushtle  │
  │ /libc  │      pf rules   │  client  │   (ssnet frames) │  server  │
  │ resolv │                 └──────────┘                  └──────────┘
  └────────┘                                                     │
                                                                 ▼
                                                     real cluster targets
```

`ssnet` is sshuttle's wire protocol (8-byte header + ≤64 KB payload).
Rushtle implements the subset that matters for kubectl tunneling. Frames
are **sequential** — there is one stdin reader on each side, so frames
arrive in send order. Don't write code that assumes out-of-order delivery.

---

## Three modes — file-level effects

| Mode | `cfg.Rushtle` | `cfg.RushtleServer` | Local | Pod | Default deploy name |
|------|---------------|---------------------|-------|-----|---------------------|
| default | `false` | `false` | sshuttle | python+sshuttle | `<user>-sshuttle-proxy` |
| rushtle-server | `false` | `true` | sshuttle | rushtle (`-c` shim) | `<user>-rushtle-proxy` |
| rushtle | `true` | `false` | rushtle | rushtle | `<user>-rushtle-proxy` |

User prefix is `SUDO_USER` if set, else `USER` — `--rushtle` requires
root for iptables, so `sudo -E kubectl sshuttle --rushtle …` is the
common invocation. The two name conventions exist so a user can run both
modes against the same namespace without collision.

`--name` override detection: use `rootCmd.PersistentFlags().Changed("name")`,
**not** string-compare against the default. A user passing the literal
default value as `--name` should still be honored as an explicit override.

---

## The >1 KB truncation workaround

This bug is the reason for half the complexity in this repo. It manifests
on tailscale-fronted apiservers (and possibly other middleware): single
writes >1 KB into kubectl-exec stdin get silently dropped.

Two parallel mitigations:

1. **`sshuttle` path** (`cmd/sshproxy.go`). When `--chunk-bytes N` is
   passed, `runKubectlChunked` spawns kubectl as a child and pumps
   stdin in N-byte chunks with inter-chunk sleep. Triggered by env vars
   `KUBECTL_SSHUTTLE_CHUNK_BYTES` and `KUBECTL_SSHUTTLE_CHUNK_DELAY_US`
   set by `runSshuttle` when the user passes the flag.

2. **`rushtle` path** (`rushtle/src/ssnet.rs`). Frames are already
   capped at 776 bytes total (`HDR_LEN + CHUNK = 8 + 768`). Inter-frame
   sleep is controlled by `RUSHTLE_FRAME_DELAY_US` env. `runRushtle` in
   `cmd/connect.go` propagates `cfg.ChunkDelayUS` into that env var when
   `cfg.ChunkBytes > 0` so the same UX flag works in both modes.

When debugging hangs in either mode, look for these knobs first.

---

## Code conventions

### Frames are ordered

There is one reader per side. `CMD_TCP_DATA` cannot arrive after
`CMD_TCP_EOF` on the same channel. Don't write defensive code for
out-of-order delivery — it adds complexity without value.

### Half-close is mostly absent

Removing a channel from `tcp_chans` (server) / `channels` (client) on
`CMD_TCP_EOF` drops the local writer half — that's the intended
shutdown(WR). The reader task is independent and ends on local
read-EOF/error. **Do not** add separate logic for `STOP_SENDING` vs
`EOF` unless you've traced sshuttle's own behavior; both arms intentionally
collapse to the same path here.

### `tcp_chans.lock().await` scope

Keep the lock scope tiny. Holding the channel-map lock across an await
that touches the network is a gateway to deadlocks under load.

### iptables chain management

`firewall::install` calls `iptables_create_chain` which **treats
"chain already exists" as non-fatal** — sshuttle parity. A prior crash
or kill -9 leaves a stale chain; we reuse it.

`firewall::remove` (per-backend `remove_one`):

```
loop iptables -D OUTPUT -j chain   # drain ALL jumps (a duplicate from
                                   # a prior install can leave a second
                                   # reference, then -X fails busy)
iptables -F chain
iptables -X chain                  # retry once with 100ms sleep on
                                   # failure — nf_tables backend
                                   # sometimes races with rule GC
```

Never delete a chain without first draining `-D` to empty.

### macOS pf

`PfiocNatlook` is **exactly 96 bytes** — `DIOCNATLOOK` ioctl encodes
that size in its number (0xc0604417, 0x60=96). The struct uses 4-byte
`pf_state_xport` unions (port in first 2 bytes, network order) plus a
12-byte tail pad. There's a compile-time assert; don't disable it.

Probe direction `PF_OUT` first, then fall back to `PF_IN` (sshuttle's
order on darwin).

### Cobra version flag

`var version = "dev"` in `cmd/root.go`, injected at link time via
`-ldflags "-X github.com/jjo/kubectl-sshuttle/cmd.version=…"`. Set on
`rootCmd.Version` so `--version` works automatically. Goreleaser uses
`{{ .Version }}+{{ .ShortCommit }}` template.

### Rust version flag

`build.rs` honors `RUSHTLE_GIT_REV_OVERRIDE` env first, then
`git rev-parse --short HEAD`, then falls back to `"unknown"`. Docker
builds pass the host's git sha via `--build-arg GIT_REV=…` because
`.dockerignore` excludes `.git/`. Clap version is built with
`concat!(env!("CARGO_PKG_VERSION"), "+", env!("RUSHTLE_GIT_REV"))`.

---

## Docker traps

These are real bugs that already shipped once each. Read carefully.

### The "stub build first" anti-pattern

```dockerfile
# DO NOT DO THIS — Docker normalizes COPY mtimes, so cargo's incremental
# check sees the stub-built target as fresh and skips the real rebuild.
RUN mkdir -p src && echo "fn main(){}" > src/main.rs && \
    cargo build --release || true
RUN rm -rf src
COPY src ./src
RUN cargo build --release          # ← actually does nothing, ships stub
```

Symptom: 330 KB binary that exits 0 silently with no output. Use a
**single** `COPY src ./src` + **single** `cargo build`.

### Multiarch via qemu, not cross-linker

`Dockerfile` does **not** pin `--platform=$BUILDPLATFORM`. Buildx runs
the builder under qemu for the target platform, so cargo compiles
natively without a cross-linker. Hardcoding `x86_64-unknown-linux-musl`
breaks `linux/arm64` builds silently — use `TARGETARCH` and let cargo
default to the host triple.

### `.git` not in build context

`build.rs` runs `git rev-parse` but `.dockerignore` excludes `.git/`.
Without the `RUSHTLE_GIT_REV_OVERRIDE` arg, you get `unknown`. Always
pass `--build-arg GIT_REV=$(git rev-parse --short HEAD)` (the Makefile
target does this).

### `rust:1.X-alpine3.Y` is a floating tag

`rust:1.91-alpine3.20` will pull a different patch release after the
next 1.91.x. Pin a full semver or digest before tagging a release.

---

## Build & test

```bash
make build                       # go binary with version baked in
make test                        # go tests (proxy/, cmd/)
make rushtle                     # native rushtle binary
make rushtle-test                # smoketest scripts under scripts/
make rushtle-image               # docker build with GIT_REV baked in
make rushtle-image-multiarch     # buildx multi-arch (linux amd64+arm64)
make release-snapshot            # goreleaser dry-run
make krew-install-local          # install snapshot via krew
```

`make test` and `cargo test` both run; verify both before claiming
"done". `cargo clippy --release -- -D warnings` should also pass.

### Smoketests

`scripts/rushtle-*-smoketest.py` drive `rushtle server` with hand-
crafted ssnet frames. They cover:

- `rushtle-smoketest.py` — TCP CONNECT + DATA echo
- `rushtle-dns-smoketest.py` — DNS_REQ resolution
- `rushtle-udp-smoketest.py` — UDP open/data/close
- `rushtle-host-smoketest.py` — HOST_REQ → HOST_LIST
- `rushtle-bootstrap-smoketest.py` — sshuttle assembler bootstrap consume
- `rushtle-shim-smoketest.py` — `rushtle -c PYSCRIPT` shim path

Known fragility: scripts call `proc.stderr.read()` after `proc.terminate()`,
which can deadlock if stderr fills the pipe buffer. If you see hangs,
drain stderr concurrently.

---

## Reviewing prior agent output — verify, don't trust

Past sessions surfaced reviewer findings that turned out to be wrong
because they assumed weaker frame ordering than the protocol provides.
Specifically:

- "TCP half-close drops in-flight data" — false. Frames are sequential
  on stdin; `CMD_TCP_DATA` cannot arrive after `CMD_TCP_EOF` on the
  same channel.
- "`Err` arm doesn't emit `CMD_TCP_EOF`" — false. The `Err` arm uses
  `break`, falling through to the post-loop `send(CMD_TCP_EOF)`.

Before applying any reviewer suggestion to this repo:

1. Re-read the actual code path top to bottom.
2. Trace the frame for a concrete scenario.
3. Check sshuttle's own implementation for parity intent.

A reviewer's confidence rating is not a substitute for verification.

---

## Things to never do

- **Never** introduce out-of-order frame handling — the protocol is
  ordered.
- **Never** delete an iptables chain without draining all `-D OUTPUT`
  jumps first.
- **Never** assume `cfg.Name != defaultDeployName()` means the user
  didn't pass `--name` — use `Flags().Changed`.
- **Never** add `--platform=$BUILDPLATFORM` to the rushtle Dockerfile
  builder stage without also bundling a cross-linker.
- **Never** ship a Dockerfile that does "stub build then real build"
  in two `COPY src` stages.
- **Never** silently drop errors in the chunked stdin pump
  (`cmd/sshproxy.go:runKubectlChunked`) — cancel the context so kubectl
  dies and `cmd.Wait()` unblocks with a real exit.

---

## Commit style

Conventional Commits. Imperative subject, ≤72 chars. Body explains the
**why**, not the what. When fixing review findings, group by severity
(Critical / Important). When a fix has user-visible behavior, include a
sample CLI output in the body.

Co-author trailer for AI assistance:

```
Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
```

---

## Useful pointers

- Wire protocol reference: `sshuttle/ssnet.py` upstream + `rushtle/src/ssnet.rs`.
- Server bootstrap protocol: `sshuttle/assembler.py` (consumed in
  `rushtle/src/server.rs::consume_sshuttle_bootstrap`).
- The 35-second "unexpected EOF" symptom in interactive sessions is
  usually apiserver idle timeout, not a chunking bug. Confirm with
  debug logs (`-vv`) before reaching for `--chunk-bytes`.

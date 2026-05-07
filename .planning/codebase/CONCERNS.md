# Codebase Concerns

**Analysis Date:** 2026-05-07

## Tech Debt

**Tailscale-fronted apiserver stdin truncation:**
- Issue: When the Kubernetes apiserver is fronted by Tailscale (or other middleware that coalesces small writes), writes >1KB into a `kubectl exec` stdin can be silently dropped. Real failure mode is rate-driven coalescing — middleware merges adjacent small writes and truncates the resulting buffer — not a single-write size limit.
- Files: `cmd/sshproxy.go` (`runKubectlChunked`), `rushtle/src/ssnet.rs` (`FRAME_DELAY`), `rushtle/src/client.rs` / `cmd/connect.go` (link probe + auto-fallback)
- Impact: Drives roughly half the codebase complexity. Three independent mitigations are required: (1) Go-side `runKubectlChunked` chunks writes into kubectl-exec stdin; (2) rushtle ssnet inserts a `FRAME_DELAY` between frames to defeat coalescing; (3) startup link probe auto-falls back to chunked/sshuttle path when the link looks lossy.
- Fix approach: Long-term, prefer a streaming transport that does not depend on `kubectl exec` stdin (e.g. portforward or a sidecar TCP proxy). Short-term, keep all three mitigations layered; do not remove any without re-running probes against a Tailscale-fronted cluster.
- Production blocker: No — mitigations are in place and shipping.

**Server-side debug logs require code edit:**
- Issue: There is no runtime flag to enable verbose logging on the server (in-cluster) side of rushtle. README documents recompiling rushtle to get server-side debug output.
- Files: `cmd/connect.go` (`kubectlExecRushtleServer`), `rushtle/src/server.rs`, `rushtle/src/main.rs`, `README.md`
- Impact: Field debugging of in-cluster behavior requires a custom build and re-pushing the image — slow loop.
- Fix approach: Add a `--remote-verbose` flag on the client side that is plumbed through `kubectlExecRushtleServer` into the `rushtle` server invocation as `-v`/`RUST_LOG` or equivalent.
- Production blocker: No — operational annoyance only.

**TCP_STOP_SENDING handling weak:**
- Issue: Both EOF and `STOP_SENDING` collapse to channel removal. The reader task on the server side keeps generating `TCP_DATA` frames after a `STOP_SENDING` because it only checks for local socket close, not the channel-removed signal.
- Files: `rushtle/src/ssnet.rs`, `rushtle/src/server.rs`
- Impact: Wasted bytes on the wire after the peer has stopped reading. sshuttle treats EOF and STOP_SENDING similarly, so behavior matches the reference implementation.
- Fix approach: Track a `stopped_sending` flag per channel and short-circuit reader emission. Future cleanup.
- Production blocker: No — minor inefficiency.

## Known Bugs

**No backoff/retry on kubectl-exec session death:**
- Symptoms: When the apiserver (or middleware) kills the underlying `kubectl exec` session — typically on idle timeout around 30s — rushtle exits and the user must re-invoke `kubectl sshuttle`.
- Files: `cmd/connect.go`, `cmd/sshproxy.go`, `rushtle/src/client.rs`
- Trigger: Idle TCP/UDP traffic for ~30s on a Tailscale-fronted cluster, or any apiserver restart/upgrade during a session.
- Workaround: Re-run `kubectl sshuttle ...`. No reconnect logic exists.
- Production blocker: No — but degrades long-running session UX.

**iptables stale chain on hard kill:**
- Symptoms: On `kill -9` or hard panic, the iptables NAT chain installed by rushtle is left behind. Subsequent runs may fail or networking is partially blackholed.
- Files: `rushtle/src/firewall/linux.rs`, `rushtle/src/main.rs` (`cleanup_sig`), `README.md` (manual recovery instructions)
- Trigger: SIGKILL, OOM, or panic before the signal handler runs.
- Workaround: Manual `iptables -t nat -F <chain> && iptables -t nat -X <chain>` per README.
- Production blocker: No — recoverable, documented.

## Security Considerations

**`--rushtle` requires root for iptables:**
- Risk: User must wrap the command in `sudo -E kubectl sshuttle --rushtle ...`. The rushtle binary cannot drop privileges meaningfully because the redirect listening sockets and signal handlers also need to manipulate iptables on shutdown.
- Files: `cmd/connect.go`, `rushtle/src/firewall/linux.rs`, `rushtle/src/main.rs`
- Current mitigation: `SUDO_USER` is preserved so default deploy/namespace defaults still resolve to the invoking user. rushtle's internal `iptables_cmd` self-`sudo`s for sshuttle parity.
- Recommendations: Document the privilege model clearly; consider a setuid helper or a Linux capability bounding set (`CAP_NET_ADMIN` only) in a future revision. Not currently planned.
- Production blocker: No — same posture as upstream sshuttle.

## Performance Bottlenecks

**Probe takes ~3s on healthy clusters:**
- Problem: The link-quality probe at startup adds ~3s of latency before traffic flows.
- Files: `cmd/connect.go`, `rushtle/src/client.rs` (`PROBE_TIMEOUT`, `PROBE_BURST_FRAMES`)
- Cause: Probe sends a burst of frames and waits for echo within `PROBE_TIMEOUT` to decide whether to auto-fall back to chunked/sshuttle mode.
- Improvement path: `--probe-fallback-us 0` disables probing entirely (loses auto-fallback). Lowering `PROBE_TIMEOUT` and tightening `PROBE_BURST_FRAMES` would cut latency but raises the false-positive fallback rate on healthy-but-jittery links.
- Production blocker: No — startup-only cost.

**iptables cleanup synchronous in signal handler:**
- Problem: `cleanup_sig()` calls `firewall::remove`, which uses blocking `std::process::Command::status` inside the tokio signal task. Cleanup itself can take ~5s under contention.
- Files: `rushtle/src/main.rs` (`cleanup_sig`), `rushtle/src/firewall/linux.rs`
- Cause: Blocking `iptables` invocations inside an async signal handler. With the multi-threaded tokio runtime other tasks are unaffected, and per-call wait is now bounded by `iptables -w 5`.
- Improvement path: Move cleanup to `tokio::task::spawn_blocking`, or switch to a non-blocking netfilter API (`rustables`). Bounded and tolerable today.
- Production blocker: No.

## Fragile Areas

**macOS pf path is untested on hardware:**
- Files: `rushtle/src/firewall/macos.rs`
- Why fragile: `PfiocNatlook` is defined as a 96-byte struct with a compile-time `assert_eq!(size_of::<PfiocNatlook>(), 96)` — but the actual `ioctl` against `/dev/pf` has not been verified on real macOS Sequoia hardware. Skeleton only; `cargo check --target aarch64-apple-darwin` is clean.
- Safe modification: Do not refactor without a macOS test rig. Treat this file as unverified scaffolding; any change should be accompanied by a real-device run.
- Test coverage: None (no CI for `aarch64-apple-darwin` runtime; only `cargo check`).
- Production blocker: Yes for macOS users — feature should be advertised as experimental until validated on hardware.

## Scaling Limits

**DNS uses UDP only, no TCP fallback:**
- Current capacity: Only UDP/53 is redirected through the tunnel.
- Limit: DNS responses that exceed 512 bytes (or signal `TC=1`) and require a TCP retry to port 53 are not tunneled. Resolvers will either fail or bypass the tunnel.
- Scaling path: Add TCP/53 redirection alongside UDP/53 in `rushtle/src/firewall/linux.rs` and a TCP listener in the rushtle client. Only matters for unusual queries (large TXT, DNSSEC, some SRV).
- Production blocker: No — edge case for typical Kubernetes service discovery.

## Dependencies at Risk

**Linux release binaries are glibc, not static musl:**
- Risk: `scripts/build-rushtle-prebuilt.sh` (`build_linux_native_amd64`) uses `Dockerfile.builder` which is `rust:slim-bookworm` (Debian glibc). The resulting Linux release binaries shipped via krew tarballs are dynamically linked against `/lib64/ld-linux-x86-64.so.2`.
- Files: `scripts/build-rushtle-prebuilt.sh`, `rushtle/Dockerfile.builder`, `rushtle/Dockerfile` (runtime image is alpine+musl static — used only inside the cluster)
- Impact: Krew install on minimal Alpine hosts (or musl-only environments) will fail at exec time with a missing dynamic linker. Most distros are fine.
- Migration plan: Add an `x86_64-unknown-linux-musl` (and `aarch64-unknown-linux-musl`) target build path in `build-rushtle-prebuilt.sh`, ship those in the krew tarball, and document the change in `CHANGELOG.md`.
- Production blocker: Partial — blocks Alpine/musl users; OK on glibc distros.

## Missing Critical Features

**Krew tarball size:**
- Problem: Each krew tarball bundles per-platform `rushtle` (~1–2 MB stripped) plus the `kubectl-sshuttle` Go binary. Total ~5–10 MB per arch.
- Files: `scripts/build-rushtle-prebuilt.sh`, `prebuilt/`, `krew-index-plugins-sshuttle.yaml`
- Blocks: Slower `kubectl krew install` on metered/slow networks; larger storage footprint per supported arch.
- Production blocker: No — within typical krew plugin range.

## Test Coverage Gaps

**Rust unit tests absent:**
- What's not tested: ssnet protocol parsing, frame encoding/decoding, `parse_connect`, `parse_udp_data`. No `#[cfg(test)]` modules in the rushtle crate.
- Files: `rushtle/src/ssnet.rs`, `rushtle/src/client.rs`, `rushtle/src/server.rs`, `rushtle/src/firewall/*.rs`
- Risk: Wire-format regressions and parser bugs are caught only by Python smoketests at the integration level (`scripts/rushtle-*-smoketest.py` invoked from `Makefile`). Edge cases (truncated frames, oversized payloads, malformed CONNECT) are not exercised.
- Priority: High — this is the protocol core and benefits most from cheap unit tests.

**macOS firewall path untested end-to-end:**
- What's not tested: `PfiocNatlook` ioctl behavior, redirect rule installation, cleanup on signal — see "macOS pf path is untested on hardware" above.
- Files: `rushtle/src/firewall/macos.rs`
- Risk: Silent failure or system-level pf state corruption on real macOS.
- Priority: High before advertising macOS support as stable.

---

*Concerns audit: 2026-05-07*

# External Integrations

**Analysis Date:** 2026-05-07

This project sits between the user's host (kernel networking + DNS resolver) and a Kubernetes pod's stdin/stdout streams. It does not link any cloud-provider SDKs, ORMs, or HTTP clients — every external surface is either a syscall, a child process, or a wire protocol. This document enumerates each.

## Kubernetes apiserver

**Mechanism:** child-process `kubectl` invocation, never the Go client-go library.

**Entry points:**
- `cmd/sshproxy.go:51-68` — the hidden `ssh-proxy` subcommand resolves `kubectl` via `exec.LookPath("kubectl")` and either `syscall.Exec`s it (default) or fork+pipes through `runKubectlChunked` for the chunked-stdin workaround.
- `cmd/connect.go:147-160` (`kubectlExecRushtleServer`) — builds the `kubectl ... exec -i deploy/<name> -- rushtle server` shell string used as the `--rushtle` mode remote command (passed through `sh -c`).
- `cmd/create.go:33-39` — `kubectl apply -f -` with the rendered Deployment YAML on stdin.
- `cmd/root.go:128-148` — `kubectlArgs` / `runKubectl` helpers prepend `--context` and `-n <namespace>` to every call.

**Wire shape:** `kubectl exec -i deploy/<deployment-name> -- <remote-cmd>`, with the local sshuttle/rushtle client driving stdin (frames out) and reading stdout (frames in). The deployment selector is `app: <name>` (`proxy/deployment.go:80-87`); kubectl resolves this to a single pod and proxies the exec stream over the apiserver's WebSocket/SPDY channel.

**Known constraint — kubectl-exec >1KB stdin truncation:** Some apiserver fronts (notably tailscale-fronted clusters) drop single websocket writes larger than ~1KB. Two mitigations:
- **Go side** (`cmd/sshproxy.go:82-...`, `runKubectlChunked`) — fork kubectl as a child and write its stdin in `chunkBytes`-sized chunks separated by `chunkDelayUS` microseconds, so each chunk becomes its own websocket frame.
- **Rust side** — frames are capped at `CHUNK = 768` bytes (`rushtle/src/ssnet.rs:37`) and an inter-frame sleep `RUSHTLE_FRAME_DELAY_US` keeps adjacent frames from being coalesced into a >1KB websocket message.

## sshuttle ssnet wire protocol

**Compatibility target:** byte-exact with upstream `sshuttle/ssnet.py` and `sshuttle/server.py`. This is the protocol spoken between the local sshuttle/rushtle client and the in-pod rushtle server.

**Frame format** (`rushtle/src/ssnet.rs:1-31`):
```
magic     b"SS"      (2 bytes)
channel   u16 BE     (2 bytes)
cmd       u16 BE     (2 bytes)
datalen   u16 BE     (2 bytes)
payload   datalen bytes
```
Header struct equivalent to upstream `HDR_FMT = '!ccHHH'`, `HDR_LEN = 8`.

**Implemented commands** (`rushtle/src/ssnet.rs:12-24`):
- `CMD_TCP_CONNECT` — payload `b"family,dstip,dstport"` ASCII (family = libc socket family int).
- `CMD_TCP_DATA` / `CMD_TCP_EOF` / `CMD_TCP_CLOSE` — TCP byte stream.
- `CMD_UDP_OPEN` / `CMD_UDP_DATA` / `CMD_UDP_CLOSE` — UDP forwarding (DNS uses these).
- `CMD_HOST_REQ` / `CMD_HOST_LIST` — seed-hostname propagation.
- `CMD_ROUTES` — newline-joined `family,ip,width` lines.
- `CMD_DNS_REQ` / `CMD_DNS_RESPONSE` — raw DNS payloads.
- `CMD_PING` / `CMD_PONG` — used by the startup link probe (rushtle-only extension, ignored by upstream).

**Bootstrap compatibility — sshuttle's assembler.py:** Stock sshuttle prepends a Python-source bootstrap to the remote stream (a copy of `assembler.py`, then a stdin.read(N) hand-off to zlib-compressed module records). When the local side is unmodified `sshuttle` and the remote is `rushtle server`, rushtle has to consume those leading bytes before entering ssnet mode:
- `rushtle server --compat-bootstrap` — explicit flag (`rushtle/src/main.rs:31-44`).
- `rushtle -c <PYSCRIPT>` — the python-shim path. When sshuttle invokes the remote with `--python=/usr/local/bin/rushtle`, the `-c` / `<pyscript>` argv shape lands in rushtle's `main.rs:128-147`, which parses `stdin.read(N)` out of the pyscript to learn how many bootstrap bytes to drain, then enters `server::run(true, n)`.
- The local-side wiring is in `cmd/connect.go:60-99` (`runSshuttle`), which calls `sshuttle ... --python=/usr/local/bin/rushtle` for `--rushtle-server` mode and `--python=python3` for default mode.

## NAT / packet redirect (Linux client)

**Mechanism:** `iptables` and `ip6tables` NAT table REDIRECT rules into local-bound rushtle listeners.

**File:** `rushtle/src/firewall/linux.rs`.

**Operations:**
- `iptables_create_chain` / `iptables_cmd` (`linux.rs:96-167`) — invoke iptables/ip6tables directly, prefixing `sudo -n -p '[local sudo] Password: '` when not root, with `-w 5` to bound xtables-lock waits.
- `install` (`linux.rs:169+`) creates a per-process chain following sshuttle/methods/nat.py rule order:
  1. Per-nameserver `udp --dport 53 -j REDIRECT --to-ports <dns_listen_port>` rules (only when `--dns`).
  2. Per-subnet `tcp -d <cidr> -j REDIRECT --to-ports <listen_port>`.
  3. Trailing `-m addrtype --src-type LOCAL -j RETURN` escape.
- Original destination recovered via `getsockopt(SOL_IP, SO_ORIGINAL_DST)` (IPv4) or `getsockopt(IPPROTO_IPV6, IP6T_SO_ORIGINAL_DST=80)` (IPv6) — `linux.rs:48-94`. `IP6T_SO_ORIGINAL_DST` is hard-coded since `libc` doesn't expose it everywhere.

## NAT / packet redirect (macOS client)

**Mechanism:** PacketFilter (`pf`) `rdr-anchor` + `DIOCNATLOOK` ioctl on `/dev/pf`.

**File:** `rushtle/src/firewall/macos.rs`. Mirrors `sshuttle/methods/pf.py` (referenced inline at `macos.rs:13-17`).

**Operations:**
1. Build pf rules (`rdr pass on lo0 inet proto tcp from any to <CIDR> -> 127.0.0.1 port <listen_port>` plus a pass-out rule).
2. Load via `pfctl -a com.rushtle/<pid> -f -` into a per-pid anchor.
3. `pfctl -E` to enable pf and capture the disable-token for cleanup.
4. On accept(), recover original dst with the `DIOCNATLOOK` ioctl against `/dev/pf` (`PF_DEV = "/dev/pf"`, `DIOCNATLOOK = 0xc0604417` — `macos.rs:28-43`). The `pfioc_natlook` struct layout is hand-rolled to match darwin's pfvar.h (96 bytes, `static_assert` at `macos.rs:90`).

**Status:** code path is in-tree and wired through `firewall/mod.rs`, but the file's own header comment (`macos.rs:18`) flags it as "untested on hardware. Skeleton ready for iteration on macOS."

## DNS

**Local resolver discovery** (`rushtle/src/firewall/linux.rs:15-42`, `read_local_nameservers_v4`):
- Reads `/etc/resolv.conf` AND `/run/systemd/resolve/resolv.conf` (when present), parsing only `nameserver <ipv4>` lines.
- Reading both is intentional — on systemd-resolved hosts `/etc/resolv.conf` typically lists only `127.0.0.53` while the real upstreams sit in the systemd file. Capturing both means libc-resolver traffic AND systemd-resolved's own outbound queries hit the redirect.
- IPv6 nameservers are deliberately ignored (no ip6tables NAT for v6 DNS yet).

**Outbound redirect:** Per-nameserver UDP/53 → local listener on `127.0.0.1:<dns_listen_port>` (default `12353`, `rushtle/src/client.rs:20`). The local listener UDP-forwards each query as a `CMD_DNS_REQ` ssnet frame to the in-pod server, which resolves it against the cluster's DNS (CoreDNS in typical setups) and replies via `CMD_DNS_RESPONSE`.

**Fallback:** if no nameservers parse, install a catch-all `udp --dport 53 -j REDIRECT` rule (`rushtle/src/firewall/linux.rs:188-196`).

## Krew plugin distribution

**Index:** [`kubernetes-sigs/krew-index`](https://github.com/kubernetes-sigs/krew-index). Plugin name: `sshuttle`.

**PR automation:** `rajatjindal/krew-release-bot@v0.0.50` runs as the final step of the release workflow (`.github/workflows/release.yml:109`). It reads `.krew.yaml`, expands `addURIAndSha` against the just-published GitHub Release tarballs, and opens a PR against `kubernetes-sigs/krew-index` adding/updating `plugins/sshuttle.yaml`.

**Manifest structure:** `.krew.yaml` declares 4 platforms (linux/{amd64,arm64}, darwin/{amd64,arm64}); each entry lists `kubectl-sshuttle`, `LICENSE`, and `rushtle` as the files extracted from the tarball. `bin: kubectl-sshuttle` is what krew symlinks into the user's `~/.krew/bin/`.

## Container registries

**Docker Hub — `xjjo/rushtle`:**
- Single-arch push: `make rushtle-push` (`Makefile:53-55`).
- Multi-arch (`linux/amd64,linux/arm64`) push: `make rushtle-image-multiarch` (`Makefile:72-78`).
- Pulled by the proxy Deployment when `--rushtle` or `--rushtle-server` is used (default `--rushtle-image=xjjo/rushtle`, `cmd/root.go:82`).

**Docker Hub — `xjjo/sshuttle`:**
- Push: `make sshuttle-push` (`Makefile:63-64`).
- Pulled by the proxy Deployment in default sshuttle mode (`--image=xjjo/sshuttle`, `cmd/root.go:78`).
- Image name discriminates the deployment template: `proxy/deployment.go:36-49` (`isPrebakedImage`) checks for the substrings `sshuttle`/`rushtle` in the image basename and switches between the prebaked template (non-root, no apt/pip startup) and the legacy `python:3.12` template that runs `apt-get install openssh-client && pip install sshuttle` at pod startup (`proxy/deployment.go:140-147`).

**Combined push:** `make images-push` (`Makefile:67`).

**No registry credentials in repo** — pushes assume the user is already `docker login`ed.

## GitHub Releases

**Trigger:** annotated tag `v*.*.*` push (`.github/workflows/release.yml:4-5`).

**Per release:** 4 archive tarballs + checksum file:
- `kubectl-sshuttle_v<version>_linux_amd64.tar.gz`
- `kubectl-sshuttle_v<version>_linux_arm64.tar.gz`
- `kubectl-sshuttle_v<version>_darwin_amd64.tar.gz`
- `kubectl-sshuttle_v<version>_darwin_arm64.tar.gz`
- `checksums.txt`

**Each tarball contains:**
- `kubectl-sshuttle` — Go binary (the krew-installed `bin:`).
- `rushtle` — Rust binary for the same OS/arch (built natively per platform in CI; static glibc via `Dockerfile.builder` for linux, native cargo via `dtolnay/rust-toolchain` for darwin).
- `LICENSE`.

**Auth:** `GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}` consumed by goreleaser-action (`.github/workflows/release.yml:108`).

## Authentication

- **Cluster auth:** delegated entirely to `kubectl` (whatever auth the user already has — exec plugin, kubeconfig token, OIDC, etc.). This codebase never reads kubeconfig directly.
- **Local sudo:** rushtle's iptables/pf installer prefixes `sudo -n -p '[local sudo] Password: '` when EUID != 0 (`rushtle/src/firewall/linux.rs:112-121`).
- **No third-party auth** — no OAuth, no API keys, no service-account JSON files.

## Monitoring & Observability

- **Logs:** Rust side uses `tracing` to stderr, level controlled by `-v/-vv` or `RUST_LOG`. Go side uses `fmt.Fprintf(os.Stderr, ...)` directly.
- **Metrics / tracing exporters:** none.
- **Error tracking:** none (no Sentry / Honeycomb / etc.).

## Webhooks & Callbacks

- **Incoming:** none — no HTTP server in this project.
- **Outgoing:** none — `krew-release-bot` opens a GitHub PR but that's a one-shot CI step, not a runtime webhook.

## Environment Variables (External-Surface)

| Name | Consumer | Purpose |
|------|----------|---------|
| `KUBECTL_SSHUTTLE_CONTEXT` | `cmd/sshproxy.go` | Pass kubectl context across the connect→sshuttle→ssh-proxy fork chain |
| `KUBECTL_SSHUTTLE_NAMESPACE` | `cmd/sshproxy.go` | Same, for namespace |
| `KUBECTL_SSHUTTLE_NAME` | `cmd/sshproxy.go` | Same, for deployment name |
| `KUBECTL_SSHUTTLE_CHUNK_BYTES` | `cmd/sshproxy.go:28` | Chunked-stdin workaround size (e.g. 768) |
| `KUBECTL_SSHUTTLE_CHUNK_DELAY_US` | `cmd/sshproxy.go:29` | Inter-chunk sleep in microseconds |
| `RUSHTLE_FRAME_DELAY_US` | `rushtle/src/ssnet.rs:53-59` | Per-frame microsecond sleep on the rust side |
| `RUSHTLE_BIN` | `cmd/connect.go:166` | Override path to local rushtle binary |
| `RUSHTLE_GIT_REV_OVERRIDE` | `rushtle/build.rs` | Inject git sha at compile time when `.git` is unavailable |
| `BUILD_ALL_LINUX` | `scripts/build-rushtle-prebuilt.sh` | Also build linux/arm64 locally (via QEMU) |
| `GIT_REV` | `Makefile`, `scripts/build-rushtle-prebuilt.sh` | Short git sha for image/binary version stamping |
| `SUDO_USER` / `USER` | `cmd/root.go:107-117` | Derive per-user proxy deployment name |

---

*Integration audit: 2026-05-07*

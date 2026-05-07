<!-- refreshed: 2026-05-07 -->
# Architecture

**Analysis Date:** 2026-05-07

## System Overview

`kubectl-sshuttle` is a Cobra-based Go CLI plugin that manages a one-pod
Deployment in a Kubernetes cluster and tunnels host traffic through it.
Three transport modes share the same `create`/`delete`/`status` plumbing
but diverge entirely on the data path:

```text
HOST (laptop)                                              POD (k8s)
─────────────                                              ─────────

DEFAULT  (sshuttle locally, python+sshuttle in pod)
┌──────────────────┐   stdin/stdout    ┌──────────────────┐
│ kubectl-sshuttle │  (kubectl exec    │ python3          │
│   ssh-proxy      │───-i deploy/...──▶│   -c <bootstrap> │
│  (transport)     │     sh -c)        │   = sshuttle/    │
│      ▲           │                   │     server.py    │
│      │ pipe      │                   └────────┬─────────┘
│ ┌────┴───────┐   │                            │ TCP/DNS to
│ │ sshuttle   │   │                            ▼ cluster
│ │  (python)  │   │                          (anywhere
│ │  iptables  │   │                           reachable
│ │  REDIRECT  │   │                           from the pod)
│ └────────────┘   │
└──────────────────┘

--rushtle-server  (sshuttle locally, rushtle server in pod via python-shim)
┌──────────────────┐   stdin/stdout    ┌──────────────────┐
│ kubectl-sshuttle │  (kubectl exec    │ rushtle (binary  │
│   ssh-proxy      │───-i deploy/...──▶│   acts as `-c`   │
│      ▲           │     sh -c         │   shim) →        │
│      │ pipe      │     rushtle -c …) │ rushtle server   │
│ ┌────┴───────┐   │                   │  --compat-       │
│ │ sshuttle   │   │                   │  bootstrap       │
│ │  (python)  │   │                   │  (eats sshuttle  │
│ │  iptables  │   │                   │   assembler.py)  │
│ │  REDIRECT  │   │                   └──────────────────┘
│ └────────────┘   │
└──────────────────┘

--rushtle  (pure Rust, no sshuttle, no python)
┌──────────────────┐   ssnet frames    ┌──────────────────┐
│ rushtle client   │  (8-byte hdr +    │ rushtle server   │
│ ─ NAT (iptables/ │  payload over     │ ─ TCP/UDP/DNS    │
│   pf)            │  kubectl-exec     │   spawners       │
│ ─ TCP/UDP/DNS    │  stdin/stdout)    │ ─ /etc/hosts     │
│   listeners      │◀─────────────────▶│   responder      │
│ ─ link probe     │                   │ ─ single stdin   │
│   + auto frame-  │                   │   reader loop    │
│   delay fallback │                   │                  │
└──────────────────┘                   └──────────────────┘
   `cmd/connect.go`                       `rushtle/src/server.rs`
   `rushtle/src/client.rs`
```

## Component Responsibilities

| Component | Responsibility | File |
|-----------|----------------|------|
| Cobra root | Persistent flags (`--context`, `--namespace`, `--rushtle`, `--rushtle-server`, `--chunk-bytes`, …) and mutex validation | `cmd/root.go` |
| `connect` | Verify deploy readiness, dispatch to `runRushtle` or `runSshuttle` | `cmd/connect.go` |
| `create` | Render and `kubectl apply` the proxy Deployment, wait for readiness | `cmd/create.go` |
| `delete` | `kubectl delete deploy/<name>` | `cmd/delete.go` |
| `status` | `kubectl get pods -l app=<name>` | `cmd/status.go` |
| `ssh-proxy` (hidden) | sshuttle's `--ssh-cmd` transport: parses sshuttle's invocation, execs `kubectl exec -i deploy/<name> -- sh -c <remote>`, with optional chunked-stdin pump | `cmd/sshproxy.go` |
| `proxy.DeploymentYAML` | Pick `prebakedTmpl` (image basename matches `*sshuttle*`/`*rushtle*`) vs `legacyTmpl`, render with `imagePullPolicy`/`Component`/`PullPolicy` | `proxy/deployment.go` |
| rushtle CLI dispatch | Detect `rushtle -c <PYSCRIPT>` (sshuttle's python-shim shape) → server compat mode; otherwise parse `client`/`server` subcommands | `rushtle/src/main.rs` |
| rushtle client | Bind 127.0.0.1:12300 (TCP) and :12353 (DNS UDP), spawn remote via `sh -c <cmd>`, install NAT, run link probe, demux frames | `rushtle/src/client.rs` |
| rushtle server | Read ssnet frames from stdin, spawn TCP/UDP/DNS handlers, single writer task funnels all replies to stdout | `rushtle/src/server.rs` |
| ssnet wire codec | 8-byte header (`SS` magic + channel + cmd + len BE), `read_frame`/`write_frame`, FRAME_DELAY_US, sync header | `rushtle/src/ssnet.rs` |
| firewall mod | Per-OS NAT install/remove + `original_dst` recovery | `rushtle/src/firewall/{linux,macos,stub,mod}.rs` |

## Pattern Overview

**Overall:** Three-mode polymorphic transport sharing a control plane (`create`/`delete`/`status`/proxy Deployment), diverging at the `connect` data path.

**Key Characteristics:**
- Local-process pipeline: `kubectl-sshuttle` (Go) → spawns either `sshuttle` (python) or local `rushtle` (Rust) → which spawns `kubectl exec -i deploy/<name> -- …`
- In-pod side reads from stdin, writes to stdout; kubectl-exec carries the bytes
- ssnet frames are SEQUENTIAL on stdin (single reader on each side) — no out-of-order delivery to reason about
- Ports are fixed: 127.0.0.1:12300 TCP, 127.0.0.1:12353 DNS UDP (rushtle); sshuttle uses its own
- `syscall.Exec` is used to chain Go → sshuttle / Go → rushtle so signals + stdio piping stay sane

## Layers

**Plugin entry / control plane (Go):**
- Purpose: command parsing, deployment lifecycle, env-var passthrough into transport
- Location: `cmd/`, `proxy/`, `main.go`
- Contains: Cobra wiring, kubectl shellouts, deployment template rendering
- Depends on: `kubectl` in PATH, optionally `sshuttle` and `rushtle` binaries
- Used by: end user via `kubectl sshuttle …`

**Local data plane (sshuttle path):**
- Purpose: NAT (iptables/pf) on host, classic sshuttle bootstrap
- Process: `sshuttle` (python) → forks `kubectl-sshuttle ssh-proxy` (which is *this* binary re-execed via `--ssh-cmd <self> ssh-proxy`) → `kubectl exec -i …`
- Where: `cmd/sshproxy.go` (transport) + sshuttle (external)

**Local data plane (rushtle path):**
- Purpose: NAT + listener + ssnet codec, no python
- Location: `rushtle/src/client.rs`, `rushtle/src/firewall/*.rs`
- Used by: `cmd/connect.go::runRushtle` (when `--rushtle`)

**In-pod data plane:**
- Purpose: terminate ssnet frames, open real TCP/UDP/DNS sockets in cluster
- Sshuttle mode: `python3` runs the standard sshuttle assembler.py + server.py
- Rushtle/rushtle-server mode: `rushtle server` (optionally `--compat-bootstrap` to discard sshuttle's prelude)
- Location: `rushtle/src/server.rs`

## Data Flow

### `kubectl sshuttle connect 10.0.0.0/8` (default mode)

1. `cmd/connect.go::connectCmd` checks `kubectl rollout status deploy/<name>` (`cmd/connect.go:39`)
2. Dispatch hits the `default:` arm: `runSshuttle(args, "python3")` (`cmd/connect.go:51`)
3. `runSshuttle` resolves `sshuttle` in PATH, then `syscall.Exec`s it with `--ssh-cmd "<self> ssh-proxy" -r ignored --python=python3 …` (`cmd/connect.go:99`)
4. sshuttle invokes `<self> ssh-proxy …` per remote command — that re-enters this binary at `cmd/sshproxy.go::sshProxyCmd`
5. `ssh-proxy` parses everything after `--` as the remote command, builds `kubectl exec -i deploy/<name> -- sh -c <remoteCmd>` and either `syscall.Exec`s kubectl OR pumps stdin chunked when `KUBECTL_SSHUTTLE_CHUNK_BYTES > 0` (`cmd/sshproxy.go:62-68`)

### `kubectl sshuttle --rushtle-server connect …`

1-2. Same as default, but `runSshuttle(args, "/usr/local/bin/rushtle")` (`cmd/connect.go:49`) — sshuttle is told the in-pod python is actually `rushtle`
3. sshuttle invokes the remote as `rushtle -c <PYSCRIPT>` — `rushtle/src/main.rs:128-147` detects this shape, parses `assembler_bytes` out of `stdin.read(N)` in PYSCRIPT, and calls `server::run(true, n)` (compat-bootstrap mode)
4. `server::run` reads + discards `assembler_bytes` of assembler.py source then `consume_sshuttle_bootstrap` walks (name\n, nbytes\n, nbytes-of-zlib) records until the empty-name terminator (`rushtle/src/server.rs:211-245`)
5. Server emits `\0\0SSHUTTLE0001` sync header and enters the ssnet frame loop

### `kubectl sshuttle --rushtle connect …`

1. `connect` dispatches to `runRushtle(args)` (`cmd/connect.go:47`)
2. `runRushtle` resolves the local rushtle binary (`--rushtle-bin` env var, sibling of self, or PATH; `cmd/connect.go:162`), builds `kctl = "kubectl --context X -n Y exec -i deploy/<name> -- rushtle server"`, and `syscall.Exec`s `rushtle client --cmd "<kctl>" --probe-fallback-us <us> [args…]`
3. `client::run` (`rushtle/src/client.rs:41`):
   - Binds `127.0.0.1:12300` TCP (and `[::1]:12300` if v6 subnets given) and `127.0.0.1:12353` UDP if `--dns`
   - Spawns the kubectl-exec child with stdin/stdout piped
   - Reads sync header (`ssnet::read_sync_header`)
   - Runs the link probe (`ensure_link`)
   - Installs NAT chain `RUSHTLE_<pid>` (Linux) / pf anchor `com.rushtle/<pid>` (macOS) via `firewall::install`
   - Spawns: writer task (mpsc → stdin), demux task (stdout → frame router), DNS recv task, IPv4 + IPv6 accept loops
4. Per accepted TCP conn: `firewall::original_dst` recovers the real destination; client sends `CMD_TCP_CONNECT(family,ip,port)` then bidirectional `CMD_TCP_DATA` until `CMD_TCP_EOF`/`CMD_TCP_STOP_SENDING`

### Frame protocol (ssnet)

`rushtle/src/ssnet.rs:1-31` — header layout:

| Bytes | Field | Notes |
|-------|-------|-------|
| 0..2 | magic | always `b"SS"` (`MAGIC`) |
| 2..4 | channel | u16 BE |
| 4..6 | cmd | u16 BE — `CMD_*` constants |
| 6..8 | datalen | u16 BE — `MAX_PAYLOAD = 65535` |
| 8..  | payload | `datalen` bytes |

Command codes (`ssnet.rs:87-101`):
`CMD_EXIT 0x4200`, `CMD_PING 0x4201`, `CMD_PONG 0x4202`,
`CMD_TCP_CONNECT 0x4203`, `CMD_TCP_STOP_SENDING 0x4204`, `CMD_TCP_EOF 0x4205`, `CMD_TCP_DATA 0x4206`,
`CMD_ROUTES 0x4207`, `CMD_HOST_REQ 0x4208`, `CMD_HOST_LIST 0x4209`,
`CMD_DNS_REQ 0x420a`, `CMD_DNS_RESPONSE 0x420b`,
`CMD_UDP_OPEN 0x420c`, `CMD_UDP_DATA 0x420d`, `CMD_UDP_CLOSE 0x420e`.

Payload conventions documented inline in `ssnet.rs:11-24`. `parse_connect`, `parse_udp_data`, `encode_host_list` in same file.

**State Management:**
- Client: `ChanMap = Arc<Mutex<HashMap<u16, mpsc::Sender<Vec<u8>>>>>` (TCP); `DnsMap = Arc<Mutex<HashMap<u16, SocketAddr>>>` (DNS in-flight); `next_id: Arc<Mutex<u16>>` for channel allocation (`rushtle/src/client.rs:38-39, 196`)
- Server: `tcp_chans` and `udp_chans` HashMaps keyed by channel; single `out_tx` mpsc into the writer task ensures one writer to stdout (`rushtle/src/server.rs:64-97`)

## Key Abstractions

**Frame:**
- Purpose: ssnet wire unit (channel, cmd, payload)
- Location: `rushtle/src/ssnet.rs:103-135`
- Pattern: `Frame::new(channel, cmd, data)` + `read_frame`/`write_frame` async helpers

**Channel:**
- Purpose: per-connection logical stream id (u16) multiplexed over the single stdin/stdout pipe
- Allocated by `next_id` (client) and echoed by server; `0` is the control channel (PING/ROUTES/HOST_LIST)
- Probe channels reserved at `PROBE_CHANNEL_BASE = 0xFF00` (`rushtle/src/client.rs:377`)

**ClientArgs / sshuttle env vars:**
- Plugin → transport handoff for sshuttle path uses env vars `KUBECTL_SSHUTTLE_{CONTEXT,NAMESPACE,NAME,CHUNK_BYTES,CHUNK_DELAY_US}` (`cmd/sshproxy.go:19-30`)
- Plugin → rushtle handoff uses `--cmd`/`--probe-fallback-us` flags + `RUSHTLE_FRAME_DELAY_US` env (`cmd/connect.go:114-145`)

**Link probe (`ensure_link`):**
- Purpose: detect kubectl-exec stdio paths that coalesce small frames into >1 KB websocket messages and drop them (observed on tailscale-fronted apiservers)
- Location: `rushtle/src/client.rs:407-541`
- Method: send `PROBE_BURST_FRAMES = 16` PINGs of `PROBE_FRAME_PAYLOAD = 256` bytes (264 total per frame, deterministic per-frame content) on channels `0xFF00..0xFF10`; wait `PROBE_TIMEOUT = 3s` for all PONGs
- Fallback: on miss, `ssnet::set_frame_delay_us(fallback_us)` (default 2000 µs, `--probe-fallback-us`) and retry once; if still failing, surface a clear error suggesting `--rushtle-server`

## Entry Points

**`main.go::main`:**
- Location: `main.go`
- Triggers: `kubectl sshuttle …` invocation (krew-installed plugin)
- Responsibilities: delegate to `cmd.Execute()`

**`cmd.Execute`:**
- Location: `cmd/root.go:68`
- Triggers: every plugin invocation
- Responsibilities: parse persistent flags, validate mutex (`--rushtle` xor `--rushtle-server`), dispatch to subcommand RunE

**`rushtle::main`:**
- Location: `rushtle/src/main.rs:123`
- Triggers: spawned by host plugin (rushtle path) OR by sshuttle's `--python=/usr/local/bin/rushtle` (rushtle-server path) OR `kubectl exec … rushtle server` (in-pod, both rushtle modes)
- Responsibilities: detect `-c PYSCRIPT` shim shape OR parse `Cli` (clap), then `tokio::block_on` either `client::run` or `server::run`

## Architectural Constraints

- **Threading:** Both client and server run on `tokio::runtime::Builder::new_multi_thread()` with `enable_all` (`rushtle/src/main.rs:143-155`). Stdin has a **single reader**, stdout has a **single writer task** funneling all `mpsc::Sender<Frame>` clones.
- **Frame ordering:** Frames on stdin are SEQUENTIAL — `read_frame` is called from one loop on each side (`client.rs::demux_task`, `server.rs::run` main loop). Do NOT add code that assumes out-of-order delivery; mux/demux is by `channel`, but order within the pipe is FIFO.
- **Global state:** `RESOLVERS: OnceCell<Vec<SocketAddr>>` in server (`rushtle/src/server.rs:319`), `FRAME_DELAY_US: AtomicU64` in ssnet (`ssnet.rs:48`). Both are intentionally module-level singletons.
- **NAT cleanup is signal-driven:** Client installs SIGINT + SIGTERM handlers that run `firewall::remove(&chain, has_v6)` and `process::exit(0)` (`rushtle/src/client.rs:198-220`). Plain `kill -9` orphans the iptables chain `RUSHTLE_<pid>` / pf anchor `com.rushtle/<pid>`.
- **Port conflicts:** Default redirect port `12300/tcp` and `12353/udp` are hard-coded constants in `client.rs:19-20`. Two concurrent `--rushtle` clients on the same host collide unless one passes `--listen-port`.
- **Mutex enforcement:** `--rushtle` and `--rushtle-server` are validated exclusive in `rootCmd.PersistentPreRunE` (`cmd/root.go:60-65`) so all subcommands see a single mode.

## Anti-Patterns

### Reasoning about per-channel ordering across the pipe

**What happens:** Treating each channel as if its frames could arrive interleaved with another channel's in unpredictable order.
**Why it's wrong:** stdin is a single byte-stream; `read_frame` returns frames in wire order. Channel demux only changes WHICH `mpsc` they go to, not their global arrival order.
**Do this instead:** Trust FIFO order on the pipe; only worry about ordering between independent `mpsc` consumers (which already have their own per-channel sequencing).

### Adding a second writer to stdout (server) or stdin (client)

**What happens:** A new task `tokio::io::stdout().write_all(...)` somewhere in `server.rs` to send a frame outside the main writer task.
**Why it's wrong:** The single-writer-task contract (server.rs:64-81, client.rs:106-119) is what makes `write_frame`'s flush + `FRAME_DELAY_US` sleep correct. Two writers can interleave headers and corrupt the stream.
**Do this instead:** Send a `Frame` through `out_tx` and let the writer task serialize it.

### Forgetting NAT cleanup paths

**What happens:** Adding an early `return` in `client::run` between `firewall::install` (`client.rs:182`) and the SIGINT/SIGTERM handler installation (`client.rs:199`).
**Why it's wrong:** A panic / early return between those points leaks the chain.
**Do this instead:** Ensure every error-return path in that window calls `firewall::remove(&chain, !subnets_v6.is_empty())` before bubbling up — the current code does this on probe failure (`client.rs:182-186`).

### Bypassing `runKubectlChunked` for stdin

**What happens:** Calling `syscall.Exec("kubectl", …)` from `ssh-proxy` even when `KUBECTL_SSHUTTLE_CHUNK_BYTES > 0`.
**Why it's wrong:** With `syscall.Exec` we lose ability to chunk writes; tailscale-fronted apiservers truncate >1 KB single writes (`cmd/sshproxy.go:62-68, 78-86`).
**Do this instead:** Honor the env var; the `runKubectlChunked` path forks kubectl as a child and pumps `os.Stdin` → `kubectl.StdinPipe` in `chunkBytes`-sized writes with `delay` µs sleeps.

## Error Handling

**Strategy:** Go side uses `fmt.Errorf("…: %w", err)` wrapping; Rust uses `anyhow::{Result, Context, bail}`.

**Patterns:**
- `cmd/sshproxy.go:99-101`: `if err := cmd.Start(); err != nil { return fmt.Errorf("kubectl start: %w", err) }`
- `rushtle/src/client.rs:88-90`: `read_sync_header` failure short-circuits `client::run` with `anyhow!("waiting for server sync header: {e}")`
- Probe failure: detailed message with byte counts, lost-channel indices, and remediation suggestion (`rushtle/src/client.rs:436-444, 532-540`)
- Cleanup-on-error: `firewall::install` failure triggers immediate `firewall::remove` before returning the error (`rushtle/src/client.rs:181-186`)

## Cross-Cutting Concerns

**Logging:**
- Go: `fmt.Fprintf(os.Stderr, …)` for user-facing status; no structured logger
- Rust: `tracing` + `tracing_subscriber::EnvFilter`, default `rushtle=info`, `-v` → debug, `-vv` → trace; writes to stderr only (`rushtle/src/main.rs:109-121`)

**Validation:**
- Mutex flags validated centrally in `rootCmd.PersistentPreRunE` (`cmd/root.go:60-65`)
- Frame magic checked in every `read_frame` call (`ssnet.rs:144`)
- ConnectTarget supports both 3-part (`family,ip,port`, modern sshuttle) and 2-part (`ip,port`, legacy/rushtle ≤0.1) forms (`ssnet.rs:205-215`)

**Authentication:** Not in scope — all auth is delegated to `kubectl` (kubeconfig contexts).

---

*Architecture analysis: 2026-05-07*

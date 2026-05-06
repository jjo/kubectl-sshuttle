# kubectl-sshuttle

<p align="center">
  <img src="demo/kubectl-sshuttle-cover.webp" alt="kubectl-sshuttle demo" width="800" />
</p>

A kubectl plugin that tunnels traffic through a Kubernetes cluster using
[`sshuttle`](https://github.com/sshuttle/sshuttle) — or **`rushtle`**, a
small sshuttle-compatible Rust port that ships in the same krew package.

It deploys a lightweight proxy pod inside the cluster and routes traffic
through it, letting you reach IPs and subnets that are only accessible from
within the cluster network.

## Install

### With krew

```bash
kubectl krew install sshuttle
```

The krew tarball includes both the `kubectl-sshuttle` plugin binary and a
sibling `rushtle` binary used by the `--rushtle` mode.

### From source

```bash
go install github.com/jjo/kubectl-sshuttle@latest
# rushtle is optional — only needed for --rushtle mode
make -C $(go env GOPATH)/pkg/mod/github.com/jjo/kubectl-sshuttle@latest rushtle
```

### Prerequisites

- `kubectl` configured with cluster access
- `sshuttle` installed locally (`pip install sshuttle`) — only for the
  default and `--rushtle-server` transport modes
- Local root for iptables NAT (sshuttle and rushtle self-`sudo` the
  firewall calls; you do **not** need to run kubectl-sshuttle under sudo)

## Transport modes

| Mode               | Local                | In-pod              | When to use                                                                                  |
|--------------------|----------------------|---------------------|----------------------------------------------------------------------------------------------|
| default            | `sshuttle` (python)  | python + sshuttle   | Standard sshuttle flow                                                                       |
| `--rushtle-server` | `sshuttle` (python)  | `rushtle` (rust)    | Drop-in replacement for the in-pod side. Same UX as sshuttle.                                |
| `--rushtle`        | `rushtle` (rust)     | `rushtle` (rust)    | Pure-rust on both ends. **Required for tailscale-fronted apiservers** (>1KB stdin truncation). |

All three modes share the same `create` / `connect` / `delete` commands.
Default deploy names: `<user>-sshuttle-proxy` for the python flow,
`<user>-rushtle-proxy` for the rust flows. They coexist in the same
namespace.

## Usage

```bash
# Standard flow (python+sshuttle on both ends)
kubectl sshuttle --context my-cluster create
kubectl sshuttle --context my-cluster connect 10.0.0.0/8
kubectl sshuttle --context my-cluster connect -- --dns 10.0.0.0/8

# Rushtle in pod, sshuttle locally
kubectl sshuttle --context my-cluster --rushtle-server create
kubectl sshuttle --context my-cluster --rushtle-server connect 10.0.0.0/8

# Pure rushtle (linux/macos local; works through tailscale-fronted clusters)
kubectl sshuttle --context my-cluster --rushtle create
kubectl sshuttle --context my-cluster --rushtle connect 10.0.0.0/8 -- --dns

# Status / cleanup
kubectl sshuttle --context my-cluster --rushtle status
kubectl sshuttle --context my-cluster --rushtle delete
```

## Commands

| Command   | Description                                              |
|-----------|----------------------------------------------------------|
| `create`  | Deploy the proxy pod and wait for readiness              |
| `connect` | Start the tunnel (requires `create` first)               |
| `status`  | Show proxy pod status                                    |
| `delete`  | Remove the proxy deployment                              |

## Flags

| Flag                  | Default                    | Description                                                                       |
|-----------------------|----------------------------|-----------------------------------------------------------------------------------|
| `--context`           | current context            | kubectl context                                                                   |
| `-n, --namespace`     | `default`                  | namespace for the proxy pod                                                       |
| `--name`              | `$USER-sshuttle-proxy` (or `-rushtle-proxy` in rushtle modes) | proxy deployment name           |
| `--image`             | `xjjo/sshuttle`            | proxy pod image (sshuttle mode); pre-baked, runs as uid 1001                      |
| `--rushtle`           | off                        | rushtle on both ends                                                              |
| `--rushtle-server`    | off                        | rushtle in pod only, sshuttle locally                                             |
| `--rushtle-image`     | `xjjo/rushtle`             | container image for rushtle modes                                                 |
| `--rushtle-bin`       | sibling-of-self / `$RUSHTLE_BIN` / `PATH` | path to local rushtle binary                                       |
| `--chunk-bytes`       | `0` (off)                  | chunk sshuttle's stdin into N-byte writes (workaround for `kubectl exec` >1KB stdin truncation) |
| `--chunk-delay-us`    | `2000`                     | inter-chunk sleep in microseconds                                                 |
| `--timeout`           | `120s`                     | readiness timeout for `create`                                                    |

## How it works

### Default / `--rushtle-server`

1. **`create`** deploys a Deployment running `xjjo/sshuttle` (sshuttle
   pre-baked, non-root) or `xjjo/rushtle` (Rust binary, non-root). Both
   images run as uid 1001 with `runAsNonRoot: true` and dropped
   capabilities. Readiness is signaled via a file probe.

   **Heuristic**: when `--image` matches `*/sshuttle*` or `*/rushtle*`,
   the deployment template skips the legacy `apt-get + pip install`
   startup command and adds restrictive `securityContext`. Pointing
   `--image` at e.g. `python:3.12-slim` falls back to the legacy
   install-at-startup flow (root in pod).

2. **`connect`** runs `sshuttle` locally with a custom SSH transport
   (`--ssh-cmd "kubectl-sshuttle ssh-proxy"`) that pipes through
   `kubectl exec` into the proxy pod. With `--rushtle-server`, the in-pod
   `python` is replaced by the rushtle binary acting as a `python -c` shim
   (consumes sshuttle's bootstrap, then enters ssnet mode).

### `--rushtle` (pure rust)

3. The local `rushtle` binary takes over sshuttle's role: installs iptables
   NAT redirect rules, runs a UDP/53 listener for DNS, spawns
   `kubectl exec -i deploy/<name> -- rushtle server`, and forwards traffic
   through ssnet frames. No python. No sshuttle bootstrap. All frames are
   capped at 768 bytes payload so they survive middleware that drops
   websocket writes above ~1 KB.

```
laptop --> [iptables NAT REDIRECT] --> rushtle/sshuttle --> kubectl exec --> rushtle/python --> cluster network
```

## Tailscale (and other proxies that truncate `kubectl exec` >1 KB)

Some apiservers fronted by tailscale or similar middleware silently drop
single websocket writes above ~1 KB. sshuttle's 17 KB python bootstrap dies
mid-stream; even post-bootstrap, sshuttle's 64 KB ssnet frames don't make
it through.

Fix:

```bash
# pure rushtle: every frame is <1KB and inter-frame paced
sudo -E env "RUSHTLE_FRAME_DELAY_US=2000" \
     kubectl sshuttle --context my-cluster --rushtle connect 10.0.0.0/8 -- --dns
```

For default / `--rushtle-server` mode, add `--chunk-bytes 768` so the
plugin chunks sshuttle's stdin into <1 KB writes before forwarding to
`kubectl exec`:

```bash
kubectl sshuttle --rushtle-server --chunk-bytes 768 connect 10.0.0.0/8 -- --dns
```

## DNS

`-- --dns` enables DNS forwarding. Same behavior as sshuttle:

- Reads nameservers from `/etc/resolv.conf` AND
  `/run/systemd/resolve/resolv.conf` (when present)
- Adds an iptables NAT REDIRECT rule for each nameserver IP, sending
  UDP/53 → local DNS port (rushtle: 12353; sshuttle picks its own)
- `/etc/resolv.conf` is **not** modified
- On systemd-resolved hosts, the stub resolver `127.0.0.53` and the real
  upstreams (e.g. `192.168.1.1`, `1.1.1.1`) both get redirect rules, so
  any app that uses libc resolution gets cluster DNS

The remote rushtle server resolves queries via the pod's
`/etc/resolv.conf` (cluster CoreDNS).

## Debugging

### Verbose logs

`rushtle` honors `RUST_LOG` (standard `tracing-subscriber` filter syntax).
The plugin propagates the env to the local `rushtle` subprocess via
`syscall.Exec(... os.Environ())`, so prefix the env on the kubectl-sshuttle
invocation:

```bash
RUST_LOG=rushtle=debug sudo -E kubectl sshuttle --context my-cluster \
    --rushtle connect 10.0.0.0/8 -- --dns
```

Equivalent: pass `-vv` after `--` (forwarded as a `clap` global flag to
`rushtle client`):

```bash
sudo -E kubectl sshuttle --rushtle connect 10.0.0.0/8 -- --dns -vv
```

`sudo -E` is required to preserve `RUST_LOG` across the privilege change
(`--rushtle` mode needs root for iptables).

In-pod `rushtle server` runs without verbose flags by default. To get
its debug logs, edit `cmd/connect.go::kubectlExecRushtleServer` and
append `"-vv"` to the exec argv, then rebuild.

### Reading the logs

Both sides log every framed message symmetrically — `tx` for outbound,
`rx` for inbound:

```text
DEBUG rushtle::client: tx ch=54 cmd=TCP_CONNECT len=20
INFO  rushtle::server: ch=54 TCP -> 10.48.11.254:80
DEBUG rushtle::server: tx ch=54 cmd=TCP_DATA len=768
DEBUG rushtle::client: rx ch=54 cmd=TCP_DATA len=768
```

| Symptom in logs | Likely cause |
|---|---|
| Client `tx ... TCP_DATA`, no server `rx` of it | kubectl-exec layer dropped the frame (apiserver / tailscale truncation) |
| Server `rx`, server `tx ... TCP_DATA`, no client `rx` | response direction dropped — same class of bug, opposite path |
| Both sides flowing, then 30 s gap → `unexpected EOF` | apiserver idle-timeout closed the kubectl-exec session (not a frame issue) |
| `link probe failed ... lost N/16 PONGs` | startup probe detected truncation, FRAME_DELAY auto-fallback engaged |

### Forcing chunking

If the auto-probe doesn't catch your cluster's failure mode, force
inter-frame delay manually:

```bash
# Pin a frame delay (microseconds). 2000us = 2ms is a good default.
RUSHTLE_FRAME_DELAY_US=2000 sudo -E kubectl sshuttle --rushtle connect 10.0.0.0/8

# Or via the plugin's --chunk-bytes flag (also pre-sets FRAME_DELAY):
sudo -E kubectl sshuttle --rushtle --chunk-bytes 768 --chunk-delay-us 2000 \
    connect 10.0.0.0/8
```

To disable the probe entirely (fastest startup, no auto-fallback):

```bash
sudo -E kubectl sshuttle --rushtle connect 10.0.0.0/8 -- \
    --probe-fallback-us 0
```

### Stale iptables chain

If rushtle was killed with `kill -9` (or the host crashed) the NAT chain
`RUSHTLE_<pid>` may persist. List and remove manually:

```bash
sudo iptables -t nat -L OUTPUT -n | grep RUSHTLE_
# pick the chain name, then:
sudo iptables -t nat -D OUTPUT -j RUSHTLE_<pid>
sudo iptables -t nat -F RUSHTLE_<pid>
sudo iptables -t nat -X RUSHTLE_<pid>
```

A subsequent rushtle start reuses an existing same-pid chain (treats it
as already-installed), but a different-pid leftover does no harm — it's
just dead rules.

## Building

```bash
make build              # plugin binary → ./kubectl-sshuttle
make rushtle            # local rushtle  → rushtle/target/release/rushtle
make rushtle-image      # docker image   → $(RUSHTLE_IMAGE)
make rushtle-push       # push image     → docker.io/xjjo/rushtle:latest

# release / packaging
make rushtle-prebuilt   # cross-build linux/amd64 rushtle for goreleaser
make release-snapshot   # local goreleaser snapshot (4 platform tarballs)
make krew-install-local # install host snapshot via krew (for testing)

# CI builds linux/arm64 + darwin/{amd64,arm64} on the matching runners.
```

### Smoke tests (no kubernetes required)

```bash
make rushtle-test
```

Drives `rushtle server` with crafted ssnet frames in Python and verifies:
TCP CONNECT/DATA echo, DNS_REQ → upstream resolver, UDP_OPEN/UDP_DATA bidi,
HOST_REQ → HOST_LIST from `/etc/hosts`, `--compat-bootstrap` consumes
sshuttle assembler protocol, `-c` python-shim mode parses `stdin.read(N)`.

## Wire protocol (`rushtle`)

`rushtle` speaks sshuttle's [`ssnet`](https://github.com/sshuttle/sshuttle/blob/main/sshuttle/ssnet.py)
framing on stdin/stdout, byte-exact:

```
magic     (2 bytes)  b"SS"
channel   (u16 BE)   per-flow demux id, 0 = control
cmd       (u16 BE)   sshuttle CMD constants (0x4200…)
datalen   (u16 BE)   payload length
payload   N bytes    command-specific
```

Implemented commands: `EXIT`, `PING`/`PONG`, `TCP_CONNECT`, `TCP_DATA`,
`TCP_EOF`, `TCP_STOP_SENDING`, `ROUTES`, `HOST_REQ`/`HOST_LIST`,
`DNS_REQ`/`DNS_RESPONSE`, `UDP_OPEN`/`UDP_DATA`/`UDP_CLOSE`. CONNECT
payload is sshuttle's `family,ip,port` ASCII tuple.

## License

Apache-2.0

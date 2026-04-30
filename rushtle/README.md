# rushtle

A small Rust port of [sshuttle](https://github.com/sshuttle/sshuttle), built
to plug into [`kubectl-sshuttle`](..) as an alternative to the
python-on-pod transport.

## Why

When `kubectl-sshuttle` runs the stock python sshuttle, the client uploads an
~18 KB python "assembler" bootstrap over `kubectl exec` stdin. On some
clusters fronted by network proxies (Tailscale, AWS NLB+TLS, custom kube-apiserver
gateways) that stdin stream is silently truncated above ~1 KB, leaving the
python parser to crash mid-decompress with a `SyntaxError` from `helpers.py`.

`rushtle` fixes that by shipping a single static binary in the proxy image —
no inline upload, no python.

## Wire compatibility

`rushtle` speaks the sshuttle [`ssnet`](
https://github.com/sshuttle/sshuttle/blob/main/sshuttle/ssnet.py) framing on
stdin/stdout:

| field    | width   | notes                                  |
| -------- | ------- | -------------------------------------- |
| magic    | 2 bytes | `b"SS"`                                |
| channel  | u16 BE  | per-flow demux id, 0 = control         |
| cmd      | u16 BE  | sshuttle CMD constants (`0x4200…`)     |
| datalen  | u16 BE  | payload length, max 65535              |
| payload  | N bytes | command-specific                       |

Implemented commands: `PING`/`PONG`, `TCP_CONNECT`, `TCP_DATA`, `TCP_EOF`,
`TCP_STOP_SENDING`, `EXIT`. **CONNECT** payload is sshuttle's exact
`"<ip>,<port>"` ASCII string.

A stock `sshuttle` *client* will not interop directly because it always sends
its python bootstrap first. After the bootstrap is consumed (or skipped via
a patched client) the wire-level frames match.

## Building

Native debug build:

```sh
cargo build
```

Release build (small, stripped, LTO):

```sh
cargo build --release
```

Docker image (static musl binary):

```sh
docker build -t xjjo/rushtle .
```

## Usage

### Server (in-pod)

Reads ssnet frames on stdin, opens TCP to requested targets, writes responses
back on stdout. Stateless — no listener, no config.

```sh
rushtle server                 # via kubectl exec stdin
rushtle -v server              # info logs
rushtle -vv server             # debug logs
```

### Client (local, Linux only)

Redirects outbound TCP for one or more CIDRs to the remote `rushtle server`
via `iptables -t nat REDIRECT`. Needs root (or `CAP_NET_ADMIN`).

```sh
sudo rushtle client \
  --cmd "kubectl exec -i deploy/proxy -- rushtle server" \
  10.0.0.0/8 172.16.0.0/12
```

Flags:

- `--cmd` shell command that runs `rushtle server`. Anything that pipes
  bidirectional bytes will work — kubectl exec, ssh, docker exec, etc.
- `--listen-port` redirect target port (default `12300`).
- `--no-iptables` skip iptables setup (useful when something else manages
  the rules; e.g. testing).

## Truncation workaround

Some kubernetes apiservers proxied through middleware (notably tailscale's
`ts.net` ingress) drop single websocket writes above ~1 KB on `kubectl exec`
stdin/stdout. Stock sshuttle's 17 KB python bootstrap fails outright; even
post-bootstrap, large ssnet `TCP_DATA` frames (sshuttle uses 64 KB) die.

rushtle mitigates this two ways:

1. **Frame size cap** — `ssnet::CHUNK = 768`. TCP_DATA payloads stay under
   1 KB, so each frame is one safe websocket write.
2. **Inter-frame pacing** — set `RUSHTLE_FRAME_DELAY_US=2000` (2 ms) before
   running rushtle on truncating clusters. After each `flush()`, rushtle
   sleeps the configured time so kubectl drains the pipe between frames
   rather than batching them into one big websocket write.

For `--rushtle-server` mode (sshuttle locally), `kubectl-sshuttle` exposes
`--chunk-bytes 768 --chunk-delay-us 2000` which forks `kubectl exec` instead
of `syscall.Exec`'ing it, and chunks sshuttle's stdin in software before
sending to kubectl. Survives sshuttle's monolithic 17 KB bootstrap write.

## Limits (v1)

- Client side: Linux native (`iptables`); macOS (`pfctl` + `DIOCNATLOOK`)
  scaffolded but unverified on hardware.
- TCP fully supported; DNS via `--dns`; general UDP via `UDP_OPEN`/`UDP_DATA`.
- No latency-control flow window — relies on TCP backpressure end-to-end.

These are tracked as future work; protocol-level extension is straightforward
because the frame codec already understands all sshuttle CMD codes.

## Testing without iptables

`scripts/rushtle-smoketest.py` exercises the server end-to-end by crafting
ssnet frames in python and verifying a TCP echo round-trip. No root needed:

```sh
make rushtle-test
```

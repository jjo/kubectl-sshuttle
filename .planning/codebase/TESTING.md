# Testing Patterns

**Analysis Date:** 2026-05-07

This repo has two test surfaces: Go unit tests (run via `go test`) and Python smoketests that drive the Rust `rushtle` binary as a subprocess. There are **no Rust `#[cfg(test)]` unit tests** today — the Rust side is exercised end-to-end by the Python harnesses.

## Go Unit Tests

### Framework

- Standard library `testing` only — no third-party assertion library.
- Tests live next to the code they cover, suffix `_test.go`, same package (white-box):
  - `cmd/sshproxy_test.go` — covers `ParseSSHArgs` and `BuildKubectlExecArgs`.
  - `proxy/deployment_test.go` — covers `DeploymentYAML` rendering and the `imageTag` / `isPrebakedImage` / `imagePullPolicy` helpers.

### Patterns

**Substring assertion on rendered YAML** (`proxy/deployment_test.go`):

```go
yaml, err := DeploymentYAML(DeploymentConfig{
    Name: "jjo-sshuttle-proxy", Namespace: "default", Image: "xjjo/sshuttle",
})
for _, want := range []string{
    "name: jjo-sshuttle-proxy",
    "runAsNonRoot: true",
    "imagePullPolicy: Always", // no tag = latest = Always
} {
    if !strings.Contains(yaml, want) {
        t.Errorf("YAML missing %q\n---\n%s", want, yaml)
    }
}
```

Negative assertions use the same shape with `strings.Contains` checked for unwanted strings (e.g. legacy `apt-get update` should NOT appear when the image is prebaked).

**Table-driven tests** for pure helpers (`TestImagePullPolicy`, `TestIsPrebakedImage`, `TestParseSSHArgs`, `TestBuildKubectlExecArgs`):

```go
cases := []struct {
    image, want string
}{
    {"xjjo/rushtle", "Always"},
    {"xjjo/rushtle:latest", "Always"},
    {"xjjo/rushtle:v1.2.3", "IfNotPresent"},
    {"localhost:5000/foo:v1", "IfNotPresent"},  // registry-with-port
    {"foo@sha256:abc", "Always"},               // digest-only, no tag
    {"foo:1@sha256:abc", "IfNotPresent"},       // tag + digest
    ...
}
for _, c := range cases {
    got := imagePullPolicy(c.image)
    if got != c.want { t.Errorf(...) }
}
```

The image-reference cases deliberately cover the three tricky shapes:
- `host:port/name` — colon in the host part, not a tag.
- `name@sha256:...` — digest only, no tag.
- `name:tag@sha256:...` — both.

Sub-tests use `t.Run(tt.name, func(t *testing.T) { ... })` (see `cmd/sshproxy_test.go`).

### Run commands

```bash
make test          # → go test ./... -v
go test ./...      # equivalent
go test ./proxy/   # one package
```

Result: 13 test cases pass across `cmd/` and `proxy/` (3 packages including the empty `main` package).

## Rust Tests

### Unit tests

There are **no `#[cfg(test)]` unit tests** in `rushtle/src/` currently. `cargo test` runs but executes nothing.

### Lint

`cargo clippy --release -- -D warnings` must be clean — warnings are treated as errors per the rule in `AGENTS.md`.

### Build verification

```bash
cd rushtle && cargo build --release
# → target/release/rushtle
```

The Makefile's `rushtle` target wraps this.

## Python Smoketests

### Location and run

All smoketests live in `scripts/rushtle-*-smoketest.py` and are driven by:

```bash
make rushtle-test
```

which runs each script with `cwd=rushtle/` so the relative path `./target/release/rushtle` resolves.

### What's covered

| Script | What it asserts |
|---|---|
| `rushtle-smoketest.py` | TCP CONNECT + DATA echo. Spawns rushtle server, opens a local TCP echo socket on `127.0.0.1:9999`, drives `CMD_TCP_CONNECT` + `CMD_TCP_DATA` against it, expects `b"ABC"` back. Also verifies the initial PING(`chicken`) and ROUTES frames. |
| `rushtle-dns-smoketest.py` | `CMD_DNS_REQ` for `example.com.` resolves via the host's `/etc/resolv.conf` upstream and returns a `CMD_DNS_RESPONSE` with matching txid. |
| `rushtle-udp-smoketest.py` | `CMD_UDP_OPEN` (family=2) + `CMD_UDP_DATA` round-trip against a local UDP echo on `127.0.0.1:9998`. Verifies bidirectional `peerip,peerport,<bytes>` framing. |
| `rushtle-host-smoketest.py` | `CMD_HOST_REQ` returns a `CMD_HOST_LIST` populated from `/etc/hosts` (asserts `b"localhost"` is present). |
| `rushtle-bootstrap-smoketest.py` | `--compat-bootstrap` consumes a fake sshuttle assembler-protocol module list, then emits the sync header + initial PING + ROUTES. |
| `rushtle-shim-smoketest.py` | `rushtle -c <PYSCRIPT>` python-shim shape parses `stdin.read(N)` out of the script, eats N bytes of fake assembler.py source, then consumes module records and emits the sync header. |

### Pattern

Each script:

1. Spawns `subprocess.Popen(["./target/release/rushtle", "-vv", "server", ...], stdin=PIPE, stdout=PIPE, stderr=PIPE)`.
2. Reads the 14-byte sync header `\0\0SSHUTTLE0001` and drains the initial `PING(chicken)` + `ROUTES` frames.
3. Writes one or more crafted ssnet frames using the shared header struct:
   ```python
   HDR = struct.Struct("!2sHHH")  # SS, channel, cmd, len
   def frame(channel, cmd, data=b""):
       return HDR.pack(b"SS", channel, cmd, len(data)) + data
   ```
4. Reads response frames with a deadline loop (`time.time() + N` seconds).
5. `proc.terminate()` + `proc.wait(timeout=2)` (kill on timeout).
6. Drains `proc.stderr.read()` to surface rushtle's tracing logs on failure.
7. `print("OK: ...")` on success, `print("FAIL: ...")` + `sys.exit(1)` on mismatch.

The wire `CMD_*` constants are duplicated in each script (mirror of `rushtle/src/ssnet.rs`). When adding a new command, update both sides.

### Known fragility

`proc.stderr.read()` is called **after** `proc.terminate()`. If rushtle has produced more stderr than the OS pipe buffer (~64 KB on Linux) before terminating, that read can deadlock waiting for EOF while the kernel buffer is full of un-drained data. Today's tests are short and don't trip this, but if a test starts producing large amounts of stderr (e.g. trace-level logging or large transfers), drain stderr in a background thread concurrently with the test instead of after `terminate()`.

### What is NOT tested by smoketests

- **iptables setup**: the client smoketests are bypassed — they invoke `rushtle server` directly and craft frames against it, because the real client requires `iptables`/root.
- **macOS pf**: no smoketest exercises `firewall/macos.rs` — that path is verified manually.
- **End-to-end via `kubectl exec`**: not covered by automation; verified manually with a kind cluster (`make rushtle-kind-load`).

## Coverage

No coverage tooling configured. No coverage targets enforced.

## CI

- `.github/workflows/release.yml` runs **only on tag push** (`v*.*.*`).
- It builds rushtle binaries per platform (linux/{amd64,arm64} via buildx, darwin/{amd64,arm64} on `macos-latest`) and runs `goreleaser`.
- **No CI test job.** Tests are presubmit-local only — run `make test` and `make rushtle-test` before pushing.

---

*Testing analysis: 2026-05-07*

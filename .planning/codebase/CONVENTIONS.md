# Coding Conventions

**Analysis Date:** 2026-05-07

## Languages and Tooling

This repo is bilingual:

- **Go** (1.25.0, see `go.mod`) — the kubectl plugin host (`kubectl-sshuttle` binary).
- **Rust** (edition 2021, see `rushtle/Cargo.toml`) — the in-pod proxy / Rust client (`rushtle` binary).
- **Python 3** — smoketest harnesses only (`scripts/rushtle-*-smoketest.py`); not shipped at runtime.

`AGENTS.md` exists at the repo root and contains operating rules for AI agents that supplement these conventions.

## Go Conventions

### Layout

- Single-package CLI under `cmd/`, one file per cobra subcommand:
  - `cmd/root.go` — root command, global flags, `Config` struct, helpers.
  - `cmd/connect.go`, `cmd/create.go`, `cmd/delete.go`, `cmd/status.go` — user-facing subcommands.
  - `cmd/sshproxy.go` — hidden `ssh-proxy` subcommand (called by sshuttle, not by users).
- Renderers / non-CLI logic live in sibling packages: `proxy/deployment.go` for the Deployment YAML template.
- `main.go` is two lines — only invokes `cmd.Execute()`.

### Configuration

- Single global `cfg Config` struct in `cmd/root.go` holds every persistent flag value.
- Flags are bound with `rootCmd.PersistentFlags().<Type>Var(&cfg.<Field>, ...)`.
- Cross-flag validation (e.g. mutually-exclusive `--rushtle` / `--rushtle-server`) is done once in `rootCmd.PersistentPreRunE`, not duplicated per subcommand.

### Naming

- Default deploy name is computed: `<user>-sshuttle-proxy` (or `<user>-rushtle-proxy` in rushtle modes), where `<user>` comes from `SUDO_USER` (preferred) or `USER`. See `userPrefix()` in `cmd/root.go`.
- `effectiveName()` decides which name to actually use. To detect whether the user passed `--name` it uses `rootCmd.PersistentFlags().Changed("name")` — **never** compare `cfg.Name` to `defaultDeployName()` (a user passing `--name <user>-sshuttle-proxy` literally would be silently rewritten in rushtle modes; this regression is documented and was caught in a prior fix).
- Exported identifiers use Go `CamelCase`; unexported use `camelCase`. Cobra commands are named `<verb>Cmd` (e.g. `connectCmd`, `sshProxyCmd`).
- Package names are short, lowercase, no underscores (`cmd`, `proxy`).

### Subcommand pattern

Each subcommand file follows this structure:

```go
var fooCmd = &cobra.Command{
    Use:   "foo",
    Short: "...",
    RunE:  func(cmd *cobra.Command, args []string) error { ... },
}

func init() {
    rootCmd.AddCommand(fooCmd)
}
```

Use `RunE` (not `Run`) so errors propagate and `rootCmd.Execute()` exits non-zero.

### Error handling

- Errors are wrapped with `fmt.Errorf("context: %w", err)` to preserve the chain.
- User-facing errors include actionable hints (e.g. `"sshuttle not found in PATH — install it first (pip install sshuttle)"` in `cmd/connect.go`).
- `SilenceUsage: true` is set on subcommands so a runtime failure doesn't dump the usage text.

### Process control

- `syscall.Exec` is used to replace the current process when handing off to `kubectl` / `sshuttle` / `rushtle` — preserves direct stdio piping and signal semantics. See `cmd/sshproxy.go::RunE` and `cmd/connect.go::runSshuttle`/`runRushtle`.
- The chunked-stdin path in `runKubectlChunked` (`cmd/sshproxy.go`) uses `exec.CommandContext` + a `signal.NotifyContext`-derived ctx so a kubectl write failure cancels the context and unblocks `cmd.Wait()`.

### kubectl invocation

- `kubectlArgs(extra...)` in `cmd/root.go` is the canonical way to build a kubectl arg list — it handles `--context` (omitted when empty) and `-n <namespace>` once.
- `runKubectl(args...)` runs kubectl with stdio inherited; use it for short-lived calls. For exec'ing into the long-running tunnel use `syscall.Exec` directly.

### Logging

- No structured logger in Go code. Diagnostics use `fmt.Fprintf(os.Stderr, ...)`.
- `os.Stdout` is reserved for the actual proxied byte stream — never log to it.

### Version stamping

`var version = "dev"` in `cmd/root.go` is overridden at link time:

```
-ldflags "-X github.com/jjo/kubectl-sshuttle/cmd.version=<git-describe>+<git-short-sha>"
```

The Makefile composes `VERSION + "+" + GIT_REV`. `Version()` exposes the value.

## Rust (rushtle) Conventions

### Layout

```
rushtle/
├── Cargo.toml
├── build.rs
└── src/
    ├── main.rs        # clap CLI + python-shim shortcut
    ├── client.rs      # local side: ssnet writer + main loop + iptables
    ├── server.rs      # in-pod side: ssnet over stdio
    ├── ssnet.rs       # wire protocol (Frame, cmd codes, frame-delay knob)
    └── firewall/
        ├── mod.rs     # platform dispatch
        ├── linux.rs   # iptables
        ├── macos.rs   # pf (PacketFilter)
        └── stub.rs    # other OSes
```

### Error handling

- `anyhow::Result<T>` is the return type everywhere — no custom error enum.
- Use `.with_context(|| "...")` to add layers; `?` to propagate.

### Logging

- All diagnostics go through `tracing` (`info!`, `debug!`, `warn!`, `error!`). No `println!` / `eprintln!` anywhere in `rushtle/src/`.
- `tracing-subscriber` writes to **stderr** (`with_writer(std::io::stderr)`). Stdout is reserved for ssnet frames.
- Verbosity comes from `-v` / `-vv` (`info` / `debug` / `trace`). Honors `RUST_LOG` if set (`EnvFilter::try_from_default_env`).
- For frame logs use `Frame::cmd_name(frame.cmd)` to get a human-readable command name (e.g. `TCP_CONNECT`) instead of hex.

### Symmetric tx/rx logging

Reader and writer loops in both `client.rs` and `server.rs` log frames with the same prefix shape:

```rust
tracing::debug!("tx ch={} cmd={} len={}", frame.channel, Frame::cmd_name(frame.cmd), frame.payload.len());
tracing::debug!("rx ch={} cmd={} len={}", frame.channel, Frame::cmd_name(frame.cmd), frame.payload.len());
```

This makes a `-vv` capture of either side directly diff-able.

### Concurrency

- `tokio` multi-thread runtime, full feature set in `Cargo.toml`.
- Per-channel state lives in `Arc<Mutex<HashMap<u16, ...>>>` (`tcp_chans`, `udp_chans` in `server.rs`). **Lock scope is kept tiny** — typically a `.lock().await.remove(&ch)` or `.get(&ch).cloned()` — to avoid holding the mutex across `.await` points and deadlocking.
- Cross-task communication uses `tokio::sync::mpsc` channels; the writer task owns stdout.

### Wire protocol invariants (ssnet.rs / server.rs / client.rs)

- **Frame ordering**: frames are read sequentially from stdin in a single reader task. Out-of-order delivery is not handled — do not introduce parallel readers or re-ordering buffers.
- **Half-close collapse**: `CMD_TCP_EOF` and `CMD_TCP_STOP_SENDING` are intentionally handled by the same `match` arm in both `client.rs` and `server.rs`. They both translate to "drop the inbound side of this channel". Don't split them.
- **Per-frame payload cap**: `ssnet::CHUNK = 768` keeps `HDR_LEN + payload < 1 KB`, surviving middleware (notably tailscale-fronted apiservers) that drops single websocket writes above ~1 KB.
- **Frame delay knob**: `RUSHTLE_FRAME_DELAY_US` env var (read in `init_frame_delay_from_env`) and `set_frame_delay_us()` runtime override let the link probe in `client::run` dial in chunking on the fly. Reads use `Ordering::Relaxed` — the value is advisory.

### Linux iptables (firewall/linux.rs)

- All `iptables` invocations go through `iptables_cmd` / `iptables_cmd_quiet` wrappers.
- `iptables_cmd_quiet` is for calls where a non-zero exit is **expected** (drain-loop terminator, "chain already exists" on reuse) — it suppresses stderr noise.
- `-w 5` is always passed — bounds the xtables-lock wait to 5 s.
- `iptables_create_chain` retries `-N` once after checking for existing chain (handles "Chain already exists" idempotently).
- **Drain pattern**: when removing rules, loop `-D OUTPUT -j <chain>` until it fails — there may be multiple jumps and a single delete only removes one. Loop is bounded (`> 16` triggers a bail).
- **`-X` retry**: chain delete is retried once after a 100 ms sleep to absorb the `nf_tables` GC race where a freshly-flushed chain still has a pending reference.

### macOS pf (firewall/macos.rs)

- `PfiocNatlook` struct is exactly 96 bytes (matches darwin `struct pfioc_natlook`). Enforced at compile time:
  ```rust
  const _: () = assert!(std::mem::size_of::<PfiocNatlook>() == 96);
  ```
  Don't change field order without updating the assert.
- Lookup direction order: **PF_OUT first, fall back to PF_IN** — matches sshuttle's `pf.py` order. Don't flip.
- Anchor name is per-pid (`com.rushtle/<pid>`) so concurrent rushtle instances don't stomp each other.

### Version stamping

`build.rs` sets `cargo:rustc-env=RUSHTLE_GIT_REV=...`. Source order:
1. `RUSHTLE_GIT_REV_OVERRIDE` env var (used by Docker / CI when `.git` isn't in the build context).
2. `git rev-parse --short HEAD`.
3. Literal `unknown` fallback.

Final version string: `concat!(env!("CARGO_PKG_VERSION"), "+", env!("RUSHTLE_GIT_REV"))` → e.g. `rushtle 0.2.0+ace3751`.

### Release profile

`Cargo.toml` `[profile.release]` is tuned for binary size:
```toml
strip = true
lto = "thin"
codegen-units = 1
opt-level = "z"
panic = "abort"
```

## Python (smoketest scripts) Conventions

- Module-level docstring describes the test in 2–4 lines.
- Direct `subprocess.Popen` against `./target/release/rushtle` (cwd is `rushtle/`, set by the Makefile target).
- Wire protocol bytes are encoded with `struct.Struct("!2sHHH")` named `HDR` and command codes hard-coded as `CMD_*` constants (mirror the Rust definitions in `ssnet.rs`).
- Fail mode: `print("FAIL: ...")` + `sys.exit(1)`. Success: `print("OK: ...")`.

## Comment Conventions

- Tightly-coupled invariants get inline `//` (Rust) / `//` (Go) comments explaining **why**, not what (see e.g. the `effectiveName()` doc-comment about the `Changed("name")` pitfall).
- Module-level Rust files use `//!` doc-comments to capture the file's purpose and any wire-protocol references (e.g. `ssnet.rs` documents the `HDR_FMT` mapping back to sshuttle's python source).
- Go uses doc-comments before exported identifiers (`// Foo does X`).

## Commit Conventions

- **Conventional Commits** format: `feat:`, `fix:`, `docs:`, `chore:`, etc. Imperative subject, ≤72 chars.
- Body explains the why; wraps at 72.
- AI-assisted commits include a `Co-Authored-By:` trailer per `AGENTS.md`:
  ```
  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  ```

## Build / Verify

Before claiming a change is done, run:

- `make test` (or `go test ./...`) — Go unit tests.
- `cd rushtle && cargo build --release` — produces `target/release/rushtle`.
- `cargo clippy --release -- -D warnings` — must be clean (no warnings tolerated).
- `make rushtle-test` — Python smoketests against the built rushtle binary.

---

*Convention analysis: 2026-05-07*

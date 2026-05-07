# Codebase Structure

**Analysis Date:** 2026-05-07

## Directory Layout

```
kubectl-sshuttle/
├── main.go                      # Tiny shim: cmd.Execute()
├── go.mod / go.sum              # Go module: cobra + indirect deps
├── Makefile                     # Top-level build orchestrator
├── .goreleaser.yml              # Release packaging (kubectl-sshuttle + rushtle)
├── .krew.yaml                   # krew plugin manifest
├── kubectl-sshuttle.yaml        # krew-index plugin spec
├── krew-index-plugins-sshuttle.yaml
├── README.md / CHANGELOG.md / LICENSE / AGENTS.md
│
├── cmd/                         # Cobra subcommands (Go)
│   ├── root.go                  # Persistent flags + Config struct + version
│   ├── connect.go               # `connect`: dispatches runRushtle / runSshuttle
│   ├── create.go                # `create`: kubectl apply + rollout status
│   ├── delete.go                # `delete`: kubectl delete deploy
│   ├── status.go                # `status`: kubectl get pods -l app=
│   ├── sshproxy.go              # hidden `ssh-proxy` (sshuttle transport + chunked stdin pump)
│   └── sshproxy_test.go         # Tests for ParseSSHArgs / BuildKubectlExecArgs
│
├── proxy/                       # Proxy Deployment template + helpers (Go)
│   ├── deployment.go            # DeploymentYAML, isPrebakedImage, imageTag, imagePullPolicy
│   ├── deployment_test.go       # Tests
│   └── Dockerfile               # Pre-baked sshuttle image (python:3.12-slim, runs as uid=1001)
│
├── rushtle/                     # Rust workspace (sshuttle-compatible client+server)
│   ├── Cargo.toml               # Pkg `rushtle` v0.2.0; tokio/clap/anyhow/tracing/libc
│   ├── Cargo.lock
│   ├── build.rs                 # Embeds RUSHTLE_GIT_REV via env at build time
│   ├── Dockerfile               # Pre-baked rushtle image (xjjo/rushtle)
│   ├── Dockerfile.builder       # CI cross-build container
│   ├── README.md
│   ├── .dockerignore
│   ├── src/
│   │   ├── main.rs              # CLI dispatch + python -c shim path
│   │   ├── client.rs            # Host side: NAT install, listeners, accept_loop, link probe
│   │   ├── server.rs            # In-pod: ssnet frame loop, TCP/UDP/DNS spawners, bootstrap eater
│   │   ├── ssnet.rs             # Wire protocol: 8-byte header + payload, CMD_*, FRAME_DELAY
│   │   └── firewall/
│   │       ├── mod.rs           # cfg-gated re-export
│   │       ├── linux.rs         # iptables/ip6tables NAT REDIRECT + SO_ORIGINAL_DST
│   │       ├── macos.rs         # pf rdr rules + DIOCNATLOOK
│   │       └── stub.rs          # Other-OS fallback (compiles, errors at runtime)
│   └── target/                  # Cargo build artifacts (gitignored)
│
├── prebuilt/                    # Cross-built rushtle binaries for goreleaser archives
│   ├── rushtle_linux_amd64/
│   ├── rushtle_linux_arm64/
│   ├── rushtle_darwin_amd64/
│   └── rushtle_darwin_arm64/
│
├── scripts/                     # Build + smoketest scripts
│   ├── build-rushtle-prebuilt.sh    # Cross-build rushtle for prebuilt/ (used by goreleaser)
│   ├── rushtle-smoketest.py         # Headline smoketest
│   ├── rushtle-host-smoketest.py    # Host-side
│   ├── rushtle-shim-smoketest.py    # `rushtle -c PYSCRIPT` python-shim path
│   ├── rushtle-bootstrap-smoketest.py  # consume_sshuttle_bootstrap
│   ├── rushtle-dns-smoketest.py     # DNS_REQ / DNS_RESPONSE round-trip
│   └── rushtle-udp-smoketest.py     # UDP_OPEN / UDP_DATA / UDP_CLOSE
│
├── .github/workflows/
│   └── release.yml              # goreleaser-driven release pipeline
│
├── .planning/
│   └── codebase/                # GSD codebase maps (this directory)
│
└── demo/                        # Recorded demo (vhs / mp4 / gif / webp)
    ├── kubectl-sshuttle-cover.tape
    ├── kubectl-sshuttle-cover.{mp4,gif,webp}
    ├── kubectl-sshuttle-cover-static.webp
    └── kubectl-sshuttle-cover.convert.sh
```

## Directory Purposes

**`cmd/`:**
- Purpose: Cobra subcommands and the global `Config` struct
- Contains: One `*.go` per subcommand + `root.go` (flags) + `sshproxy.go` (hidden transport)
- Key files: `cmd/root.go` (flags + `effectiveName()`/`defaultDeployName()`), `cmd/connect.go` (transport dispatch), `cmd/sshproxy.go` (`runKubectlChunked`)

**`proxy/`:**
- Purpose: Proxy Deployment manifest template + image
- Contains: `deployment.go` (Go template + helpers), `Dockerfile` (prebaked sshuttle image)
- Key files: `proxy/deployment.go::DeploymentYAML` is the only export

**`rushtle/src/`:**
- Purpose: Rust port — local client (host) and remote server (pod) for sshuttle's ssnet protocol
- Contains: One module per concern
- Key files: `main.rs` (CLI + python-shim shortcut), `client.rs` (host plane), `server.rs` (pod plane), `ssnet.rs` (wire protocol)
- `firewall/`: per-OS NAT install/remove + `original_dst` recovery, gated on `target_os`

**`scripts/`:**
- Purpose: build artifacts + smoketests; not bundled with the plugin
- Key files: `build-rushtle-prebuilt.sh` (used by `.goreleaser.yml`), `rushtle-*-smoketest.py` (run via `make`)

**`prebuilt/`:**
- Purpose: Cross-compiled rushtle binaries goreleaser packages alongside `kubectl-sshuttle` so the krew tarball ships both
- Generated: yes (by `scripts/build-rushtle-prebuilt.sh` + CI matrix)
- Committed: layout is committed; binary artifacts populated by build

**`.github/workflows/`:**
- Single workflow `release.yml`: triggered on tag, runs goreleaser

## Key File Locations

**Entry Points:**
- `main.go`: Go binary entry — delegates to `cmd.Execute()`
- `cmd/root.go:68 Execute()`: Cobra root dispatcher
- `rushtle/src/main.rs:123 main()`: Rust binary entry (with `-c PYSCRIPT` shim short-circuit at line 128)

**Configuration:**
- `cmd/root.go:13 Config` struct + `init()` PersistentFlags (line 74) — single source of truth for plugin flags
- `rushtle/src/main.rs:18 Cli` clap struct + per-subcommand variants
- `rushtle/Cargo.toml`: Rust deps + release profile (strip, lto=thin, opt-level=z, panic=abort)

**Core Logic:**
- `proxy/deployment.go:170 DeploymentYAML`: renders Deployment manifest (legacy vs prebaked template)
- `cmd/connect.go:60 runSshuttle` / `:114 runRushtle`: the two transport entrypoints
- `cmd/sshproxy.go:82 runKubectlChunked`: chunked-stdin pump for `>1 KB` truncation workaround
- `rushtle/src/ssnet.rs:137 read_frame` / `:157 write_frame`: wire codec
- `rushtle/src/client.rs:407 ensure_link`: link health probe + auto FRAME_DELAY fallback
- `rushtle/src/server.rs:31 run`: in-pod ssnet loop

**Testing:**
- `cmd/sshproxy_test.go`: unit tests for `ParseSSHArgs` + `BuildKubectlExecArgs`
- `proxy/deployment_test.go`: unit tests for `DeploymentYAML` + helpers
- `scripts/rushtle-*-smoketest.py`: integration smoketests (no Rust unit-test suite present)

## Naming Conventions

**Go files:**
- One subcommand per file (`connect.go`, `create.go`, `delete.go`, `status.go`); helpers and hidden subcommands in `sshproxy.go`
- Tests co-located: `*_test.go` next to the implementation

**Rust files:**
- One module per file under `rushtle/src/`
- Per-OS implementations split into `firewall/{linux,macos,stub}.rs` and re-exported from `firewall/mod.rs` via `cfg(target_os = …)`

**Variables (Go):**
- `cfg` — package-level Config (`cmd/root.go:27`)
- `envXxx` constants for env var names (`cmd/sshproxy.go:19-30`)

**Variables (Rust):**
- SCREAMING_SNAKE_CASE for module-level constants (`CHUNK`, `MAX_PAYLOAD`, `PROBE_*`, `REDIRECT_PORT_TCP`)
- `tx`/`rx` suffixes for tokio channel ends; `*_task` for spawned `JoinHandle`s

## Where to Add New Code

**New Cobra subcommand:**
- File: `cmd/<name>.go`
- Pattern: declare `var <name>Cmd = &cobra.Command{…}` and register via `init() { rootCmd.AddCommand(<name>Cmd) }`
- Use `effectiveName()`, `kubectlArgs(...)`, `runKubectl(...)` helpers from `cmd/root.go` for kubectl operations

**New persistent flag:**
- File: `cmd/root.go`
- Add field to `Config` struct, then `rootCmd.PersistentFlags().XxxVar(&cfg.NewField, …)` in `init()`

**New ssnet command code:**
- File: `rushtle/src/ssnet.rs` — add `pub const CMD_NEW: u16 = 0x42xx;` and a `Frame::cmd_name` arm (line 115)
- Server handler: `rushtle/src/server.rs::run` match arm (line 119)
- Client demux: `rushtle/src/client.rs` `_demux_task` match arm (line 148)

**New per-OS firewall behavior:**
- File: `rushtle/src/firewall/{linux,macos}.rs`
- Public surface MUST stay in lockstep across linux/macos/stub: `install`, `remove`, `original_dst`. Re-export via `firewall/mod.rs`.

**New deployment template variant:**
- File: `proxy/deployment.go`
- Pattern: add a new `const xxxTmpl = "..."` and switch on it inside `DeploymentYAML`; extend `templateData` if new fields are needed
- Add cases to `proxy/deployment_test.go`

**New smoketest:**
- File: `scripts/rushtle-<feature>-smoketest.py`
- Wire into `Makefile` if it should run via `make smoketest`

**New CI step:**
- File: `.github/workflows/release.yml` (single workflow; no separate CI/lint workflow at present)

## Special Directories

**`prebuilt/`:**
- Purpose: rushtle binaries cross-built for the four supported {os,arch} targets, consumed by goreleaser
- Generated: yes — `scripts/build-rushtle-prebuilt.sh` populates locally; CI matrix overwrites for non-native targets
- Committed: subdirectory layout yes, binary contents transient

**`rushtle/target/`:**
- Purpose: standard Cargo build output
- Generated: yes
- Committed: no (gitignored)

**`demo/`:**
- Purpose: README cover assets (vhs `.tape` source + rendered mp4/gif/webp)
- Generated: rendered assets are produced by `kubectl-sshuttle-cover.convert.sh` from the `.tape` source
- Committed: yes (assets shipped via README)

**`.planning/codebase/`:**
- Purpose: GSD codebase maps (architecture, structure, conventions, etc.)
- Generated: yes (by `/gsd-map-codebase`)
- Committed: typically yes

---

*Structure analysis: 2026-05-07*

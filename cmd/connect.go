package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"

	"github.com/spf13/cobra"
)

var connectCmd = &cobra.Command{
	Use:   "connect [flags] [--] SUBNETS... [sshuttle-flags]",
	Short: "Start a tunnel through the proxy pod",
	Long: `Start a VPN tunnel through the proxy pod.

Three transport modes:

  default                 sshuttle locally, python+sshuttle in pod
  --rushtle-server        sshuttle locally, rushtle server in pod (recommended
                          fix for kubectl-exec >1KB stdin truncation seen on
                          some tailscale-fronted clusters)
  --rushtle               rushtle locally and in pod (no sshuttle/python at all)

  kubectl sshuttle connect 10.0.0.0/8
  kubectl sshuttle connect -- --dns 10.0.0.0/8 172.16.0.0/12
  kubectl sshuttle --rushtle-server connect -- --dns 10.0.0.0/8
  sudo -E kubectl sshuttle --rushtle connect 10.0.0.0/8`,
	Args:         cobra.MinimumNArgs(1),
	SilenceUsage: true,
	RunE: func(cmd *cobra.Command, args []string) error {
		// --rushtle / --rushtle-server mutex is validated in rootCmd's
		// PersistentPreRunE so it applies uniformly to create/connect/etc.

		name := effectiveName()
		check := exec.Command("kubectl", kubectlArgs("rollout", "status", "deploy/"+name, "--timeout=5s")...)
		if err := check.Run(); err != nil {
			return fmt.Errorf("proxy deploy/%s is not ready — run 'kubectl sshuttle %screate' first",
				name, modeFlagPrefix())
		}

		switch {
		case cfg.Rushtle:
			return runRushtle(args)
		case cfg.RushtleServer:
			return runSshuttle(args, "/usr/local/bin/rushtle")
		default:
			return runSshuttle(args, "python3")
		}
	},
}

// runSshuttle invokes sshuttle locally with this binary as the --ssh-cmd
// transport. `pythonBin` is what runs in the pod — `python3` for legacy
// mode, `/usr/local/bin/rushtle-shim` for --rushtle-server (which execs
// `rushtle server --compat-bootstrap` so sshuttle's bootstrap is consumed).
func runSshuttle(args []string, pythonBin string) error {
	self, err := os.Executable()
	if err != nil {
		return fmt.Errorf("resolving self binary: %w", err)
	}

	sshuttlePath, err := exec.LookPath("sshuttle")
	if err != nil {
		return fmt.Errorf("sshuttle not found in PATH — install it first (pip install sshuttle)")
	}

	sshCmd := self + " ssh-proxy"

	sshuttleArgs := []string{
		"sshuttle",
		"--ssh-cmd", sshCmd,
		"-r", "ignored",
		"--python=" + pythonBin,
	}
	sshuttleArgs = append(sshuttleArgs, args...)

	name := effectiveName()
	env := append(os.Environ(),
		envContext+"="+cfg.Context,
		envNamespace+"="+cfg.Namespace,
		envName+"="+name,
	)
	if cfg.ChunkBytes > 0 {
		env = append(env,
			fmt.Sprintf("%s=%d", envChunk, cfg.ChunkBytes),
			fmt.Sprintf("%s=%d", envChunkDelayUS, cfg.ChunkDelayUS),
		)
	}

	if cfg.RushtleServer {
		fmt.Fprintf(os.Stderr, "Starting sshuttle (local) → rushtle (pod) via deploy/%s...\n", name)
	} else {
		fmt.Fprintf(os.Stderr, "Starting sshuttle via deploy/%s...\n", name)
	}
	return syscall.Exec(sshuttlePath, sshuttleArgs, env)
}

// runRushtle invokes the local rushtle binary, telling it to run
// `rushtle server` inside the pod via kubectl exec. Pure rushtle, no
// sshuttle, no python.
//
// `--chunk-bytes` mapping: in sshuttle path the workaround chunks
// kubectl stdin at the syscall level (sshproxy.go's runKubectlChunked).
// rushtle's frames are already capped at CHUNK=768 bytes (well under the
// 1KB tailscale-fronted-apiserver truncation limit), but back-to-back
// frames can still be coalesced into a >1KB websocket message if written
// without delay. ssnet honors `RUSHTLE_FRAME_DELAY_US` to add an
// inter-frame sleep — we forward `--chunk-delay-us` into it whenever the
// user opts in via `--chunk-bytes > 0`. Same UX as sshuttle mode.
func runRushtle(args []string) error {
	bin, err := resolveRushtleBin()
	if err != nil {
		return err
	}

	kctl := kubectlExecRushtleServer()

	rushtleArgs := []string{"rushtle", "client", "--cmd", kctl}
	// `--chunk-delay-us` doubles as the probe-fallback value in --rushtle
	// mode: rushtle's startup probe sends a 2 KB PING — on timeout it sets
	// FRAME_DELAY to this value and retries. So the user's existing flag
	// drives both sshuttle's chunked stdin pump (via env) AND rushtle's
	// auto-fallback delay (via CLI), keeping the UX consistent.
	rushtleArgs = append(rushtleArgs, "--probe-fallback-us", strconv.Itoa(cfg.ChunkDelayUS))
	rushtleArgs = append(rushtleArgs, args...)

	env := os.Environ()
	if cfg.ChunkBytes > 0 {
		// Pre-set FRAME_DELAY so the very first frame already chunks —
		// useful when the user knows their cluster needs it and wants to
		// skip the probe's 3 s detection window. The probe will still
		// run and confirm the delay value works.
		env = append(env, fmt.Sprintf("RUSHTLE_FRAME_DELAY_US=%d", cfg.ChunkDelayUS))
		fmt.Fprintf(os.Stderr,
			"rushtle: pinning per-frame delay %dus (RUSHTLE_FRAME_DELAY_US) — explicit chunking requested\n",
			cfg.ChunkDelayUS)
	}

	fmt.Fprintf(os.Stderr, "Starting rushtle via deploy/%s...\n  remote: %s\n", effectiveName(), kctl)
	return syscall.Exec(bin, rushtleArgs, env)
}

// kubectlExecRushtleServer returns the shell command the local rushtle client
// will fork to talk to the in-pod rushtle server.
func kubectlExecRushtleServer() string {
	parts := []string{"kubectl"}
	if cfg.Context != "" {
		parts = append(parts, "--context", cfg.Context)
	}
	parts = append(parts,
		"-n", cfg.Namespace,
		"exec", "-i", "deploy/"+effectiveName(), "--",
		"rushtle", "server",
	)
	return strings.Join(parts, " ")
}

func resolveRushtleBin() (string, error) {
	if cfg.RushtleBin != "" {
		return cfg.RushtleBin, nil
	}
	if v := os.Getenv("RUSHTLE_BIN"); v != "" {
		return v, nil
	}
	// Prefer a sibling rushtle next to our own executable — that's how the
	// krew tarball ships it: ~/.krew/store/sshuttle/<v>/rushtle.
	if self, err := os.Executable(); err == nil {
		if real, err := filepath.EvalSymlinks(self); err == nil {
			candidate := filepath.Join(filepath.Dir(real), "rushtle")
			if st, err := os.Stat(candidate); err == nil && !st.IsDir() {
				return candidate, nil
			}
		}
	}
	bin, err := exec.LookPath("rushtle")
	if err != nil {
		return "", fmt.Errorf("rushtle binary not found (looked next to kubectl-sshuttle and in PATH) — set --rushtle-bin or `make -C rushtle`")
	}
	return bin, nil
}

func modeFlagPrefix() string {
	if cfg.Rushtle {
		return "--rushtle "
	}
	if cfg.RushtleServer {
		return "--rushtle-server "
	}
	return ""
}

func init() {
	rootCmd.AddCommand(connectCmd)
}

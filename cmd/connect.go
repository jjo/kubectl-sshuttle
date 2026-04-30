package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
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
		if cfg.Rushtle && cfg.RushtleServer {
			return fmt.Errorf("--rushtle and --rushtle-server are mutually exclusive")
		}

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
func runRushtle(args []string) error {
	bin, err := resolveRushtleBin()
	if err != nil {
		return err
	}

	kctl := kubectlExecRushtleServer()

	rushtleArgs := []string{"rushtle", "client", "--cmd", kctl}
	rushtleArgs = append(rushtleArgs, args...)

	fmt.Fprintf(os.Stderr, "Starting rushtle via deploy/%s...\n  remote: %s\n", effectiveName(), kctl)
	return syscall.Exec(bin, rushtleArgs, os.Environ())
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

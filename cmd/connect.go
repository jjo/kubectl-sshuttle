package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"syscall"

	"github.com/spf13/cobra"
)

var connectCmd = &cobra.Command{
	Use:   "connect [flags] [--] SUBNETS... [sshuttle-flags]",
	Short: "Start sshuttle tunnel through the proxy pod",
	Long: `Start an sshuttle VPN tunnel through the proxy pod.
All positional arguments are passed directly to sshuttle.
Use -- to pass sshuttle flags like --dns:

  kubectl sshuttle connect 10.0.0.0/8
  kubectl sshuttle connect -- --dns 10.0.0.0/8 172.16.0.0/12`,
	Args:         cobra.MinimumNArgs(1),
	SilenceUsage: true,
	RunE: func(cmd *cobra.Command, args []string) error {
		// Quick readiness check — if this fails, the pod isn't there.
		check := exec.Command("kubectl", kubectlArgs("rollout", "status", "deploy/"+cfg.Name, "--timeout=5s")...)
		if err := check.Run(); err != nil {
			return fmt.Errorf("proxy deploy/%s is not ready — run 'kubectl sshuttle create' first", cfg.Name)
		}

		// Find our own binary so sshuttle can call back into ssh-proxy.
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
			"--python=python3",
		}
		sshuttleArgs = append(sshuttleArgs, args...)

		// Pass config to ssh-proxy via environment.
		env := append(os.Environ(),
			envContext+"="+cfg.Context,
			envNamespace+"="+cfg.Namespace,
			envName+"="+cfg.Name,
		)

		fmt.Fprintf(os.Stderr, "Starting sshuttle via deploy/%s...\n", cfg.Name)
		// Replace process with sshuttle — it needs direct tty/signal handling.
		return syscall.Exec(sshuttlePath, sshuttleArgs, env)
	},
}

func init() {
	rootCmd.AddCommand(connectCmd)
}

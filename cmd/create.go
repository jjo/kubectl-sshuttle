package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"strings"

	"github.com/jjo/kubectl-sshuttle/proxy"
	"github.com/spf13/cobra"
)

var createCmd = &cobra.Command{
	Use:   "create",
	Short: "Create the sshuttle proxy deployment and wait for readiness",
	RunE: func(cmd *cobra.Command, args []string) error {
		yaml, err := proxy.DeploymentYAML(proxy.DeploymentConfig{
			Name:      cfg.Name,
			Namespace: cfg.Namespace,
			Image:     cfg.Image,
		})
		if err != nil {
			return fmt.Errorf("generating deployment: %w", err)
		}

		// kubectl apply -f -
		apply := exec.Command("kubectl", kubectlArgs("apply", "-f", "-")...)
		apply.Stdin = strings.NewReader(yaml)
		apply.Stdout = os.Stdout
		apply.Stderr = os.Stderr
		if err := apply.Run(); err != nil {
			return fmt.Errorf("kubectl apply: %w", err)
		}

		// Wait for rollout
		fmt.Fprintf(os.Stderr, "Waiting for proxy pod readiness (installing sshuttle + deps)...\n")
		return runKubectl("rollout", "status", "deploy/"+cfg.Name, "--timeout="+cfg.Timeout)
	},
}

func init() {
	rootCmd.AddCommand(createCmd)
}

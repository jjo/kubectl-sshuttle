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
	Short: "Create the proxy deployment and wait for readiness",
	RunE: func(cmd *cobra.Command, args []string) error {
		image := cfg.Image
		useRushtleImg := cfg.Rushtle || cfg.RushtleServer
		if useRushtleImg {
			image = cfg.RushtleImage
		}
		yaml, err := proxy.DeploymentYAML(proxy.DeploymentConfig{
			Name:      effectiveName(),
			Namespace: cfg.Namespace,
			Image:     image,
			Rushtle:   useRushtleImg,
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

		if useRushtleImg {
			fmt.Fprintf(os.Stderr, "Waiting for rushtle proxy pod readiness...\n")
		} else {
			fmt.Fprintf(os.Stderr, "Waiting for proxy pod readiness (installing sshuttle + deps)...\n")
		}
		return runKubectl("rollout", "status", "deploy/"+effectiveName(), "--timeout="+cfg.Timeout)
	},
}

func init() {
	rootCmd.AddCommand(createCmd)
}

package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"strings"

	"github.com/spf13/cobra"
)

// Config holds the global flags shared across subcommands.
type Config struct {
	Context   string
	Namespace string
	Name      string
	Image     string
	Timeout   string
}

var cfg Config

var rootCmd = &cobra.Command{
	Use:   "kubectl-sshuttle",
	Short: "Tunnel traffic through a Kubernetes cluster using sshuttle",
	Long: `kubectl-sshuttle manages a proxy pod in a Kubernetes cluster and uses
sshuttle to tunnel traffic through it. This lets you reach IPs and
subnets that are only accessible from inside the cluster.

  kubectl sshuttle --context my-cluster create
  kubectl sshuttle --context my-cluster connect 10.0.0.0/8
  kubectl sshuttle --context my-cluster delete`,
}

func Execute() {
	if err := rootCmd.Execute(); err != nil {
		os.Exit(1)
	}
}

func init() {
	defaultName := "sshuttle-proxy"
	if u := os.Getenv("USER"); u != "" {
		defaultName = u + "-sshuttle-proxy"
	}

	rootCmd.PersistentFlags().StringVar(&cfg.Context, "context", "", "kubectl context (default: current context)")
	rootCmd.PersistentFlags().StringVarP(&cfg.Namespace, "namespace", "n", "default", "namespace for the proxy pod")
	rootCmd.PersistentFlags().StringVar(&cfg.Name, "name", defaultName, "proxy deployment name")
	rootCmd.PersistentFlags().StringVar(&cfg.Image, "image", "python:3.12-slim", "proxy pod image")
	rootCmd.PersistentFlags().StringVar(&cfg.Timeout, "timeout", "120s", "readiness timeout for create")
}

// kubectlArgs returns base kubectl args with --context and --namespace set.
func kubectlArgs(extra ...string) []string {
	var args []string
	if cfg.Context != "" {
		args = append(args, "--context", cfg.Context)
	}
	args = append(args, "-n", cfg.Namespace)
	args = append(args, extra...)
	return args
}

// runKubectl executes kubectl with the given args, inheriting stdio.
func runKubectl(args ...string) error {
	cmd := exec.Command("kubectl", kubectlArgs(args...)...)
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("kubectl %s: %w", strings.Join(args, " "), err)
	}
	return nil
}

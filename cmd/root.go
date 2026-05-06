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
	Context       string
	Namespace     string
	Name          string
	Image         string
	Timeout       string
	Rushtle       bool
	RushtleServer bool
	RushtleImage  string
	RushtleBin    string
	ChunkBytes    int
	ChunkDelayUS  int
}

var cfg Config

// version is overridden at build time via:
//
//	-ldflags "-X github.com/jjo/kubectl-sshuttle/cmd.version=<git-describe>"
//
// See Makefile `VERSION` / `GIT_REV` for how this is populated. `dev` is the
// dev-build fallback (`go build` / `go install` with no ldflags).
var version = "dev"

// Version returns the embedded version string. Exposed so other packages
// (e.g. cmd/connect.go's diagnostic banner) can reference it.
func Version() string { return version }

var rootCmd = &cobra.Command{
	Use:     "kubectl-sshuttle",
	Version: version,
	Short:   "Tunnel traffic through a Kubernetes cluster using sshuttle",
	Long: `kubectl-sshuttle manages a proxy pod in a Kubernetes cluster and uses
sshuttle (or rushtle, the Rust port) to tunnel traffic through it. This
lets you reach IPs and subnets that are only accessible from inside the
cluster.

  kubectl sshuttle --context my-cluster create
  kubectl sshuttle --context my-cluster connect 10.0.0.0/8
  kubectl sshuttle --context my-cluster delete

Use --rushtle to swap the python+sshuttle proxy for a static Rust binary.
Same flow, no python bootstrap, fixes >1KB stdin truncation seen on some
tailscale-fronted clusters.`,
	// Validate mutually-exclusive flag combinations once, here, instead of
	// in each subcommand's RunE — otherwise `create` silently picks one
	// mode while `connect` rejects the same combo, which is confusing.
	PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
		if cfg.Rushtle && cfg.RushtleServer {
			return fmt.Errorf("--rushtle and --rushtle-server are mutually exclusive")
		}
		return nil
	},
}

func Execute() {
	if err := rootCmd.Execute(); err != nil {
		os.Exit(1)
	}
}

func init() {
	rootCmd.PersistentFlags().StringVar(&cfg.Context, "context", "", "kubectl context (default: current context)")
	rootCmd.PersistentFlags().StringVarP(&cfg.Namespace, "namespace", "n", "default", "namespace for the proxy pod")
	rootCmd.PersistentFlags().StringVar(&cfg.Name, "name", defaultDeployName(), "proxy deployment name")
	rootCmd.PersistentFlags().StringVar(&cfg.Image, "image", "xjjo/sshuttle", "proxy pod image (sshuttle mode; pre-baked sshuttle, runs non-root)")
	rootCmd.PersistentFlags().StringVar(&cfg.Timeout, "timeout", "120s", "readiness timeout for create")
	rootCmd.PersistentFlags().BoolVar(&cfg.Rushtle, "rushtle", false, "rushtle on both ends (rust client local, rust server in pod, no sshuttle/python)")
	rootCmd.PersistentFlags().BoolVar(&cfg.RushtleServer, "rushtle-server", false, "sshuttle locally, rushtle server in pod (rushtle binary acts as `python -c` shim — fixes the kubectl-exec >1KB stdin truncation workaround)")
	rootCmd.PersistentFlags().StringVar(&cfg.RushtleImage, "rushtle-image", "xjjo/rushtle", "container image when --rushtle or --rushtle-server")
	rootCmd.PersistentFlags().StringVar(&cfg.RushtleBin, "rushtle-bin", "", "path to local rushtle binary (default: $RUSHTLE_BIN or look up in PATH)")
	rootCmd.PersistentFlags().IntVar(&cfg.ChunkBytes, "chunk-bytes", 0, "chunk sshuttle/rushtle stdin into N-byte writes (workaround for kubectl-exec >1KB truncation on tailscale-fronted apiservers; e.g. 768)")
	rootCmd.PersistentFlags().IntVar(&cfg.ChunkDelayUS, "chunk-delay-us", 2000, "inter-chunk sleep in microseconds (only used when --chunk-bytes > 0)")
}

// effectiveName returns the deployment name to use for the current mode.
// Default mode (python+sshuttle) uses `<user>-sshuttle-proxy`. Rushtle modes
// use `<user>-rushtle-proxy` so the two can coexist in the same namespace
// without colliding.
//
// `--name` override is detected via cobra's Flags().Changed — comparing
// `cfg.Name` against `defaultDeployName()` is unsafe because a user
// passing `--name <user>-sshuttle-proxy` (the literal default value)
// would be silently rewritten to `-rushtle-proxy` in rushtle modes.
func effectiveName() string {
	if rootCmd.PersistentFlags().Changed("name") {
		return cfg.Name
	}
	if cfg.Rushtle || cfg.RushtleServer {
		return userPrefix() + "rushtle-proxy"
	}
	return cfg.Name
}

// userPrefix returns the `<user>-` prefix derived from SUDO_USER (preferred)
// or USER. Empty string if neither is set.
func userPrefix() string {
	if u := os.Getenv("SUDO_USER"); u != "" {
		return u + "-"
	}
	if u := os.Getenv("USER"); u != "" {
		return u + "-"
	}
	return ""
}

// defaultDeployName is the persistent flag default — `<user>-sshuttle-proxy`
// regardless of mode. effectiveName() rewrites to `-rushtle-proxy` when in
// a rushtle mode and the user hasn't overridden --name. Prefers SUDO_USER
// over USER so a sudo'd connect targets the same deploy name.
func defaultDeployName() string {
	return userPrefix() + "sshuttle-proxy"
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

package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"strings"
	"syscall"

	"github.com/spf13/cobra"
)

// Environment variables used to pass config from connect → sshuttle → ssh-proxy.
const (
	envContext   = "KUBECTL_SSHUTTLE_CONTEXT"
	envNamespace = "KUBECTL_SSHUTTLE_NAMESPACE"
	envName      = "KUBECTL_SSHUTTLE_NAME"
)

var sshProxyCmd = &cobra.Command{
	Use:                "ssh-proxy",
	Hidden:             true,
	Short:              "Internal: kubectl exec transport for sshuttle",
	DisableFlagParsing: true,
	SilenceUsage:       true,
	RunE: func(cmd *cobra.Command, args []string) error {
		context := os.Getenv(envContext)
		namespace := os.Getenv(envNamespace)
		name := os.Getenv(envName)
		if name == "" {
			return fmt.Errorf("ssh-proxy: %s not set (this command is called by 'connect', not directly)", envName)
		}

		remoteCmd, err := ParseSSHArgs(args)
		if err != nil {
			return err
		}

		kubectlPath, err := exec.LookPath("kubectl")
		if err != nil {
			return fmt.Errorf("kubectl not found in PATH: %w", err)
		}

		// Wrap in sh -c to match SSH semantics — sshuttle expects
		// the transport to run the command through a shell.
		shellCmd := []string{"sh", "-c", strings.Join(remoteCmd, " ")}
		execArgs := BuildKubectlExecArgs(context, namespace, name, shellCmd)
		// syscall.Exec replaces the process — sshuttle needs direct stdio piping.
		return syscall.Exec(kubectlPath, append([]string{"kubectl"}, execArgs...), os.Environ())
	},
}

// ParseSSHArgs extracts the remote command from sshuttle's SSH invocation args.
// sshuttle calls: <ssh-cmd> [-p PORT] HOST [--] PYTHON -c SCRIPT
// We return everything after "--".
func ParseSSHArgs(args []string) ([]string, error) {
	for i, arg := range args {
		if arg == "--" && i < len(args)-1 {
			return args[i+1:], nil
		}
	}
	return nil, fmt.Errorf("ssh-proxy: no '--' separator found in args: %v", args)
}

// BuildKubectlExecArgs constructs the kubectl exec argument list.
func BuildKubectlExecArgs(context, namespace, name string, remoteCmd []string) []string {
	var args []string
	if context != "" {
		args = append(args, "--context", context)
	}
	if namespace != "" {
		args = append(args, "-n", namespace)
	}
	args = append(args, "exec", "-i", "deploy/"+name, "--")
	args = append(args, remoteCmd...)
	return args
}

func init() {
	rootCmd.AddCommand(sshProxyCmd)
}

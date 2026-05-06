package cmd

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/spf13/cobra"
)

// Environment variables used to pass config from connect → sshuttle → ssh-proxy.
const (
	envContext   = "KUBECTL_SSHUTTLE_CONTEXT"
	envNamespace = "KUBECTL_SSHUTTLE_NAMESPACE"
	envName      = "KUBECTL_SSHUTTLE_NAME"

	// envChunk forces ssh-proxy to fork+pipe into kubectl with chunked stdin
	// instead of syscall.Exec'ing kubectl directly. Set the value to the
	// max bytes per chunk (e.g. 768). Use envChunkDelayUS to add an
	// inter-chunk sleep.
	envChunk        = "KUBECTL_SSHUTTLE_CHUNK_BYTES"
	envChunkDelayUS = "KUBECTL_SSHUTTLE_CHUNK_DELAY_US"
)

var sshProxyCmd = &cobra.Command{
	Use:                "ssh-proxy",
	Hidden:             true,
	Short:              "Internal: kubectl exec transport for sshuttle",
	DisableFlagParsing: true,
	SilenceUsage:       true,
	RunE: func(cmd *cobra.Command, args []string) error {
		ctxName := os.Getenv(envContext)
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
		execArgs := BuildKubectlExecArgs(ctxName, namespace, name, shellCmd)

		chunkBytes, _ := strconv.Atoi(os.Getenv(envChunk))
		if chunkBytes > 0 {
			delayUs, _ := strconv.Atoi(os.Getenv(envChunkDelayUS))
			return runKubectlChunked(kubectlPath, execArgs, chunkBytes, time.Duration(delayUs)*time.Microsecond)
		}
		// Default: replace process — preserves direct stdio piping +
		// signal handling that sshuttle expects.
		return syscall.Exec(kubectlPath, append([]string{"kubectl"}, execArgs...), os.Environ())
	},
}

// runKubectlChunked spawns kubectl as a child and proxies stdio with
// chunking on the path that goes TO kubectl (= sshuttle's stdin).
// Each chunk is written separately + flushed + slept, so kubectl reads
// each chunk as its own pipe-read and forwards it as a separate
// websocket frame. Survives middleware (e.g. tailscale-fronted apiserver
// proxies) that drop single writes above ~1 KB.
//
// Stdout/stderr from kubectl are passed through unchanged (no chunking
// needed on the read direction — that path is already framed by the
// ssnet protocol coming from rushtle).
func runKubectlChunked(kubectlPath string, args []string, chunkBytes int, delay time.Duration) error {
	if chunkBytes <= 0 {
		chunkBytes = 768
	}
	fmt.Fprintf(os.Stderr, "ssh-proxy: chunking sshuttle stdin into %d-byte writes (delay=%v)\n", chunkBytes, delay)

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	cmd := exec.CommandContext(ctx, kubectlPath, args...)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return fmt.Errorf("kubectl stdin pipe: %w", err)
	}

	if err := cmd.Start(); err != nil {
		return fmt.Errorf("kubectl start: %w", err)
	}

	// Pump os.Stdin → kubectl stdin in chunks.
	//
	// On write error (kubectl pipe broken / kubectl died) we cancel the
	// context — that propagates through `exec.CommandContext` to terminate
	// kubectl and unblock cmd.Wait() with an exit error sshuttle can see.
	// Without this the goroutine returned silently and the parent could
	// wedge if kubectl somehow stayed up after its stdin was rejected.
	go func() {
		defer stdin.Close()
		buf := make([]byte, chunkBytes)
		for {
			n, rerr := os.Stdin.Read(buf)
			if n > 0 {
				if _, werr := stdin.Write(buf[:n]); werr != nil {
					fmt.Fprintf(os.Stderr, "ssh-proxy: kubectl stdin write: %v\n", werr)
					cancel()
					return
				}
				if delay > 0 {
					time.Sleep(delay)
				}
			}
			if rerr == io.EOF {
				return
			}
			if rerr != nil {
				fmt.Fprintf(os.Stderr, "ssh-proxy: stdin read: %v\n", rerr)
				cancel()
				return
			}
		}
	}()

	if err := cmd.Wait(); err != nil {
		// Surface kubectl's exit code for sshuttle's error reporting.
		if ee, ok := err.(*exec.ExitError); ok {
			os.Exit(ee.ExitCode())
		}
		return err
	}
	return nil
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

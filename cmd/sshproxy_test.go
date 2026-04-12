package cmd

import (
	"reflect"
	"testing"
)

func TestParseSSHArgs(t *testing.T) {
	tests := []struct {
		name    string
		args    []string
		want    []string
		wantErr bool
	}{
		{
			name: "standard sshuttle invocation",
			args: []string{"-p", "0", "ignored", "--", "python3", "-c", "import sshuttle"},
			want: []string{"python3", "-c", "import sshuttle"},
		},
		{
			name: "no port flag",
			args: []string{"ignored", "--", "/usr/bin/python3", "-c", "code"},
			want: []string{"/usr/bin/python3", "-c", "code"},
		},
		{
			name:    "no separator",
			args:    []string{"-p", "0", "host", "python3"},
			wantErr: true,
		},
		{
			name:    "separator at end",
			args:    []string{"-p", "0", "host", "--"},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParseSSHArgs(tt.args)
			if (err != nil) != tt.wantErr {
				t.Fatalf("ParseSSHArgs() error = %v, wantErr %v", err, tt.wantErr)
			}
			if !tt.wantErr && !reflect.DeepEqual(got, tt.want) {
				t.Errorf("ParseSSHArgs() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestBuildKubectlExecArgs(t *testing.T) {
	tests := []struct {
		name      string
		context   string
		namespace string
		depName   string
		remoteCmd []string
		want      []string
	}{
		{
			name:      "with context",
			context:   "bm-dev",
			namespace: "default",
			depName:   "jjo-sshuttle-proxy",
			remoteCmd: []string{"python3", "-c", "code"},
			want:      []string{"--context", "bm-dev", "-n", "default", "exec", "-i", "deploy/jjo-sshuttle-proxy", "--", "python3", "-c", "code"},
		},
		{
			name:      "no context",
			context:   "",
			namespace: "kube-system",
			depName:   "test-proxy",
			remoteCmd: []string{"sh", "-c", "echo hi"},
			want:      []string{"-n", "kube-system", "exec", "-i", "deploy/test-proxy", "--", "sh", "-c", "echo hi"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := BuildKubectlExecArgs(tt.context, tt.namespace, tt.depName, tt.remoteCmd)
			if !reflect.DeepEqual(got, tt.want) {
				t.Errorf("BuildKubectlExecArgs() = %v, want %v", got, tt.want)
			}
		})
	}
}

package proxy

import (
	"strings"
	"testing"
)

func TestDeploymentYAML(t *testing.T) {
	yaml, err := DeploymentYAML(DeploymentConfig{
		Name:      "jjo-sshuttle-proxy",
		Namespace: "default",
		Image:     "python:3.12-slim",
	})
	if err != nil {
		t.Fatal(err)
	}

	for _, want := range []string{
		"name: jjo-sshuttle-proxy",
		"namespace: default",
		"image: python:3.12-slim",
		"app: jjo-sshuttle-proxy",
		"touch /tmp/ready",
		"pip install sshuttle",
		"readinessProbe:",
		"test, -f, /tmp/ready",
	} {
		if !strings.Contains(yaml, want) {
			t.Errorf("YAML missing %q\n---\n%s", want, yaml)
		}
	}
}

func TestDeploymentYAMLCustomImage(t *testing.T) {
	yaml, err := DeploymentYAML(DeploymentConfig{
		Name:      "test-proxy",
		Namespace: "kube-system",
		Image:     "python:3.11-alpine",
	})
	if err != nil {
		t.Fatal(err)
	}

	for _, want := range []string{
		"name: test-proxy",
		"namespace: kube-system",
		"image: python:3.11-alpine",
		"app: test-proxy",
	} {
		if !strings.Contains(yaml, want) {
			t.Errorf("YAML missing %q\n---\n%s", want, yaml)
		}
	}
}

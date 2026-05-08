package proxy

import (
	"strings"
	"testing"
)

func TestDeploymentYAMLPrebakedSshuttle(t *testing.T) {
	yaml, err := DeploymentYAML(DeploymentConfig{
		Name:      "jjo-sshuttle-proxy",
		Namespace: "default",
		Image:     "xjjo/sshuttle",
	})
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{
		"name: jjo-sshuttle-proxy",
		"namespace: default",
		"image: xjjo/sshuttle",
		"app: jjo-sshuttle-proxy",
		"runAsNonRoot: true",
		"runAsUser: 1001",
		"imagePullPolicy: Always", // no tag = latest = Always
	} {
		if !strings.Contains(yaml, want) {
			t.Errorf("YAML missing %q\n---\n%s", want, yaml)
		}
	}
	for _, unwanted := range []string{
		"apt-get update",
		"pip install sshuttle",
	} {
		if strings.Contains(yaml, unwanted) {
			t.Errorf("YAML should NOT contain %q (prebaked image)\n---\n%s", unwanted, yaml)
		}
	}
}

func TestDeploymentYAMLPrebakedRushtle(t *testing.T) {
	yaml, err := DeploymentYAML(DeploymentConfig{
		Name:      "jjo-rushtle-proxy",
		Namespace: "default",
		Image:     "xjjo/rushtle:v0.2.0",
		Rushtle:   true,
	})
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{
		"image: xjjo/rushtle:v0.2.0",
		"runAsNonRoot: true",
		"app.kubernetes.io/component: rushtle",
		"imagePullPolicy: IfNotPresent", // pinned tag
	} {
		if !strings.Contains(yaml, want) {
			t.Errorf("YAML missing %q\n---\n%s", want, yaml)
		}
	}
}

func TestDeploymentYAMLLegacyImage(t *testing.T) {
	yaml, err := DeploymentYAML(DeploymentConfig{
		Name:      "test-proxy",
		Namespace: "kube-system",
		Image:     "python:3.12-slim",
	})
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{
		"name: test-proxy",
		"image: python:3.12-slim",
		"apt-get update",
		"pip install sshuttle",
	} {
		if !strings.Contains(yaml, want) {
			t.Errorf("YAML missing %q\n---\n%s", want, yaml)
		}
	}
	if strings.Contains(yaml, "runAsNonRoot") {
		t.Errorf("legacy image should NOT add runAsNonRoot\n---\n%s", yaml)
	}
}

func TestImagePullPolicy(t *testing.T) {
	cases := []struct {
		image, want string
	}{
		{"xjjo/rushtle", "Always"},
		{"xjjo/rushtle:latest", "Always"},
		{"xjjo/rushtle:v1.2.3", "IfNotPresent"},
		{"ghcr.io/foo/bar:dev", "IfNotPresent"},
		{"localhost:5000/foo", "Always"},
		{"localhost:5000/foo:latest", "Always"},
		{"localhost:5000/foo:v1", "IfNotPresent"},
		{"foo@sha256:abc", "Always"},
		{"foo:1@sha256:abc", "IfNotPresent"},
	}
	for _, c := range cases {
		got := imagePullPolicy(c.image)
		if got != c.want {
			t.Errorf("imagePullPolicy(%q) = %q, want %q", c.image, got, c.want)
		}
	}
}

func TestIsPrebakedImage(t *testing.T) {
	cases := []struct {
		image string
		want  bool
	}{
		{"xjjo/sshuttle", true},
		{"xjjo/sshuttle:1.2", true},
		{"xjjo/rushtle", true},
		{"ghcr.io/foo/sshuttle:dev", true},
		{"ghcr.io/foo/rushtle-dev:latest", true},
		{"localhost:5000/team/my-sshuttle:beta", true},
		{"python:3.12-slim", false},
		{"ubuntu:22.04", false},
		{"alpine", false},
		{"foo/sshuttle@sha256:abc", true},
	}
	for _, c := range cases {
		got := isPrebakedImage(c.image)
		if got != c.want {
			t.Errorf("isPrebakedImage(%q) = %v, want %v", c.image, got, c.want)
		}
	}
}

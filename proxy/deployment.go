package proxy

import (
	"bytes"
	"strings"
	"text/template"
)

// DeploymentConfig holds the parameters for generating the proxy Deployment YAML.
type DeploymentConfig struct {
	Name      string
	Namespace string
	Image     string
	// Rushtle controls only the deployment label; whether to use the
	// prebaked-image template is a function of the Image name itself
	// (`*/sshuttle*` or `*/rushtle*` → prebaked, no apt/pip, non-root).
	Rushtle bool
}

// imagePullPolicy returns "Always" when the image reference ends in `:latest`
// or has no explicit tag (which Kubernetes treats as `:latest`). Anything
// else gets `IfNotPresent` so a pinned tag isn't re-pulled on every restart.
func imagePullPolicy(image string) string {
	tag := imageTag(image)
	if tag == "" || tag == "latest" {
		return "Always"
	}
	return "IfNotPresent"
}

// isPrebakedImage returns true when the image basename (last path component,
// minus tag) contains "sshuttle" or "rushtle". Those images are expected to
// have sshuttle/rushtle baked in and to run as a non-root user — so the
// deployment template can skip the legacy `apt-get + pip install` startup
// command and add restrictive securityContext.
func isPrebakedImage(image string) bool {
	ref := image
	if at := strings.LastIndex(ref, "@"); at >= 0 {
		ref = ref[:at]
	}
	base := ref
	if slash := strings.LastIndex(ref, "/"); slash >= 0 {
		base = ref[slash+1:]
	}
	if colon := strings.LastIndex(base, ":"); colon >= 0 {
		base = base[:colon]
	}
	return strings.Contains(base, "sshuttle") || strings.Contains(base, "rushtle")
}

// imageTag returns the tag portion of an image reference (empty if none).
// Handles `host:port/path/name:tag@digest` correctly.
func imageTag(image string) string {
	ref := image
	if at := strings.LastIndex(ref, "@"); at >= 0 {
		ref = ref[:at]
	}
	if slash := strings.LastIndex(ref, "/"); slash >= 0 {
		if colon := strings.LastIndex(ref[slash+1:], ":"); colon >= 0 {
			return ref[slash+1+colon+1:]
		}
	} else if colon := strings.LastIndex(ref, ":"); colon >= 0 {
		return ref[colon+1:]
	}
	return ""
}

// prebakedTmpl: image already has sshuttle/rushtle baked in; pod runs as
// non-root, no startup commands.
const prebakedTmpl = `apiVersion: apps/v1
kind: Deployment
metadata:
  name: {{ .Name }}
  namespace: {{ .Namespace }}
  labels:
    app.kubernetes.io/name: {{ .Name }}
    app.kubernetes.io/component: {{ .Component }}
spec:
  replicas: 1
  selector:
    matchLabels:
      app: {{ .Name }}
  template:
    metadata:
      labels:
        app: {{ .Name }}
        app.kubernetes.io/component: {{ .Component }}
    spec:
      securityContext:
        runAsNonRoot: true
        runAsUser: 1001
        runAsGroup: 1001
        seccompProfile:
          type: RuntimeDefault
      containers:
        - name: proxy
          image: {{ .Image }}
          imagePullPolicy: {{ .PullPolicy }}
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: false
            capabilities:
              drop: [ALL]
          readinessProbe:
            exec:
              command: [test, -f, /tmp/ready]
            initialDelaySeconds: 2
            periodSeconds: 3
          resources:
            requests:
              cpu: 10m
              memory: 16Mi
            limits:
              cpu: 200m
              memory: 256Mi
`

// legacyTmpl: vanilla python image; install sshuttle at runtime, run as root.
// Used when --image points at something that doesn't look like a prebaked
// sshuttle/rushtle image.
const legacyTmpl = `apiVersion: apps/v1
kind: Deployment
metadata:
  name: {{ .Name }}
  namespace: {{ .Namespace }}
spec:
  replicas: 1
  selector:
    matchLabels:
      app: {{ .Name }}
  template:
    metadata:
      labels:
        app: {{ .Name }}
    spec:
      containers:
        - name: proxy
          image: {{ .Image }}
          imagePullPolicy: {{ .PullPolicy }}
          command:
            - sh
            - -c
            - |
              apt-get update && apt-get install -y openssh-client &&
              pip install sshuttle &&
              touch /tmp/ready &&
              sleep infinity
          readinessProbe:
            exec:
              command: [test, -f, /tmp/ready]
            initialDelaySeconds: 5
            periodSeconds: 5
          resources:
            requests:
              cpu: 10m
              memory: 256Mi
            limits:
              cpu: 100m
              memory: 1Gi
`

// templateData adds derived fields for template rendering.
type templateData struct {
	DeploymentConfig
	PullPolicy string
	Component  string // "sshuttle" or "rushtle" — drives the k8s label
}

// DeploymentYAML renders the proxy Deployment manifest.
func DeploymentYAML(c DeploymentConfig) (string, error) {
	tmpl := legacyTmpl
	if isPrebakedImage(c.Image) {
		tmpl = prebakedTmpl
	}
	t, err := template.New("deployment").Parse(tmpl)
	if err != nil {
		return "", err
	}
	component := "sshuttle"
	if c.Rushtle {
		component = "rushtle"
	}
	data := templateData{
		DeploymentConfig: c,
		PullPolicy:       imagePullPolicy(c.Image),
		Component:        component,
	}
	var buf bytes.Buffer
	if err := t.Execute(&buf, data); err != nil {
		return "", err
	}
	return buf.String(), nil
}

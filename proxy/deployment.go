package proxy

import (
	"bytes"
	"text/template"
)

// DeploymentConfig holds the parameters for generating the proxy Deployment YAML.
type DeploymentConfig struct {
	Name      string
	Namespace string
	Image     string
}

const deploymentTmpl = `apiVersion: apps/v1
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

// DeploymentYAML renders the proxy Deployment manifest.
func DeploymentYAML(c DeploymentConfig) (string, error) {
	t, err := template.New("deployment").Parse(deploymentTmpl)
	if err != nil {
		return "", err
	}
	var buf bytes.Buffer
	if err := t.Execute(&buf, c); err != nil {
		return "", err
	}
	return buf.String(), nil
}

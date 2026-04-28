# kubectl-sshuttle

NOTE: below demo "video" as taken _from my laptop_, _not_ from inside an exec'd Pod.:

<p align="center">
  <img src="demo/kubectl-sshuttle-cover.webp" alt="kubectl-sshuttle demo" width="800" />
</p>

A kubectl plugin that tunnels traffic through a Kubernetes cluster using [sshuttle](https://github.com/sshuttle/sshuttle).

It deploys a lightweight proxy pod inside the cluster and uses `sshuttle` to route traffic through it, letting you reach IPs and subnets that are only accessible from within the cluster network.

## Install

### From source

```bash
go install github.com/jjo/kubectl-sshuttle@latest
```

### With krew

```bash
kubectl krew install sshuttle
```

### Prerequisites

- `kubectl` configured with cluster access
- `sshuttle` installed locally (`pip install sshuttle`)

## Usage

```bash
# Create the proxy pod and wait for it to be ready
kubectl sshuttle --context my-cluster create

# Tunnel traffic to specific subnets
kubectl sshuttle --context my-cluster connect 10.0.0.0/8
kubectl sshuttle --context my-cluster connect 192.168.1.0/24 172.16.0.0/12

# Pass sshuttle flags (use -- to separate)
kubectl sshuttle --context my-cluster connect -- --dns 10.0.0.0/8

# Check proxy pod status
kubectl sshuttle --context my-cluster status

# Clean up
kubectl sshuttle --context my-cluster delete
```

## Commands

| Command   | Description                                              |
|-----------|----------------------------------------------------------|
| `create`  | Deploy the proxy pod and wait for readiness              |
| `connect` | Start sshuttle tunnel (requires `create` first)          |
| `status`  | Show proxy pod status                                    |
| `delete`  | Remove the proxy deployment                              |

## Flags

| Flag          | Default                    | Description                    |
|---------------|----------------------------|--------------------------------|
| `--context`   | current context            | kubectl context                |
| `-n, --namespace` | `default`              | namespace for the proxy pod    |
| `--name`      | `$USER-sshuttle-proxy`     | proxy deployment name          |
| `--image`     | `python:3.12-slim`         | proxy pod image                |
| `--timeout`   | `120s`                     | readiness timeout for `create` |

## How it works

1. **`create`** deploys a single-replica Deployment running a Python image. The pod installs `sshuttle` and `openssh-client`, then signals readiness via a file-based probe.

2. **`connect`** verifies the proxy pod is ready, then execs `sshuttle` locally with a custom SSH transport (`--ssh-cmd`) that pipes through `kubectl exec` into the proxy pod.

3. All traffic to the specified subnets is routed through the cluster via sshuttle's transparent proxy.

```
laptop --> sshuttle --> kubectl exec --> proxy pod --> cluster network --> target IPs
```

## License

Apache-2.0

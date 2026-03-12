# P4: Container Image & Helm Chart

**Status**: not started

**Spec reference**: `docs/spec/09-multi-agent.md`, `docs/spec/08-kubernetes.md`

## Dependencies
- **Blocked by**: P1-supervisor-main (working supervisor binary), P2-s3-upload (S3 credentials in K8s)
- **Blocks**: P4-cross-agent

## Parallelizable with
- All P3 tasks (container image just packages the binary)

## What needs to be done

### Container Image
- Update `deploy/Dockerfile`:
  - Multi-stage: build supervisor + CLI in builder stage
  - Runtime: debian-slim + ca-certificates + mitmdump + python3 (for mitmdump)
  - Install mitmdump via pip
  - Copy supervisor and argus CLI binaries
  - Generate default CA on build (or first run)
  - Entrypoint: supervisor
- Publish to container registry (GitHub Container Registry or similar)

### Helm Chart
- `deploy/helm/argus/`:
  - `Chart.yaml`, `values.yaml`
  - `templates/deployment.yaml`: per-agent pod with SYS_PTRACE capability
  - `templates/serviceaccount.yaml`: for IRSA/Workload Identity
  - `templates/configmap.yaml`: supervisor config
  - `templates/pvc.yaml`: /data volume (or emptyDir)
  - Values: agent_id, agent_command, S3 bucket, resource limits, image tag
  - No hostPID, no privileged, no SYS_ADMIN — only SYS_PTRACE

### Kubernetes Compatibility
- Yama ptrace_scope=1 compatible (parent traces child only)
- Default AppArmor profile compatible
- Default seccomp profile compatible (supervisor installs its own on child)

## How to test
```bash
docker build -t argus-supervisor -f deploy/Dockerfile .
helm template deploy/helm/argus/ --set agentId=test --set agentCommand="echo hello"
```

## Branch
- **Branch**: `p4-container-image`
- **Target**: `main`

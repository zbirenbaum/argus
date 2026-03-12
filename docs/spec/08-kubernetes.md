# Kubernetes Deployment

## Requirements

| Requirement | Detail |
|-------------|--------|
| Capability | SYS_PTRACE (only hard requirement) |
| Providers | GKE Standard, GKE Autopilot, EKS EC2, EKS Fargate, AKS |
| Not needed | SYS_ADMIN, NET_ADMIN, privileged mode |
| Yama | ptrace_scope 1 (default) is fine — supervisor traces descendants only |
| AppArmor | K8s 1.31+ default is fine — ptrace within same pod |
| Seccomp | SYS_PTRACE capability auto-allows ptrace syscall in containerd |
| Credentials | IRSA (EKS) or Workload Identity (GKE) for S3/GCS |

## Pod Structure

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: agent-argus
spec:
  serviceAccountName: argus-sa
  containers:
    - name: agent
      image: your-registry/agent-argus:latest
      securityContext:
        capabilities:
          add: ["SYS_PTRACE"]
      volumeMounts:
        - name: data
          mountPath: /data
        - name: workspace
          mountPath: /workspace
  volumes:
    - name: data
      emptyDir:
        sizeLimit: 4Gi
    - name: workspace
      emptyDir: {}
```

Single container. Supervisor is entrypoint, handles S3 streaming directly — no sidecar.

## Invisibility

**Detection vectors:**
- /proc/self/status TracerPid field (non-zero when traced)
- ptrace(PTRACE_TRACEME) fails if already traced
- Timing side channels (traced syscalls measurably slower)

**Mitigations:**
- Bind-mount modified /proc/self/status or intercept reads via ptrace
- Intercept agent's ptrace syscall, return success
- Timing: impractical for network-I/O-bound agent workloads
- AI agents (Python/Node.js): don't check

**Environment detection (proxy/TLS):**
- Agent could detect HTTPS_PROXY, custom CA, SSLKEYLOGFILE
- If full invisibility required: rely on ptrace-only TLS capture (post-MVP)
- Current agent frameworks: don't check

## Performance

| Source | Cost | Mitigation |
|--------|------|------------|
| Syscall interception | 2 ctx switches per trapped syscall | seccomp-bpf: only trap ~55 syscalls |
| Buffer capture | process_vm_readv per read/write | Single syscall, bulk read |
| CAS writes | Local disk I/O | Async flush |
| Tree recomputation | Hash affected subtree | Only changed paths |
| Event appends | Sequential I/O | Buffered writer |

**Agent workloads:** Long LLM API waits, bursty small file I/O. Overhead during I/O bursts: 10-30%. During LLM wait: ~0%. Net end-to-end: <5%.

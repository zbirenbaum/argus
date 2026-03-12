# Multi-Agent & Orchestration

Each agent runs its own supervisor. All stream to the same S3 bucket. Cross-agent visibility comes from querying the shared bucket — not from cross-container ptrace.

## Container Image

```dockerfile
# --- sandbox-base (published to registry) ---
FROM rust:1.77-slim AS builder
COPY . /src
RUN cargo build --release --target x86_64-unknown-linux-musl

FROM python:3.12-slim AS mitm
RUN pip install mitmproxy

FROM ubuntu:24.04 AS sandbox-base
COPY --from=builder /src/target/x86_64-unknown-linux-musl/release/supervisor /usr/local/bin/supervisor
COPY --from=mitm /usr/local/bin/mitmdump /usr/local/bin/mitmdump
COPY --from=mitm /usr/local/lib/python3.12/site-packages /usr/local/lib/python3.12/site-packages
```

```dockerfile
# --- Per-agent image ---
FROM your-registry/sandbox-base:latest AS sandbox

FROM python:3.12-slim
COPY --from=sandbox /usr/local/bin/supervisor /usr/local/bin/supervisor
COPY --from=sandbox /usr/local/bin/mitmdump /usr/local/bin/mitmdump
COPY --from=sandbox /usr/local/lib/python3.12/site-packages /usr/local/lib/python3.12/site-packages

COPY . /app
COPY supervisor.yaml /etc/supervisor.yaml
ENTRYPOINT ["/usr/local/bin/supervisor", "--config", "/etc/supervisor.yaml", "--"]
CMD ["python3", "/app/agent.py"]
```

## Helm Chart

```yaml
# values.yaml
agents:
  - name: researcher
    image: my-registry/research-agent:latest
  - name: coder
    image: my-registry/coding-agent:latest
storage:
  bucket: my-sandbox-bucket
  region: us-west-2
serviceAccount:
  annotations:
    eks.amazonaws.com/role-arn: arn:aws:iam::123456789:role/sandbox-s3-writer
```

**Generates per agent:**
- Pod with supervisor entrypoint, SYS_PTRACE, volume mounts
- ConfigMap with supervisor.yaml (unique agent_id, shared bucket)
- Shared ServiceAccount with S3 write permissions

**Also generates:**
- Optional shared PostgreSQL StatefulSet (uninstrumented, queries captured via network)

## Auto-Registration

Each supervisor emits `agent_start` to S3 on boot:

```json
{"seq":0,"type":"agent_start","agent_id":"researcher-abc",
 "ts_wall":"2026-03-11T14:00:00Z","config":{...},"node":"gke-pool-1","pod":"researcher-abc-xyz"}
```

No central coordinator. Query layer discovers agents via S3 listing.

## Clock Synchronization

| Timestamp | Source | Cross-node | Use |
|-----------|--------|------------|-----|
| ts_monotonic | CLOCK_MONOTONIC_RAW | No | Local ordering |
| ts_wall | CLOCK_REALTIME (NTP) | Yes, ~1ms | Cross-agent correlation |

**Within node:** Containers share host clock. CLOCK_MONOTONIC consistent.
**Across nodes:** CLOCK_REALTIME via NTP. Managed K8s: <1ms within region.

**Ordering:** Within agent: seq. Across agents: ts_wall, break ties by agent_id.

**Vector clock:** Reserved in event schema (`vclock` field). Implementation deferred. For future sub-millisecond causal ordering on shared resources.

## Same-Pod Multi-Container

**Per-container supervisor (recommended):** Each container runs own supervisor. All stream to same bucket with different agent_id. Cross-container correlation via resource path + ts_wall in unified event log.

**Shared process namespace (alternative):** shareProcessNamespace: true. Single supervisor traces all. Yama scope restrictions apply.

## Cross-Agent Query Layer

Standalone service or CLI. Reads S3. No running supervisor needed.

```
GET /agents                                    → List all agents in bucket
GET /timeline?agents=researcher,coder&since=1h → Interleaved by ts_wall
GET /correlation?write_agent=a&read_agent=b    → Write-then-read patterns
```

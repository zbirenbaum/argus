# Multi-Agent & Orchestration

Each agent runs its own supervisor. All stream to the same S3 bucket. Cross-agent visibility comes from querying the shared bucket — not from cross-container ptrace.

## How Shared Resource Access Is Captured

The supervisor doesn't need special multi-agent logic to see shared resources. The existing capture layers already surface them:

| Shared Resource | How Supervisor Sees It | Event Type |
|----------------|----------------------|------------|
| S3/GCS bucket | HTTP PUT/GET via TLS decryption (Layer 1/2) — URL contains bucket + key | `http_request` with path like `s3://bucket/data.csv` |
| PostgreSQL | Wire protocol over TCP — captured via network interception | `net_send`/`net_recv` with parseable frontend/backend messages |
| Redis | RESP protocol over TCP — text-based, trivial to parse | `net_send`/`net_recv` with parseable commands |
| HTTP APIs | Full request/response via MITM proxy | `http_request`/`http_response` |
| Shared NFS/EFS mount | Normal file syscalls (open/read/write) on the mount | `read`/`write` events with paths on the shared mount |

Agent A writes `PUT s3://bucket/model.pt` → captured as `http_request` event with the S3 key.
Agent B reads `GET s3://bucket/model.pt` → captured as `http_request` event with the same key.
Both events are in the same S3 bucket. The query layer correlates them by resource path and ts_wall.

The key insight: **as long as each agent is instrumented, the shared resource doesn't need to be.** The S3 bucket, the PostgreSQL database, the Redis cache — none of them need modification. The supervisors observe from the agent side.

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

**Vector clock (see `02-event-schema.md` for full schema):**

The `vclock` field is a `HashMap<agent_id, counter>` reserved in the event envelope. It enables causal ordering beyond NTP accuracy. When Agent B reads a resource that Agent A wrote, B merges A's counter into its vector clock — establishing that B's subsequent actions are causally after A's write.

Not MVP. The field exists in the schema (serialized as null), the structure is defined, and consumers can ignore it until it's populated. Implementation requires agents to exchange counter state via a shared S3 metadata file or lightweight coordination service.

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

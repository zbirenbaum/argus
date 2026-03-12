# P4: Cross-Agent Queries & Orchestration

**Status**: not started

**Spec reference**: `docs/spec/09-multi-agent.md`, `docs/spec/10-api-reference.md`

## Dependencies
- **Blocked by**: P4-container-image (multi-agent requires deployable images), P3-query-api (extends query layer), P2-s3-upload (reads other agents' data from S3)
- **Blocks**: nothing — terminal feature

## Parallelizable with
- P5-polish tasks

## What needs to be done

### Agent Discovery
- `GET /agents`: scan S3 for `events/{agent_id}/` prefixes, read agent_start events
- Auto-registration: supervisor writes agent_start event to S3 on boot

### Cross-Agent Query Layer
- `GET /timeline?agents=a,b&after_ts=&before_ts=`: merged event timeline across agents, sorted by ts_wall
- `GET /correlation?path=&agents=`: find events across agents touching same path
- Read remote agents' event segments directly from S3

### CLI
- `argus agents` — list all agents in bucket
- `argus timeline --agents a,b` — merged timeline
- `argus correlate --path /workspace/foo.py` — cross-agent activity on a path

### Helm Chart Updates
- Per-agent pod template with unique agent_id
- Shared ServiceAccount for S3 access
- Optional shared PostgreSQL StatefulSet for agents that need a database

## How to test
```bash
cargo test -p sandbox --lib api -- --ignored
cargo test -p argus-cli -- --ignored
```
Integration test (ignored, needs S3 + multiple agents): deploy two agents, both write to same bucket, query timeline across both.

## Branch
- **Branch**: `p4-cross-agent`
- **Target**: `main`

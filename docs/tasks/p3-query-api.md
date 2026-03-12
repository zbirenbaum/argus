# P3: Query & Content API

**Status**: not started

**Spec reference**: `docs/spec/10-api-reference.md`, `docs/spec/05-indexing-queries.md`

## Dependencies
- **Blocked by**: P3-indexes (query engine), P3-merkle-tree (tree API), P2-cas (content retrieval), P2-pause-resume-api (axum server foundation)
- **Blocks**: P3-realtime-api (extends same server)

## Parallelizable with
- P3-restore

## What needs to be done
- Extend `crates/sandbox/src/api/`:

### Event Queries
- `GET /events?path=&pid=&type=&after_seq=&before_seq=&after_ts=&before_ts=&limit=&offset=`
- `GET /file_history/{path}` — all events for a file path
- `GET /process_tree` — process hierarchy with optional stdio inline

### Stdio Reconstruction
- `GET /stdio/{pid}?stream=stdout|stderr|stdin` — concatenated output for a process
- `GET /pipeline/{pipe_id}` — data flow through a pipe

### Content
- `GET /content/{hash}` — raw content by CAS hash
- `GET /content/{hash}/text` — content decoded as UTF-8
- `GET /diff?from_hash=&to_hash=` — unified diff between two content hashes

### Tree
- `GET /tree?seq=&ts=&path=` — directory listing from Merkle tree at point in time
- `GET /tree/diff?from_seq=&to_seq=` — changed files between two points

### System
- `GET /health` — liveness check
- `GET /storage/status` — CAS stats, S3 upload queue, buffer usage
- `GET /connections` — active network connections

## How to test
```bash
cargo test -p sandbox --lib api -- --ignored
```
Integration tests: start server, ingest sample events, query by each filter type, verify stdio reconstruction output.

## Branch
- **Branch**: `p3-query-api`
- **Target**: `main`

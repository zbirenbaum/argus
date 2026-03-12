# P5: Polish & Production Readiness

**Status**: not started

**Spec reference**: `docs/spec/06-agent-controls.md`, `docs/spec/10-api-reference.md`

## Dependencies
- **Blocked by**: P3-realtime-api (WebSocket infra), P2-pause-resume-api (approval system), all core features
- **Blocks**: nothing — final phase

## Parallelizable with
- P4 tasks

## What needs to be done

### WebSocket Approvals
- `ws://…/ws/approvals`: bidirectional — receive pending approvals, send approve/deny
- Webhook notifications: POST to configurable URL when approval pending

### CLI Polish (`crates/cli/`)
- Consistent output formatting (table, JSON, plain)
- All REST endpoints accessible via CLI subcommands
- Error messages with context and suggestions
- `--format json|table|plain` flag
- Help text for all commands

### Graceful Shutdown
- On SIGTERM:
  1. Stop accepting new API connections
  2. Pause all traced processes
  3. Flush current event segment
  4. Persist digest cache to disk + S3
  5. Upload final event segment to S3
  6. Create final checkpoint
  7. Exit

### Config Validation
- On startup: validate all config fields with clear error messages
- Check S3 connectivity (HEAD bucket)
- Check data_dir is writable
- Check workspace_dir exists

### Health Checks
- `GET /health`: K8s liveness/readiness probe
  - Liveness: supervisor alive, tracer loop running
  - Readiness: S3 connected, digest cache loaded, mitmdump running

## How to test
```bash
cargo test -p sandbox --lib api
cargo test -p argus-cli
```
Integration tests: graceful shutdown flushes all data, health check returns correct status, CLI commands produce expected output.

## Branch
- **Branch**: `p5-polish`
- **Target**: `main`

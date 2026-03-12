# P2: Storage Pipeline + CLI

## Status: done

## Spec Reference
- `docs/spec/03-storage.md` — CAS, event log, digest cache, S3 integration, local buffer
- `docs/spec/10-api-reference.md` — CLI commands (log, cat, stdio)

## What Was Done

### StoragePipeline (`crates/sandbox/src/storage/pipeline.rs`)
- Unified orchestration of CAS store, event log, upload pool, digest cache, local buffer
- `store_content(data)` — local CAS write → digest cache check → S3 upload if new → local buffer tracking
- `append_event(event)` — event log with auto-rotation and segment upload
- `process_confirmations()` — non-blocking drain of upload confirmations, updates digest cache + prunes local buffer
- `save_digest_cache()` — persist to disk + upload snapshot to S3
- `shutdown()` — finalize event log, save digest cache, drain upload pool
- Unit tests: 8 pass (pipeline_tests.rs)

### CLI (`crates/cli/src/main.rs`)
- `argus log` — read JSONL from event dir, filter by --path/--pid/--type/--since/--limit, human-readable or --json output
- `argus cat <hash>` — read CAS object by hash, write raw bytes to stdout
- `argus stdio <pid>` — reconstruct stdio from events, fetch content from CAS, filter by --stream

### S3 Client Fix (`crates/sandbox/src/storage/s3.rs`)
- Explicitly configure `aws-smithy-http-client::Builder` with rustls-ring TLS provider
- Set HTTP client on both `aws_config` loader and S3 config builder (needed for credential resolution + S3 calls)

### Infrastructure
- `docker-compose.yml` — MinIO service with health check, bucket init, argus-dev service
- Integration tests: 3 pass against MinIO (pipeline_integration_test.rs, #[ignore])

## What Works
- CAS objects upload to S3 asynchronously
- Event segments upload on rotation and finalize
- Digest cache deduplication (second store of same content skips upload)
- Upload confirmations update digest cache and prune local buffer
- Digest cache snapshots persist to disk and upload to S3
- CLI reads local JSONL and CAS files directly (no HTTP server needed)
- Full MinIO round-trip: store → upload → verify in S3

## What's Missing
- Periodic digest cache snapshot upload (timer-based, not yet wired to supervisor loop)
- S3 fallback on local CAS miss (read from S3 when local evicted)
- Large file chunking (4MB Rabin fingerprint from spec)
- Durability mode "remote" (wait for S3 confirmation before resuming tracee)

## How to Test

```bash
# Unit tests (pipeline)
docker exec argus-arm64 bash -c "cd /workspace && cargo test -p sandbox pipeline"

# Full workspace
docker exec argus-arm64 bash -c "cd /workspace && cargo test --workspace"

# MinIO integration tests
docker compose up -d minio minio-init
docker network connect argus-run_default argus-arm64
docker exec -e AWS_ACCESS_KEY_ID=minioadmin -e AWS_SECRET_ACCESS_KEY=minioadmin \
  -e AWS_REGION=us-east-1 -e MINIO_ENDPOINT=http://argus-run-minio-1:9000 \
  argus-arm64 bash -c "cd /workspace && cargo test -p sandbox pipeline_minio -- --ignored"
```

## Branch
main

# P2: S3 Upload Pipeline

**Status**: not started

**Spec reference**: `docs/spec/03-storage.md` (S3 integration, upload pool)

## Dependencies
- **Blocked by**: P2-cas (needs ContentHash type and CAS store to read from)
- **Blocks**: P2-event-segments, P2-digest-cache (needs S3 for remote persistence)

## Parallelizable with
- All P1 tasks, P2-pause-resume-api, P2-content-capture

## What needs to be done
- `crates/sandbox/src/storage/s3.rs`:
  - `S3Client`: wraps aws-sdk-s3, configured from `S3Config`
  - `upload_cas_object(hash: &ContentHash, data: Vec<u8>) -> Result<()>`: PUT to `cas/{hash[0:2]}/{hash[2:]}`
  - `upload_event_segment(agent_id: &str, segment_seq: u64, data: Vec<u8>) -> Result<()>`: PUT to `events/{agent_id}/{segment_seq}.jsonl`
  - `upload_checkpoint(agent_id: &str, seq: u64, data: Vec<u8>) -> Result<()>`
  - `download_object(key: &str) -> Result<Vec<u8>>`
  - `list_prefix(prefix: &str) -> Result<Vec<String>>`

- `crates/sandbox/src/storage/upload_pool.rs`:
  - `UploadPool`: tokio task pool (configurable concurrency, default 4)
  - `submit(job: UploadJob) -> Result<()>`: non-blocking enqueue
  - `UploadJob` enum: CasObject, EventSegment, Checkpoint, DigestCache
  - Retry with exponential backoff (3 attempts)
  - Track: pending count, uploaded count, failed count
  - On failure after retries: log error, keep in local buffer for retry later
  - Graceful shutdown: drain queue before exit

- Add `aws-sdk-s3` and `aws-config` to sandbox dependencies

## How to test
```bash
cargo test -p sandbox --lib storage::s3
cargo test -p sandbox --lib storage::upload_pool
```
Unit tests: upload pool queuing, retry logic (mock S3 client with failures), graceful drain.
Integration test (ignored, needs S3/minio): round-trip upload+download.

## Branch
- **Branch**: `p2-s3-upload`
- **Target**: `main`

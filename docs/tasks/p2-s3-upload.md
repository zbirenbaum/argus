# P2: S3 Upload Pipeline

**Status**: done

**Spec reference**: `docs/spec/03-storage.md` (S3 integration, upload pool)

## Dependencies
- **Blocked by**: P2-cas (needs ContentHash type and CAS store to read from)
- **Blocks**: P2-event-segments, P2-digest-cache (needs S3 for remote persistence)

## Parallelizable with
- All P1 tasks, P2-pause-resume-api, P2-content-capture

## What was done
- `crates/sandbox/Cargo.toml` — added `aws-sdk-s3` and `aws-config` dependencies
- `crates/sandbox/src/storage/mod.rs` — module declarations and re-exports
- `crates/sandbox/src/storage/s3.rs` — `ObjectStore` trait (RPITIT) and `S3Client` implementation
- `crates/sandbox/src/storage/object_store_dyn.rs` — `DynObjectStore` type-erased wrapper for dynamic dispatch
- `crates/sandbox/src/storage/upload_pool.rs` — `UploadPool`, `UploadJob`, `UploadStats`, `UploadConfirmation`

## What works
- `ObjectStore` trait with `put`, `get`, `exists`, `list` methods
- `S3Client` configured from `S3Config` (region, bucket, prefix, optional custom endpoint)
- Path-style access forced for custom endpoints (MinIO, LocalStack)
- Key construction: CAS, event segments, checkpoints, digest cache snapshots
- `DynObjectStore` bridges RPITIT trait to dynamic dispatch via boxed futures
- `UploadPool` with configurable concurrency via tokio workers
- Non-blocking `submit()` via mpsc channel
- Exponential backoff retry with configurable max attempts
- Atomic `UploadStats` (pending, uploaded, failed, bytes_uploaded)
- `UploadConfirmation` channel for downstream eviction/cache updates
- Graceful `shutdown()` drains queue and returns stats

## What's missing
- Integration test with real S3/MinIO (needs container orchestration)

## How to test
```bash
cargo test -p sandbox --lib storage
```
16 tests covering: key construction, job routing, submit+process, stats tracking, retry-then-succeed, exhaust-retries-marks-failed, shutdown drains queue, confirmation channel.

## Branch
- **Branch**: `p2-s3-upload`
- **Target**: `main`

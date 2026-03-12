# P2: Event Segments & Local Buffer

**Status**: done

**Spec reference**: `docs/spec/03-storage.md` (event log, local buffer, durability)

## Dependencies
- **Blocked by**: P1-events (event serialization), P2-s3-upload (segment upload)
- **Blocks**: P3-indexes (indexes built from event segments)

## Parallelizable with
- P1-tracer-loop, P1-state, P1-seccomp, P2-cas, P2-content-capture, P2-pause-resume-api

## What was done
- `crates/sandbox/src/storage/event_log.rs`: `EventLog` append-only JSONL writer with segment rotation at configurable size threshold (default 64 MiB), fsync, and upload pool integration
- `crates/sandbox/src/storage/event_log_tests.rs`: 10 unit tests covering JSONL format, rotation, sequential naming, finalize, durability modes, size tracking
- `crates/sandbox/src/storage/local_buffer.rs`: `LocalBuffer` bounded LRU cache with upload confirmation tracking and eviction of confirmed-only entries
- `crates/sandbox/src/storage/local_buffer_tests.rs`: 7 unit tests covering tracking, pruning, eviction order, tolerance of pre-deleted files
- `crates/sandbox/src/storage/mod.rs`: added `event_log` and `local_buffer` modules with `#[doc(inline)]` re-exports
- `crates/sandbox/Cargo.toml`: moved `tempfile` from dev-dependencies to dependencies (pre-existing issue: `cas/store.rs` uses it at non-test scope)
- `crates/sandbox/src/storage/upload_pool_tests.rs`: fixed missing `S3Client` import (pre-existing issue)

## What works
- `EventLog::append()` serializes events as JSONL, one line per event
- Segment rotation at configurable byte threshold (default 64 MiB)
- Sequential segment naming (`0.jsonl`, `1.jsonl`, ...)
- `DurabilityMode::Local` fsyncs after every append
- `DurabilityMode::Memory` buffers without per-append fsync
- Completed segments submitted to `UploadPool` as `UploadJob::EventSegment`
- `EventLog::finalize()` fsyncs and submits the current segment
- `LocalBuffer::track()` registers files for eviction tracking
- `LocalBuffer::confirm_upload()` marks entries as safe to evict
- `LocalBuffer::prune()` evicts oldest confirmed entries until under limit

## What's missing
- `DurabilityMode::Remote` does not block until S3 upload is confirmed (would require async or callback plumbing from the upload pool)
- Time-based segment rotation (only size-based is implemented)
- Segment retention floor: spec says "never evict most recent N event segments" but this is not enforced in `LocalBuffer::prune()`

## How to test
```bash
cargo test -p sandbox --lib storage::event_log
cargo test -p sandbox --lib storage::local_buffer
```

## Branch
- **Branch**: `p2-event-segments`
- **Target**: `main`

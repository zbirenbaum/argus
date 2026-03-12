# P2: Event Segments & Local Buffer

**Status**: not started

**Spec reference**: `docs/spec/03-storage.md` (event log, local buffer, durability)

## Dependencies
- **Blocked by**: P1-events (event serialization), P2-s3-upload (segment upload)
- **Blocks**: P3-indexes (indexes built from event segments)

## Parallelizable with
- P1-tracer-loop, P1-state, P1-seccomp, P2-cas, P2-content-capture, P2-pause-resume-api

## What needs to be done
- `crates/sandbox/src/storage/event_log.rs`:
  - `EventLog`: append-only JSONL writer
  - Segments: new file every 64MB, named `{segment_seq}.jsonl`
  - Local path: `/data/events/{segment_seq}.jsonl`
  - On segment completion: submit to upload pool for S3 upload
  - `append(event: &EventEnvelope) -> Result<()>`: serialize + write + newline
  - `flush() -> Result<()>`: fsync current segment
  - `current_segment_size() -> u64`

- `crates/sandbox/src/storage/local_buffer.rs`:
  - `LocalBuffer`: bounded LRU cache for CAS objects and event segments
  - `max_size`: configurable (default 10GB)
  - Eviction: only evict objects confirmed uploaded to S3
  - Track upload confirmation from upload pool
  - `prune() -> Result<usize>`: evict oldest confirmed objects until under limit

- Durability mode enforcement:
  - Memory: events buffered in memory, flushed on segment completion
  - Local: fsync after every append
  - Remote: block until S3 upload confirmed (via upload pool callback)

## How to test
```bash
cargo test -p sandbox --lib storage::event_log
cargo test -p sandbox --lib storage::local_buffer
```
Unit tests: segment rotation at 64MB, JSONL format correctness, LRU eviction order, durability mode fsync behavior.

## Branch
- **Branch**: `p2-event-segments`
- **Target**: `main`

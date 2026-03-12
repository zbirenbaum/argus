# P3: Secondary Indexes & Query Engine

**Status**: done

**Spec reference**: `docs/spec/05-indexing-queries.md`

## Dependencies
- **Blocked by**: P1-events (event types to index), P2-event-segments (events to index over)
- **Blocks**: P3-query-api

## What was done

### Files added/changed
- `crates/sandbox/src/events/envelope.rs` - Added `EventPayload::event_type_tag()`, `pid()`, `paths()` helper methods
- `crates/sandbox/src/index/mod.rs` - Module declarations and re-exports
- `crates/sandbox/src/index/path_index.rs` - Path index with SHA-256 hashed filenames, prefix lookup, disk persistence
- `crates/sandbox/src/index/path_index_tests.rs` - 9 tests
- `crates/sandbox/src/index/pid_index.rs` - PID index with process tree metadata, disk persistence
- `crates/sandbox/src/index/pid_index_tests.rs` - 9 tests
- `crates/sandbox/src/index/type_index.rs` - Type index mapping event tags to sequence numbers, disk persistence
- `crates/sandbox/src/index/type_index_tests.rs` - 6 tests
- `crates/sandbox/src/index/query.rs` - Query engine intersecting path/pid/type filters with seq/time range and limit
- `crates/sandbox/src/index/query_tests.rs` - 14 tests
- `crates/sandbox/Cargo.toml` - Added `hex` dependency for path hashing

## What works
- **Path index**: Insert, exact lookup, prefix lookup, disk append, rebuild from disk
- **PID index**: Insert, lookup, process tree (upsert/mark_exit/query), disk persistence, rebuild
- **Type index**: Insert, lookup, iteration over all types, disk persistence, rebuild
- **Query engine**: Single-filter queries (path, pid, type), multi-filter intersection, path prefix, seq range, time range (via `query_events`), limit, sorted results
- **EventPayload helpers**: `event_type_tag()` returns serde tag string, `pid()` extracts PID, `paths()` extracts filesystem paths

## What's missing
- Glob-based path queries (spec mentions glob, current impl uses prefix)
- Streaming JSONL output (belongs in P3-query-api HTTP layer)
- Offset-based pagination (limit is implemented, offset deferred to API layer)
- Rebuild from S3 segments (rebuild currently reads local disk only)

## How to test
```bash
cargo test -p sandbox --lib index
```

## Branch
- **Branch**: `p3-indexes`
- **Target**: `main`

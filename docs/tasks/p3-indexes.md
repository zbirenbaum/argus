# P3: Secondary Indexes & Query Engine

**Status**: not started

**Spec reference**: `docs/spec/05-indexing-queries.md`

## Dependencies
- **Blocked by**: P1-events (event types to index), P2-event-segments (events to index over)
- **Blocks**: P3-query-api

## Parallelizable with
- All P1 tasks (only needs event types defined), P2-cas, P2-s3-upload, P2-content-capture, P3-merkle-tree, P3-restore

## What needs to be done
- `crates/sandbox/src/index/mod.rs`:

### Path Index
- `PathIndex`: maps path → list of (seq, event_type) entries
- Append-only, keyed by path hash
- Supports glob queries: `get_by_glob("src/**/*.rs")`

### Process Index
- `PidIndex`: maps pid → list of (seq, event_type) entries
- Includes process lineage (parent chain)

### Type Index
- `TypeIndex`: maps event_type → list of seq entries
- Fast filtering by event category

### Query Engine
- `QueryEngine`: combines indexes for compound queries
- Filters: time range (seq or timestamp), pid, path (exact or glob), event type, combination
- Returns iterator over matching EventEnvelopes
- Pagination: offset + limit
- Streaming: return JSONL for large result sets

### Index Maintenance
- On each event: append to all relevant indexes
- On restart: rebuild from event segments (scan all local + S3 segments)
- Persist indexes to `/data/indexes/`

## How to test
```bash
cargo test -p sandbox --lib index
```
Unit tests: insert+query for each index type, compound query with multiple filters, glob matching, pagination.

## Branch
- **Branch**: `p3-indexes`
- **Target**: `main`

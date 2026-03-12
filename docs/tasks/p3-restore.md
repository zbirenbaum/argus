# P3: Restore

**Status**: not started

**Spec reference**: `docs/spec/04-snapshots-restore.md` (restore modes)

## Dependencies
- **Blocked by**: P3-merkle-tree (needs tree to know what files existed at a point in time), P2-cas (retrieve content), P2-s3-upload (pull from S3 if not local)
- **Blocks**: nothing — terminal feature

## Parallelizable with
- P3-indexes, P3-query-api, P3-realtime-api

## What needs to be done
- `crates/sandbox/src/snapshot/restore.rs`:
  - **Full restore (new dir)**: create new directory, walk tree at target seq/timestamp, write all files from CAS
  - **Full restore (in-place)**: pause agent, take pre-restore snapshot, overwrite workspace files, resume
  - **Selective restore**: restore specific paths only, by glob or explicit list
  - **Undo last N**: find tree_hash N events ago, restore to that state
  - Binary search by timestamp: scan event log for closest seq to target ts
  - Content retrieval: try local CAS first, fall back to S3
  - Pre-restore snapshot: always create a checkpoint before in-place restore (safety net)

- API endpoints (in `crates/sandbox/src/api/`):
  - `POST /restore { mode, target_seq?, target_ts?, paths?, undo_n? }`
  - `POST /restore/undo` — restore to pre-restore snapshot

## How to test
```bash
cargo test -p sandbox --lib snapshot::restore -- --ignored
```
Integration tests:
1. Write files, capture events, restore to earlier state, verify files match
2. Selective restore of single file
3. Undo restore returns to pre-restore state
4. In-place restore pauses agent first

## Branch
- **Branch**: `p3-restore`
- **Target**: `main`

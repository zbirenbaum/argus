# P3: Merkle Tree & Checkpoints

**Status**: not started

**Spec reference**: `docs/spec/04-snapshots-restore.md` (Merkle tree, checkpoints)

## Dependencies
- **Blocked by**: P2-cas (blob storage), P2-content-capture (hashes on events), P2-write-locking (reliable before/after)
- **Blocks**: P3-restore, P3-tree-api

## Parallelizable with
- P2-s3-upload, P2-pause-resume-api, P2-event-segments, P2-tls-content, P3-indexes

## What needs to be done
- `crates/sandbox/src/snapshot/merkle.rs`:
  - Three object types stored in CAS:
    - `Blob`: raw file content (already in CAS from content capture)
    - `Tree`: sorted list of (name, mode, hash) entries — like git tree objects
    - `Commit`: tree_hash, parent_commit_hash, seq, timestamp
  - `MerkleTree`: in-memory tree structure mirroring watched filesystem
  - On mutating event (write, rename, unlink, mkdir, chmod, truncate, link, symlink):
    1. Update affected leaf node
    2. Rehash all ancestor tree nodes up to root
    3. Create new Commit object
    4. Attach `tree_hash` to the event
  - `root_hash() -> ContentHash`: current tree root

- `crates/sandbox/src/snapshot/checkpoint.rs`:
  - `Checkpoint`: serialized MerkleTree + ProcessTree + FdTables + PipeRegistry + PtyRegistry
  - Every 1000 events: serialize checkpoint to binary (bincode), store in CAS, upload to S3
  - Path: `checkpoints/{agent_id}/{seq}.bin`
  - On restart: load latest checkpoint from S3, replay events since checkpoint seq

## How to test
```bash
cargo test -p sandbox --lib snapshot
```
Unit tests: tree insert/update/rehash, commit chain, checkpoint serialize/deserialize round-trip.
Integration test (ignored): write files, verify tree hashes match expected, checkpoint + replay matches original state.

## Branch
- **Branch**: `p3-merkle-tree`
- **Target**: `main`

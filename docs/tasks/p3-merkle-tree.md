# P3: Merkle Tree & Checkpoints

**Status**: done

**Spec reference**: `docs/spec/04-snapshots-restore.md` (Merkle tree, checkpoints, diff)

## Dependencies
- **Blocked by**: P2-cas (blob storage), P2-content-capture (hashes on events), P2-write-locking (reliable before/after)
- **Blocks**: P3-restore, P3-tree-api

## Parallelizable with
- P2-s3-upload, P2-pause-resume-api, P2-event-segments, P2-tls-content, P3-indexes

## What was done
- `crates/sandbox/src/snapshot/mod.rs`: module declarations and re-exports
- `crates/sandbox/src/snapshot/tree.rs`:
  - `MerkleTree` — in-memory Merkle tree with flat `BTreeMap<PathBuf, ContentHash>` file map
  - `TreeEntry` — blob/directory enum for CAS object representation
  - `TreeObject` — serializable directory listing stored in CAS
  - `Commit` — root tree hash + timestamps + parent commit hash
  - `update()`, `remove()`, `rename()` — mutating operations that invalidate cached root
  - `root_hash()` — computes Merkle root by building virtual directory tree, hashing bottom-up
  - `commit()` — stores tree objects and commit in CAS, returns commit hash
  - `files()`, `file_count()`, `contains()`, `get()` — query accessors
- `crates/sandbox/src/snapshot/checkpoint.rs`:
  - `serialize_checkpoint()` / `deserialize_checkpoint()` — bincode round-trip for `MerkleTree`
  - `checkpoint_s3_key()` — builds S3 path `checkpoints/{agent_id}/{seq}.bin`
  - `DEFAULT_CHECKPOINT_INTERVAL` constant (1000)
- `crates/sandbox/src/snapshot/diff.rs`:
  - `diff_trees()` — walks two trees, reports Added/Deleted/Modified entries
  - `DiffEntry` / `DiffKind` — diff result types
  - Results sorted by path

## What works
- Tree mutation (update, remove, rename) with root hash invalidation and caching
- Deterministic hashing: same files in any insertion order produce identical root hash
- Nested directory structure hashing (virtual directory tree built from flat paths)
- CAS storage of tree objects and commit objects with parent chain
- Checkpoint binary serialization/deserialization round-trip
- Tree diff with correct Added/Deleted/Modified classification
- Rename detected as add+delete pair in diffs

## What's missing
- Checkpoint does not include ProcessTree/FdTables/PipeRegistry/PtyRegistry (spec mentions this but those types don't exist yet)
- No integration with the tracer loop (automatic tree_hash attachment to mutating events)
- No automatic checkpoint triggering every N events
- No S3 upload integration for checkpoints
- No checkpoint loading on restart

## How to test
```bash
cargo test -p sandbox --lib snapshot
```

## Branch
- **Branch**: `p3-merkle-tree`
- **Target**: `main`

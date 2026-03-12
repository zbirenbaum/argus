# P3: Restore

**Status**: in progress

**Spec reference**: `docs/spec/04-snapshots-restore.md` (restore modes)

## Dependencies
- **Blocked by**: P3-merkle-tree (done), P2-cas (done), P2-s3-upload (done)
- **Blocks**: nothing — terminal feature

## What was done
- `crates/argus/src/snapshot/restore.rs`: full restore and selective restore functions
  - `restore_full`: walks tree, pulls content from CAS, writes all files to target dir
  - `restore_selective`: same but only for specified paths
  - `restore_from_hash` / `restore_selective_from_hash`: convenience wrappers that load tree from CAS by hash
  - Atomic writes via temp-file-then-rename
  - 8 unit tests passing

- `crates/argus/src/api/state.rs` (Bridge additions):
  - `tree: ArcSwap<MerkleTree>` — latest tree snapshot, swapped on every mutating event
  - `cas: Arc<dyn Cas>` — required CAS backend (not optional)
  - `tree_hashes: DashMap<u64, String>` — seq → tree_hash index for point-in-time restore
  - New methods: `store_tree`, `load_tree`, `insert_tree_hash`, `get_tree_hash`, `cas()`
  - `Bridge::new` now requires `cas: Arc<dyn Cas>`
  - 2 new tests for tree and tree_hash operations

- `crates/argus/src/api/routes.rs`:
  - `GET /tree` — returns current tree snapshot (all files with hashes)
  - `POST /restore` — restores to past seq, supports full and selective modes

- `crates/argus/src/api/types.rs`:
  - `RestoreRequest`, `RestoreResponse`, `TreeSnapshotResponse`, `TreeFileEntry`

- `crates/argus/src/api/errors.rs`:
  - `SeqNotFound`, `RestoreFailed` error variants

- `crates/argus/src/tracer/trace_loop.rs`:
  - `store_tree()` now also swaps tree into Bridge via `ArcSwap`
  - `emit()` automatically extracts tree_hash from payload and records seq → tree_hash in Bridge

- `crates/argus/src/events/envelope.rs`:
  - `EventPayload::tree_hash()` method extracts tree_hash from any payload variant

- `crates/argus/src/snapshot/tree.rs`:
  - `RefCell<Option<ContentHash>>` → `Mutex<Option<ContentHash>>` for Send + Sync
  - Manual Clone and PartialEq impls

- `crates/supervisor/src/main.rs`:
  - Creates second LocalCas handle for Bridge (same directory, safe for append-only CAS)
  - Passes `Arc<dyn Cas>` to `new_shared_state`

## What works
- Full restore to a new directory from any past seq
- Selective restore (specific paths) from any past seq
- Tree snapshot API endpoint
- Tree hash tracking per event seq
- Lock-free tree sharing between tracer and API

## What's missing
- In-place restore (pause agent, overwrite workspace)
- Undo-last-N (syntactic sugar over selective restore)
- Timestamp-based restore (binary search event log for closest seq)
- `POST /restore/undo` endpoint
- `/tree/diff` endpoint (needs two tree hashes)
- Integration test (test 11 from spec)

## How to test
```bash
docker exec argus-x86 bash -c "cd /workspace && cargo test -p argus --lib -- snapshot::restore"
docker exec argus-x86 bash -c "cd /workspace && cargo test -p argus --lib -- api::state::tests"
docker exec argus-x86 bash -c "cd /workspace && cargo test -p argus --lib -- api::routes::tests"
docker exec argus-x86 bash -c "cd /workspace && cargo test -p argus --lib -- snapshot::tree"
```

## Branch
- **Branch**: `restore-wiring`
- **Target**: `main`

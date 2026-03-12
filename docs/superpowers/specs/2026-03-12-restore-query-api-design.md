# Restore Module + Query API Design

## Goal

Implement the restore engine and query/content API endpoints so validation tests 8-12 can run. The specs are fully defined in `docs/spec/04-snapshots-restore.md`, `05-indexing-queries.md`, and `10-api-reference.md`.

## Key Design Decisions

### Event Storage: Disk-Based, Not In-Memory

Events are NOT stored in `SharedState`. The JSONL segment files on disk are the source of truth. The query engine reads from segment files using indexes for fast lookup. No `Vec<Event>` in memory.

A small ring buffer of recent events (configurable, default last 30 seconds) can be added later for real-time streaming endpoints (WebSocket), but is not needed for the query API.

### SharedState Extensions

`SupervisorState` gains new fields:

```rust
pub struct SupervisorState {
    // existing fields...
    paused: bool,
    agent_id: String,
    started_at: Instant,
    seq_gen: SequenceGenerator,
    pending_approvals: HashMap<String, PendingApprovalEntry>,
    event_tx: Option<mpsc::UnboundedSender<Event>>,

    // new fields
    cas: Option<Arc<CasStore>>,
    merkle_tree: Option<MerkleTree>,
    path_index: Option<PathIndex>,
    pid_index: Option<PidIndex>,
    type_index: Option<TypeIndex>,
    event_dir: Option<PathBuf>,          // path to JSONL segment directory
    checkpoint_history: Vec<CheckpointRef>, // (seq, tree snapshot) for restore
}
```

All new fields are `Option` to maintain backward compatibility with existing tests that create `SupervisorState::new("test")`. Production code uses a new constructor that wires everything up.

### Restore Engine

Located in `crates/argus/src/snapshot/restore.rs`.

Core operations:

1. **find_seq_at_timestamp** — binary search over JSONL segment files for largest seq where `ts_wall <= T`. Reads segment files from disk, not memory.

2. **build_tree_at_seq** — load nearest checkpoint before target seq, then replay mutating events (write, unlink, rename, truncate, link, symlink, mkdir, rmdir) from checkpoint seq to target seq to reconstruct the MerkleTree at that point.

3. **restore_to_directory** — walk the reconstructed tree, pull each file's content from CAS, write to target directory. Returns `RestoreResult { seq, ts, tree_hash, files_restored, bytes_restored }`.

4. **restore_selective** — same as above but filtered to specified paths/prefixes.

5. **restore_in_place** — calls existing pause machinery (`SupervisorState::set_paused(true)`), takes a pre-restore checkpoint, overwrites workspace files, then resumes (`set_paused(false)`). Does NOT reimplement pause — uses the same `SharedState` flag the pause/resume API uses.

6. **undo_last_n** — scans backward through events to find the seq N mutating events ago, then restores to that point.

### API Routes

New route modules, registered in `api/mod.rs`:

**`api/query_routes.rs`** — Event queries
- `GET /events` — reads JSONL segments from disk, filters via QueryEngine indexes, streams as JSONL
- `GET /file_history` — shorthand for path-filtered event query

**`api/content_routes.rs`** — CAS content retrieval
- `GET /content/{hash}` — raw bytes from CAS (application/octet-stream)
- `GET /content/{hash}/text` — UTF-8 decoded, 415 if invalid
- `GET /diff` — unified diff between two CAS hashes

**`api/tree_routes.rs`** — Merkle tree queries
- `GET /tree` — directory listing at a point in time (seq or timestamp)
- `GET /tree/diff` — changed files between two sequence numbers

**`api/restore_routes.rs`** — Restore operations
- `POST /restore` — dispatches to restore engine based on mode
- `POST /restore/undo` — undo last N mutations

**`api/system_routes.rs`** — System status
- `GET /storage/status` — CAS stats, event log info
- `GET /connections` — placeholder (depends on net module)

### Request/Response Types

All in `api/query_types.rs`:

```rust
// GET /events query params
struct EventsQuery {
    path: Option<String>,
    path_prefix: Option<String>,
    pid: Option<u32>,
    r#type: Option<String>,
    since: Option<String>,
    until: Option<String>,
    seq_from: Option<u64>,
    seq_to: Option<u64>,
    limit: Option<usize>,
}

// GET /tree query params
struct TreeQuery {
    seq: Option<u64>,
    ts: Option<String>,
    path_prefix: Option<String>,
}

// GET /tree/diff query params
struct TreeDiffQuery {
    from_seq: u64,
    to_seq: u64,
}

// POST /restore request body
struct RestoreRequest {
    timestamp: Option<String>,
    seq: Option<u64>,
    mode: RestoreMode,      // "new_directory" | "in_place" | "selective"
    target: Option<String>, // target directory for new_directory mode
    path: Option<String>,   // for selective mode
    force: Option<bool>,    // required for in_place
}

// POST /restore/undo request body
struct UndoRequest {
    last: Option<u64>,
    last_by_pid: Option<u32>,
}

// POST /restore response
struct RestoreResponse {
    restored_to_seq: u64,
    restored_to_ts: String,
    tree_hash: String,
    files_restored: u64,
    bytes_restored: u64,
    pre_restore_snapshot_seq: Option<u64>,
}

// GET /tree response
struct TreeResponse {
    tree_hash: String,
    seq: u64,
    entries: Vec<TreeEntry>,
}

struct TreeEntry {
    name: String,
    entry_type: String, // "file" or "directory"
    hash: String,
    size: Option<u64>,
}

// GET /tree/diff response
struct TreeDiffResponse {
    from_seq: u64,
    to_seq: u64,
    added: Vec<DiffFileEntry>,
    modified: Vec<ModifiedFileEntry>,
    deleted: Vec<DiffFileEntry>,
}

// GET /file_history response
struct FileHistoryResponse {
    path: String,
    events: Vec<FileHistoryEntry>,
}

// GET /storage/status response
struct StorageStatusResponse {
    local_buffer: LocalBufferStatus,
    digest_cache: DigestCacheStatus,
}
```

### Error Handling

Extend `ApiError` with new variants:
- `ContentNotFound { hash }` — 404 for missing CAS objects
- `InvalidTimestamp { value }` — 400 for unparseable timestamps
- `RestoreFailed { reason }` — 500 for restore errors
- `NotUtf8 { hash }` — 415 for /content/{hash}/text on binary content
- `BadRequest { message }` — 400 for missing required params

### Reading Events from Disk

New function in `storage/event_log.rs` (or a new `storage/event_reader.rs`):

```rust
pub fn read_events_from_segments(
    event_dir: &Path,
    seq_range: Option<(u64, u64)>,
) -> Result<Vec<Event>>
```

Reads all `.jsonl` segment files in `event_dir`, deserializes each line, optionally filters by seq range. The indexes tell us which seqs to look for; this function fetches the full event records.

For large event logs, a streaming iterator would be better, but `Vec<Event>` bounded by the index-narrowed seq set is fine for the initial implementation.

### In-Place Restore Flow

```
POST /restore { mode: "in_place", timestamp: "...", force: true }
  1. Validate force=true (reject without it)
  2. state.set_paused(true) — same flag pause_handler uses
  3. Emit AgentPause event with reason "restore"
  4. Take pre-restore checkpoint (serialize current MerkleTree)
  5. find_seq_at_timestamp(timestamp)
  6. build_tree_at_seq(target_seq)
  7. Diff current tree vs target tree
  8. For each changed/added file: read from CAS, write to workspace
  9. For each deleted file: remove from workspace
  10. Update in-memory MerkleTree to match target
  11. state.set_paused(false)
  12. Emit AgentResume event
  13. Return RestoreResponse with pre_restore_snapshot_seq
```

### File Organization

```
crates/argus/src/
  snapshot/
    mod.rs              # add `pub mod restore;`
    restore.rs          # NEW: RestoreEngine + RestoreResult
  storage/
    event_reader.rs     # NEW: read events from JSONL segments
    mod.rs              # add `pub mod event_reader;`
  api/
    mod.rs              # register new routes
    routes.rs           # existing (unchanged)
    state.rs            # extend SupervisorState
    types.rs            # existing (unchanged)
    errors.rs           # extend ApiError
    query_types.rs      # NEW: request/response types for new endpoints
    query_routes.rs     # NEW: GET /events, /file_history
    content_routes.rs   # NEW: GET /content/{hash}, /content/{hash}/text, /diff
    tree_routes.rs      # NEW: GET /tree, /tree/diff
    restore_routes.rs   # NEW: POST /restore, /restore/undo
    system_routes.rs    # NEW: GET /storage/status, /connections
```

### Test Priority

Test-blocking endpoints (build first):
1. `GET /content/{hash}` — needed for Test 8 (`argus cat <hash>`)
2. `GET /events` — needed for Test 11 (`argus log --path ... --type write`)
3. `POST /restore` with `mode: "new_directory"` — needed for Test 11
4. `GET /tree` — needed for Test 12 (`argus snapshot --seq 0`)

Complete API (build second):
5. `GET /content/{hash}/text`, `GET /diff`
6. `GET /file_history`
7. `GET /tree/diff`
8. `POST /restore/undo`
9. `GET /storage/status`
10. `GET /connections`

### Testing Strategy

Unit tests per module:
- `restore.rs`: test find_seq_at_timestamp, build_tree_at_seq, restore_to_directory with fixture events/CAS
- `event_reader.rs`: test reading JSONL segments
- Each route file: test handlers with mock state using axum's `oneshot`

Integration tests (marked `#[ignore]`):
- Full restore flow: write events, checkpoint, restore to new dir, verify files
- Query flow: ingest events, query by path/pid/type, verify results

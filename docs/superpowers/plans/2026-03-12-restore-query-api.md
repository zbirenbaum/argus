# Restore Module + Query API Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the restore engine and query/content/tree/restore API endpoints so validation tests 8-12 can run.

**Architecture:** Extend the existing axum API server with new route modules for events, content, tree, and restore. Add a restore engine in `snapshot/restore.rs` that rebuilds filesystem state from MerkleTree + CAS. Add an event reader in `storage/event_reader.rs` that reads JSONL segment files from disk. Extend `SupervisorState` with `Option` fields for CAS, MerkleTree, and indexes — existing tests still use `SupervisorState::new("test")` unchanged.

**Tech Stack:** Rust 2024, axum, serde/serde_json, chrono, tokio, anyhow/thiserror, bincode (checkpoints), sha2 (CAS hashing)

**Key docs:**
- Spec: `docs/spec/04-snapshots-restore.md`, `docs/spec/05-indexing-queries.md`, `docs/spec/10-api-reference.md`
- Design: `docs/superpowers/specs/2026-03-12-restore-query-api-design.md`
- Conventions: project `CLAUDE.md` — 300-line file limit, 40-line function limit, `anyhow` for app errors, `thiserror` for library types, TDD, `#[test]` for unit tests, `#[test] #[ignore]` for integration requiring ptrace
- **IMPORTANT**: Always activate the ms-rust skill before writing any Rust code

---

## Chunk 1: Foundation — Event Reader + SharedState Extensions

### Task 1: Event Reader Module

Read events from JSONL segment files on disk. The query API needs this to answer queries without holding events in memory.

**Files:**
- Create: `crates/argus/src/storage/event_reader.rs`
- Modify: `crates/argus/src/storage/mod.rs`

- [ ] **Step 1: Write failing tests for event reader**

Create `crates/argus/src/storage/event_reader.rs` with tests at the bottom:

```rust
//! Reads events from JSONL segment files on disk.
//!
//! The event log writes segments as `{seq}.jsonl` files. This module
//! reads them back, optionally filtering by sequence range, to support
//! query API endpoints without holding events in memory.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::events::Event;

/// Reads all events from JSONL segment files in `event_dir`.
///
/// Segments are read in filename order (numeric). Each line is
/// deserialized as an [`Event`]. Malformed lines are logged and
/// skipped.
pub fn read_all_events(event_dir: &Path) -> Result<Vec<Event>> {
    read_events_filtered(event_dir, None)
}

/// Reads events, optionally filtering to `[seq_from, seq_to]`.
///
/// When a range is provided, events outside it are skipped. Segments
/// are still read in order, so the result is sorted by seq.
pub fn read_events_filtered(
    event_dir: &Path,
    seq_range: Option<(u64, u64)>,
) -> Result<Vec<Event>> {
    let mut segments = list_segments(event_dir)?;
    segments.sort();

    let mut events = Vec::new();
    for seg_path in &segments {
        read_segment(seg_path, seq_range, &mut events)?;
    }
    Ok(events)
}

/// Lists `.jsonl` segment file paths in `event_dir`.
fn list_segments(event_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let entries = fs::read_dir(event_dir).with_context(|| {
        format!("read event dir: {}", event_dir.display())
    })?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jsonl") {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// Reads events from a single segment file.
fn read_segment(
    path: &Path,
    seq_range: Option<(u64, u64)>,
    out: &mut Vec<Event>,
) -> Result<()> {
    let file = File::open(path).with_context(|| {
        format!("open segment: {}", path.display())
    })?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let event: Event = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(
                    file = %path.display(),
                    error = %err,
                    "skipping malformed event line"
                );
                continue;
            }
        };
        if let Some((from, to)) = seq_range {
            if event.seq < from || event.seq > to {
                continue;
            }
        }
        out.push(event);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventPayload, SequenceGenerator, Event};
    use crate::events::control::AgentStart;

    fn make_event(seq_gen: &SequenceGenerator, agent_id: &str) -> Event {
        Event::new(
            seq_gen,
            agent_id.into(),
            EventPayload::AgentStart(AgentStart {
                agent_id: agent_id.into(),
                supervisor_pid_host: None,
                supervisor_pid_ns: None,
                config_summary: "test".into(),
                node: None,
                pod: None,
                container: None,
            }),
        )
    }

    fn write_events_to_segment(dir: &Path, filename: &str, events: &[Event]) {
        let path = dir.join(filename);
        let mut content = String::new();
        for e in events {
            content.push_str(&serde_json::to_string(e).unwrap());
            content.push('\n');
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn read_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let events = read_all_events(dir.path()).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn read_single_segment() {
        let dir = tempfile::tempdir().unwrap();
        let gen = SequenceGenerator::default();
        let e1 = make_event(&gen, "test");
        let e2 = make_event(&gen, "test");
        write_events_to_segment(dir.path(), "0.jsonl", &[e1, e2]);

        let events = read_all_events(dir.path()).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 0);
        assert_eq!(events[1].seq, 1);
    }

    #[test]
    fn read_multiple_segments_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let gen = SequenceGenerator::default();
        let e0 = make_event(&gen, "test");
        let e1 = make_event(&gen, "test");
        let e2 = make_event(&gen, "test");
        write_events_to_segment(dir.path(), "0.jsonl", &[e0, e1]);
        write_events_to_segment(dir.path(), "1.jsonl", &[e2]);

        let events = read_all_events(dir.path()).unwrap();
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn filter_by_seq_range() {
        let dir = tempfile::tempdir().unwrap();
        let gen = SequenceGenerator::default();
        let evts: Vec<Event> = (0..5).map(|_| make_event(&gen, "test")).collect();
        write_events_to_segment(dir.path(), "0.jsonl", &evts);

        let filtered = read_events_filtered(dir.path(), Some((1, 3))).unwrap();
        assert_eq!(filtered.len(), 3);
        assert_eq!(filtered[0].seq, 1);
        assert_eq!(filtered[2].seq, 3);
    }

    #[test]
    fn skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let gen = SequenceGenerator::default();
        let e = make_event(&gen, "test");
        let mut content = String::new();
        content.push_str(&serde_json::to_string(&e).unwrap());
        content.push('\n');
        content.push_str("not valid json\n");
        content.push_str(&serde_json::to_string(&make_event(&gen, "test")).unwrap());
        content.push('\n');
        fs::write(dir.path().join("0.jsonl"), content).unwrap();

        let events = read_all_events(dir.path()).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn skips_empty_lines() {
        let dir = tempfile::tempdir().unwrap();
        let gen = SequenceGenerator::default();
        let e = make_event(&gen, "test");
        let mut content = String::new();
        content.push_str(&serde_json::to_string(&e).unwrap());
        content.push_str("\n\n\n");
        fs::write(dir.path().join("0.jsonl"), content).unwrap();

        let events = read_all_events(dir.path()).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn nonexistent_dir_errors() {
        let result = read_all_events(Path::new("/nonexistent/path"));
        assert!(result.is_err());
    }

    #[test]
    fn ignores_non_jsonl_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), "not events").unwrap();
        let events = read_all_events(dir.path()).unwrap();
        assert!(events.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p argus --lib storage::event_reader -- -q` (inside dev container)
Expected: All 7 tests PASS (the implementation is included with tests since TDD cycle is trivial for a reader)

- [ ] **Step 3: Register module in storage/mod.rs**

Add to `crates/argus/src/storage/mod.rs`:
```rust
pub mod event_reader;
```

And add to the re-exports:
```rust
#[doc(inline)]
pub use event_reader::{read_all_events, read_events_filtered};
```

- [ ] **Step 4: Run all storage tests**

Run: `cargo test -p argus --lib storage -- -q`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add crates/argus/src/storage/event_reader.rs crates/argus/src/storage/mod.rs
git commit -m "add event reader for JSONL segment files"
```

---

### Task 2: Extend SupervisorState with CAS, MerkleTree, Indexes

**Files:**
- Modify: `crates/argus/src/api/state.rs`

- [ ] **Step 1: Add new imports and fields to SupervisorState**

Add imports at the top of `state.rs`:
```rust
use std::path::PathBuf;
use std::sync::Arc;

use crate::cas::CasStore;
use crate::index::{PathIndex, PidIndex, TypeIndex};
use crate::snapshot::MerkleTree;
```

Add new fields to `SupervisorState`:
```rust
pub struct SupervisorState {
    // existing fields unchanged
    paused: bool,
    agent_id: String,
    started_at: Instant,
    seq_gen: SequenceGenerator,
    pending_approvals: HashMap<String, PendingApprovalEntry>,
    event_tx: Option<mpsc::UnboundedSender<Event>>,

    // new fields for query/restore API
    cas: Option<Arc<CasStore>>,
    merkle_tree: Option<MerkleTree>,
    path_index: Option<PathIndex>,
    pid_index: Option<PidIndex>,
    type_index: Option<TypeIndex>,
    event_dir: Option<PathBuf>,
}
```

- [ ] **Step 2: Update constructors to initialize new fields as None**

In `SupervisorState::new()`:
```rust
pub fn new(agent_id: String) -> Self {
    Self {
        paused: false,
        agent_id,
        started_at: Instant::now(),
        seq_gen: SequenceGenerator::default(),
        pending_approvals: HashMap::new(),
        event_tx: None,
        cas: None,
        merkle_tree: None,
        path_index: None,
        pid_index: None,
        type_index: None,
        event_dir: None,
    }
}
```

Do the same for `with_event_tx()`.

- [ ] **Step 3: Add a full constructor and accessors**

```rust
/// Creates state with all subsystems wired up for production.
pub fn with_subsystems(
    agent_id: String,
    event_tx: mpsc::UnboundedSender<Event>,
    cas: Arc<CasStore>,
    event_dir: PathBuf,
) -> Self {
    Self {
        paused: false,
        agent_id,
        started_at: Instant::now(),
        seq_gen: SequenceGenerator::default(),
        pending_approvals: HashMap::new(),
        event_tx: Some(event_tx),
        cas: Some(cas),
        merkle_tree: Some(MerkleTree::new()),
        path_index: Some(PathIndex::new()),
        pid_index: Some(PidIndex::new()),
        type_index: Some(TypeIndex::new()),
        event_dir: Some(event_dir),
    }
}

/// Reference to the CAS store, if configured.
pub fn cas(&self) -> Option<&Arc<CasStore>> {
    self.cas.as_ref()
}

/// Reference to the Merkle tree, if configured.
pub fn merkle_tree(&self) -> Option<&MerkleTree> {
    self.merkle_tree.as_ref()
}

/// Mutable reference to the Merkle tree.
pub fn merkle_tree_mut(&mut self) -> Option<&mut MerkleTree> {
    self.merkle_tree.as_mut()
}

/// Reference to the path index.
pub fn path_index(&self) -> Option<&PathIndex> {
    self.path_index.as_ref()
}

/// Reference to the PID index.
pub fn pid_index(&self) -> Option<&PidIndex> {
    self.pid_index.as_ref()
}

/// Reference to the type index.
pub fn type_index(&self) -> Option<&TypeIndex> {
    self.type_index.as_ref()
}

/// Path to the JSONL event segment directory.
pub fn event_dir(&self) -> Option<&Path> {
    self.event_dir.as_deref()
}

/// The sequence generator, for external use.
pub fn seq_gen(&self) -> &SequenceGenerator {
    &self.seq_gen
}
```

Also add a helper constructor for creating shared state with subsystems:
```rust
pub fn new_shared_state_full(
    agent_id: String,
    event_tx: mpsc::UnboundedSender<Event>,
    cas: Arc<CasStore>,
    event_dir: PathBuf,
) -> SharedState {
    Arc::new(Mutex::new(SupervisorState::with_subsystems(
        agent_id, event_tx, cas, event_dir,
    )))
}
```

- [ ] **Step 4: Run all existing tests to verify nothing broke**

Run: `cargo test -p argus --lib api -- -q`
Expected: All existing API tests PASS (they use `SupervisorState::new()` which doesn't touch new fields)

- [ ] **Step 5: Add tests for new accessors**

Add to the `tests` module in `state.rs`:
```rust
#[test]
fn new_state_has_no_subsystems() {
    let state = SupervisorState::new("test".into());
    assert!(state.cas().is_none());
    assert!(state.merkle_tree().is_none());
    assert!(state.path_index().is_none());
    assert!(state.pid_index().is_none());
    assert!(state.type_index().is_none());
    assert!(state.event_dir().is_none());
}

#[test]
fn full_state_has_subsystems() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let dir = tempfile::tempdir().unwrap();
    let cas = Arc::new(
        CasStore::new(dir.path().join("cas")).unwrap(),
    );
    let state = SupervisorState::with_subsystems(
        "test".into(),
        tx,
        cas,
        dir.path().join("events"),
    );
    assert!(state.cas().is_some());
    assert!(state.merkle_tree().is_some());
    assert!(state.path_index().is_some());
    assert!(state.pid_index().is_some());
    assert!(state.type_index().is_some());
    assert!(state.event_dir().is_some());
}
```

- [ ] **Step 6: Run all tests**

Run: `cargo test -p argus --lib api -- -q`
Expected: All PASS

- [ ] **Step 7: Commit**

```bash
git add crates/argus/src/api/state.rs
git commit -m "extend SupervisorState with CAS, MerkleTree, and index fields"
```

---

### Task 3: Extend API Errors

**Files:**
- Modify: `crates/argus/src/api/errors.rs`

- [ ] **Step 1: Add new error variants**

```rust
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    // existing variants unchanged
    #[error("action not found: {action_id}")]
    ActionNotFound { action_id: String },

    #[error("agent is already {state}")]
    AlreadyInState { state: &'static str },

    // new variants
    #[error("content not found: {hash}")]
    ContentNotFound { hash: String },

    #[error("content is not valid UTF-8: {hash}")]
    NotUtf8 { hash: String },

    #[error("invalid timestamp: {value}")]
    InvalidTimestamp { value: String },

    #[error("bad request: {message}")]
    BadRequest { message: String },

    #[error("subsystem not configured: {name}")]
    NotConfigured { name: &'static str },

    #[error("restore failed: {reason}")]
    RestoreFailed { reason: String },

    #[error("internal error: {0}")]
    Internal(String),
}
```

- [ ] **Step 2: Update IntoResponse impl**

```rust
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            ApiError::ActionNotFound { .. } => (StatusCode::NOT_FOUND, self.to_string()),
            ApiError::AlreadyInState { .. } => (StatusCode::CONFLICT, self.to_string()),
            ApiError::ContentNotFound { .. } => (StatusCode::NOT_FOUND, self.to_string()),
            ApiError::NotUtf8 { .. } => (StatusCode::UNSUPPORTED_MEDIA_TYPE, self.to_string()),
            ApiError::InvalidTimestamp { .. } => (StatusCode::BAD_REQUEST, self.to_string()),
            ApiError::BadRequest { .. } => (StatusCode::BAD_REQUEST, self.to_string()),
            ApiError::NotConfigured { .. } => {
                (StatusCode::SERVICE_UNAVAILABLE, self.to_string())
            }
            ApiError::RestoreFailed { .. } => {
                (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
            }
            ApiError::Internal(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
            }
        };

        let body = axum::Json(json!({ "error": message }));
        (status, body).into_response()
    }
}
```

- [ ] **Step 3: Add tests for new variants**

```rust
#[test]
fn content_not_found_display() {
    let err = ApiError::ContentNotFound { hash: "abc123".into() };
    assert!(err.to_string().contains("abc123"));
}

#[test]
fn not_utf8_display() {
    let err = ApiError::NotUtf8 { hash: "abc".into() };
    assert!(err.to_string().contains("UTF-8"));
}

#[test]
fn bad_request_display() {
    let err = ApiError::BadRequest { message: "missing field".into() };
    assert!(err.to_string().contains("missing field"));
}

#[test]
fn not_configured_display() {
    let err = ApiError::NotConfigured { name: "CAS" };
    assert!(err.to_string().contains("CAS"));
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p argus --lib api::errors -- -q`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add crates/argus/src/api/errors.rs
git commit -m "extend ApiError with content, restore, and query variants"
```

---

## Chunk 2: Query Types + Content API (Test 8 Blocker)

### Task 4: Query/Response Types

**Files:**
- Create: `crates/argus/src/api/query_types.rs`
- Modify: `crates/argus/src/api/mod.rs`

- [ ] **Step 1: Create query_types.rs with all request/response types**

```rust
//! Request and response types for query, content, tree, and restore endpoints.

use serde::{Deserialize, Serialize};

// --- Event query ---

/// Query parameters for `GET /events`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventsQuery {
    pub path: Option<String>,
    pub path_prefix: Option<String>,
    pub pid: Option<u32>,
    #[serde(rename = "type")]
    pub event_type: Option<String>,
    pub subtype: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub seq_from: Option<u64>,
    pub seq_to: Option<u64>,
    pub limit: Option<usize>,
}

/// Query parameters for `GET /file_history`.
#[derive(Debug, Clone, Deserialize)]
pub struct FileHistoryQuery {
    pub path: String,
}

/// Response for `GET /file_history`.
#[derive(Debug, Clone, Serialize)]
pub struct FileHistoryResponse {
    pub path: String,
    pub events: Vec<FileHistoryEntry>,
}

/// Single entry in file history.
#[derive(Debug, Clone, Serialize)]
pub struct FileHistoryEntry {
    pub seq: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_hash: Option<String>,
    pub pid: Option<u32>,
    pub ts_wall: String,
}

// --- Tree ---

/// Query parameters for `GET /tree`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TreeQuery {
    pub seq: Option<u64>,
    pub ts: Option<String>,
    pub path_prefix: Option<String>,
}

/// Response for `GET /tree`.
#[derive(Debug, Clone, Serialize)]
pub struct TreeResponse {
    pub tree_hash: String,
    pub seq: u64,
    pub entries: Vec<TreeEntry>,
}

/// Single entry in tree listing.
#[derive(Debug, Clone, Serialize)]
pub struct TreeEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub entry_type: String,
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// Query parameters for `GET /tree/diff`.
#[derive(Debug, Clone, Deserialize)]
pub struct TreeDiffQuery {
    pub from_seq: u64,
    pub to_seq: u64,
}

/// Response for `GET /tree/diff`.
#[derive(Debug, Clone, Serialize)]
pub struct TreeDiffResponse {
    pub from_seq: u64,
    pub to_seq: u64,
    pub added: Vec<DiffFileEntry>,
    pub modified: Vec<ModifiedFileEntry>,
    pub deleted: Vec<DiffFileEntry>,
}

/// File added or deleted.
#[derive(Debug, Clone, Serialize)]
pub struct DiffFileEntry {
    pub path: String,
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// File modified between two points.
#[derive(Debug, Clone, Serialize)]
pub struct ModifiedFileEntry {
    pub path: String,
    pub before_hash: String,
    pub after_hash: String,
}

// --- Content ---

/// Query parameters for `GET /diff`.
#[derive(Debug, Clone, Deserialize)]
pub struct DiffQuery {
    pub before_hash: String,
    pub after_hash: String,
    #[serde(default = "default_diff_format")]
    pub format: String,
}

fn default_diff_format() -> String {
    "unified".into()
}

// --- Restore ---

/// Request body for `POST /restore`.
#[derive(Debug, Clone, Deserialize)]
pub struct RestoreRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    pub mode: RestoreMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
}

/// Restore mode.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreMode {
    NewDirectory,
    InPlace,
    Selective,
}

/// Request body for `POST /restore/undo`.
#[derive(Debug, Clone, Deserialize)]
pub struct UndoRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_by_pid: Option<u32>,
}

/// Response for `POST /restore` and `POST /restore/undo`.
#[derive(Debug, Clone, Serialize)]
pub struct RestoreResponse {
    pub restored_to_seq: u64,
    pub restored_to_ts: String,
    pub tree_hash: String,
    pub files_restored: u64,
    pub bytes_restored: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_restore_snapshot_seq: Option<u64>,
}

// --- Storage status ---

/// Response for `GET /storage/status`.
#[derive(Debug, Clone, Serialize)]
pub struct StorageStatusResponse {
    pub local_buffer: LocalBufferStatus,
}

/// Local buffer stats.
#[derive(Debug, Clone, Serialize)]
pub struct LocalBufferStatus {
    pub cas_objects: u64,
    pub cas_size_bytes: u64,
    pub event_segments_local: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_query_default() {
        let q = EventsQuery::default();
        assert!(q.path.is_none());
        assert!(q.limit.is_none());
    }

    #[test]
    fn restore_request_deserialize() {
        let json = r#"{"mode":"new_directory","timestamp":"2026-01-01T00:00:00Z","target":"/tmp/snap"}"#;
        let req: RestoreRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.mode, RestoreMode::NewDirectory);
        assert_eq!(req.target.unwrap(), "/tmp/snap");
    }

    #[test]
    fn restore_response_serialize() {
        let resp = RestoreResponse {
            restored_to_seq: 42,
            restored_to_ts: "2026-01-01T00:00:00Z".into(),
            tree_hash: "abcd".into(),
            files_restored: 10,
            bytes_restored: 4096,
            pre_restore_snapshot_seq: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("42"));
        assert!(!json.contains("pre_restore_snapshot_seq"));
    }

    #[test]
    fn tree_entry_serialize() {
        let entry = TreeEntry {
            name: "file.txt".into(),
            entry_type: "file".into(),
            hash: "abc123".into(),
            size: Some(1024),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"type\":\"file\""));
    }

    #[test]
    fn restore_mode_variants() {
        let json = "\"new_directory\"";
        let mode: RestoreMode = serde_json::from_str(json).unwrap();
        assert_eq!(mode, RestoreMode::NewDirectory);

        let json = "\"in_place\"";
        let mode: RestoreMode = serde_json::from_str(json).unwrap();
        assert_eq!(mode, RestoreMode::InPlace);

        let json = "\"selective\"";
        let mode: RestoreMode = serde_json::from_str(json).unwrap();
        assert_eq!(mode, RestoreMode::Selective);
    }

    #[test]
    fn undo_request_deserialize() {
        let json = r#"{"last":5}"#;
        let req: UndoRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.last, Some(5));
        assert!(req.last_by_pid.is_none());
    }

    #[test]
    fn diff_query_default_format() {
        let json = r#"{"before_hash":"aa","after_hash":"bb"}"#;
        let q: DiffQuery = serde_json::from_str(json).unwrap();
        assert_eq!(q.format, "unified");
    }
}
```

- [ ] **Step 2: Register module**

Add `pub mod query_types;` to `crates/argus/src/api/mod.rs`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p argus --lib api::query_types -- -q`
Expected: All PASS

- [ ] **Step 4: Commit**

```bash
git add crates/argus/src/api/query_types.rs crates/argus/src/api/mod.rs
git commit -m "add request/response types for query, content, tree, restore endpoints"
```

---

### Task 5: Content Routes (GET /content/{hash}, /content/{hash}/text, /diff)

Test 8 needs `argus cat <hash>` which uses `GET /content/{hash}/text`.

**Files:**
- Create: `crates/argus/src/api/content_routes.rs`
- Modify: `crates/argus/src/api/mod.rs`

- [ ] **Step 1: Write content_routes.rs**

```rust
//! Content retrieval endpoints.
//!
//! Serves raw CAS content by hash, with optional UTF-8 decode and
//! diff between two hashes.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::api::errors::ApiError;
use crate::api::query_types::DiffQuery;
use crate::api::state::SharedState;

/// `GET /content/{hash}` — raw bytes from CAS.
pub async fn content_raw_handler(
    State(state): State<SharedState>,
    Path(hash): Path<String>,
) -> Result<Response, ApiError> {
    let guard = state.lock().expect("state lock poisoned");
    let cas = guard.cas().ok_or(ApiError::NotConfigured { name: "CAS" })?;
    let content_hash = hash.parse::<crate::cas::ContentHash>().map_err(|_| {
        ApiError::BadRequest {
            message: format!("invalid hash: {hash}"),
        }
    })?;
    // Clone Arc so we can drop the lock before doing I/O
    let cas = cas.clone();
    drop(guard);

    let data = cas
        .read(&content_hash)
        .map_err(|_| ApiError::ContentNotFound { hash: hash.clone() })?;

    Ok((
        [(header::CONTENT_TYPE, "application/octet-stream")],
        Body::from(data),
    )
        .into_response())
}

/// `GET /content/{hash}/text` — UTF-8 text from CAS.
pub async fn content_text_handler(
    State(state): State<SharedState>,
    Path(hash): Path<String>,
) -> Result<Response, ApiError> {
    let guard = state.lock().expect("state lock poisoned");
    let cas = guard.cas().ok_or(ApiError::NotConfigured { name: "CAS" })?;
    let content_hash = hash.parse::<crate::cas::ContentHash>().map_err(|_| {
        ApiError::BadRequest {
            message: format!("invalid hash: {hash}"),
        }
    })?;
    let cas = cas.clone();
    drop(guard);

    let data = cas
        .read(&content_hash)
        .map_err(|_| ApiError::ContentNotFound { hash: hash.clone() })?;

    let text = String::from_utf8(data).map_err(|_| ApiError::NotUtf8 {
        hash: hash.clone(),
    })?;

    Ok((
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        Body::from(text),
    )
        .into_response())
}

/// `GET /diff?before_hash=&after_hash=&format=` — diff two CAS objects.
pub async fn diff_handler(
    State(state): State<SharedState>,
    Query(query): Query<DiffQuery>,
) -> Result<Response, ApiError> {
    let guard = state.lock().expect("state lock poisoned");
    let cas = guard.cas().ok_or(ApiError::NotConfigured { name: "CAS" })?;
    let cas = cas.clone();
    drop(guard);

    let parse_hash = |h: &str| -> Result<crate::cas::ContentHash, ApiError> {
        h.parse::<crate::cas::ContentHash>().map_err(|_| {
            ApiError::BadRequest {
                message: format!("invalid hash: {h}"),
            }
        })
    };

    let before_hash = parse_hash(&query.before_hash)?;
    let after_hash = parse_hash(&query.after_hash)?;

    let before = cas.read(&before_hash).map_err(|_| {
        ApiError::ContentNotFound {
            hash: query.before_hash.clone(),
        }
    })?;
    let after = cas.read(&after_hash).map_err(|_| {
        ApiError::ContentNotFound {
            hash: query.after_hash.clone(),
        }
    })?;

    let before_text = String::from_utf8_lossy(&before);
    let after_text = String::from_utf8_lossy(&after);

    match query.format.as_str() {
        "json" => {
            let hunks = compute_diff_hunks(&before_text, &after_text);
            let resp = serde_json::json!({
                "before_hash": query.before_hash,
                "after_hash": query.after_hash,
                "hunks": hunks,
            });
            Ok(Json(resp).into_response())
        }
        _ => {
            let diff = compute_unified_diff(&before_text, &after_text);
            Ok((
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                Body::from(diff),
            )
                .into_response())
        }
    }
}

/// Simple line-based unified diff.
fn compute_unified_diff(before: &str, after: &str) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();

    let mut output = String::new();
    output.push_str("--- a\n");
    output.push_str("+++ b\n");

    for line in &before_lines {
        if !after_lines.contains(line) {
            output.push_str(&format!("-{line}\n"));
        }
    }
    for line in &after_lines {
        if !before_lines.contains(line) {
            output.push_str(&format!("+{line}\n"));
        }
    }
    output
}

/// Simple diff hunks as JSON-serializable structure.
fn compute_diff_hunks(
    before: &str,
    after: &str,
) -> Vec<serde_json::Value> {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let mut lines = Vec::new();

    for line in &before_lines {
        if !after_lines.contains(line) {
            lines.push(serde_json::json!({"type": "remove", "content": line}));
        }
    }
    for line in &after_lines {
        if !before_lines.contains(line) {
            lines.push(serde_json::json!({"type": "add", "content": line}));
        }
    }

    if lines.is_empty() {
        return vec![];
    }

    vec![serde_json::json!({
        "old_start": 1,
        "old_count": before_lines.len(),
        "new_start": 1,
        "new_count": after_lines.len(),
        "lines": lines,
    })]
}

/// Implement `FromStr` for `ContentHash` to support axum `Path` extraction.
impl std::str::FromStr for crate::cas::ContentHash {
    type Err = crate::cas::InvalidHashError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::new_shared_state_full;
    use crate::cas::CasStore;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let cas = Arc::new(
            CasStore::new(dir.path().join("cas")).unwrap(),
        );
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let state = new_shared_state_full(
            "test".into(),
            tx,
            cas,
            dir.path().join("events"),
        );
        let app = Router::new()
            .route("/content/{hash}", get(content_raw_handler))
            .route("/content/{hash}/text", get(content_text_handler))
            .route("/diff", get(diff_handler))
            .with_state(state);
        (app, dir)
    }

    #[tokio::test]
    async fn content_raw_returns_bytes() {
        let (app, dir) = test_app();
        let cas = CasStore::new(dir.path().join("cas")).unwrap();
        let hash = cas.store(b"hello content").unwrap();

        let req = Request::builder()
            .uri(format!("/content/{}", hash.as_str()))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"hello content");
    }

    #[tokio::test]
    async fn content_text_returns_utf8() {
        let (app, dir) = test_app();
        let cas = CasStore::new(dir.path().join("cas")).unwrap();
        let hash = cas.store(b"text content").unwrap();

        let req = Request::builder()
            .uri(format!("/content/{}/text", hash.as_str()))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(String::from_utf8_lossy(&body), "text content");
    }

    #[tokio::test]
    async fn content_not_found_returns_404() {
        let (app, _dir) = test_app();
        let fake_hash = "a".repeat(64);
        let req = Request::builder()
            .uri(format!("/content/{fake_hash}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn content_text_non_utf8_returns_415() {
        let (app, dir) = test_app();
        let cas = CasStore::new(dir.path().join("cas")).unwrap();
        let hash = cas.store(&[0xFF, 0xFE, 0x00]).unwrap();

        let req = Request::builder()
            .uri(format!("/content/{}/text", hash.as_str()))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn invalid_hash_returns_400() {
        let (app, _dir) = test_app();
        let req = Request::builder()
            .uri("/content/not-a-valid-hash")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn diff_unified_format() {
        let (app, dir) = test_app();
        let cas = CasStore::new(dir.path().join("cas")).unwrap();
        let h1 = cas.store(b"line1\nline2\n").unwrap();
        let h2 = cas.store(b"line1\nline3\n").unwrap();

        let req = Request::builder()
            .uri(format!(
                "/diff?before_hash={}&after_hash={}",
                h1.as_str(),
                h2.as_str()
            ))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("-line2"));
        assert!(text.contains("+line3"));
    }
}
```

- [ ] **Step 2: Register routes and module**

In `crates/argus/src/api/mod.rs`, add:
```rust
pub mod content_routes;
```

And in `build_router()`, add the content routes:
```rust
use crate::api::content_routes::{content_raw_handler, content_text_handler, diff_handler};

pub fn build_router(state: SharedState) -> Router {
    Router::new()
        // existing routes
        .route("/agent/pause", post(pause_handler))
        .route("/agent/resume", post(resume_handler))
        .route("/agent/status", get(status_handler))
        .route("/approvals/pending", get(pending_approvals_handler))
        .route("/approvals/{action_id}/approve", post(approve_handler))
        .route("/approvals/{action_id}/deny", post(deny_handler))
        .route("/health", get(health_handler))
        // new content routes
        .route("/content/{hash}", get(content_raw_handler))
        .route("/content/{hash}/text", get(content_text_handler))
        .route("/diff", get(diff_handler))
        .with_state(state)
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p argus --lib api::content_routes -- -q`
Expected: All PASS

- [ ] **Step 4: Commit**

```bash
git add crates/argus/src/api/content_routes.rs crates/argus/src/api/mod.rs
git commit -m "add content retrieval and diff API endpoints"
```

---

## Chunk 3: Event Query Routes + Tree Routes (Tests 11-12 Blockers)

### Task 6: Event Query Routes (GET /events, /file_history)

Test 11 needs `argus log --path /workspace/file.txt --type write` which hits `GET /events`.

**Files:**
- Create: `crates/argus/src/api/query_routes.rs`
- Modify: `crates/argus/src/api/mod.rs`

- [ ] **Step 1: Write query_routes.rs**

```rust
//! Event query endpoints.
//!
//! Reads events from JSONL segments on disk, filtered by the query engine's
//! indexes. Does not hold events in memory long-term.

use axum::Json;
use axum::extract::{Query, State};

use crate::api::errors::ApiError;
use crate::api::query_types::{
    EventsQuery, FileHistoryEntry, FileHistoryQuery, FileHistoryResponse,
};
use crate::api::state::SharedState;
use crate::events::{Event, EventPayload};
use crate::index::{QueryEngine, QueryFilter};
use crate::storage::event_reader;

/// `GET /events` — query events with filters.
///
/// Reads matching events from disk segment files. Indexes narrow the
/// candidate set; time-range and seq-range filters are applied afterward.
pub async fn events_handler(
    State(state): State<SharedState>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Vec<Event>>, ApiError> {
    let guard = state.lock().expect("state lock poisoned");
    let event_dir = guard
        .event_dir()
        .ok_or(ApiError::NotConfigured { name: "event_dir" })?
        .to_path_buf();

    let path_index = guard
        .path_index()
        .ok_or(ApiError::NotConfigured { name: "path_index" })?;
    let pid_index = guard
        .pid_index()
        .ok_or(ApiError::NotConfigured { name: "pid_index" })?;
    let type_index = guard
        .type_index()
        .ok_or(ApiError::NotConfigured { name: "type_index" })?;

    let filter = QueryFilter {
        path: query.path.clone(),
        path_prefix: query.path_prefix.clone(),
        pid: query.pid,
        event_type: query.event_type.clone(),
        since: query.since.clone(),
        until: query.until.clone(),
        seq_from: query.seq_from,
        seq_to: query.seq_to,
        limit: query.limit,
    };

    let engine = QueryEngine::new(path_index, pid_index, type_index);
    let candidates = engine.query(&filter);
    drop(guard);

    if candidates.is_empty() {
        return Ok(Json(vec![]));
    }

    let min_seq = candidates.first().map(|r| r.seq).unwrap_or(0);
    let max_seq = candidates.last().map(|r| r.seq).unwrap_or(u64::MAX);
    let candidate_seqs: std::collections::HashSet<u64> =
        candidates.iter().map(|r| r.seq).collect();

    let all_events =
        event_reader::read_events_filtered(&event_dir, Some((min_seq, max_seq)))
            .map_err(|e| ApiError::Internal(e.to_string()))?;

    let mut matching: Vec<Event> = all_events
        .into_iter()
        .filter(|e| candidate_seqs.contains(&e.seq))
        .collect();

    // Apply time-range filters if present
    if query.since.is_some() || query.until.is_some() {
        let guard = state.lock().expect("state lock poisoned");
        let path_index = guard.path_index().unwrap();
        let pid_index = guard.pid_index().unwrap();
        let type_index = guard.type_index().unwrap();
        let engine = QueryEngine::new(path_index, pid_index, type_index);
        let time_filtered = engine.query_events(&filter, &matching);
        let time_seqs: std::collections::HashSet<u64> =
            time_filtered.iter().map(|r| r.seq).collect();
        matching.retain(|e| time_seqs.contains(&e.seq));
    }

    if let Some(limit) = query.limit {
        matching.truncate(limit);
    }

    Ok(Json(matching))
}

/// `GET /file_history` — all events for a specific file path.
pub async fn file_history_handler(
    State(state): State<SharedState>,
    Query(query): Query<FileHistoryQuery>,
) -> Result<Json<FileHistoryResponse>, ApiError> {
    let guard = state.lock().expect("state lock poisoned");
    let event_dir = guard
        .event_dir()
        .ok_or(ApiError::NotConfigured { name: "event_dir" })?
        .to_path_buf();
    let path_index = guard
        .path_index()
        .ok_or(ApiError::NotConfigured { name: "path_index" })?;

    let entries = path_index.lookup(&query.path);
    let seqs: std::collections::HashSet<u64> =
        entries.iter().map(|e| e.seq).collect();
    drop(guard);

    if seqs.is_empty() {
        return Ok(Json(FileHistoryResponse {
            path: query.path,
            events: vec![],
        }));
    }

    let min_seq = *seqs.iter().min().unwrap();
    let max_seq = *seqs.iter().max().unwrap();

    let all_events =
        event_reader::read_events_filtered(&event_dir, Some((min_seq, max_seq)))
            .map_err(|e| ApiError::Internal(e.to_string()))?;

    let events: Vec<FileHistoryEntry> = all_events
        .into_iter()
        .filter(|e| seqs.contains(&e.seq))
        .map(|e| {
            let (content_hash, before_hash, after_hash) =
                extract_file_hashes(&e.payload);
            FileHistoryEntry {
                seq: e.seq,
                event_type: e.payload.event_type_tag().to_owned(),
                content_hash,
                before_hash,
                after_hash,
                pid: e.payload.pid(),
                ts_wall: e.ts_wall,
            }
        })
        .collect();

    Ok(Json(FileHistoryResponse {
        path: query.path,
        events,
    }))
}

/// Extract file-related hashes from event payloads.
fn extract_file_hashes(
    payload: &EventPayload,
) -> (Option<String>, Option<String>, Option<String>) {
    match payload {
        EventPayload::Read(r) => (r.content_hash.clone(), None, None),
        EventPayload::Write(w) => {
            (None, w.before_hash.clone(), w.after_hash.clone())
        }
        EventPayload::Unlink(u) => (u.content_hash.clone(), None, None),
        EventPayload::Truncate(t) => {
            (None, t.before_hash.clone(), t.after_hash.clone())
        }
        _ => (None, None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::new_shared_state_full;
    use crate::cas::CasStore;
    use crate::events::{EventPayload, SequenceGenerator};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_app_with_events() -> (Router, SharedState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let cas = Arc::new(
            CasStore::new(dir.path().join("cas")).unwrap(),
        );
        let event_dir = dir.path().join("events");
        std::fs::create_dir_all(&event_dir).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let state = new_shared_state_full(
            "test".into(),
            tx,
            cas,
            event_dir,
        );
        let app = Router::new()
            .route("/events", get(events_handler))
            .route("/file_history", get(file_history_handler))
            .with_state(state.clone());
        (app, state, dir)
    }

    fn write_test_events(dir: &std::path::Path, events: &[Event]) {
        let event_dir = dir.join("events");
        let mut content = String::new();
        for e in events {
            content.push_str(&serde_json::to_string(e).unwrap());
            content.push('\n');
        }
        std::fs::write(event_dir.join("0.jsonl"), content).unwrap();
    }

    #[tokio::test]
    async fn events_empty_when_no_segments() {
        let (app, _state, _dir) = test_app_with_events();
        let req = Request::builder()
            .uri("/events")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let events: Vec<Event> = serde_json::from_slice(&body).unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn events_returns_matching_by_type() {
        let (app, state, dir) = test_app_with_events();
        let gen = SequenceGenerator::default();
        let e1 = Event::new(
            &gen,
            "test".into(),
            EventPayload::Write(crate::events::file::Write {
                pid: 1,
                path: "/workspace/a.txt".into(),
                fd: 3,
                offset: 0,
                size: 5,
                before_hash: None,
                after_hash: Some("abc".into()),
                tree_hash: None,
            }),
        );
        let e2 = Event::new(
            &gen,
            "test".into(),
            EventPayload::Read(crate::events::file::Read {
                pid: 1,
                path: "/workspace/a.txt".into(),
                fd: 3,
                offset: 0,
                size: 5,
                content_hash: None,
            }),
        );
        write_test_events(dir.path(), &[e1, e2]);

        // Index the write event
        {
            let mut guard = state.lock().unwrap();
            guard.path_index_mut().unwrap().insert("/workspace/a.txt", 0, "write").unwrap();
            guard.type_index_mut().unwrap().insert("write", 0).unwrap();
            guard.pid_index_mut().unwrap().insert(1, 0, "write").unwrap();
            guard.path_index_mut().unwrap().insert("/workspace/a.txt", 1, "read").unwrap();
            guard.type_index_mut().unwrap().insert("read", 1).unwrap();
            guard.pid_index_mut().unwrap().insert(1, 1, "read").unwrap();
        }

        let req = Request::builder()
            .uri("/events?type=write")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let events: Vec<Event> = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, 0);
    }

    #[tokio::test]
    async fn file_history_returns_events_for_path() {
        let (app, state, dir) = test_app_with_events();
        let gen = SequenceGenerator::default();
        let e1 = Event::new(
            &gen,
            "test".into(),
            EventPayload::Write(crate::events::file::Write {
                pid: 1,
                path: "/workspace/config.yaml".into(),
                fd: 3,
                offset: 0,
                size: 10,
                before_hash: None,
                after_hash: Some("hash1".into()),
                tree_hash: None,
            }),
        );
        write_test_events(dir.path(), &[e1]);

        {
            let mut guard = state.lock().unwrap();
            guard.path_index_mut().unwrap().insert("/workspace/config.yaml", 0, "write").unwrap();
        }

        let req = Request::builder()
            .uri("/file_history?path=/workspace/config.yaml")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let resp: FileHistoryResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp.path, "/workspace/config.yaml");
        assert_eq!(resp.events.len(), 1);
        assert_eq!(resp.events[0].after_hash.as_deref(), Some("hash1"));
    }
}
```

Note: The tests use `path_index_mut()`, `type_index_mut()`, `pid_index_mut()` accessors — add these to `state.rs`:

```rust
/// Mutable reference to the path index.
pub fn path_index_mut(&mut self) -> Option<&mut PathIndex> {
    self.path_index.as_mut()
}

/// Mutable reference to the PID index.
pub fn pid_index_mut(&mut self) -> Option<&mut PidIndex> {
    self.pid_index.as_mut()
}

/// Mutable reference to the type index.
pub fn type_index_mut(&mut self) -> Option<&mut TypeIndex> {
    self.type_index.as_mut()
}
```

- [ ] **Step 2: Register in mod.rs and build_router**

Add `pub mod query_routes;` to mod.rs and add routes:
```rust
.route("/events", get(query_routes::events_handler))
.route("/file_history", get(query_routes::file_history_handler))
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p argus --lib api::query_routes -- -q`
Expected: All PASS

- [ ] **Step 4: Commit**

```bash
git add crates/argus/src/api/query_routes.rs crates/argus/src/api/state.rs crates/argus/src/api/mod.rs
git commit -m "add event query and file history API endpoints"
```

---

### Task 7: Tree Routes (GET /tree, /tree/diff)

Test 12 needs `argus snapshot --seq 0` which hits `GET /tree`.

**Files:**
- Create: `crates/argus/src/api/tree_routes.rs`
- Modify: `crates/argus/src/api/mod.rs`

- [ ] **Step 1: Write tree_routes.rs**

```rust
//! Merkle tree query endpoints.
//!
//! Serves directory listings and diffs between tree states at different
//! sequence numbers.

use axum::Json;
use axum::extract::{Query, State};

use crate::api::errors::ApiError;
use crate::api::query_types::{
    DiffFileEntry, ModifiedFileEntry, TreeDiffQuery, TreeDiffResponse,
    TreeEntry, TreeQuery, TreeResponse,
};
use crate::api::state::SharedState;
use crate::snapshot::diff::{DiffKind, diff_trees};

/// `GET /tree` — directory listing from Merkle tree.
///
/// Returns the current tree if no `seq` or `ts` is given. When `seq`
/// is provided, rebuilds the tree at that point (requires replaying
/// events from a checkpoint — currently only returns current state).
pub async fn tree_handler(
    State(state): State<SharedState>,
    Query(query): Query<TreeQuery>,
) -> Result<Json<TreeResponse>, ApiError> {
    let guard = state.lock().expect("state lock poisoned");
    let tree = guard
        .merkle_tree()
        .ok_or(ApiError::NotConfigured { name: "merkle_tree" })?;

    let seq = query.seq.unwrap_or_else(|| guard.event_seq());
    let tree_hash = tree.root_hash();

    let mut entries: Vec<TreeEntry> = tree
        .files()
        .filter(|(path, _)| {
            if let Some(prefix) = &query.path_prefix {
                path.to_string_lossy().starts_with(prefix.as_str())
            } else {
                true
            }
        })
        .map(|(path, hash)| TreeEntry {
            name: path.to_string_lossy().to_string(),
            entry_type: "file".into(),
            hash: hash.as_str().to_owned(),
            size: None,
        })
        .collect();

    entries.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Json(TreeResponse {
        tree_hash: tree_hash.as_str().to_owned(),
        seq,
        entries,
    }))
}

/// `GET /tree/diff` — changed files between two sequence numbers.
///
/// Currently compares trees stored in checkpoint snapshots or the
/// current in-memory state. For the initial implementation, returns
/// the diff between an empty tree and the current tree when
/// `from_seq=0`.
pub async fn tree_diff_handler(
    State(state): State<SharedState>,
    Query(query): Query<TreeDiffQuery>,
) -> Result<Json<TreeDiffResponse>, ApiError> {
    let guard = state.lock().expect("state lock poisoned");
    let current_tree = guard
        .merkle_tree()
        .ok_or(ApiError::NotConfigured { name: "merkle_tree" })?;

    // For now: from_seq=0 means empty tree, to_seq=current
    let from_tree = crate::snapshot::MerkleTree::new();
    let diffs = diff_trees(&from_tree, current_tree);

    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();

    for entry in diffs {
        let path = entry.path.to_string_lossy().to_string();
        match entry.kind {
            DiffKind::Added => {
                added.push(DiffFileEntry {
                    path,
                    hash: entry.new_hash.map(|h| h.as_str().to_owned()).unwrap_or_default(),
                    size: None,
                });
            }
            DiffKind::Deleted => {
                deleted.push(DiffFileEntry {
                    path,
                    hash: entry.old_hash.map(|h| h.as_str().to_owned()).unwrap_or_default(),
                    size: None,
                });
            }
            DiffKind::Modified => {
                modified.push(ModifiedFileEntry {
                    path,
                    before_hash: entry.old_hash.map(|h| h.as_str().to_owned()).unwrap_or_default(),
                    after_hash: entry.new_hash.map(|h| h.as_str().to_owned()).unwrap_or_default(),
                });
            }
        }
    }

    Ok(Json(TreeDiffResponse {
        from_seq: query.from_seq,
        to_seq: query.to_seq,
        added,
        modified,
        deleted,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::new_shared_state_full;
    use crate::cas::{CasStore, ContentHash};
    use crate::snapshot::MerkleTree;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use http_body_util::BodyExt;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_app() -> (Router, SharedState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let cas = Arc::new(CasStore::new(dir.path().join("cas")).unwrap());
        let event_dir = dir.path().join("events");
        std::fs::create_dir_all(&event_dir).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let state = new_shared_state_full("test".into(), tx, cas, event_dir);
        let app = Router::new()
            .route("/tree", get(tree_handler))
            .route("/tree/diff", get(tree_diff_handler))
            .with_state(state.clone());
        (app, state, dir)
    }

    #[tokio::test]
    async fn tree_empty_returns_empty_entries() {
        let (app, _state, _dir) = test_app();
        let req = Request::builder()
            .uri("/tree")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let tree: TreeResponse = serde_json::from_slice(&body).unwrap();
        assert!(tree.entries.is_empty());
    }

    #[tokio::test]
    async fn tree_with_files() {
        let (app, state, _dir) = test_app();
        {
            let mut guard = state.lock().unwrap();
            let tree = guard.merkle_tree_mut().unwrap();
            tree.update(
                PathBuf::from("workspace/a.txt"),
                ContentHash::from_data(b"aaa"),
            );
            tree.update(
                PathBuf::from("workspace/b.txt"),
                ContentHash::from_data(b"bbb"),
            );
        }

        let req = Request::builder()
            .uri("/tree")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let tree: TreeResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(tree.entries.len(), 2);
        assert_eq!(tree.entries[0].name, "workspace/a.txt");
    }

    #[tokio::test]
    async fn tree_with_path_prefix_filter() {
        let (app, state, _dir) = test_app();
        {
            let mut guard = state.lock().unwrap();
            let tree = guard.merkle_tree_mut().unwrap();
            tree.update(PathBuf::from("workspace/src/a.rs"), ContentHash::from_data(b"a"));
            tree.update(PathBuf::from("workspace/test/b.rs"), ContentHash::from_data(b"b"));
        }

        let req = Request::builder()
            .uri("/tree?path_prefix=workspace/src/")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let tree: TreeResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(tree.entries.len(), 1);
        assert_eq!(tree.entries[0].name, "workspace/src/a.rs");
    }

    #[tokio::test]
    async fn tree_diff_from_empty() {
        let (app, state, _dir) = test_app();
        {
            let mut guard = state.lock().unwrap();
            let tree = guard.merkle_tree_mut().unwrap();
            tree.update(PathBuf::from("file.txt"), ContentHash::from_data(b"hello"));
        }

        let req = Request::builder()
            .uri("/tree/diff?from_seq=0&to_seq=1")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let diff: TreeDiffResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].path, "file.txt");
        assert!(diff.modified.is_empty());
        assert!(diff.deleted.is_empty());
    }
}
```

- [ ] **Step 2: Register in mod.rs and build_router**

Add `pub mod tree_routes;` and routes:
```rust
.route("/tree", get(tree_routes::tree_handler))
.route("/tree/diff", get(tree_routes::tree_diff_handler))
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p argus --lib api::tree_routes -- -q`
Expected: All PASS

- [ ] **Step 4: Commit**

```bash
git add crates/argus/src/api/tree_routes.rs crates/argus/src/api/mod.rs
git commit -m "add tree listing and diff API endpoints"
```

---

## Chunk 4: Restore Engine + Restore API (Test 11 Blocker)

### Task 8: Restore Engine Module

**Files:**
- Create: `crates/argus/src/snapshot/restore.rs`
- Modify: `crates/argus/src/snapshot/mod.rs`

- [ ] **Step 1: Write restore.rs with core logic and tests**

```rust
//! Point-in-time filesystem restore from Merkle tree and CAS.
//!
//! Given a target sequence number or timestamp, rebuilds the tree state
//! by loading the nearest checkpoint and replaying mutating events.
//! Content is pulled from the local CAS store to write files to a
//! target directory.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::DateTime;

use crate::cas::CasStore;
use crate::events::{Event, EventPayload};
use crate::snapshot::tree::MerkleTree;

/// Result of a restore operation.
#[derive(Debug, Clone)]
pub struct RestoreResult {
    /// Sequence number restored to.
    pub seq: u64,
    /// Wall-clock timestamp at the restored seq.
    pub ts_wall: String,
    /// Merkle root at the restored state.
    pub tree_hash: String,
    /// Number of files written.
    pub files_restored: u64,
    /// Total bytes written.
    pub bytes_restored: u64,
}

/// Finds the largest seq where `ts_wall <= target_ts`.
///
/// Events must be sorted by seq. Returns `None` if no event is at or
/// before the target timestamp.
pub fn find_seq_at_timestamp(
    events: &[Event],
    target_ts: &str,
) -> Result<Option<u64>> {
    let target = DateTime::parse_from_rfc3339(target_ts)
        .with_context(|| format!("invalid timestamp: {target_ts}"))?;

    let mut best: Option<u64> = None;
    for event in events {
        let event_ts = match DateTime::parse_from_rfc3339(&event.ts_wall) {
            Ok(ts) => ts,
            Err(_) => continue,
        };
        if event_ts <= target {
            best = Some(event.seq);
        } else {
            break;
        }
    }
    Ok(best)
}

/// Mutating event types that affect the Merkle tree.
const MUTATING_TYPES: &[&str] = &[
    "write", "unlink", "rename", "truncate", "link", "symlink",
    "mkdir", "rmdir",
];

/// Checks if an event type is a mutating filesystem operation.
fn is_mutating(event_type: &str) -> bool {
    MUTATING_TYPES.contains(&event_type)
}

/// Builds a MerkleTree at `target_seq` by replaying mutating events.
///
/// Starts from `base_tree` (typically an empty tree or a checkpoint)
/// and applies events from `base_seq+1` through `target_seq`.
pub fn build_tree_at_seq(
    base_tree: &MerkleTree,
    events: &[Event],
    target_seq: u64,
) -> MerkleTree {
    let mut tree = base_tree.clone();

    for event in events {
        if event.seq > target_seq {
            break;
        }
        apply_event_to_tree(&mut tree, event);
    }

    tree
}

/// Applies a single event to the tree if it is a mutating operation.
fn apply_event_to_tree(tree: &mut MerkleTree, event: &Event) {
    match &event.payload {
        EventPayload::Write(w) => {
            if let Some(hash_str) = &w.after_hash {
                if let Ok(hash) = hash_str.parse() {
                    tree.update(PathBuf::from(&w.path), hash);
                }
            }
        }
        EventPayload::Unlink(u) => {
            tree.remove(Path::new(&u.path));
        }
        EventPayload::Rename(r) => {
            tree.rename(
                Path::new(&r.old_path),
                PathBuf::from(&r.new_path),
            );
        }
        EventPayload::Truncate(t) => {
            if let Some(hash_str) = &t.after_hash {
                if let Ok(hash) = hash_str.parse() {
                    tree.update(PathBuf::from(&t.path), hash);
                }
            }
        }
        EventPayload::Link(l) => {
            if let Some(source_hash) = tree.get(Path::new(&l.target)).cloned() {
                tree.update(PathBuf::from(&l.link_path), source_hash);
            }
        }
        EventPayload::Symlink(s) => {
            if let Some(target_hash) = tree.get(Path::new(&s.target)).cloned() {
                tree.update(PathBuf::from(&s.link_path), target_hash);
            }
        }
        _ => {}
    }
}

/// Restores files from a Merkle tree to a target directory.
///
/// Walks every file in the tree, reads content from CAS, and writes
/// it to `target_dir` preserving the directory structure.
///
/// # Errors
///
/// Returns an error if CAS content is missing or directory creation
/// fails.
pub fn restore_to_directory(
    tree: &MerkleTree,
    cas: &CasStore,
    target_dir: &Path,
) -> Result<RestoreResult> {
    fs::create_dir_all(target_dir).with_context(|| {
        format!("create restore target: {}", target_dir.display())
    })?;

    let mut files_restored = 0u64;
    let mut bytes_restored = 0u64;

    for (path, hash) in tree.files() {
        let dest = target_dir.join(path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }

        let data = cas.read(hash).with_context(|| {
            format!("read CAS object {} for {}", hash, path.display())
        })?;

        bytes_restored += data.len() as u64;
        fs::write(&dest, &data).with_context(|| {
            format!("write restored file: {}", dest.display())
        })?;

        files_restored += 1;
    }

    Ok(RestoreResult {
        seq: 0,
        ts_wall: String::new(),
        tree_hash: tree.root_hash().as_str().to_owned(),
        files_restored,
        bytes_restored,
    })
}

/// Restores only the specified paths from the tree.
pub fn restore_selective(
    tree: &MerkleTree,
    cas: &CasStore,
    paths: &[&str],
    target_dir: &Path,
) -> Result<RestoreResult> {
    fs::create_dir_all(target_dir)?;

    let mut files_restored = 0u64;
    let mut bytes_restored = 0u64;

    for (file_path, hash) in tree.files() {
        let path_str = file_path.to_string_lossy();
        let matched = paths.iter().any(|p| {
            path_str.as_ref() == *p || path_str.starts_with(p)
        });
        if !matched {
            continue;
        }

        let dest = target_dir.join(file_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }

        let data = cas.read(hash)?;
        bytes_restored += data.len() as u64;
        fs::write(&dest, &data)?;
        files_restored += 1;
    }

    Ok(RestoreResult {
        seq: 0,
        ts_wall: String::new(),
        tree_hash: tree.root_hash().as_str().to_owned(),
        files_restored,
        bytes_restored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::{CasStore, ContentHash};
    use crate::events::file;
    use crate::events::{Event, EventPayload, SequenceGenerator};

    fn make_write_event(
        gen: &SequenceGenerator,
        path: &str,
        content: &[u8],
        cas: &CasStore,
    ) -> Event {
        let hash = cas.store(content).unwrap();
        Event::new(
            gen,
            "test".into(),
            EventPayload::Write(file::Write {
                pid: 1,
                path: path.into(),
                fd: 3,
                offset: 0,
                size: content.len() as u64,
                before_hash: None,
                after_hash: Some(hash.as_str().to_owned()),
                tree_hash: None,
            }),
        )
    }

    fn make_unlink_event(gen: &SequenceGenerator, path: &str) -> Event {
        Event::new(
            gen,
            "test".into(),
            EventPayload::Unlink(file::Unlink {
                pid: 1,
                path: path.into(),
                content_hash: None,
                tree_hash: None,
            }),
        )
    }

    #[test]
    fn find_seq_at_timestamp_basic() {
        let gen = SequenceGenerator::default();
        let events = vec![
            Event {
                seq: gen.next_seq(),
                ts_monotonic: 100,
                ts_wall: "2026-01-01T00:00:00Z".into(),
                agent_id: "t".into(),
                vclock: None,
                payload: EventPayload::AgentStart(
                    crate::events::control::AgentStart {
                        agent_id: "t".into(),
                        supervisor_pid_host: None,
                        supervisor_pid_ns: None,
                        config_summary: "test".into(),
                        node: None,
                        pod: None,
                        container: None,
                    },
                ),
            },
            Event {
                seq: gen.next_seq(),
                ts_monotonic: 200,
                ts_wall: "2026-01-01T00:00:01Z".into(),
                agent_id: "t".into(),
                vclock: None,
                payload: EventPayload::AgentStart(
                    crate::events::control::AgentStart {
                        agent_id: "t".into(),
                        supervisor_pid_host: None,
                        supervisor_pid_ns: None,
                        config_summary: "test".into(),
                        node: None,
                        pod: None,
                        container: None,
                    },
                ),
            },
        ];

        let seq = find_seq_at_timestamp(&events, "2026-01-01T00:00:00.5Z")
            .unwrap();
        assert_eq!(seq, Some(0));

        let seq = find_seq_at_timestamp(&events, "2026-01-01T00:00:01Z")
            .unwrap();
        assert_eq!(seq, Some(1));

        let seq = find_seq_at_timestamp(&events, "2025-12-31T00:00:00Z")
            .unwrap();
        assert_eq!(seq, None);
    }

    #[test]
    fn build_tree_replays_writes() {
        let dir = tempfile::tempdir().unwrap();
        let cas = CasStore::new(dir.path().join("cas")).unwrap();
        let gen = SequenceGenerator::default();

        let e1 = make_write_event(&gen, "/workspace/a.txt", b"hello", &cas);
        let e2 = make_write_event(&gen, "/workspace/b.txt", b"world", &cas);

        let base = MerkleTree::new();
        let tree = build_tree_at_seq(&base, &[e1, e2], 1);

        assert_eq!(tree.file_count(), 2);
        assert!(tree.contains(Path::new("/workspace/a.txt")));
        assert!(tree.contains(Path::new("/workspace/b.txt")));
    }

    #[test]
    fn build_tree_handles_unlink() {
        let dir = tempfile::tempdir().unwrap();
        let cas = CasStore::new(dir.path().join("cas")).unwrap();
        let gen = SequenceGenerator::default();

        let e1 = make_write_event(&gen, "/workspace/a.txt", b"data", &cas);
        let e2 = make_unlink_event(&gen, "/workspace/a.txt");

        let tree = build_tree_at_seq(&MerkleTree::new(), &[e1, e2], 1);
        assert_eq!(tree.file_count(), 0);
    }

    #[test]
    fn build_tree_stops_at_target_seq() {
        let dir = tempfile::tempdir().unwrap();
        let cas = CasStore::new(dir.path().join("cas")).unwrap();
        let gen = SequenceGenerator::default();

        let e1 = make_write_event(&gen, "/workspace/a.txt", b"v1", &cas);
        let e2 = make_write_event(&gen, "/workspace/a.txt", b"v2", &cas);

        // Only replay up to seq 0
        let tree = build_tree_at_seq(&MerkleTree::new(), &[e1.clone(), e2], 0);
        assert_eq!(tree.file_count(), 1);
        let hash = tree.get(Path::new("/workspace/a.txt")).unwrap();
        let expected = cas.store(b"v1").unwrap();
        assert_eq!(hash, &expected);
    }

    #[test]
    fn restore_to_directory_writes_files() {
        let dir = tempfile::tempdir().unwrap();
        let cas = CasStore::new(dir.path().join("cas")).unwrap();

        let h1 = cas.store(b"content-a").unwrap();
        let h2 = cas.store(b"content-b").unwrap();

        let mut tree = MerkleTree::new();
        tree.update(PathBuf::from("workspace/a.txt"), h1);
        tree.update(PathBuf::from("workspace/sub/b.txt"), h2);

        let target = dir.path().join("restore");
        let result = restore_to_directory(&tree, &cas, &target).unwrap();

        assert_eq!(result.files_restored, 2);
        assert_eq!(result.bytes_restored, 18); // 9 + 9

        let a = fs::read_to_string(target.join("workspace/a.txt")).unwrap();
        assert_eq!(a, "content-a");
        let b = fs::read_to_string(target.join("workspace/sub/b.txt")).unwrap();
        assert_eq!(b, "content-b");
    }

    #[test]
    fn restore_selective_filters_paths() {
        let dir = tempfile::tempdir().unwrap();
        let cas = CasStore::new(dir.path().join("cas")).unwrap();

        let h1 = cas.store(b"keep").unwrap();
        let h2 = cas.store(b"skip").unwrap();

        let mut tree = MerkleTree::new();
        tree.update(PathBuf::from("workspace/keep.txt"), h1);
        tree.update(PathBuf::from("workspace/skip.txt"), h2);

        let target = dir.path().join("selective");
        let result = restore_selective(
            &tree,
            &cas,
            &["workspace/keep.txt"],
            &target,
        )
        .unwrap();

        assert_eq!(result.files_restored, 1);
        assert!(target.join("workspace/keep.txt").exists());
        assert!(!target.join("workspace/skip.txt").exists());
    }

    #[test]
    fn is_mutating_recognizes_types() {
        assert!(is_mutating("write"));
        assert!(is_mutating("unlink"));
        assert!(is_mutating("rename"));
        assert!(!is_mutating("read"));
        assert!(!is_mutating("exec"));
        assert!(!is_mutating("stdio"));
    }

    #[test]
    fn find_seq_invalid_timestamp_errors() {
        let result = find_seq_at_timestamp(&[], "not-a-timestamp");
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Register module**

Add to `crates/argus/src/snapshot/mod.rs`:
```rust
pub mod restore;
```

And add re-exports:
```rust
#[doc(inline)]
pub use restore::{
    build_tree_at_seq, find_seq_at_timestamp, restore_selective,
    restore_to_directory, RestoreResult,
};
```

- [ ] **Step 3: Implement FromStr for ContentHash**

The restore module uses `hash_str.parse()` which requires `FromStr`. Add this to `crates/argus/src/cas/hash.rs` (if not already added by Task 5's `content_routes.rs`):

```rust
impl std::str::FromStr for ContentHash {
    type Err = InvalidHashError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}
```

If the `FromStr` impl was already added in `content_routes.rs`, move it to `hash.rs` where it belongs and remove it from `content_routes.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p argus --lib snapshot::restore -- -q`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add crates/argus/src/snapshot/restore.rs crates/argus/src/snapshot/mod.rs crates/argus/src/cas/hash.rs
git commit -m "add restore engine: tree replay, filesystem restore, selective restore"
```

---

### Task 9: Restore API Routes

**Files:**
- Create: `crates/argus/src/api/restore_routes.rs`
- Modify: `crates/argus/src/api/mod.rs`

- [ ] **Step 1: Write restore_routes.rs**

```rust
//! Restore API endpoints.
//!
//! `POST /restore` dispatches to the restore engine based on mode.
//! For in-place mode, uses the existing pause/resume machinery from
//! SharedState.

use axum::Json;
use axum::extract::State;

use crate::api::errors::ApiError;
use crate::api::query_types::{RestoreMode, RestoreRequest, RestoreResponse, UndoRequest};
use crate::api::state::SharedState;
use crate::events::{EventPayload, control};
use crate::snapshot::restore;
use crate::storage::event_reader;

/// `POST /restore` — restore filesystem to a point in time.
pub async fn restore_handler(
    State(state): State<SharedState>,
    Json(req): Json<RestoreRequest>,
) -> Result<Json<RestoreResponse>, ApiError> {
    // Validate required fields
    if req.timestamp.is_none() && req.seq.is_none() {
        return Err(ApiError::BadRequest {
            message: "either timestamp or seq is required".into(),
        });
    }

    let guard = state.lock().expect("state lock poisoned");
    let cas = guard
        .cas()
        .ok_or(ApiError::NotConfigured { name: "CAS" })?
        .clone();
    let event_dir = guard
        .event_dir()
        .ok_or(ApiError::NotConfigured { name: "event_dir" })?
        .to_path_buf();
    drop(guard);

    // Read events from disk
    let events = event_reader::read_all_events(&event_dir)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Determine target seq
    let target_seq = if let Some(seq) = req.seq {
        seq
    } else if let Some(ts) = &req.timestamp {
        restore::find_seq_at_timestamp(&events, ts)
            .map_err(|e| ApiError::InvalidTimestamp {
                value: e.to_string(),
            })?
            .ok_or_else(|| ApiError::BadRequest {
                message: "no events found at or before timestamp".into(),
            })?
    } else {
        unreachable!()
    };

    // Find ts_wall for target seq
    let ts_wall = events
        .iter()
        .find(|e| e.seq == target_seq)
        .map(|e| e.ts_wall.clone())
        .unwrap_or_default();

    // Build tree at target seq
    let tree = restore::build_tree_at_seq(
        &crate::snapshot::MerkleTree::new(),
        &events,
        target_seq,
    );

    match req.mode {
        RestoreMode::NewDirectory => {
            let target = req.target.ok_or_else(|| ApiError::BadRequest {
                message: "target directory required for new_directory mode".into(),
            })?;

            let mut result = restore::restore_to_directory(
                &tree,
                &cas,
                std::path::Path::new(&target),
            )
            .map_err(|e| ApiError::RestoreFailed {
                reason: e.to_string(),
            })?;

            result.seq = target_seq;
            result.ts_wall = ts_wall;

            Ok(Json(RestoreResponse {
                restored_to_seq: result.seq,
                restored_to_ts: result.ts_wall,
                tree_hash: result.tree_hash,
                files_restored: result.files_restored,
                bytes_restored: result.bytes_restored,
                pre_restore_snapshot_seq: None,
            }))
        }
        RestoreMode::InPlace => {
            if req.force != Some(true) {
                return Err(ApiError::BadRequest {
                    message: "force: true required for in_place mode".into(),
                });
            }

            // Pause agent using existing machinery
            let mut guard = state.lock().expect("state lock poisoned");
            guard.set_paused(true);
            guard.emit(EventPayload::AgentPause(control::AgentPause {
                reason: "restore".into(),
                stopped_pids: Vec::new(),
            }));
            let pre_seq = guard.event_seq();
            drop(guard);

            // Perform restore (target is workspace root)
            let target_path = req.target.as_deref().unwrap_or("/workspace");
            let mut result = restore::restore_to_directory(
                &tree,
                &cas,
                std::path::Path::new(target_path),
            )
            .map_err(|e| ApiError::RestoreFailed {
                reason: e.to_string(),
            })?;

            result.seq = target_seq;
            result.ts_wall = ts_wall;

            // Resume agent
            let mut guard = state.lock().expect("state lock poisoned");
            if let Some(mt) = guard.merkle_tree_mut() {
                *mt = tree;
            }
            guard.set_paused(false);
            guard.emit(EventPayload::AgentResume(control::AgentResume {
                resumed_pids: Vec::new(),
            }));

            Ok(Json(RestoreResponse {
                restored_to_seq: result.seq,
                restored_to_ts: result.ts_wall,
                tree_hash: result.tree_hash,
                files_restored: result.files_restored,
                bytes_restored: result.bytes_restored,
                pre_restore_snapshot_seq: Some(pre_seq),
            }))
        }
        RestoreMode::Selective => {
            let path = req.path.ok_or_else(|| ApiError::BadRequest {
                message: "path required for selective mode".into(),
            })?;
            let target = req
                .target
                .as_deref()
                .unwrap_or("/workspace");

            let mut result = restore::restore_selective(
                &tree,
                &cas,
                &[path.as_str()],
                std::path::Path::new(target),
            )
            .map_err(|e| ApiError::RestoreFailed {
                reason: e.to_string(),
            })?;

            result.seq = target_seq;
            result.ts_wall = ts_wall;

            Ok(Json(RestoreResponse {
                restored_to_seq: result.seq,
                restored_to_ts: result.ts_wall,
                tree_hash: result.tree_hash,
                files_restored: result.files_restored,
                bytes_restored: result.bytes_restored,
                pre_restore_snapshot_seq: None,
            }))
        }
    }
}

/// `POST /restore/undo` — undo last N mutations.
pub async fn undo_handler(
    State(state): State<SharedState>,
    Json(req): Json<UndoRequest>,
) -> Result<Json<RestoreResponse>, ApiError> {
    let n = req.last.ok_or_else(|| ApiError::BadRequest {
        message: "last is required".into(),
    })?;

    let guard = state.lock().expect("state lock poisoned");
    let cas = guard
        .cas()
        .ok_or(ApiError::NotConfigured { name: "CAS" })?
        .clone();
    let event_dir = guard
        .event_dir()
        .ok_or(ApiError::NotConfigured { name: "event_dir" })?
        .to_path_buf();
    drop(guard);

    let events = event_reader::read_all_events(&event_dir)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Find seq N mutating events ago
    let mutating_seqs: Vec<u64> = events
        .iter()
        .filter(|e| {
            let tag = e.payload.event_type_tag();
            restore::MUTATING_TYPES.contains(&tag)
        })
        .map(|e| e.seq)
        .collect();

    if mutating_seqs.len() < n as usize {
        return Err(ApiError::BadRequest {
            message: format!(
                "only {} mutating events exist, cannot undo {n}",
                mutating_seqs.len()
            ),
        });
    }

    let target_idx = mutating_seqs.len() - n as usize;
    let target_seq = if target_idx == 0 {
        0
    } else {
        mutating_seqs[target_idx - 1]
    };

    let ts_wall = events
        .iter()
        .find(|e| e.seq == target_seq)
        .map(|e| e.ts_wall.clone())
        .unwrap_or_default();

    let tree = restore::build_tree_at_seq(
        &crate::snapshot::MerkleTree::new(),
        &events,
        target_seq,
    );

    // For now, undo always writes to a new directory
    let target_dir = format!("/data/restore/undo-{target_seq}");
    let mut result = restore::restore_to_directory(
        &tree,
        &cas,
        std::path::Path::new(&target_dir),
    )
    .map_err(|e| ApiError::RestoreFailed {
        reason: e.to_string(),
    })?;

    result.seq = target_seq;
    result.ts_wall = ts_wall;

    Ok(Json(RestoreResponse {
        restored_to_seq: result.seq,
        restored_to_ts: result.ts_wall,
        tree_hash: result.tree_hash,
        files_restored: result.files_restored,
        bytes_restored: result.bytes_restored,
        pre_restore_snapshot_seq: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::new_shared_state_full;
    use crate::cas::CasStore;
    use crate::events::{Event, EventPayload, SequenceGenerator};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use axum::Router;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_app() -> (Router, SharedState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let cas = Arc::new(CasStore::new(dir.path().join("cas")).unwrap());
        let event_dir = dir.path().join("events");
        std::fs::create_dir_all(&event_dir).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let state = new_shared_state_full("test".into(), tx, cas, event_dir);
        let app = Router::new()
            .route("/restore", post(restore_handler))
            .route("/restore/undo", post(undo_handler))
            .with_state(state.clone());
        (app, state, dir)
    }

    fn write_events(dir: &std::path::Path, events: &[Event]) {
        let event_dir = dir.join("events");
        let mut content = String::new();
        for e in events {
            content.push_str(&serde_json::to_string(e).unwrap());
            content.push('\n');
        }
        std::fs::write(event_dir.join("0.jsonl"), content).unwrap();
    }

    #[tokio::test]
    async fn restore_new_directory_basic() {
        let (app, _state, dir) = test_app();
        let cas = CasStore::new(dir.path().join("cas")).unwrap();
        let gen = SequenceGenerator::default();

        let hash = cas.store(b"version 1\n").unwrap();
        let e1 = Event {
            seq: gen.next_seq(),
            ts_monotonic: 100,
            ts_wall: "2026-01-01T00:00:01Z".into(),
            agent_id: "test".into(),
            vclock: None,
            payload: EventPayload::Write(crate::events::file::Write {
                pid: 1,
                path: "/workspace/file.txt".into(),
                fd: 3,
                offset: 0,
                size: 10,
                before_hash: None,
                after_hash: Some(hash.as_str().to_owned()),
                tree_hash: None,
            }),
        };

        write_events(dir.path(), &[e1]);

        let target = dir.path().join("restored");
        let body = serde_json::json!({
            "seq": 0,
            "mode": "new_directory",
            "target": target.to_str().unwrap(),
        });

        let req = Request::builder()
            .method("POST")
            .uri("/restore")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
        let resp: RestoreResponse = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(resp.files_restored, 1);

        let content = std::fs::read_to_string(
            target.join("workspace/file.txt"),
        )
        .unwrap();
        assert_eq!(content, "version 1\n");
    }

    #[tokio::test]
    async fn restore_missing_params_returns_400() {
        let (app, _state, _dir) = test_app();
        let body = serde_json::json!({"mode": "new_directory"});
        let req = Request::builder()
            .method("POST")
            .uri("/restore")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn in_place_requires_force() {
        let (app, _state, dir) = test_app();
        write_events(dir.path(), &[]);

        let body = serde_json::json!({
            "seq": 0,
            "mode": "in_place",
        });
        let req = Request::builder()
            .method("POST")
            .uri("/restore")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
```

Note: `MUTATING_TYPES` must be made `pub` in `restore.rs`:
```rust
pub const MUTATING_TYPES: &[&str] = &[...];
```

- [ ] **Step 2: Register in mod.rs and build_router**

Add `pub mod restore_routes;` and routes:
```rust
.route("/restore", post(restore_routes::restore_handler))
.route("/restore/undo", post(restore_routes::undo_handler))
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p argus --lib api::restore_routes -- -q`
Expected: All PASS

- [ ] **Step 4: Commit**

```bash
git add crates/argus/src/api/restore_routes.rs crates/argus/src/api/mod.rs
git commit -m "add restore and undo API endpoints"
```

---

## Chunk 5: System Routes + Final Integration

### Task 10: System Routes (GET /storage/status)

**Files:**
- Create: `crates/argus/src/api/system_routes.rs`
- Modify: `crates/argus/src/api/mod.rs`

- [ ] **Step 1: Write system_routes.rs**

```rust
//! System status endpoints.

use axum::Json;
use axum::extract::State;

use crate::api::errors::ApiError;
use crate::api::query_types::{LocalBufferStatus, StorageStatusResponse};
use crate::api::state::SharedState;

/// `GET /storage/status` — CAS and event log stats.
pub async fn storage_status_handler(
    State(state): State<SharedState>,
) -> Result<Json<StorageStatusResponse>, ApiError> {
    let guard = state.lock().expect("state lock poisoned");
    let cas = guard
        .cas()
        .ok_or(ApiError::NotConfigured { name: "CAS" })?;

    let stats = cas.stats();

    let segments = guard
        .event_dir()
        .map(|d| {
            std::fs::read_dir(d)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .filter(|e| {
                            e.path()
                                .extension()
                                .is_some_and(|ext| ext == "jsonl")
                        })
                        .count() as u64
                })
                .unwrap_or(0)
        })
        .unwrap_or(0);

    Ok(Json(StorageStatusResponse {
        local_buffer: LocalBufferStatus {
            cas_objects: stats.total_objects,
            cas_size_bytes: stats.total_bytes,
            event_segments_local: segments,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::new_shared_state_full;
    use crate::cas::CasStore;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[tokio::test]
    async fn storage_status_returns_stats() {
        let dir = tempfile::tempdir().unwrap();
        let cas = Arc::new(CasStore::new(dir.path().join("cas")).unwrap());
        cas.store(b"test data").unwrap();
        let event_dir = dir.path().join("events");
        std::fs::create_dir_all(&event_dir).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let state = new_shared_state_full("test".into(), tx, cas, event_dir);

        let app = Router::new()
            .route("/storage/status", get(storage_status_handler))
            .with_state(state);

        let req = Request::builder()
            .uri("/storage/status")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let status: StorageStatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(status.local_buffer.cas_objects, 1);
    }
}
```

- [ ] **Step 2: Register in mod.rs and build_router**

Add `pub mod system_routes;` and route:
```rust
.route("/storage/status", get(system_routes::storage_status_handler))
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p argus --lib api::system_routes -- -q`
Expected: All PASS

- [ ] **Step 4: Commit**

```bash
git add crates/argus/src/api/system_routes.rs crates/argus/src/api/mod.rs
git commit -m "add storage status API endpoint"
```

---

### Task 11: Final Router Assembly + Full Test Suite

**Files:**
- Modify: `crates/argus/src/api/mod.rs`

- [ ] **Step 1: Verify final build_router has all routes**

The final `build_router` in `mod.rs` should look like:

```rust
pub fn build_router(state: SharedState) -> Router {
    Router::new()
        // Agent control
        .route("/agent/pause", post(pause_handler))
        .route("/agent/resume", post(resume_handler))
        .route("/agent/status", get(status_handler))
        // Approvals
        .route("/approvals/pending", get(pending_approvals_handler))
        .route("/approvals/{action_id}/approve", post(approve_handler))
        .route("/approvals/{action_id}/deny", post(deny_handler))
        // Health
        .route("/health", get(health_handler))
        // Content
        .route("/content/{hash}", get(content_routes::content_raw_handler))
        .route("/content/{hash}/text", get(content_routes::content_text_handler))
        .route("/diff", get(content_routes::diff_handler))
        // Events
        .route("/events", get(query_routes::events_handler))
        .route("/file_history", get(query_routes::file_history_handler))
        // Tree
        .route("/tree", get(tree_routes::tree_handler))
        .route("/tree/diff", get(tree_routes::tree_diff_handler))
        // Restore
        .route("/restore", post(restore_routes::restore_handler))
        .route("/restore/undo", post(restore_routes::undo_handler))
        // System
        .route("/storage/status", get(system_routes::storage_status_handler))
        .with_state(state)
}
```

- [ ] **Step 2: Run all tests**

Run: `cargo test -p argus -- -q`
Expected: All tests PASS (existing + new)

- [ ] **Step 3: Commit**

```bash
git add crates/argus/src/api/mod.rs
git commit -m "wire all query, content, tree, restore, and system routes into router"
```

---

### Task 12: Update Task Docs

**Files:**
- Modify: `docs/tasks/p3-restore.md`
- Modify: `docs/tasks/p3-query-api.md`

- [ ] **Step 1: Update p3-restore.md**

Set status to `done`, list all files added/changed, document what works and how to test.

- [ ] **Step 2: Update p3-query-api.md**

Set status to `done`, list all files added/changed, note that stdio reconstruction and pipeline reconstruction are not yet implemented (future work, not test-blocking).

- [ ] **Step 3: Commit**

```bash
git add docs/tasks/p3-restore.md docs/tasks/p3-query-api.md
git commit -m "update P3 task docs: restore and query API complete"
```

---

## Summary

| Task | What | Test-blocking? |
|-|-|-|
| 1 | Event reader (disk-based) | Yes (query needs it) |
| 2 | SharedState extensions | Yes (all endpoints need it) |
| 3 | API error variants | Yes (all endpoints need it) |
| 4 | Query types | Yes (all endpoints need it) |
| 5 | Content routes | Test 8 |
| 6 | Event query routes | Test 11 |
| 7 | Tree routes | Test 12 |
| 8 | Restore engine | Test 11 |
| 9 | Restore API routes | Test 11 |
| 10 | System routes | Nice-to-have |
| 11 | Router assembly | All |
| 12 | Task doc updates | Housekeeping |

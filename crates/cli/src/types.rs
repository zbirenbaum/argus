// Rust guideline compliant 2026-02-21
//! Response types for API endpoints not yet defined in `argus::api::types`.
//!
//! These are deserialization-only DTOs matching the supervisor JSON wire
//! format. Types already in `argus::api::types` are reused directly.

use serde::{Deserialize, Serialize};

/// Response body for `GET /file_history`.
#[derive(Debug, Deserialize)]
pub struct FileHistoryResponse {
    pub path: String,
    pub events: Vec<FileHistoryEntry>,
}

/// Single entry in a file's history.
#[derive(Debug, Deserialize)]
pub struct FileHistoryEntry {
    pub seq: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub content_hash: Option<String>,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
    pub pid: u32,
    pub ts_wall: String,
}

/// Response body for `GET /stdio` (non-streaming).
#[derive(Debug, Deserialize)]
pub struct StdioResponse {
    pub pid: u32,
    pub binary: String,
    pub argv: Vec<String>,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub stdin: Option<String>,
    pub stdout_dest: Option<String>,
    pub stderr_dest: Option<String>,
}

/// Response body for `GET /process_tree`.
#[derive(Debug, Deserialize)]
pub struct ProcessTreeNode {
    pub pid: u32,
    pub binary: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub stdout: Option<String>,
    #[serde(default)]
    pub stderr: Option<String>,
    #[serde(default)]
    pub connected_via: Option<String>,
    #[serde(default)]
    pub children: Vec<ProcessTreeNode>,
}

/// Response body for `GET /pipeline`.
#[derive(Debug, Deserialize)]
pub struct PipelineResponse {
    pub shell_pid: u32,
    pub stages: Vec<PipelineStage>,
    pub pipes: Vec<PipeInfo>,
}

/// Single stage in a shell pipeline.
#[derive(Debug, Deserialize)]
pub struct PipelineStage {
    pub pid: u32,
    pub binary: String,
    pub argv: Vec<String>,
    pub input_pipe: Option<u64>,
    pub output_pipe: Option<u64>,
    pub output_size: u64,
}

/// Pipe metadata in a pipeline.
#[derive(Debug, Deserialize)]
pub struct PipeInfo {
    pub inode: u64,
    pub writer_pid: u32,
    pub reader_pid: u32,
    pub bytes: u64,
}

/// Response body for `GET /tree`.
#[derive(Debug, Deserialize)]
pub struct TreeResponse {
    pub tree_hash: String,
    pub seq: u64,
    pub entries: Vec<TreeEntry>,
}

/// Single entry in a filesystem snapshot tree.
#[derive(Debug, Deserialize)]
pub struct TreeEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub entry_type: String,
    pub hash: String,
    pub size: u64,
    pub mode: u32,
}

/// Response body for `GET /tree/diff`.
#[derive(Debug, Deserialize)]
pub struct TreeDiffResponse {
    pub from_seq: u64,
    pub to_seq: u64,
    pub added: Vec<TreeDiffEntry>,
    pub modified: Vec<TreeDiffModified>,
    pub deleted: Vec<TreeDiffEntry>,
}

/// Added or deleted entry in a tree diff.
#[derive(Debug, Deserialize)]
pub struct TreeDiffEntry {
    pub path: String,
    pub hash: String,
    pub size: u64,
}

/// Modified entry in a tree diff.
#[derive(Debug, Deserialize)]
pub struct TreeDiffModified {
    pub path: String,
    pub before_hash: String,
    pub after_hash: String,
}

/// Response body for `POST /restore` and `POST /restore/undo`.
#[derive(Debug, Deserialize)]
pub struct RestoreResponse {
    pub restored_to_seq: u64,
    pub restored_to_ts: String,
    pub tree_hash: String,
    pub files_restored: u64,
    pub bytes_restored: u64,
    pub pre_restore_snapshot_seq: u64,
}

/// Request body for `POST /restore`.
#[derive(Debug, Serialize)]
pub struct RestoreRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_place: Option<bool>,
}

/// Request body for `POST /restore/undo`.
#[derive(Debug, Serialize)]
pub struct UndoRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_by_pid: Option<u32>,
}

/// Response body for `GET /connections`.
#[derive(Debug, Deserialize)]
pub struct ConnectionsResponse {
    pub connections: Vec<ConnectionInfo>,
}

/// Single network connection entry.
#[derive(Debug, Deserialize)]
pub struct ConnectionInfo {
    pub pid: u32,
    pub fd: i32,
    #[serde(rename = "type")]
    pub conn_type: String,
    pub dest_addr: String,
    pub dest_port: u16,
    pub sni: Option<String>,
    pub connected_at: String,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub tls: bool,
    pub active: bool,
}

/// Response body for `GET /storage/status`.
#[derive(Debug, Deserialize)]
pub struct StorageStatusResponse {
    pub local_buffer: LocalBufferStatus,
    pub remote: RemoteStatus,
    pub digest_cache: DigestCacheStatus,
}

/// Local buffer portion of storage status.
#[derive(Debug, Deserialize)]
pub struct LocalBufferStatus {
    pub cas_size_bytes: u64,
    pub cas_objects: u64,
    pub events_segments_local: u64,
    pub pending_uploads: u64,
}

/// Remote backend portion of storage status.
#[derive(Debug, Deserialize)]
pub struct RemoteStatus {
    pub backend: String,
    pub bucket: String,
    pub cas_objects_known: u64,
    pub events_segments_uploaded: u64,
}

/// Digest cache portion of storage status.
#[derive(Debug, Deserialize)]
pub struct DigestCacheStatus {
    pub entries: u64,
    pub last_snapshot_uploaded: Option<String>,
    pub ttl: u64,
}

/// Response body for `GET /agents`.
#[derive(Debug, Deserialize)]
pub struct AgentsResponse {
    pub agents: Vec<AgentInfo>,
}

/// Single agent info in cross-agent query.
#[derive(Debug, Deserialize)]
pub struct AgentInfo {
    pub agent_id: String,
    pub started: String,
    pub node: String,
    pub pod: String,
    pub last_event: Option<String>,
}

/// Response body for `GET /rules`.
#[derive(Debug, Deserialize)]
pub struct RulesResponse {
    pub block: Vec<serde_json::Value>,
    pub pause_before: Vec<serde_json::Value>,
}

/// Response body for `POST /rules` and `DELETE /rules/{index}`.
#[derive(Debug, Deserialize)]
pub struct RulesAppliedResponse {
    pub applied: bool,
    pub rule_count: u64,
}

/// Response body for `GET /correlation`.
#[derive(Debug, Deserialize)]
pub struct CorrelationResponse {
    pub correlations: Vec<Correlation>,
}

/// Single cross-agent correlation entry.
#[derive(Debug, Deserialize)]
pub struct Correlation {
    pub resource: String,
    pub write: CorrelationEvent,
    pub read: CorrelationEvent,
    pub latency_ms: f64,
}

/// Event reference within a correlation.
#[derive(Debug, Deserialize)]
pub struct CorrelationEvent {
    pub agent_id: String,
    pub seq: u64,
    pub ts_wall: String,
}

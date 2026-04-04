// Rust guideline compliant 2026-02-21
//! Axum route handlers for the supervisor REST API.
//!
//! Each handler takes shared state via axum's `State` extractor and
//! returns JSON responses. All state access is lock-free through the
//! `Bridge` struct.

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::Utc;

use std::path::PathBuf;

use crate::api::errors::ApiError;
use crate::api::state::{SharedState, resolve_approval};
use crate::api::types::{
    ApproveResponse, DenyResponse, HealthResponse, PauseResponse, PendingAction,
    PendingApprovalsResponse, RestoreFileRequest, RestoreFileResponse, RestoreRequest,
    RestoreResponse, ResumeResponse, RulesAppliedResponse, SnapshotsResponse, StatusResponse,
    TreeFileEntry, TreeQueryParams, TreeSnapshotResponse,
};
use crate::cas::ContentHash;
use crate::config::RuleSet;
use crate::events::{ApprovalDecision, EventPayload};
use crate::events::control;
use crate::snapshot::restore;

/// `POST /agent/pause` — freeze all traced processes.
///
/// # Errors
///
/// Returns `409 Conflict` if the agent is already paused.
pub async fn pause_handler(
    State(state): State<SharedState>,
) -> Result<Json<PauseResponse>, ApiError> {
    if !state.set_paused(true) {
        return Err(ApiError::AlreadyInState { state: "paused" });
    }

    state.emit(EventPayload::AgentPause(control::AgentPause {
        reason: "api_request".into(),
        stopped_pids: Vec::new(),
    }));

    Ok(Json(PauseResponse {
        status: "paused".into(),
        stopped_processes: Vec::new(),
    }))
}

/// `POST /agent/resume` — resume all traced processes.
///
/// # Errors
///
/// Returns `409 Conflict` if the agent is already running.
pub async fn resume_handler(
    State(state): State<SharedState>,
) -> Result<Json<ResumeResponse>, ApiError> {
    if !state.set_paused(false) {
        return Err(ApiError::AlreadyInState { state: "running" });
    }

    state.emit(EventPayload::AgentResume(control::AgentResume {
        resumed_pids: Vec::new(),
    }));

    Ok(Json(ResumeResponse {
        status: "running".into(),
        resumed_count: 0,
    }))
}

/// `GET /agent/status` — current supervisor status snapshot.
pub async fn status_handler(State(state): State<SharedState>) -> Json<StatusResponse> {
    let status = if state.is_paused() { "paused" } else { "running" };

    Json(StatusResponse {
        status: status.into(),
        agent_id: state.agent_id().to_owned(),
        uptime_seconds: state.uptime_seconds(),
        event_count: state.event_seq(),
        processes: Vec::new(),
    })
}

/// `GET /approvals/pending` — list actions awaiting a decision.
pub async fn pending_approvals_handler(
    State(state): State<SharedState>,
) -> Json<PendingApprovalsResponse> {
    let pending = state
        .pending_actions()
        .into_iter()
        .map(|e| PendingAction {
            action_id: e.action_id,
            pid: e.pid,
            process: e.process,
            syscall: e.syscall,
            path: e.path,
            timestamp: e.timestamp,
            rule_matched: e.rule_matched,
        })
        .collect();

    Json(PendingApprovalsResponse { pending })
}

/// `POST /approvals/{action_id}/approve` — allow a blocked syscall.
///
/// # Errors
///
/// Returns `404 Not Found` if the action ID does not exist.
pub async fn approve_handler(
    State(state): State<SharedState>,
    Path(action_id): Path<String>,
) -> Result<Json<ApproveResponse>, ApiError> {
    let entry = resolve_approval(&state, &action_id, ApprovalDecision::Approve).ok_or(
        ApiError::ActionNotFound {
            action_id: action_id.clone(),
        },
    )?;

    state.emit(EventPayload::ApprovalGranted(control::ApprovalGranted {
        pid: entry.pid,
        rule_name: entry.rule_matched.clone(),
        approver: "api".into(),
    }));

    Ok(Json(ApproveResponse {
        action_id,
        result: "approved".into(),
        pid: entry.pid,
    }))
}

/// `POST /approvals/{action_id}/deny` — inject EPERM for a blocked syscall.
///
/// # Errors
///
/// Returns `404 Not Found` if the action ID does not exist.
pub async fn deny_handler(
    State(state): State<SharedState>,
    Path(action_id): Path<String>,
) -> Result<Json<DenyResponse>, ApiError> {
    let entry = resolve_approval(&state, &action_id, ApprovalDecision::Deny).ok_or(
        ApiError::ActionNotFound {
            action_id: action_id.clone(),
        },
    )?;

    state.emit(EventPayload::ApprovalDenied(control::ApprovalDenied {
        pid: entry.pid,
        rule_name: entry.rule_matched.clone(),
        approver: "api".into(),
    }));

    Ok(Json(DenyResponse {
        action_id,
        result: "denied".into(),
        pid: entry.pid,
        injected_errno: "EPERM".into(),
    }))
}

/// `GET /health` — basic liveness check.
pub async fn health_handler(State(state): State<SharedState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        agent_id: state.agent_id().to_owned(),
        event_count: state.event_seq(),
    })
}

/// `GET /rules` — current active rule set.
pub async fn get_rules_handler(State(state): State<SharedState>) -> Json<RuleSet> {
    let rules = state.load_rules();
    Json((**rules).clone())
}

/// `POST /rules` — replace the entire rule set atomically.
///
/// # Errors
///
/// Returns `400 Bad Request` if the JSON body is invalid.
pub async fn set_rules_handler(
    State(state): State<SharedState>,
    Json(mut new_rules): Json<RuleSet>,
) -> Result<Json<RulesAppliedResponse>, ApiError> {
    new_rules.compile_patterns();
    let count = new_rules.rule_count();

    state.store_rules(new_rules);
    let loaded = state.load_rules();
    state.emit(EventPayload::RulesUpdated(control::RulesUpdated {
        block_count: loaded.block.len(),
        pause_before_count: loaded.pause_before.len(),
        source: "api".into(),
    }));

    Ok(Json(RulesAppliedResponse {
        applied: true,
        rule_count: count,
    }))
}

/// `DELETE /rules/{index}` — remove a single rule by global index.
///
/// Indices `0..block.len()` refer to block rules; indices
/// `block.len()..` refer to pause-before-action rules.
///
/// # Errors
///
/// Returns `404 Not Found` if the index is out of bounds.
pub async fn delete_rule_handler(
    State(state): State<SharedState>,
    Path(index): Path<usize>,
) -> Result<Json<RulesAppliedResponse>, ApiError> {
    let current = state.load_rules();
    let total = current.rule_count();

    if index >= total {
        return Err(ApiError::RuleIndexOutOfBounds { index, total });
    }

    let mut new_rules = (**current).clone();
    let block_len = new_rules.block.len();
    if index < block_len {
        new_rules.block.remove(index);
    } else {
        new_rules.pause_before.remove(index - block_len);
    }

    let count = new_rules.rule_count();
    state.store_rules(new_rules);
    let loaded = state.load_rules();
    state.emit(EventPayload::RulesUpdated(control::RulesUpdated {
        block_count: loaded.block.len(),
        pause_before_count: loaded.pause_before.len(),
        source: "api".into(),
    }));

    Ok(Json(RulesAppliedResponse {
        applied: true,
        rule_count: count,
    }))
}

/// `GET /tree` — filesystem tree snapshot.
///
/// Without query parameters, returns the current live tree. With
/// `?seq=N`, loads the tree at that event sequence from CAS.
///
/// # Errors
///
/// Returns 404 if no tree hash exists for the given seq, or 500
/// if the CAS load fails.
pub async fn tree_handler(
    State(state): State<SharedState>,
    Query(params): Query<TreeQueryParams>,
) -> Result<Json<TreeSnapshotResponse>, ApiError> {
    if let Some(seq) = params.seq {
        let tree_hash_str = state.get_tree_hash(seq).ok_or(ApiError::SeqNotFound { seq })?;

        let tree_hash = ContentHash::try_from(tree_hash_str.clone()).map_err(|e| {
            ApiError::RestoreFailed {
                reason: format!("invalid tree hash: {e}"),
            }
        })?;

        let cas = state.cas().clone();
        let tree = tokio::task::spawn_blocking(move || {
            crate::snapshot::MerkleTree::load(&tree_hash, cas.as_ref())
        })
        .await
        .map_err(|e| ApiError::RestoreFailed {
            reason: format!("task panicked: {e}"),
        })?
        .map_err(|e| ApiError::RestoreFailed {
            reason: format!("tree load failed: {e}"),
        })?;

        let files: Vec<TreeFileEntry> = tree
            .files()
            .map(|(path, hash)| TreeFileEntry {
                path: path.display().to_string(),
                hash: hash.to_string(),
            })
            .collect();

        return Ok(Json(TreeSnapshotResponse {
            tree_hash: tree_hash_str,
            seq,
            file_count: tree.file_count(),
            files,
        }));
    }

    let tree = state.load_tree();
    let files: Vec<TreeFileEntry> = tree
        .files()
        .map(|(path, hash)| TreeFileEntry {
            path: path.display().to_string(),
            hash: hash.to_string(),
        })
        .collect();

    Ok(Json(TreeSnapshotResponse {
        tree_hash: tree.root_hash().to_string(),
        seq: state.event_seq(),
        file_count: tree.file_count(),
        files,
    }))
}

/// `GET /snapshots` — list all browsable snapshots.
pub async fn snapshots_handler(
    State(state): State<SharedState>,
) -> Json<SnapshotsResponse> {
    let snapshots = state.load_snapshots();
    Json(SnapshotsResponse {
        snapshots: (**snapshots).clone(),
    })
}

/// `GET /cas/{hash}` — raw file content from CAS.
///
/// Returns UTF-8 text content. Binary files are not supported.
///
/// # Errors
///
/// Returns 400 if the hash is malformed, 404/500 if the CAS
/// lookup fails or the content is not valid UTF-8.
pub async fn cas_content_handler(
    State(state): State<SharedState>,
    Path(hash_str): Path<String>,
) -> Result<String, ApiError> {
    let hash = ContentHash::try_from(hash_str).map_err(|e| ApiError::InvalidBody {
        reason: format!("invalid hash: {e}"),
    })?;

    let cas = state.cas().clone();
    let content = tokio::task::spawn_blocking(move || cas.get(&hash))
        .await
        .map_err(|e| ApiError::RestoreFailed {
            reason: format!("CAS task panicked: {e}"),
        })?
        .map_err(|e| ApiError::RestoreFailed {
            reason: format!("CAS lookup failed: {e}"),
        })?;

    String::from_utf8(content).map_err(|_| ApiError::RestoreFailed {
        reason: "content is not valid UTF-8".into(),
    })
}

/// `POST /restore` — restore filesystem to a past snapshot.
///
/// Requires `seq` in the request body to identify the target
/// snapshot. Supports `"full"` mode (all files to target dir) and
/// `"selective"` mode (specific paths only).
///
/// # Errors
///
/// Returns 404 if no tree hash exists for the given seq, or 500
/// if the restore operation fails.
pub async fn restore_handler(
    State(state): State<SharedState>,
    Json(req): Json<RestoreRequest>,
) -> Result<Json<RestoreResponse>, ApiError> {
    let seq = req.seq.ok_or_else(|| ApiError::InvalidBody {
        reason: "seq is required".into(),
    })?;

    let tree_hash_str = state.get_tree_hash(seq).ok_or(ApiError::SeqNotFound { seq })?;

    let tree_hash = ContentHash::try_from(tree_hash_str.clone()).map_err(|e| {
        ApiError::RestoreFailed {
            reason: format!("invalid tree hash: {e}"),
        }
    })?;

    let cas = state.cas();
    let target_dir = PathBuf::from(
        req.target
            .as_deref()
            .unwrap_or("/tmp/argus-restore"),
    );

    let stats = match req.mode.as_str() {
        "selective" => {
            let mut paths = Vec::new();
            if let Some(p) = &req.path {
                paths.push(PathBuf::from(p));
            }
            if let Some(ps) = &req.paths {
                paths.extend(ps.iter().map(PathBuf::from));
            }
            if paths.is_empty() {
                return Err(ApiError::InvalidBody {
                    reason: "selective mode requires path or paths".into(),
                });
            }
            restore::restore_selective_from_hash(&tree_hash, &paths, cas.as_ref(), &target_dir)
                .map_err(|e| ApiError::RestoreFailed {
                    reason: e.to_string(),
                })?
        }
        _ => {
            restore::restore_from_hash(&tree_hash, cas.as_ref(), &target_dir).map_err(|e| {
                ApiError::RestoreFailed {
                    reason: e.to_string(),
                }
            })?
        }
    };

    Ok(Json(RestoreResponse {
        restored_to_seq: seq,
        restored_to_ts: String::new(),
        tree_hash: tree_hash_str,
        files_restored: stats.files_restored,
        bytes_restored: stats.bytes_restored,
    }))
}

/// `POST /restore/file` — restore a single file directly from CAS by content hash.
///
/// Bypasses the Merkle tree lookup, which breaks for files that went through
/// a `.tmp` → rename chain: the UI tracks the final path but the tree stores
/// the `.tmp` path at the captured seq. Taking path + content_hash directly
/// avoids that ambiguity.
///
/// Writes atomically: content lands in a sibling temp file, then is renamed
/// into place so the agent never sees a partial write.
///
/// # Errors
///
/// Returns `400 Bad Request` if `content_hash` is malformed, or `500` if
/// the CAS lookup or write fails.
pub async fn restore_file_handler(
    State(state): State<SharedState>,
    Json(req): Json<RestoreFileRequest>,
) -> Result<Json<RestoreFileResponse>, ApiError> {
    let dest = PathBuf::from(&req.path);

    // Path traversal guard: only allow writes under /workspace or /tmp.
    let canonical = dest.canonicalize().unwrap_or_else(|_| dest.clone());
    if !canonical.starts_with("/workspace") && !canonical.starts_with("/tmp") {
        return Err(ApiError::InvalidBody {
            reason: format!("path must be under /workspace or /tmp, got: {}", dest.display()),
        });
    }

    let hash = ContentHash::try_from(req.content_hash.clone()).map_err(|e| {
        ApiError::InvalidBody {
            reason: format!("invalid content_hash: {e}"),
        }
    })?;

    let cas = state.cas().clone();
    let content = tokio::task::spawn_blocking(move || cas.get(&hash))
        .await
        .map_err(|e| ApiError::RestoreFailed {
            reason: format!("CAS task panicked: {e}"),
        })?
        .map_err(|e| ApiError::RestoreFailed {
            reason: format!("CAS lookup failed: {e}"),
        })?;

    // Create parent directories so the restore works even if the path is new.
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| ApiError::RestoreFailed {
                reason: format!("failed to create parent directories: {e}"),
            })?;
    }

    // Write to a sibling temp file in the same directory, then rename
    // atomically — same filesystem guarantees rename is atomic on Linux.
    let tmp_path = dest.with_extension(format!(
        "argus-restore-{}.tmp",
        uuid::Uuid::new_v4().as_simple()
    ));

    tokio::fs::write(&tmp_path, &content)
        .await
        .map_err(|e| ApiError::RestoreFailed {
            reason: format!("failed to write temp file: {e}"),
        })?;

    if let Err(e) = tokio::fs::rename(&tmp_path, &dest).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(ApiError::RestoreFailed {
            reason: format!("failed to rename temp file to destination: {e}"),
        });
    }

    Ok(Json(RestoreFileResponse {
        bytes_written: content.len() as u64,
    }))
}

/// Submits a pending approval from the tracer thread.
///
/// Called by the tracer when a syscall matches a pause-before-action
/// rule. Returns a oneshot receiver that the tracer blocks on to get
/// the human decision.
pub fn submit_pending_approval(
    state: &SharedState,
    pid: u32,
    process: String,
    syscall: String,
    path: Option<String>,
    rule_matched: String,
) -> (String, tokio::sync::oneshot::Receiver<ApprovalDecision>) {
    let action_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let timestamp = Utc::now().to_rfc3339();

    let entry = crate::api::types::PendingApprovalEntry {
        action_id: action_id.clone(),
        pid,
        process,
        syscall,
        path,
        timestamp,
        rule_matched,
        decision_tx: Some(tx),
    };

    state.insert_pending(entry);

    (action_id, rx)
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod tests;

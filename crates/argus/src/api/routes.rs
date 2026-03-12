// Rust guideline compliant 2026-02-21
//! Axum route handlers for the supervisor REST API.
//!
//! Each handler takes shared state via axum's `State` extractor and
//! returns JSON responses. All state access is lock-free through the
//! `Bridge` struct.

use axum::Json;
use axum::extract::{Path, State};
use chrono::Utc;

use crate::api::errors::ApiError;
use crate::api::state::{SharedState, resolve_approval};
use crate::api::types::{
    ApproveResponse, DenyResponse, HealthResponse, PauseResponse, PendingAction,
    PendingApprovalsResponse, ResumeResponse, RulesAppliedResponse, StatusResponse,
};
use crate::config::RuleSet;
use crate::events::{ApprovalDecision, EventPayload};
use crate::events::control;

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

// Rust guideline compliant 2026-02-21
use super::*;
use crate::api::build_router;
use crate::api::state::new_shared_state;
use crate::api::types::RestoreFileResponse;
use crate::cas::{Cas, MemoryCas};
use crate::events::EventPayload;
use crate::pipeline::RecordBus;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

fn test_cas() -> Arc<dyn crate::cas::Cas> {
    Arc::new(MemoryCas::new())
}

fn test_cas_with_content(content: &[u8]) -> (Arc<dyn crate::cas::Cas>, String) {
    let cas = Arc::new(MemoryCas::new());
    let hash = cas.put(content).unwrap();
    let hash_str = hash.to_string();
    (cas, hash_str)
}

fn test_bus() -> RecordBus {
    RecordBus::new(vec![])
}

fn test_router() -> Router {
    build_router(new_shared_state("test-agent".into(), test_cas(), test_bus()))
}

fn test_router_with_state() -> (Router, SharedState) {
    let state = new_shared_state("test-agent".into(), test_cas(), test_bus());
    let router = build_router(state.clone());
    (router, state)
}

fn test_router_with_events() -> (
    Router,
    SharedState,
    tokio::sync::broadcast::Receiver<crate::events::Event>,
) {
    let state = new_shared_state("test-agent".into(), test_cas(), test_bus());
    let rx = state.subscribe_events();
    let router = build_router(state.clone());
    (router, state, rx)
}

async fn post_empty(app: &Router, uri: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).to_string())
}

async fn get_json(app: &Router, uri: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).to_string())
}

#[tokio::test]
async fn pause_then_resume() {
    let app = test_router();
    let (status, body) = post_empty(&app, "/agent/pause").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("paused"));

    let (status, body) = post_empty(&app, "/agent/resume").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("running"));
}

#[tokio::test]
async fn double_pause_returns_conflict() {
    let app = test_router();
    let (status, _) = post_empty(&app, "/agent/pause").await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = post_empty(&app, "/agent/pause").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("already"));
}

#[tokio::test]
async fn double_resume_returns_conflict() {
    let app = test_router();
    let (status, body) = post_empty(&app, "/agent/resume").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("already"));
}

#[tokio::test]
async fn status_reflects_paused_state() {
    let app = test_router();
    let (_, body) = get_json(&app, "/agent/status").await;
    assert!(body.contains("running"));

    post_empty(&app, "/agent/pause").await;
    let (_, body) = get_json(&app, "/agent/status").await;
    assert!(body.contains("paused"));
}

#[tokio::test]
async fn health_returns_ok() {
    let app = test_router();
    let (status, body) = get_json(&app, "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("ok"));
    assert!(body.contains("test-agent"));
}

#[tokio::test]
async fn pending_approvals_empty() {
    let app = test_router();
    let (status, body) = get_json(&app, "/approvals/pending").await;
    assert_eq!(status, StatusCode::OK);
    let resp: PendingApprovalsResponse = serde_json::from_str(&body).unwrap();
    assert!(resp.pending.is_empty());
}

#[tokio::test]
async fn approve_nonexistent_returns_404() {
    let app = test_router();
    let (status, _) = post_empty(&app, "/approvals/fake-id/approve").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deny_nonexistent_returns_404() {
    let app = test_router();
    let (status, _) = post_empty(&app, "/approvals/fake-id/deny").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn submit_and_approve_pending() {
    let (app, state) = test_router_with_state();

    let (action_id, rx) = submit_pending_approval(
        &state,
        42,
        "python".into(),
        "unlink".into(),
        Some("/workspace/foo.txt".into()),
        "unlink /workspace/**".into(),
    );

    let (_, body) = get_json(&app, "/approvals/pending").await;
    let resp: PendingApprovalsResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(resp.pending.len(), 1);
    assert_eq!(resp.pending[0].action_id, action_id);

    let uri = format!("/approvals/{action_id}/approve");
    let (status, body) = post_empty(&app, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("approved"));

    let decision = rx.await.unwrap();
    assert_eq!(decision, ApprovalDecision::Approve);
}

#[tokio::test]
async fn submit_and_deny_pending() {
    let (app, state) = test_router_with_state();

    let (action_id, rx) = submit_pending_approval(
        &state,
        99,
        "bash".into(),
        "exec".into(),
        None,
        "exec rm".into(),
    );

    let uri = format!("/approvals/{action_id}/deny");
    let (status, body) = post_empty(&app, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("denied"));
    assert!(body.contains("EPERM"));

    let decision = rx.await.unwrap();
    assert_eq!(decision, ApprovalDecision::Deny);
}

#[tokio::test]
async fn status_shows_agent_id_and_uptime() {
    let app = test_router();
    let (_, body) = get_json(&app, "/agent/status").await;
    let resp: StatusResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(&*resp.agent_id, "test-agent");
    assert!(resp.uptime_seconds >= 0.0);
}

#[tokio::test]
async fn pause_emits_event() {
    let (app, _state, mut rx) = test_router_with_events();
    let (status, _) = post_empty(&app, "/agent/pause").await;
    assert_eq!(status, StatusCode::OK);

    let event = rx.try_recv().unwrap();
    assert!(matches!(event.payload, EventPayload::AgentPause(_)));
    assert_eq!(&*event.agent_id, "test-agent");
}

#[tokio::test]
async fn resume_emits_event() {
    let (app, _state, mut rx) = test_router_with_events();
    post_empty(&app, "/agent/pause").await;
    let _pause_event = rx.try_recv().unwrap();

    let (status, _) = post_empty(&app, "/agent/resume").await;
    assert_eq!(status, StatusCode::OK);

    let event = rx.try_recv().unwrap();
    assert!(matches!(event.payload, EventPayload::AgentResume(_)));
}

#[tokio::test]
async fn approval_emits_event() {
    let (app, state, mut rx) = test_router_with_events();

    let (action_id, _rx_decision) = submit_pending_approval(
        &state,
        50,
        "node".into(),
        "write".into(),
        Some("/workspace/data.json".into()),
        "write /workspace/**".into(),
    );

    let uri = format!("/approvals/{action_id}/approve");
    let (status, _) = post_empty(&app, &uri).await;
    assert_eq!(status, StatusCode::OK);

    let event = rx.try_recv().unwrap();
    match &event.payload {
        EventPayload::ApprovalGranted(granted) => {
            assert_eq!(granted.pid, 50);
            assert_eq!(granted.approver, "api");
        }
        other => panic!("expected ApprovalGranted, got {other:?}"),
    }
}

// --- Rules API tests ---

async fn post_json(app: &Router, uri: &str, body: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

async fn delete_req(app: &Router, uri: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn get_rules_returns_empty_by_default() {
    let app = test_router();
    let (status, body) = get_json(&app, "/rules").await;
    assert_eq!(status, StatusCode::OK);
    let rules: crate::config::RuleSet = serde_json::from_str(&body).unwrap();
    assert!(rules.block.is_empty());
    assert!(rules.pause_before.is_empty());
}

#[tokio::test]
async fn set_rules_replaces_atomically() {
    let app = test_router();
    let ruleset_json = r#"{
        "block": [{"type": "read", "paths": ["*.env"]}],
        "pause_before": [{"type": "unlink", "paths": ["/workspace/**"]}]
    }"#;

    let (status, body) = post_json(&app, "/rules", ruleset_json).await;
    assert_eq!(status, StatusCode::OK);
    let resp: RulesAppliedResponse = serde_json::from_str(&body).unwrap();
    assert!(resp.applied);
    assert_eq!(resp.rule_count, 2);

    let (_, body) = get_json(&app, "/rules").await;
    let rules: crate::config::RuleSet = serde_json::from_str(&body).unwrap();
    assert_eq!(rules.block.len(), 1);
    assert_eq!(rules.pause_before.len(), 1);
}

#[tokio::test]
async fn delete_rule_removes_block_rule() {
    let app = test_router();
    let ruleset_json = r#"{
        "block": [
            {"type": "read", "paths": ["*.env"]},
            {"type": "read", "paths": ["*.key"]}
        ],
        "pause_before": [{"type": "exec", "binaries": ["rm"]}]
    }"#;
    post_json(&app, "/rules", ruleset_json).await;

    let (status, body) = delete_req(&app, "/rules/0").await;
    assert_eq!(status, StatusCode::OK);
    let resp: RulesAppliedResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(resp.rule_count, 2);

    let (_, body) = get_json(&app, "/rules").await;
    let rules: crate::config::RuleSet = serde_json::from_str(&body).unwrap();
    assert_eq!(rules.block.len(), 1);
    assert_eq!(rules.block[0].paths, vec!["*.key"]);
}

#[tokio::test]
async fn delete_rule_removes_pause_rule() {
    let app = test_router();
    let ruleset_json = r#"{
        "block": [{"type": "read", "paths": ["*.env"]}],
        "pause_before": [{"type": "exec", "binaries": ["rm"]}]
    }"#;
    post_json(&app, "/rules", ruleset_json).await;

    // Index 1 is first pause rule (after 1 block rule)
    let (status, body) = delete_req(&app, "/rules/1").await;
    assert_eq!(status, StatusCode::OK);
    let resp: RulesAppliedResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(resp.rule_count, 1);

    let (_, body) = get_json(&app, "/rules").await;
    let rules: crate::config::RuleSet = serde_json::from_str(&body).unwrap();
    assert_eq!(rules.block.len(), 1);
    assert!(rules.pause_before.is_empty());
}

#[tokio::test]
async fn delete_rule_out_of_bounds_returns_404() {
    let app = test_router();
    let (status, _) = delete_req(&app, "/rules/0").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn set_rules_emits_rules_updated_event() {
    let (app, _state, mut rx) = test_router_with_events();
    let ruleset_json = r#"{
        "block": [{"type": "read", "paths": ["*.env"]}],
        "pause_before": []
    }"#;

    post_json(&app, "/rules", ruleset_json).await;

    let event = rx.try_recv().unwrap();
    match &event.payload {
        EventPayload::RulesUpdated(ru) => {
            assert_eq!(ru.block_count, 1);
            assert_eq!(ru.pause_before_count, 0);
            assert_eq!(ru.source, "api");
        }
        other => panic!("expected RulesUpdated, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_rule_emits_rules_updated_event() {
    let (app, _state, mut rx) = test_router_with_events();
    let ruleset_json = r#"{
        "block": [{"type": "read", "paths": ["*.env"]}],
        "pause_before": [{"type": "exec", "binaries": ["rm"]}]
    }"#;
    post_json(&app, "/rules", ruleset_json).await;
    let _ = rx.try_recv(); // consume SET event

    delete_req(&app, "/rules/0").await;

    let event = rx.try_recv().unwrap();
    match &event.payload {
        EventPayload::RulesUpdated(ru) => {
            assert_eq!(ru.block_count, 0);
            assert_eq!(ru.pause_before_count, 1);
        }
        other => panic!("expected RulesUpdated, got {other:?}"),
    }
}

// --- restore/file tests ---

#[tokio::test]
async fn restore_file_writes_content_from_cas() {
    let content = b"hello from cas";
    let (cas, hash_str) = test_cas_with_content(content);
    let state = new_shared_state("test-agent".into(), cas, test_bus());
    let app = build_router(state);

    // Write to a temp path so the test is hermetic.
    let dest = std::env::temp_dir().join("argus-restore-file-test.txt");
    let body = serde_json::json!({
        "path": dest.to_str().unwrap(),
        "content_hash": hash_str,
    })
    .to_string();

    let (status, resp_body) = post_json(&app, "/restore/file", &body).await;
    assert_eq!(status, StatusCode::OK, "unexpected error: {resp_body}");

    let resp: RestoreFileResponse = serde_json::from_str(&resp_body).unwrap();
    assert_eq!(resp.bytes_written, content.len() as u64);

    let written = std::fs::read(&dest).unwrap();
    assert_eq!(written, content);

    let _ = std::fs::remove_file(dest);
}

#[tokio::test]
async fn restore_file_bad_hash_returns_400() {
    let app = test_router();
    let body = serde_json::json!({
        "path": "/tmp/irrelevant",
        "content_hash": "not-a-valid-hash",
    })
    .to_string();

    let (status, resp_body) = post_json(&app, "/restore/file", &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "expected 400, got {resp_body}");
}

#[tokio::test]
async fn restore_file_missing_from_cas_returns_500() {
    let app = test_router();
    // A syntactically valid sha256:hex hash that won't exist in the empty CAS.
    let absent_hash = format!("sha256:{}", "a".repeat(64));
    let dest = std::env::temp_dir().join("argus-restore-absent.txt");
    let body = serde_json::json!({
        "path": dest.to_str().unwrap(),
        "content_hash": absent_hash,
    })
    .to_string();

    let (status, _) = post_json(&app, "/restore/file", &body).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn restore_file_rejects_path_outside_workspace() {
    let content = b"sneaky";
    let (cas, hash_str) = test_cas_with_content(content);
    let state = new_shared_state("test-agent".into(), cas, test_bus());
    let app = build_router(state);

    let body = serde_json::json!({
        "path": "/etc/passwd",
        "content_hash": hash_str,
    })
    .to_string();

    let (status, resp_body) = post_json(&app, "/restore/file", &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "expected 400, got {resp_body}");
    assert!(resp_body.contains("must be under /workspace or /tmp"));
}

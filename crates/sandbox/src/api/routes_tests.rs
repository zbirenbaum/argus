use super::*;
use crate::api::state::{new_shared_state, new_shared_state_with_events};
use crate::events::EventPayload;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn test_router() -> Router {
    let state = new_shared_state("test-agent".into());
    Router::new()
        .route("/agent/pause", post(pause_handler))
        .route("/agent/resume", post(resume_handler))
        .route("/agent/status", get(status_handler))
        .route("/approvals/pending", get(pending_approvals_handler))
        .route("/approvals/{action_id}/approve", post(approve_handler))
        .route("/approvals/{action_id}/deny", post(deny_handler))
        .route("/health", get(health_handler))
        .with_state(state)
}

fn test_router_with_state() -> (Router, SharedState) {
    let state = new_shared_state("test-agent".into());
    let router = Router::new()
        .route("/agent/pause", post(pause_handler))
        .route("/agent/resume", post(resume_handler))
        .route("/agent/status", get(status_handler))
        .route("/approvals/pending", get(pending_approvals_handler))
        .route("/approvals/{action_id}/approve", post(approve_handler))
        .route("/approvals/{action_id}/deny", post(deny_handler))
        .route("/health", get(health_handler))
        .with_state(state.clone());
    (router, state)
}

fn test_router_with_events() -> (
    Router,
    SharedState,
    tokio::sync::mpsc::UnboundedReceiver<crate::events::Event>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let state = new_shared_state_with_events("test-agent".into(), tx);
    let router = Router::new()
        .route("/agent/pause", post(pause_handler))
        .route("/agent/resume", post(resume_handler))
        .route("/agent/status", get(status_handler))
        .route("/approvals/pending", get(pending_approvals_handler))
        .route("/approvals/{action_id}/approve", post(approve_handler))
        .route("/approvals/{action_id}/deny", post(deny_handler))
        .route("/health", get(health_handler))
        .with_state(state.clone());
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
    assert_eq!(resp.agent_id, "test-agent");
    assert!(resp.uptime_seconds >= 0.0);
}

#[tokio::test]
async fn pause_emits_event() {
    let (app, _state, mut rx) = test_router_with_events();
    let (status, _) = post_empty(&app, "/agent/pause").await;
    assert_eq!(status, StatusCode::OK);

    let event = rx.try_recv().unwrap();
    assert!(matches!(event.payload, EventPayload::AgentPause(_)));
    assert_eq!(event.agent_id, "test-agent");
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

// Rust guideline compliant 2026-02-21
//! Axum-based REST API for the supervisor.
//!
//! Provides pause/resume control, approval management, and health
//! endpoints. The server binds to `127.0.0.1:9090` by default and
//! communicates with the tracer thread through [`state::SharedState`].

pub(crate) mod errors;
pub(crate) mod routes;
pub mod state;
pub mod types;

use std::net::SocketAddr;

use axum::Router;
use axum::routing::{get, post};

use axum::routing::delete;

use crate::api::routes::{
    approve_handler, delete_rule_handler, deny_handler, get_rules_handler, health_handler,
    pause_handler, pending_approvals_handler, restore_handler, resume_handler, set_rules_handler,
    status_handler, tree_handler,
};
use crate::api::state::SharedState;

/// Builds the axum router with all supervisor API routes.
pub fn build_router(state: SharedState) -> Router {
    Router::new()
        .route("/agent/pause", post(pause_handler))
        .route("/agent/resume", post(resume_handler))
        .route("/agent/status", get(status_handler))
        .route("/approvals/pending", get(pending_approvals_handler))
        .route("/approvals/{action_id}/approve", post(approve_handler))
        .route("/approvals/{action_id}/deny", post(deny_handler))
        .route("/rules", get(get_rules_handler).post(set_rules_handler))
        .route("/rules/{index}", delete(delete_rule_handler))
        .route("/tree", get(tree_handler))
        .route("/restore", post(restore_handler))
        .route("/health", get(health_handler))
        .with_state(state)
}

/// Starts the API server on the given address.
///
/// Runs until `shutdown` is signalled. Should be spawned on the tokio
/// runtime, not called from the tracer thread.
///
/// # Errors
///
/// Returns an error if the TCP listener cannot bind to `addr`.
pub async fn serve(
    state: SharedState,
    addr: SocketAddr,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let app = build_router(state);
    let socket = tokio::net::TcpSocket::new_v4()?;
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    let listener = socket.listen(1024)?;
    tracing::info!(
        listen.addr = %addr,
        "API server listening on {{listen.addr}}"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let mut rx = shutdown;
            let _ = rx.wait_for(|&v| v).await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::new_shared_state;
    use crate::cas::MemoryCas;
    use crate::pipeline::RecordBus;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_cas() -> Arc<dyn crate::cas::Cas> {
        Arc::new(MemoryCas::new())
    }

    fn test_bus() -> RecordBus {
        RecordBus::new(vec![])
    }

    #[tokio::test]
    async fn router_serves_health() {
        let state = new_shared_state("integration".into(), test_cas(), test_bus());
        let app = build_router(state);
        let req = Request::builder()
            .method("GET")
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("ok"));
    }

    #[tokio::test]
    async fn router_pause_resume_cycle() {
        let state = new_shared_state("cycle".into(), test_cas(), test_bus());
        let app = build_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/agent/pause")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let req = Request::builder()
            .method("GET")
            .uri("/agent/status")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("paused"));

        let req = Request::builder()
            .method("POST")
            .uri("/agent/resume")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

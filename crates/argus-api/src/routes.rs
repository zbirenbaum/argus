// Rust guideline compliant 2026-02-21
//! API routes: event queries + supervisor proxy.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::db::{EventFilter, EventStore};
use crate::proxy::SupervisorProxy;

struct AppState {
    store: Arc<EventStore>,
    proxy: SupervisorProxy,
}

pub fn router(store: Arc<EventStore>, supervisor_base: String) -> Router {
    let state = Arc::new(AppState {
        store,
        proxy: SupervisorProxy::new(supervisor_base),
    });

    Router::new()
        // Query API
        .route("/events", get(query_events))
        .route("/events/count", get(event_count))
        .route("/events/latest", get(latest_seq))
        // Supervisor proxy
        .route("/agent/status", get(proxy_get))
        .route("/agent/pause", post(proxy_post))
        .route("/agent/resume", post(proxy_post))
        .route("/approvals/pending", get(proxy_get))
        .route("/approvals/{action_id}/approve", post(proxy_post_with_path))
        .route("/approvals/{action_id}/deny", post(proxy_post_with_path))
        .route("/tree", get(proxy_get))
        .route("/restore", post(proxy_post_with_body))
        .route("/rules", get(proxy_get))
        .route("/rules", post(proxy_post_with_body))
        // Health
        .route("/health", get(health))
        .with_state(state)
}

async fn query_events(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<EventFilter>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match state.store.query(&filter) {
        Ok(events) => Ok(Json(json!({
            "events": events,
            "count": events.len(),
        }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}

async fn event_count(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match state.store.count() {
        Ok(n) => Ok(Json(json!({ "count": n }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}

async fn latest_seq(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match state.store.max_seq() {
        Ok(seq) => Ok(Json(json!({ "seq": seq }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}

async fn proxy_get(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match state.proxy.get(uri.path()).await {
        Ok(v) => Ok(Json(v)),
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("supervisor: {e}") })),
        )),
    }
}

async fn proxy_post(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match state.proxy.post(uri.path(), None).await {
        Ok(v) => Ok(Json(v)),
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("supervisor: {e}") })),
        )),
    }
}

async fn proxy_post_with_path(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match state.proxy.post(uri.path(), None).await {
        Ok(v) => Ok(Json(v)),
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("supervisor: {e}") })),
        )),
    }
}

async fn proxy_post_with_body(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match state.proxy.post(uri.path(), Some(body)).await {
        Ok(v) => Ok(Json(v)),
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("supervisor: {e}") })),
        )),
    }
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

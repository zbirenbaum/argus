// Rust guideline compliant 2026-02-21
//! API routes: event queries, SSE stream, and supervisor proxy.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::cors::CorsLayer;

use crate::db::{EventFilter, EventStore};
use crate::proxy::SupervisorProxy;

struct AppState {
    store: Arc<EventStore>,
    proxy: SupervisorProxy,
    tx: broadcast::Sender<String>,
}

/// Build the application router with CORS and all routes.
pub fn router(
    store: Arc<EventStore>,
    supervisor_base: String,
    tx: broadcast::Sender<String>,
) -> Router {
    let state = Arc::new(AppState {
        store,
        proxy: SupervisorProxy::new(supervisor_base),
        tx,
    });

    // Allow all origins/methods/headers — suitable for local dev dashboards.
    let cors = CorsLayer::permissive();

    Router::new()
        // Query API
        .route("/events", get(query_events))
        .route("/events/count", get(event_count))
        .route("/events/latest", get(latest_seq))
        .route("/events/stream", get(sse_stream))
        // Supervisor proxy
        .route("/agent/status", get(proxy_get))
        .route("/agent/pause", post(proxy_post))
        .route("/agent/resume", post(proxy_post))
        .route("/approvals/pending", get(proxy_get))
        .route("/approvals/{action_id}/approve", post(proxy_post))
        .route("/approvals/{action_id}/deny", post(proxy_post))
        .route("/tree", get(proxy_get))
        .route("/restore", post(proxy_post_with_body))
        .route("/restore/file", post(proxy_post_with_body))
        .route("/rules", get(proxy_get))
        .route("/rules", post(proxy_post_with_body))
        // Health
        .route("/health", get(health))
        .layer(cors)
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

#[derive(Debug, Deserialize)]
struct StreamParams {
    after_seq: Option<i64>,
}

// FIXME(event-consumer): SSE live stream depends on the broadcast channel which
// is fed by the disk polling loop. This adds up to POLL_INTERVAL latency to
// live events. Once file watching replaces polling, this will be near-instant.
async fn sse_stream(
    State(state): State<Arc<AppState>>,
    Query(params): Query<StreamParams>,
) -> Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>> {
    // Subscribe before replaying so no events slip through the gap.
    let rx = state.tx.subscribe();

    let replay: Vec<SseEvent> = if let Some(after_seq) = params.after_seq {
        state.store
            .replay_after(after_seq)
            .unwrap_or_default()
            .into_iter()
            .map(|data| SseEvent::default().data(data))
            .collect()
    } else {
        Vec::new()
    };

    let replay_stream = tokio_stream::iter(replay.into_iter().map(Ok::<_, Infallible>));

    let live_stream = BroadcastStream::new(rx)
        .filter_map(|result| {
            // Lagged errors mean we missed events — log and skip rather than disconnect.
            match result {
                Ok(raw) => Some(raw),
                Err(_) => None,
            }
        })
        .map(move |raw| {
            let enriched = state.store.enrich_raw(&raw);
            Ok::<_, Infallible>(SseEvent::default().data(enriched))
        });

    let stream = replay_stream.chain(live_stream);

    // 15 s keep-alive prevents proxies from closing idle connections.
    Sse::new(stream).keep_alive(
        KeepAlive::new().interval(Duration::from_secs(15)),
    )
}

/// Convert a proxy result into an axum response, forwarding the upstream status code.
fn proxy_result(
    result: Result<crate::proxy::ProxyResponse, String>,
) -> (StatusCode, Json<Value>) {
    match result {
        Ok(r) => (r.status, Json(r.body)),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": format!("supervisor: {e}") }))),
    }
}

async fn proxy_get(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
) -> (StatusCode, Json<Value>) {
    proxy_result(state.proxy.get(uri.path()).await)
}

async fn proxy_post(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
) -> (StatusCode, Json<Value>) {
    proxy_result(state.proxy.post(uri.path(), None).await)
}

async fn proxy_post_with_body(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    proxy_result(state.proxy.post(uri.path(), Some(body)).await)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

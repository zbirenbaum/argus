// Rust guideline compliant 2026-02-21
//! Event ingest: disk polling + WS drain.
//!
//! The supervisor writes all events to JSONL segment files on disk via
//! `EventLogSink`. This module polls those files as the source of truth
//! for the SQLite store. A separate task keeps the supervisor WS drained
//! so the supervisor's `BroadcastSink` never backpressures the pipeline,
//! but the WS data is discarded — disk is canonical.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::broadcast;
use tokio_tungstenite::connect_async;
use tracing::{Level, event};

use crate::db::EventStore;

// FIXME(event-consumer): polling interval is a blunt instrument. Replace with
// inotify/kqueue file watching for lower latency and less wasted I/O. The
// current 500ms poll means the dashboard can lag up to 500ms behind reality.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Polls JSONL segment files on disk and inserts new events into the store.
///
/// Broadcasts each newly inserted event's raw JSON on `tx` for the SSE
/// stream. Runs forever until cancelled.
pub async fn poll_disk(
    store: &Arc<EventStore>,
    tx: broadcast::Sender<String>,
    event_log_dir: PathBuf,
) {
    // FIXME(event-consumer): load_from_disk re-reads entire files every poll.
    // Track per-file byte offset so we only read new lines. For now this is
    // acceptable because INSERT OR IGNORE makes re-reads idempotent, but it
    // wastes I/O on large segment files.
    loop {
        if event_log_dir.is_dir() {
            let prev_max = store.max_seq().unwrap_or(0);

            match crate::replay::load_from_disk(store, &event_log_dir) {
                Ok(n) => {
                    if n > 0 {
                        // Broadcast any events that are new since last poll.
                        broadcast_new_events(store, &tx, prev_max);
                    }
                }
                Err(e) => {
                    event!(
                        name: "ingest.poll_error",
                        Level::WARN,
                        error.message = %e,
                        "disk poll failed, will retry",
                    );
                }
            }
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Broadcast events with seq > `after_seq` on the broadcast channel.
fn broadcast_new_events(
    store: &EventStore,
    tx: &broadcast::Sender<String>,
    after_seq: i64,
) {
    let Ok(events) = store.replay_after(after_seq) else {
        return;
    };
    for raw in events {
        let _ = tx.send(raw);
    }
}

/// Drains the supervisor WebSocket to prevent backpressure.
///
/// The supervisor's `BroadcastSink` will block the ptrace pipeline if no
/// consumer reads from the WS. This task keeps the connection drained by
/// reading and discarding all messages.
// FIXME(event-consumer): evaluate whether the supervisor actually backpressures
// when no WS client is connected. If BroadcastSink silently drops with zero
// subscribers (which it does — see broadcast::Sender::send), this entire drain
// task is unnecessary and can be removed. Kept for safety until confirmed.
pub async fn drain_ws(url: &str) {
    let mut backoff = Duration::from_secs(1);
    const MAX_BACKOFF: Duration = Duration::from_secs(30);

    loop {
        event!(
            name: "ingest.ws_drain.connecting",
            Level::DEBUG,
            ws.url = url,
            "connecting to supervisor WebSocket (drain only)",
        );

        match connect_async(url).await {
            Ok((ws, _)) => {
                backoff = Duration::from_secs(1);
                event!(
                    name: "ingest.ws_drain.connected",
                    Level::DEBUG,
                    "WS drain connected",
                );
                let (_write, mut read) = ws.split();
                // Read and discard all messages until disconnect.
                while let Some(msg) = read.next().await {
                    if msg.is_err() {
                        break;
                    }
                }
                event!(
                    name: "ingest.ws_drain.disconnected",
                    Level::DEBUG,
                    "WS drain disconnected",
                );
            }
            Err(_) => {}
        }

        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

// Rust guideline compliant 2026-02-21
//! WebSocket ingest: connects to supervisor /ws, persists events.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::broadcast;
use tokio_tungstenite::connect_async;
use tracing::{Level, event};

use crate::db::EventStore;

/// Connect to the supervisor WebSocket, persist events, and broadcast raw JSON.
///
/// On each successful connect, replays any JSONL files in `event_log_dir`
/// so early events emitted before the WS was up are not lost.
/// Reconnects with exponential backoff on disconnect. Runs forever
/// until the task is cancelled.
pub async fn run(
    url: &str,
    store: &Arc<EventStore>,
    tx: broadcast::Sender<String>,
    event_log_dir: Option<PathBuf>,
) {
    let mut backoff = Duration::from_secs(1);
    // Cap at 30 s to avoid making the dashboard feel stale after a supervisor restart.
    const MAX_BACKOFF: Duration = Duration::from_secs(30);

    loop {
        event!(
            name: "ingest.connecting",
            Level::INFO,
            ws.url = url,
            "connecting to supervisor WebSocket",
        );

        match connect_async(url).await {
            Ok((ws, _)) => {
                backoff = Duration::from_secs(1);
                event!(
                    name: "ingest.connected",
                    Level::INFO,
                    "connected to supervisor",
                );

                // Replay disk events on each connect so early events
                // emitted before the WS was up are captured.
                if let Some(ref dir) = event_log_dir {
                    if dir.is_dir() {
                        match crate::replay::load_from_disk(store, dir) {
                            Ok(n) => event!(
                                name: "ingest.disk_replay",
                                Level::INFO,
                                events.replayed = n,
                                "replayed {{events.replayed}} events from disk on connect",
                            ),
                            Err(e) => event!(
                                name: "ingest.disk_replay_error",
                                Level::WARN,
                                error.message = %e,
                                "disk replay failed, continuing with live stream",
                            ),
                        }
                    }
                }

                handle_connection(ws, store, &tx).await;
                event!(
                    name: "ingest.disconnected",
                    Level::WARN,
                    "supervisor WebSocket disconnected",
                );
            }
            Err(e) => {
                event!(
                    name: "ingest.connect_failed",
                    Level::WARN,
                    error.message = %e,
                    backoff_s = backoff.as_secs(),
                    "WebSocket connect failed, retrying in {{backoff_s}}s",
                );
            }
        }

        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

async fn handle_connection(
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    store: &Arc<EventStore>,
    tx: &broadcast::Sender<String>,
) {
    let (_write, mut read) = ws.split();
    let mut count: u64 = 0;

    while let Some(msg) = read.next().await {
        let text: String = match msg {
            Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => t.to_string(),
            Ok(_) => continue,
            Err(e) => {
                event!(
                    name: "ingest.read_error",
                    Level::WARN,
                    error.message = %e,
                    "WebSocket read error",
                );
                break;
            }
        };

        if let Err(e) = store.insert(&text) {
            event!(
                name: "ingest.store_error",
                Level::ERROR,
                error.message = %e,
                "failed to persist event",
            );
        } else {
            // No subscribers is fine — send errors are ignored intentionally.
            let _ = tx.send(text);
            count += 1;
            if count % 1000 == 0 {
                event!(
                    name: "ingest.progress",
                    Level::INFO,
                    events.ingested = count,
                    "ingested {{events.ingested}} events",
                );
            }
        }
    }
}

// Rust guideline compliant 2026-02-21
//! Async stream that polls SSLKEYLOGFILE for new TLS key material.
//!
//! Wraps [`KeylogWatcher`] in a `futures::Stream` that yields
//! `(TlsKeys, ContentHash, Vec<u8>)` tuples at a configurable
//! interval. The content hash and raw bytes are provided so the
//! pipeline can persist them through its own durability layer.

use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::Stream;
use tokio::time::{Interval, interval};
use tokio_util::sync::CancellationToken;

use crate::cas::ContentHash;
use crate::events::network::TlsKeys;
use crate::net::KeylogWatcher;

/// Item yielded by [`KeylogStream`].
pub type KeylogItem = (TlsKeys, ContentHash, Vec<u8>);

/// Async stream of TLS key material from SSLKEYLOGFILE.
pub struct KeylogStream {
    watcher: KeylogWatcher,
    ticker: Interval,
    cancel: CancellationToken,
    /// Buffered items from the last poll batch.
    buffer: Vec<KeylogItem>,
}

impl KeylogStream {
    /// Create a new stream polling `path` every `poll_interval`.
    pub fn new(path: PathBuf, poll_interval: Duration, cancel: CancellationToken) -> Self {
        Self {
            watcher: KeylogWatcher::new(path),
            ticker: interval(poll_interval),
            cancel,
            buffer: Vec::new(),
        }
    }
}

impl Stream for KeylogStream {
    type Item = KeylogItem;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if self.cancel.is_cancelled() {
            return Poll::Ready(None);
        }

        // Drain buffered items first.
        if let Some(item) = self.buffer.pop() {
            return Poll::Ready(Some(item));
        }

        // Wait for the next tick.
        if self.ticker.poll_tick(cx).is_pending() {
            return Poll::Pending;
        }

        // Poll the watcher for new lines.
        match self.watcher.poll_new_lines(0, -1) {
            Ok(items) => {
                self.buffer = items;
                // Reverse so pop() yields in order.
                self.buffer.reverse();
                match self.buffer.pop() {
                    Some(item) => Poll::Ready(Some(item)),
                    None => {
                        // No new data this tick — re-register for next tick.
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                }
            }
            Err(e) => {
                tracing::event!(
                    name: "pipeline.stream.keylog.error",
                    tracing::Level::WARN,
                    error.message = %e,
                    "keylog stream poll failed: {{error.message}}",
                );
                // Error is non-fatal; continue polling.
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::time::Duration;

    use futures::StreamExt;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    /// 64 hex character client_random (32 bytes).
    const TEST_CR: &str = "aabbccdd00112233aabbccdd00112233aabbccdd00112233aabbccdd00112233";

    #[tokio::test]
    async fn yields_keylog_items() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keylog.txt");
        fs::write(&path, format!("CLIENT_RANDOM {TEST_CR} deadbeef\n")).unwrap();

        let cancel = CancellationToken::new();
        let mut stream = KeylogStream::new(path, Duration::from_millis(1), cancel.clone());

        let item = stream.next().await.unwrap();
        assert_eq!(item.0.pid, 0);
        assert!(item.0.keylog_line_hash.is_some());

        cancel.cancel();
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn cancellation_ends_stream() {
        let cancel = CancellationToken::new();
        cancel.cancel();

        let mut stream = KeylogStream::new(
            PathBuf::from("/nonexistent"),
            Duration::from_secs(60),
            cancel,
        );
        assert!(stream.next().await.is_none());
    }
}

// Rust guideline compliant 2026-02-21
//! Async stream that polls the mitmdump flow output file for HTTP events.
//!
//! Wraps [`FlowWatcher`] in a `futures::Stream` that yields
//! `EventPayload` items at a configurable interval. Content blobs
//! (headers, bodies) are returned alongside each flow for the
//! pipeline to persist through its own durability layer.

use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::Stream;
use tokio::time::{Interval, interval};
use tokio_util::sync::CancellationToken;

use crate::events::EventPayload;
use crate::net::FlowWatcher;
use crate::net::flow_parser::FlowContent;

/// Item yielded by [`ProxyStream`]: an event payload plus content blobs.
pub type ProxyItem = (EventPayload, Vec<FlowContent>);

/// Async stream of HTTP flow events from the mitmdump addon.
pub struct ProxyStream {
    watcher: FlowWatcher,
    ticker: Interval,
    cancel: CancellationToken,
    /// Buffered items from the last poll batch.
    buffer: Vec<ProxyItem>,
}

impl ProxyStream {
    /// Create a new stream polling `path` every `poll_interval`.
    pub fn new(path: PathBuf, poll_interval: Duration, cancel: CancellationToken) -> Self {
        Self {
            watcher: FlowWatcher::new(path),
            ticker: interval(poll_interval),
            cancel,
            buffer: Vec::new(),
        }
    }
}

impl Stream for ProxyStream {
    type Item = ProxyItem;

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

        // Poll the watcher for new flows.
        match self.watcher.poll_new_flows(0) {
            Ok(flows) => {
                // Flatten FlowEvents into individual EventPayload items.
                for (flow, content) in flows {
                    if let Some(req) = flow.request {
                        self.buffer.push((EventPayload::HttpRequest(req), content.clone()));
                    }
                    if let Some(resp) = flow.response {
                        self.buffer.push((EventPayload::HttpResponse(resp), content));
                    }
                }
                // Reverse so pop() yields in order.
                self.buffer.reverse();
                match self.buffer.pop() {
                    Some(item) => Poll::Ready(Some(item)),
                    None => {
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                }
            }
            Err(e) => {
                tracing::event!(
                    name: "pipeline.stream.proxy.error",
                    tracing::Level::WARN,
                    error.message = %e,
                    "proxy stream poll failed: {{error.message}}",
                );
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

    fn flow_json(method: &str, url: &str, status: u16) -> String {
        format!(
            r#"{{"request":{{"method":"{method}","url":"{url}","headers":[["Host","example.com"]],"body":"aGVsbG8="}},"response":{{"status_code":{status},"headers":[["Content-Type","text/plain"]],"body":"d29ybGQ="}}}}"#,
        )
    }

    #[tokio::test]
    async fn yields_request_and_response() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("flows.jsonl");
        fs::write(&path, format!("{}\n", flow_json("GET", "https://a.com/", 200))).unwrap();

        let cancel = CancellationToken::new();
        let mut stream = ProxyStream::new(path, Duration::from_millis(1), cancel.clone());

        // First item is the request.
        let (payload, _content) = stream.next().await.unwrap();
        assert!(matches!(payload, EventPayload::HttpRequest(_)));

        // Second item is the response.
        let (payload, _content) = stream.next().await.unwrap();
        assert!(matches!(payload, EventPayload::HttpResponse(_)));

        cancel.cancel();
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn cancellation_ends_stream() {
        let cancel = CancellationToken::new();
        cancel.cancel();

        let mut stream = ProxyStream::new(
            PathBuf::from("/nonexistent"),
            Duration::from_secs(60),
            cancel,
        );
        assert!(stream.next().await.is_none());
    }
}

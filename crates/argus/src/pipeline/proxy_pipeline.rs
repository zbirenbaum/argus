// Rust guideline compliant 2026-02-21
//! Proxy pipeline: async stream of HTTP events from mitmdump flow output.
//!
//! Uses [`ProxyStream`] to poll the flow file and emits HTTP events
//! through the pipeline's own outputs and bus. Shutdown is via
//! `CancellationToken`.

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{Level, event};

use crate::events::Event;
use crate::pipeline::EmitResult;
use crate::pipeline::Record;
use crate::pipeline::context::PipelineContext;
use crate::pipeline::outputs::OutputList;
use crate::pipeline::stages::redact::RedactStage;
use crate::pipeline::streams::ProxyStream;

/// Run the proxy pipeline as a tokio task until cancelled.
///
/// Each pipeline owns its own `OutputList` and `RedactStage`
/// so no cross-task sharing is needed. Returns immediately if
/// `flow_path` is `None`.
pub(crate) async fn run(
    flow_path: Option<PathBuf>,
    ctx: PipelineContext,
    mut outputs: OutputList,
    redact: RedactStage,
    cancel: CancellationToken,
    poll_interval: Duration,
) {
    let Some(path) = flow_path else {
        event!(
            name: "pipeline.proxy.skipped",
            Level::INFO,
            "no flow output path configured, proxy pipeline not started",
        );
        return;
    };

    event!(
        name: "pipeline.proxy.started",
        Level::INFO,
        flow.path = %path.display(),
        "proxy pipeline started: {{flow.path}}",
    );

    let mut stream = ProxyStream::new(path, poll_interval, cancel);

    while let Some((payload, content_blobs)) = stream.next().await {
        // Persist content blobs (headers, bodies) via bus.
        for blob in content_blobs {
            let record = Record::Content { hash: blob.hash, data: blob.data };
            if let EmitResult::RequiredFailed(failures) = ctx.bus.emit(record.clone()) {
                if let Some(ref overflow) = ctx.overflow {
                    overflow.push(&record);
                }
                for (sink_name, err) in &failures {
                    event!(
                        name: "pipeline.emit.required_sink_failed",
                        Level::ERROR,
                        sink.name = sink_name.as_str(),
                        error.message = %err,
                        "required sink failed emitting HTTP content",
                    );
                }
            }
        }

        let mut evt = Event::new(&ctx.seq, ctx.agent_id.clone(), payload);
        event!(
            name: "pipeline.proxy.event",
            Level::DEBUG,
            event.seq = evt.seq,
            event.type_ = evt.payload.event_type_tag(),
            "proxy pipeline emitting event {{event.seq}} {{event.type_}}",
        );
        redact.redact(&mut evt);
        outputs.emit(&evt);
        let record = Record::Event(evt);
        if let EmitResult::RequiredFailed(failures) = ctx.bus.emit(record.clone()) {
            if let Some(ref overflow) = ctx.overflow {
                overflow.push(&record);
            }
            for (sink_name, err) in &failures {
                event!(
                    name: "pipeline.emit.required_sink_failed",
                    Level::ERROR,
                    sink.name = sink_name.as_str(),
                    error.message = %err,
                    "required sink failed on proxy path, buffered in overflow queue",
                );
            }
        }
    }

    event!(
        name: "pipeline.proxy.stopped",
        Level::INFO,
        "proxy pipeline stopped",
    );
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use crate::config::RedactConfig;
    use crate::events::SequenceGenerator;
    use crate::pipeline::bus::RecordBus;
    use crate::pipeline::context::PipelineContext;
    use crate::pipeline::outputs::OutputList;
    use crate::pipeline::stages::redact::RedactStage;

    fn make_ctx() -> PipelineContext {
        PipelineContext::new(
            Arc::new(SequenceGenerator::new(0)),
            RecordBus::new(vec![]),
            "test-agent".into(),
            None,
        )
    }

    fn flow_json(method: &str, url: &str, status: u16) -> String {
        format!(
            r#"{{"request":{{"method":"{method}","url":"{url}","headers":[["Host","example.com"]],"body":"aGVsbG8="}},"response":{{"status_code":{status},"headers":[["Content-Type","text/plain"]],"body":"d29ybGQ="}}}}"#,
        )
    }

    #[tokio::test]
    async fn run_skips_when_no_path() {
        let ctx = make_ctx();
        let outputs = OutputList::new();
        let redact = RedactStage::new(&RedactConfig::default());
        let cancel = CancellationToken::new();
        super::run(None, ctx, outputs, redact, cancel, Duration::from_secs(60)).await;
    }

    #[tokio::test]
    async fn run_drains_on_cancel() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("flows.jsonl");
        fs::write(&path, format!("{}\n", flow_json("GET", "https://a.com/", 200))).unwrap();

        let ctx = make_ctx();
        let outputs = OutputList::new();
        let redact = RedactStage::new(&RedactConfig::default());
        let cancel = CancellationToken::new();

        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        super::run(Some(path), ctx, outputs, redact, cancel, Duration::from_millis(1)).await;
    }

    #[tokio::test]
    async fn run_handles_missing_file() {
        let ctx = make_ctx();
        let outputs = OutputList::new();
        let redact = RedactStage::new(&RedactConfig::default());
        let cancel = CancellationToken::new();
        cancel.cancel();

        super::run(
            Some(std::path::PathBuf::from("/nonexistent/flows.jsonl")),
            ctx,
            outputs,
            redact,
            cancel,
            Duration::from_millis(1),
        ).await;
    }
}

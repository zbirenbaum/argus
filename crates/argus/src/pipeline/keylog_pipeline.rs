// Rust guideline compliant 2026-02-21
//! Keylog pipeline: async stream of TLS key events from SSLKEYLOGFILE.
//!
//! Uses [`KeylogStream`] to poll the keylog file and emits `TlsKeys`
//! events through the pipeline's own outputs and bus. Shutdown is via
//! `CancellationToken`.

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{Level, event};

use crate::events::{Event, EventPayload};
use crate::pipeline::EmitResult;
use crate::pipeline::Record;
use crate::pipeline::context::PipelineContext;
use crate::pipeline::outputs::OutputList;
use crate::pipeline::stages::redact::RedactStage;
use crate::pipeline::streams::KeylogStream;

/// Run the keylog pipeline as a tokio task until cancelled.
///
/// Each pipeline owns its own `OutputList` and `RedactStage`
/// so no cross-task sharing is needed.
pub(crate) async fn run(
    keylog_path: PathBuf,
    ctx: PipelineContext,
    mut outputs: OutputList,
    redact: RedactStage,
    cancel: CancellationToken,
    poll_interval: Duration,
) {
    event!(
        name: "pipeline.keylog.started",
        Level::INFO,
        keylog.path = %keylog_path.display(),
        "keylog pipeline started: {{keylog.path}}",
    );

    let mut stream = KeylogStream::new(keylog_path, poll_interval, cancel);

    while let Some((tls, hash, data)) = stream.next().await {
        // Persist content via bus (mirrors old process_new_lines behavior).
        let content_record = Record::Content { hash, data };
        if let EmitResult::RequiredFailed(failures) = ctx.bus.emit(content_record.clone()) {
            if let Some(ref overflow) = ctx.overflow {
                overflow.push(&content_record);
            }
            for (sink_name, err) in &failures {
                event!(
                    name: "pipeline.emit.required_sink_failed",
                    Level::ERROR,
                    sink.name = sink_name.as_str(),
                    error.message = %err,
                    "required sink failed emitting TLS key content",
                );
            }
        }

        let mut evt = Event::new(&ctx.seq, ctx.agent_id.clone(), EventPayload::TlsKeys(tls));
        event!(
            name: "pipeline.keylog.event",
            Level::DEBUG,
            event.seq = evt.seq,
            "keylog pipeline emitting TlsKeys event {{event.seq}}",
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
                    "required sink failed on keylog path, buffered in overflow queue",
                );
            }
        }
    }

    event!(
        name: "pipeline.keylog.stopped",
        Level::INFO,
        "keylog pipeline stopped",
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

    /// 64 hex character client_random (32 bytes).
    const TEST_CR: &str = "aabbccdd00112233aabbccdd00112233aabbccdd00112233aabbccdd00112233";

    #[tokio::test]
    async fn run_drains_on_cancel() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keylog.txt");
        fs::write(&path, format!("CLIENT_RANDOM {TEST_CR} deadbeef\n")).unwrap();

        let ctx = make_ctx();
        let outputs = OutputList::new();
        let redact = RedactStage::new(&RedactConfig::default());
        let cancel = CancellationToken::new();

        // Cancel immediately so the task exits after processing existing data.
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        super::run(path, ctx, outputs, redact, cancel, Duration::from_millis(1)).await;
    }

    #[tokio::test]
    async fn run_handles_missing_keylog() {
        let ctx = make_ctx();
        let outputs = OutputList::new();
        let redact = RedactStage::new(&RedactConfig::default());
        let cancel = CancellationToken::new();
        cancel.cancel();

        super::run(
            std::path::PathBuf::from("/nonexistent/sslkeylogfile"),
            ctx,
            outputs,
            redact,
            cancel,
            Duration::from_millis(1),
        ).await;
    }
}

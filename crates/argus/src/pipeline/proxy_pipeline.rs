// Rust guideline compliant 2026-02-21
//! Proxy pipeline: polls the mitmdump flow output file and emits HTTP events.
//!
//! Runs on a dedicated thread, polling the flow file at a configurable
//! interval. If no flow path is configured the pipeline exits immediately.
//! On shutdown the stop flag is set and one final drain is performed to
//! capture any flows written just before exit.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::{Level, event};

use crate::events::Event;
use crate::net::FlowWatcher;
use crate::pipeline::EmitResult;
use crate::pipeline::Record;
use crate::pipeline::context::PipelineContext;
use crate::pipeline::outputs::OutputList;
use crate::pipeline::stages::redact::RedactStage;

/// Run the proxy pipeline on the calling thread until `stop` is set.
///
/// Each pipeline thread owns its own `OutputList` and `RedactStage`
/// so no cross-thread sharing is needed. Consumers use the monotonic
/// sequence number on each event for total ordering.
pub(crate) fn run(
    flow_path: Option<PathBuf>,
    ctx: PipelineContext,
    mut outputs: OutputList,
    redact: RedactStage,
    stop: Arc<AtomicBool>,
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

    let mut watcher = FlowWatcher::new(path.clone());

    event!(
        name: "pipeline.proxy.started",
        Level::INFO,
        flow.path = %path.display(),
        "proxy pipeline started: {{flow.path}}",
    );

    loop {
        if stop.load(Ordering::Acquire) {
            event!(
                name: "pipeline.proxy.draining",
                Level::DEBUG,
                "proxy pipeline stop flag observed, performing final drain",
            );
            break;
        }
        poll_once(&mut watcher, &ctx, &mut outputs, &redact);
        std::thread::sleep(poll_interval);
    }

    // Final drain: capture any flows written between the last poll and shutdown.
    poll_once(&mut watcher, &ctx, &mut outputs, &redact);

    event!(
        name: "pipeline.proxy.stopped",
        Level::INFO,
        "proxy pipeline stopped",
    );
}

/// Poll for new flows and emit HTTP events.
fn poll_once(
    watcher: &mut FlowWatcher,
    ctx: &PipelineContext,
    outputs: &mut OutputList,
    redact: &RedactStage,
) {
    match watcher.process_new_flows(&ctx.bus, 0) {
        Ok(flows) => {
            for payload in FlowWatcher::into_event_payloads(flows) {
                let mut evt = Event::new(&ctx.seq, ctx.agent_id.clone(), payload);
                event!(
                    name: "pipeline.proxy.event",
                    Level::DEBUG,
                    event.seq = evt.seq,
                    event.type_ = evt.payload.event_type_tag(),
                    "proxy pipeline emitting event {{event.seq}} {{event.type_}}",
                );
                // Redact and deliver to this thread's user-facing outputs.
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
        }
        Err(e) => {
            event!(
                name: "pipeline.proxy.poll_error",
                Level::WARN,
                error.message = %e,
                "flow poll failed: {{error.message}}",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use tempfile::TempDir;

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

    #[test]
    fn run_skips_when_no_path() {
        let ctx = make_ctx();
        let outputs = OutputList::new();
        let redact = RedactStage::new(&RedactConfig::default());
        let stop = Arc::new(AtomicBool::new(false));
        super::run(None, ctx, outputs, redact, stop, Duration::from_secs(60));
    }

    #[test]
    fn run_drains_on_stop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("flows.jsonl");
        fs::write(&path, format!("{}\n", flow_json("GET", "https://a.com/", 200))).unwrap();

        let ctx = make_ctx();
        let outputs = OutputList::new();
        let redact = RedactStage::new(&RedactConfig::default());
        let stop = Arc::new(AtomicBool::new(false));

        stop.store(true, Ordering::Release);

        super::run(Some(path), ctx, outputs, redact, stop, Duration::from_millis(1));
    }

    #[test]
    fn run_handles_missing_file() {
        let ctx = make_ctx();
        let outputs = OutputList::new();
        let redact = RedactStage::new(&RedactConfig::default());
        let stop = Arc::new(AtomicBool::new(true));

        super::run(
            Some(std::path::PathBuf::from("/nonexistent/flows.jsonl")),
            ctx,
            outputs,
            redact,
            stop,
            Duration::from_millis(1),
        );
    }
}

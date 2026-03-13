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
use crate::pipeline::Record;
use crate::pipeline::context::PipelineContext;

/// Run the proxy pipeline on the calling thread until `stop` is set.
///
/// If `flow_path` is `None` the pipeline logs a notice and returns
/// immediately — no thread resources are consumed. Otherwise, polls
/// `flow_path` at `poll_interval`, emitting `HttpRequest`/`HttpResponse`
/// events to `ctx.bus`. A final drain is performed after the stop flag is
/// observed so that flows written just before shutdown are not lost.
pub(crate) fn run(
    flow_path: Option<PathBuf>,
    ctx: PipelineContext,
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
        poll_once(&mut watcher, &ctx);
        std::thread::sleep(poll_interval);
    }

    // Final drain: capture any flows written between the last poll and shutdown.
    poll_once(&mut watcher, &ctx);

    event!(
        name: "pipeline.proxy.stopped",
        Level::INFO,
        "proxy pipeline stopped",
    );
}

/// Poll for new flows and emit `HttpRequest`/`HttpResponse` events to the bus.
fn poll_once(watcher: &mut FlowWatcher, ctx: &PipelineContext) {
    match watcher.process_new_flows(&ctx.bus, 0) {
        Ok(flows) => {
            for payload in FlowWatcher::into_event_payloads(flows) {
                let evt = Event::new(&ctx.seq, ctx.agent_id.clone(), payload);
                event!(
                    name: "pipeline.proxy.event",
                    Level::DEBUG,
                    event.seq = evt.seq,
                    event.type_ = evt.payload.event_type_tag(),
                    "proxy pipeline emitting event {{event.seq}} {{event.type_}}",
                );
                ctx.bus.emit(Record::Event(evt));
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

    use crate::events::SequenceGenerator;
    use crate::pipeline::bus::RecordBus;
    use crate::pipeline::context::PipelineContext;

    fn make_ctx() -> PipelineContext {
        PipelineContext::new(
            Arc::new(SequenceGenerator::new(0)),
            RecordBus::new(vec![]),
            "test-agent".into(),
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
        let stop = Arc::new(AtomicBool::new(false));
        // Must return immediately without blocking even though stop is not set.
        super::run(None, ctx, stop, Duration::from_secs(60));
    }

    #[test]
    fn run_drains_on_stop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("flows.jsonl");
        fs::write(&path, format!("{}\n", flow_json("GET", "https://a.com/", 200))).unwrap();

        let ctx = make_ctx();
        let stop = Arc::new(AtomicBool::new(false));

        // Signal stop immediately so the loop exits after the first drain.
        stop.store(true, Ordering::Release);

        super::run(Some(path), ctx, stop, Duration::from_millis(1));
    }

    #[test]
    fn run_handles_missing_file() {
        let ctx = make_ctx();
        let stop = Arc::new(AtomicBool::new(true));

        super::run(
            Some(std::path::PathBuf::from("/nonexistent/flows.jsonl")),
            ctx,
            stop,
            Duration::from_millis(1),
        );
    }
}

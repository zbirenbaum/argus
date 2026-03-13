// Rust guideline compliant 2026-02-21
//! Keylog pipeline: polls SSLKEYLOGFILE and emits `TlsKeys` events.
//!
//! Runs on a dedicated thread, polling the keylog file at a configurable
//! interval. On shutdown the stop flag is set and one final drain is
//! performed to capture any lines written just before exit.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::{Level, event};

use crate::events::{Event, EventPayload};
use crate::net::KeylogWatcher;
use crate::pipeline::Record;
use crate::pipeline::context::PipelineContext;

/// Run the keylog pipeline on the calling thread until `stop` is set.
///
/// Polls the keylog file at `poll_interval`, emitting `TlsKeys` events to
/// `ctx.bus` for each new NSS Key Log line. A final drain is performed after
/// the stop flag is observed so that lines written just before shutdown are
/// not lost.
pub(crate) fn run(
    keylog_path: PathBuf,
    ctx: PipelineContext,
    stop: Arc<AtomicBool>,
    poll_interval: Duration,
) {
    let mut watcher = KeylogWatcher::new(keylog_path.clone());

    event!(
        name: "pipeline.keylog.started",
        Level::INFO,
        keylog.path = %keylog_path.display(),
        "keylog pipeline started: {{keylog.path}}",
    );

    loop {
        if stop.load(Ordering::Acquire) {
            event!(
                name: "pipeline.keylog.draining",
                Level::DEBUG,
                "keylog pipeline stop flag observed, performing final drain",
            );
            break;
        }
        poll_once(&mut watcher, &ctx);
        std::thread::sleep(poll_interval);
    }

    // Final drain: capture any lines written between the last poll and shutdown.
    poll_once(&mut watcher, &ctx);

    event!(
        name: "pipeline.keylog.stopped",
        Level::INFO,
        "keylog pipeline stopped",
    );
}

/// Poll for new keylog lines and emit `TlsKeys` events to the bus.
fn poll_once(watcher: &mut KeylogWatcher, ctx: &PipelineContext) {
    match watcher.process_new_lines(&ctx.bus, 0, -1) {
        Ok(tls_events) => {
            for tls in tls_events {
                let evt = Event::new(&ctx.seq, ctx.agent_id.clone(), EventPayload::TlsKeys(tls));
                event!(
                    name: "pipeline.keylog.event",
                    Level::DEBUG,
                    event.seq = evt.seq,
                    "keylog pipeline emitting TlsKeys event {{event.seq}}",
                );
                ctx.bus.emit(Record::Event(evt));
            }
        }
        Err(e) => {
            event!(
                name: "pipeline.keylog.poll_error",
                Level::WARN,
                error.message = %e,
                "keylog poll failed: {{error.message}}",
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
            "test-agent".to_owned(),
        )
    }

    /// 64 hex character client_random (32 bytes).
    const TEST_CR: &str = "aabbccdd00112233aabbccdd00112233aabbccdd00112233aabbccdd00112233";

    #[test]
    fn run_drains_on_stop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keylog.txt");
        fs::write(&path, format!("CLIENT_RANDOM {TEST_CR} deadbeef\n")).unwrap();

        let ctx = make_ctx();
        let stop = Arc::new(AtomicBool::new(false));

        // Signal stop immediately so the loop exits after the first drain.
        stop.store(true, Ordering::Release);

        // Should complete without blocking.
        super::run(path, ctx, stop, Duration::from_millis(1));
    }

    #[test]
    fn run_handles_missing_keylog() {
        let ctx = make_ctx();
        let stop = Arc::new(AtomicBool::new(true));

        // No file written — should handle gracefully.
        super::run(
            std::path::PathBuf::from("/nonexistent/sslkeylogfile"),
            ctx,
            stop,
            Duration::from_millis(1),
        );
    }
}

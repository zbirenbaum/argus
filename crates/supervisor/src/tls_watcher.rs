// Rust guideline compliant 2026-02-21
//! Background TLS event poller.
//!
//! Periodically reads the SSLKEYLOGFILE and mitmdump flow output,
//! building and emitting `TlsKeys`, `HttpRequest`, and `HttpResponse`
//! events. Runs on a dedicated thread alongside the ptrace loop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tracing::{event, Level};

use argus::events::{Event, EventPayload, SequenceGenerator};
use argus::net::{FlowWatcher, KeylogWatcher};
use argus::pipeline::{Record, RecordBus};

/// How often the watcher polls for new keylog lines and flow data.
/// Fast enough to capture most TLS sessions before the agent exits,
/// slow enough to avoid burning CPU on an idle file.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Spawns the TLS watcher thread.
///
/// Returns the join handle. The caller must set `stop` to `true`
/// and join the handle during shutdown.
pub fn spawn(
    keylog_path: PathBuf,
    flow_output: Option<PathBuf>,
    bus: RecordBus,
    seq_gen: SequenceGenerator,
    agent_id: String,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("tls-watcher".into())
        .spawn(move || {
            run(keylog_path, flow_output, bus, seq_gen, agent_id, stop);
        })
        .expect("failed to spawn tls-watcher thread")
}

/// Polling loop body.
fn run(
    keylog_path: PathBuf,
    flow_output: Option<PathBuf>,
    bus: RecordBus,
    seq_gen: SequenceGenerator,
    agent_id: String,
    stop: Arc<AtomicBool>,
) {
    let mut keylog = KeylogWatcher::new(keylog_path);
    let mut flow = flow_output.map(FlowWatcher::new);

    event!(
        name: "tls_watcher.started",
        Level::INFO,
        "TLS watcher thread started",
    );

    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }

        poll_keylog(&mut keylog, &bus, &seq_gen, &agent_id);

        if let Some(ref mut fw) = flow {
            poll_flows(fw, &bus, &seq_gen, &agent_id);
        }

        thread::sleep(POLL_INTERVAL);
    }

    // Final drain — capture anything written between the last poll
    // and the stop signal.
    poll_keylog(&mut keylog, &bus, &seq_gen, &agent_id);
    if let Some(ref mut fw) = flow {
        poll_flows(fw, &bus, &seq_gen, &agent_id);
    }

    event!(
        name: "tls_watcher.stopped",
        Level::INFO,
        "TLS watcher thread stopped",
    );
}

/// Reads new keylog lines and emits `TlsKeys` events via the bus.
fn poll_keylog(
    watcher: &mut KeylogWatcher,
    bus: &RecordBus,
    seq_gen: &SequenceGenerator,
    agent_id: &str,
) {
    // pid=0, fd=-1 because keylog lines come from the SSLKEYLOGFILE env var,
    // not from a specific traced process fd at poll time.
    match watcher.process_new_lines(bus, 0, -1) {
        Ok(tls_events) => {
            for tls in tls_events {
                let evt = Event::new(
                    seq_gen,
                    agent_id.to_owned(),
                    EventPayload::TlsKeys(tls),
                );
                bus.emit(Record::Event(evt));
            }
        }
        Err(e) => {
            event!(
                name: "tls_watcher.keylog.error",
                Level::WARN,
                error.message = %e,
                "keylog poll failed: {{error.message}}",
            );
        }
    }
}

/// Reads new flows and emits `HttpRequest`/`HttpResponse` events via the bus.
fn poll_flows(
    watcher: &mut FlowWatcher,
    bus: &RecordBus,
    seq_gen: &SequenceGenerator,
    agent_id: &str,
) {
    match watcher.process_new_flows(bus, 0) {
        Ok(flows) => {
            for payload in FlowWatcher::into_event_payloads(flows) {
                let evt = Event::new(
                    seq_gen,
                    agent_id.to_owned(),
                    payload,
                );
                bus.emit(Record::Event(evt));
            }
        }
        Err(e) => {
            event!(
                name: "tls_watcher.flow.error",
                Level::WARN,
                error.message = %e,
                "flow poll failed: {{error.message}}",
            );
        }
    }
}

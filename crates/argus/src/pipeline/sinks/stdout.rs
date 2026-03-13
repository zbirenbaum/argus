// Rust guideline compliant 2026-02-21
//! Blocking sink that writes events as newline-delimited JSON to stdout.
//!
//! Used by the supervisor so the validate.sh harness and external tooling
//! can consume events without reading on-disk segment files. Agent
//! stdout and stderr are drained to a separate thread so they never
//! mix with this JSONL stream.

use std::io::{self, BufWriter, Write};
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::pipeline::record::Record;
use crate::pipeline::sink::{Sink, SinkPriority};

/// Blocking sink that serializes each event as one JSON line to stdout.
///
/// Only `Record::Event` records are written; content, manifest, and
/// checkpoint records are silently ignored so the stream stays valid JSONL.
///
/// `BufWriter<Stdout>` is wrapped in a `Mutex` because `Stdout` itself is
/// not `Sync` in all configurations and the `Sink` trait requires `Sync`.
#[derive(Debug)]
pub struct StdoutSink {
    out: Mutex<BufWriter<io::Stdout>>,
}

impl StdoutSink {
    /// Creates a sink backed by buffered stdout.
    pub fn new() -> Self {
        Self {
            out: Mutex::new(BufWriter::new(io::stdout())),
        }
    }
}

impl Default for StdoutSink {
    fn default() -> Self {
        Self::new()
    }
}

impl Sink for StdoutSink {
    fn priority(&self) -> SinkPriority {
        // Blocking so events appear before the tracee resumes — consumers
        // that react to events (e.g. pause-before-action) see them in order.
        SinkPriority::Blocking
    }

    fn accept(&self, record: &Record) -> bool {
        matches!(record, Record::Event(_))
    }

    fn write(&self, record: Record) -> Result<()> {
        let Record::Event(event) = record else {
            return Ok(());
        };
        let json =
            serde_json::to_string(&event).with_context(|| format!("serialize event seq={}", event.seq))?;
        let mut out = self.out.lock().expect("stdout sink mutex poisoned");
        writeln!(out, "{json}").context("write event to stdout")?;
        out.flush().context("flush stdout after event")
    }

    fn flush(&self) -> Result<()> {
        self.out
            .lock()
            .expect("stdout sink mutex poisoned")
            .flush()
            .context("flush stdout sink")
    }

    fn name(&self) -> &str {
        "stdout"
    }
}

#[cfg(test)]
mod tests {
    use crate::events::envelope::{Event, EventPayload};
    use crate::events::control::AgentStart;
    use crate::pipeline::record::Record;
    use crate::pipeline::sink::{Sink, SinkPriority};

    use super::StdoutSink;

    fn make_event_record() -> Record {
        Record::Event(Event {
            seq: 1,
            ts_monotonic: 0,
            ts_wall: "2026-01-01T00:00:00Z".to_owned(),
            agent_id: "test".to_owned(),
            vclock: None,
            payload: EventPayload::AgentStart(AgentStart {
                agent_id: "test".to_owned(),
                supervisor_pid_host: None,
                supervisor_pid_ns: None,
                config_summary: "test".to_owned(),
                node: None,
                pod: None,
                container: None,
            }),
        })
    }

    #[test]
    fn priority_is_blocking() {
        assert_eq!(StdoutSink::new().priority(), SinkPriority::Blocking);
    }

    #[test]
    fn accepts_only_events() {
        use crate::cas::ContentHash;
        let sink = StdoutSink::new();
        assert!(sink.accept(&make_event_record()));
        assert!(!sink.accept(&Record::Content {
            hash: ContentHash::from_data(b"test"),
            data: vec![],
        }));
    }

    #[test]
    fn name_is_stdout() {
        assert_eq!(StdoutSink::new().name(), "stdout");
    }
}

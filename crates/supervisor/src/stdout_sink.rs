// Rust guideline compliant 2026-02-21
//! Stdout JSON-lines event sink.
//!
//! Writes each event as a single JSON line to stdout, matching the
//! original event writer behavior.

use std::any::Any;
use std::io::{self, Write};

use anyhow::{Context, Result};

use argus::events::Event;

use crate::event_sink::EventSink;

/// Writes events as newline-delimited JSON to stdout.
///
/// Uses `BufWriter<Stdout>` (not `StdoutLock`) so the sink is `Send`
/// and can move to the writer thread.
pub struct StdoutSink {
    out: io::BufWriter<io::Stdout>,
}

impl StdoutSink {
    /// Create a new stdout sink with buffered output.
    pub fn new() -> Self {
        Self {
            out: io::BufWriter::new(io::stdout()),
        }
    }
}

impl EventSink for StdoutSink {
    fn write(&mut self, evt: &Event) -> Result<()> {
        let json = serde_json::to_string(evt).with_context(|| {
            format!("serialize event seq={}", evt.seq)
        })?;
        writeln!(self.out, "{json}").context("write to stdout")?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.out.flush().context("flush stdout")
    }

    fn name(&self) -> &str {
        "stdout"
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use argus::events::{EventPayload, SequenceGenerator};
    use argus::events::process::Exit;

    use super::*;

    #[test]
    fn stdout_sink_name() {
        let sink = StdoutSink::new();
        assert_eq!(sink.name(), "stdout");
    }

    #[test]
    fn stdout_sink_serializes_event() {
        let seq = SequenceGenerator::default();
        let evt = Event::new(&seq, "test".into(), EventPayload::Exit(Exit {
            pid: 1,
            exit_code: 0,
            signal: None,
        }));
        let json = serde_json::to_string(&evt).unwrap();
        assert!(json.contains("\"agent_id\":\"test\""));
    }
}

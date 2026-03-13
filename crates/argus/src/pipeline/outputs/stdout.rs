// Rust guideline compliant 2026-02-21
//! Output implementation that writes enriched events as JSONL to stdout.
//!
//! Used when the supervisor runs in a terminal or CI context where an
//! external consumer reads its standard output. Each event is serialized
//! as a single JSON line followed by a newline; the buffer is flushed
//! after every event so lines are never interleaved with agent output.

use std::io::{self, BufWriter, Write};

use anyhow::{Context, Result};

use crate::events::Event;
use crate::pipeline::output::Output;

/// Writes each enriched event as one JSON line to buffered stdout.
///
/// When `flush_every_event` is true (the default), the buffer is flushed
/// after every `emit` call so consumers see events immediately. When false,
/// flushing is deferred to `BufWriter`'s natural batching or explicit
/// `flush()` calls — suitable for high-throughput pipelines piped to log
/// aggregators.
#[derive(Debug)]
pub struct StdoutOutput {
    out: BufWriter<io::Stdout>,
    flush_every_event: bool,
}

impl StdoutOutput {
    /// Creates an output backed by buffered stdout with per-event flushing.
    pub fn new() -> Self {
        Self {
            out: BufWriter::new(io::stdout()),
            flush_every_event: true,
        }
    }

    /// Creates an output with configurable flush behavior.
    pub fn with_flush_policy(flush_every_event: bool) -> Self {
        Self {
            out: BufWriter::new(io::stdout()),
            flush_every_event,
        }
    }
}

impl Default for StdoutOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl Output for StdoutOutput {
    fn emit(&mut self, event: &Event) -> Result<()> {
        let json = serde_json::to_string(event)
            .with_context(|| format!("serialize event seq={}", event.seq))?;
        writeln!(self.out, "{json}").context("write event line to stdout")?;
        if self.flush_every_event {
            self.out.flush().context("flush stdout after emit")?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.out.flush().context("flush stdout output")
    }

    fn name(&self) -> &str {
        "stdout"
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::events::control::AgentStart;
    use crate::events::envelope::{Event, EventPayload};
    use crate::pipeline::output::Output;

    use super::StdoutOutput;

    fn make_event() -> Event {
        Event {
            seq: 1,
            ts_monotonic: 0,
            ts_wall: 0,
            agent_id: "test".into(),
            vclock: None,
            redactions: Vec::new(),
            payload: EventPayload::AgentStart(AgentStart {
                agent_id: "test".to_owned(),
                supervisor_pid_host: None,
                supervisor_pid_ns: None,
                config_summary: "test".to_owned(),
                node: None,
                pod: None,
                container: None,
            }),
        }
    }

    #[test]
    fn name_is_stdout() {
        assert_eq!(StdoutOutput::new().name(), "stdout");
    }

    #[test]
    fn emit_does_not_panic() {
        // Emit writes to actual stdout; we just verify no panic/error.
        let mut out = StdoutOutput::new();
        let event = make_event();
        assert!(out.emit(&event).is_ok());
    }

    #[test]
    fn flush_does_not_error() {
        let mut out = StdoutOutput::new();
        assert!(out.flush().is_ok());
    }

    #[test]
    fn shutdown_delegates_to_flush() {
        let mut out = StdoutOutput::new();
        assert!(out.shutdown().is_ok());
    }
}

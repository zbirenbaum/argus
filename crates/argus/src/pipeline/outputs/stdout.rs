// Rust guideline compliant 2026-02-21
//! JSONL stdout output for the enriched event pipeline.
//!
//! Writes one JSON line per event to stdout. When `flush_every_event`
//! is set, each line is flushed immediately so that piped consumers
//! (e.g. validation test scripts) see events without buffering delay.

use std::io::{self, BufWriter, Write};

use anyhow::Result;

use crate::events::Event;
use crate::pipeline::output::Output;

/// Writes enriched events as JSONL to stdout.
#[derive(Debug)]
pub struct StdoutOutput {
    writer: BufWriter<io::Stdout>,
    flush_every_event: bool,
}

impl StdoutOutput {
    /// Creates a new stdout output.
    pub fn new(flush_every_event: bool) -> Self {
        Self {
            writer: BufWriter::new(io::stdout()),
            flush_every_event,
        }
    }
}

impl Output for StdoutOutput {
    fn emit(&mut self, event: &Event) -> Result<()> {
        serde_json::to_writer(&mut self.writer, event)?;
        self.writer.write_all(b"\n")?;
        if self.flush_every_event {
            self.writer.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }

    fn name(&self) -> &str {
        "stdout"
    }
}

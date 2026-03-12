// Rust guideline compliant 2026-02-21
//! Storage pipeline event sink.
//!
//! Wraps [`StoragePipeline`] behind the [`EventSink`] trait so events
//! flow to S3 through the same interface as stdout. Rotates the event
//! log segment on every flush so events stream to S3 continuously
//! rather than waiting for the 64 MiB segment threshold.

use std::any::Any;

use anyhow::{Context, Result};

use argus::events::Event;
use argus::storage::StoragePipeline;

use crate::event_sink::EventSink;

/// Forwards events to the S3 storage pipeline.
pub struct PipelineSink {
    pipeline: StoragePipeline,
}

impl PipelineSink {
    /// Wrap an existing pipeline.
    pub fn new(pipeline: StoragePipeline) -> Self {
        Self { pipeline }
    }

    /// Consume this sink and return the inner pipeline for shutdown.
    pub fn into_pipeline(self) -> StoragePipeline {
        self.pipeline
    }
}

impl EventSink for PipelineSink {
    fn write(&mut self, evt: &Event) -> Result<()> {
        self.pipeline.append_event(evt).with_context(|| {
            format!("pipeline append event seq={}", evt.seq)
        })
    }

    fn flush(&mut self) -> Result<()> {
        // Rotate the segment on flush so events upload immediately
        // instead of waiting for the size threshold.
        self.pipeline.rotate_now().context("pipeline rotate")?;
        self.pipeline
            .process_confirmations()
            .context("pipeline process confirmations")?;
        Ok(())
    }

    fn drain_confirmations(&mut self) -> Result<()> {
        self.pipeline
            .process_confirmations()
            .context("pipeline process confirmations")?;
        Ok(())
    }

    fn name(&self) -> &str {
        "s3-pipeline"
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

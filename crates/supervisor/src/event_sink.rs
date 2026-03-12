// Rust guideline compliant 2026-02-21
//! Pluggable event output destinations.
//!
//! [`EventSink`] abstracts where events go after the tracer produces
//! them. The event writer loop iterates over a list of sinks for each
//! event, decoupling output format and transport from the core loop.

use std::any::Any;

use anyhow::Result;

use argus::events::Event;

/// Receives events and writes them to an output destination.
pub trait EventSink: Send {
    /// Write a single event to this sink.
    fn write(&mut self, event: &Event) -> Result<()>;

    /// Flush any buffered data.
    fn flush(&mut self) -> Result<()>;

    /// Drain pending async confirmations (no-op by default).
    fn drain_confirmations(&mut self) -> Result<()> {
        Ok(())
    }

    /// Human-readable label for logging.
    fn name(&self) -> &str;

    /// Convert to `Any` for downcasting during shutdown.
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

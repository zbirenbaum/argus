// Rust guideline compliant 2026-02-21
//! Output trait — a composable consumer of enriched `Event` values.
//!
//! Outputs sit at the tail of the enriched pipeline, after redaction and
//! inline content stamping. Unlike `Sink`, which operates on raw `Record`
//! values, `Output` receives fully-formed `Event` structs ready for
//! external delivery (JSONL files, remote APIs, stdout, etc.).

use anyhow::Result;

use crate::events::Event;

/// A composable consumer of fully-enriched pipeline events.
///
/// Each `Output` implementation delivers events to one destination. The
/// `OutputList` fan-out wrapper calls every registered output and absorbs
/// per-output errors so that one failing destination does not block others.
///
/// Implementations must be `Send` — `OutputList` is owned by a single
/// thread (the ptrace loop) so no internal synchronization is required.
///
/// # Errors
///
/// `emit` and `flush` return `Err` on delivery failure. `OutputList`
/// logs those errors but continues delivering to remaining outputs.
pub trait Output: Send {
    /// Deliver one event to this output.
    ///
    /// # Errors
    ///
    /// Returns an error if the event cannot be delivered (e.g. I/O failure).
    fn emit(&mut self, event: &Event) -> Result<()>;

    /// Flush any internally buffered state to the destination.
    ///
    /// # Errors
    ///
    /// Returns an error if the flush fails.
    fn flush(&mut self) -> Result<()>;

    /// Graceful shutdown — flush then release resources.
    ///
    /// Default implementation delegates to `flush`. Override when teardown
    /// requires additional steps (e.g. closing a network connection or
    /// rotating a file).
    ///
    /// # Errors
    ///
    /// Returns an error if shutdown fails.
    fn shutdown(&mut self) -> Result<()> {
        self.flush()
    }

    /// Human-readable identifier for logging and metrics.
    fn name(&self) -> &str;
}

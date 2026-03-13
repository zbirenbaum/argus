// Rust guideline compliant 2026-02-21
//! Sink trait — a composable consumer of pipeline records.
//!
//! Sinks are separated by priority: blocking sinks are called inline and
//! must complete before the tracee is resumed; async sinks are best-effort
//! and may drop records under back-pressure.
//!
//! Each sink owns its mutable state directly. The `RecordBus` wraps every
//! sink in `Arc<Mutex<dyn Sink>>` so that a single bus can be cloned across
//! threads without requiring `Sync` on individual sink implementations.

use anyhow::Result;

use super::record::Record;

/// Controls whether a sink holds up the ptrace resume path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkPriority {
    /// Called synchronously before the tracee is resumed.
    ///
    /// Use this for the local event log and CAS store where durability
    /// is required before the agent can modify more state.
    Blocking,

    /// Called asynchronously; tracee is resumed regardless of completion.
    ///
    /// Use this for S3 uploads and index updates where eventual
    /// consistency is acceptable.
    Async,
}

/// A composable consumer of pipeline records.
///
/// Implementations own their mutable state directly — no internal `Mutex`
/// is required. The `RecordBus` provides the outer `Mutex<dyn Sink>` that
/// serializes concurrent access from multiple threads.
///
/// The bus calls `write` for every record the sink accepts, then `flush`
/// at checkpoint boundaries, and `shutdown` when the agent exits.
pub trait Sink: Send {
    /// Scheduling priority relative to tracee resumption.
    fn priority(&self) -> SinkPriority;

    /// Filter — return `false` to skip this record type entirely.
    ///
    /// Default accepts every record; override to filter by variant.
    fn accept(&self, _record: &Record) -> bool {
        true
    }

    /// Persist one record.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails. The bus logs errors but
    /// continues delivering to other sinks.
    fn write(&mut self, record: Record) -> Result<()>;

    /// Flush any in-memory or buffered state to durable storage.
    ///
    /// # Errors
    ///
    /// Returns an error if the flush fails.
    fn flush(&mut self) -> Result<()>;

    /// Graceful shutdown — flush then release resources.
    ///
    /// Default implementation calls `flush`. Override when teardown
    /// requires additional steps (e.g. closing network connections).
    ///
    /// # Errors
    ///
    /// Returns an error if shutdown fails.
    fn shutdown(&mut self) -> Result<()> {
        self.flush()
    }

    /// Human-readable name for logging and metrics.
    fn name(&self) -> &str;
}

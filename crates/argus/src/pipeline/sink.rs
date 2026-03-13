// Rust guideline compliant 2026-02-21
//! Sink trait — a composable consumer of pipeline records.
//!
//! Sinks are separated by priority: blocking sinks are called inline and
//! must complete before the tracee is resumed; async sinks are best-effort
//! and may drop records under back-pressure.
//!
//! Each sink manages its own interior mutability when needed. The `RecordBus`
//! wraps every sink in `Arc<dyn Sink>` so that a single bus can be cloned
//! across threads. Sinks that require mutable state use internal locking.

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
/// Implementations that require mutable state must manage their own interior
/// mutability (e.g., `Mutex<T>` inside the struct). The `RecordBus` stores
/// sinks as `Arc<dyn Sink>` and calls methods on shared references, so the
/// `Sync` bound must be satisfied.
///
/// The bus calls `write` for every record the sink accepts, then `flush`
/// at checkpoint boundaries, and `shutdown` when the agent exits.
pub trait Sink: Send + Sync {
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
    fn write(&self, record: Record) -> Result<()>;

    /// Flush any in-memory or buffered state to durable storage.
    ///
    /// # Errors
    ///
    /// Returns an error if the flush fails.
    fn flush(&self) -> Result<()>;

    /// Graceful shutdown — flush then release resources.
    ///
    /// Default implementation calls `flush`. Override when teardown
    /// requires additional steps (e.g. closing network connections).
    ///
    /// # Errors
    ///
    /// Returns an error if shutdown fails.
    fn shutdown(&self) -> Result<()> {
        self.flush()
    }

    /// Whether this sink must succeed before the tracee is resumed.
    ///
    /// When a required sink fails, the pipeline runner holds the tracee
    /// frozen and retries with backoff until the sink recovers.
    /// Default is `true` — override to `false` for best-effort sinks.
    fn required(&self) -> bool {
        true
    }

    /// Human-readable name for logging and metrics.
    fn name(&self) -> &str;
}

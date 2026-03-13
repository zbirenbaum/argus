// Rust guideline compliant 2026-02-21
//! Result type for bus emission indicating required-sink failures.

/// Outcome of delivering a record to all sinks.
///
/// The bus returns this from [`RecordBus::emit`] so callers can decide
/// whether to freeze the tracee and retry (pipeline runner) or log and
/// continue (non-ptrace paths).
#[derive(Debug)]
#[must_use]
pub enum EmitResult {
    /// All required sinks accepted the record.
    Ok,
    /// One or more required sinks failed.
    ///
    /// Contains `(sink_name, error)` pairs for every required sink that
    /// returned an error. Optional (non-required) sink failures are not
    /// included — they are logged by the bus and discarded.
    RequiredFailed(Vec<(String, anyhow::Error)>),
}

impl EmitResult {
    /// Returns `true` if all required sinks succeeded.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

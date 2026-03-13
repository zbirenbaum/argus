// Rust guideline compliant 2026-02-21
//! Sink stall status exposed to the API.

use std::time::Instant;

/// Describes a sink stall condition where required sinks are failing.
///
/// While a stall is active the ptrace runner withholds resume directives,
/// keeping the tracee frozen until all required sinks recover.
#[derive(Debug, Clone)]
pub struct StallState {
    /// Names of the failed required sinks.
    pub failed_sinks: Vec<String>,
    /// When the stall began.
    pub since: Instant,
    /// How many retry attempts have been made.
    pub retry_count: u32,
}

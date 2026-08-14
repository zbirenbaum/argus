// Rust guideline compliant 2026-02-21
//! Pipeline module: stream-based event capture architecture.
//!
//! The pipeline replaces the monolithic ptrace loop with composable
//! stages. A `PtraceStream` produces `RawSyscallStop` events; each
//! stage transforms them until they become `Event` records emitted
//! to the `RecordBus`.

pub(crate) mod emit_result;
pub(crate) mod overflow;
pub(crate) mod durability;
pub(crate) mod bus;
pub(crate) mod capture_policy;
pub(crate) mod context;
pub(crate) mod keylog_pipeline;
pub(crate) mod proxy_pipeline;
#[cfg(test)]
pub(crate) mod mock_ptrace;
#[cfg(test)]
mod validation_tests;
pub(crate) mod captured;
pub(crate) mod classified;
pub(crate) mod directive;
pub(crate) mod freeze;
pub(crate) mod ptrace_thread;
pub(crate) mod raw_stop;
pub(crate) mod tracee_registry;
pub(crate) mod record;
pub(crate) mod replay;
pub mod runner;
pub(crate) mod output;
pub(crate) mod outputs;
pub(crate) mod stall;
pub(crate) mod sink;
pub(crate) mod sinks;
pub(crate) mod stages;
pub(crate) mod streams;

// Only re-export what external crates actually use.
pub use runner::PipelineRunner;

pub(crate) use bus::RecordBus;
pub(crate) use emit_result::EmitResult;
pub(crate) use ptrace_thread::PtraceStream;
pub(crate) use tracee_registry::TraceeRegistry;
pub(crate) use record::Record;
pub(crate) use replay::RawStopRecorder;

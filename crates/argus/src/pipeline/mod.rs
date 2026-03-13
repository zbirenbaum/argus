// Rust guideline compliant 2026-02-21
//! Pipeline module: stream-based event capture architecture.
//!
//! The pipeline replaces the monolithic ptrace loop with composable
//! stages. A `PtraceStream` produces `RawSyscallStop` events; each
//! stage transforms them until they become `Event` records emitted
//! to the `RecordBus`.

pub(crate) mod bus;
pub(crate) mod capture_policy;
pub(crate) mod context;
#[cfg(test)]
pub(crate) mod mock_ptrace;
pub(crate) mod captured;
pub(crate) mod classified;
pub(crate) mod directive;
pub(crate) mod ptrace_thread;
pub(crate) mod raw_stop;
pub(crate) mod record;
pub(crate) mod replay;
pub mod runner;
pub(crate) mod sink;
pub(crate) mod sinks;
pub(crate) mod stages;

// Only re-export what external crates actually use.
pub use runner::PipelineRunner;

pub(crate) use bus::RecordBus;
pub(crate) use context::PipelineContext;
pub(crate) use capture_policy::{CaptureConfig, CaptureLevel, CapturePolicy, CaptureRule};
pub(crate) use captured::{CapturedContent, CapturedEvent};
pub(crate) use classified::{ClassifiedEvent, Classification, PipeDirection, PtyDataType, StdioType};
pub(crate) use directive::PipelineDirective;
pub(crate) use ptrace_thread::{PtraceHandle, PtraceStream};
pub(crate) use raw_stop::{RawSyscallStop, StopType, SyscallArgs};
pub(crate) use record::Record;
pub(crate) use replay::RawStopRecorder;
pub(crate) use sink::{Sink, SinkPriority};

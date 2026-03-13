// Rust guideline compliant 2026-02-21
//! Pipeline module: stream-based event capture architecture.
//!
//! The pipeline replaces the monolithic ptrace loop with composable
//! stages. A `PtraceStream` produces `RawSyscallStop` events; each
//! stage transforms them until they become `Event` records emitted
//! to the `RecordBus`.

pub mod bus;
pub mod capture_policy;
pub mod captured;
pub mod classified;
pub mod directive;
pub mod ptrace_thread;
pub mod raw_stop;
pub mod record;
pub mod replay;
pub mod sink;
pub mod stages;

pub use bus::RecordBus;
pub use capture_policy::{CaptureConfig, CaptureLevel, CapturePolicy, CaptureRule};
pub use captured::{CapturedContent, CapturedEvent};
pub use classified::{ClassifiedEvent, Classification, PipeDirection, PtyDataType, StdioType};
pub use directive::PipelineDirective;
pub use ptrace_thread::{PtraceHandle, PtraceStream};
pub use raw_stop::{RawSyscallStop, StopType, SyscallArgs};
pub use record::Record;
pub use sink::{Sink, SinkPriority};

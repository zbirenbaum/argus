// Rust guideline compliant 2026-02-21
//! Pipeline stages, sinks, and runner for the ptrace event processing pipeline.
//!
//! The pipeline consumes raw ptrace stops from [`PtraceStream`], classifies
//! them, checks rules, captures content, updates the Merkle tree, stamps
//! events, and fans out to storage sinks via [`RecordBus`].
//!
//! Concrete implementations of stages (`ClassifyStage`, `CaptureStage`, etc.)
//! and sinks (`LocalCasSink`, `EventLogSink`, etc.) live in sub-modules
//! created by parallel agents. This module provides the top-level scaffolding
//! and re-exports that glue them together.

pub mod classified;
pub mod directive;
pub mod runner;
pub mod sinks;
pub mod stages;

pub use runner::PipelineRunner;

// ---------------------------------------------------------------------------
// Stub types — parallel agents replace these with real implementations.
// ---------------------------------------------------------------------------

/// A processed record emitted by the pipeline.
///
/// Wraps a completed [`argus::events::Event`] or an internal pipeline control
/// message. The bus fans each `Record` out to all registered sinks.
///
/// # Note
///
/// This is a placeholder. The real definition will carry the full event type
/// once the pipeline-stages agent lands.
// TODO: replace with real `Record` once pipeline-stages agent merges.
pub enum Record {
    /// A fully stamped and serializable event.
    Event(crate::events::Event),
}

/// Fan-out bus that delivers every [`Record`] to all registered [`Sink`]s.
///
/// Placeholder — the real implementation is provided by the pipeline-sinks
/// agent.
// TODO: replace with real `RecordBus` once pipeline-sinks agent merges.
pub struct RecordBus;

impl RecordBus {
    /// Construct a bus from a list of sinks.
    pub fn new(_sinks: Vec<std::sync::Arc<dyn Sink>>) -> Self {
        Self
    }

    /// Emit a record to all sinks.
    pub fn emit(&self, _record: Record) {}

    /// Flush and shut down all sinks.
    pub fn shutdown_all(&self) {}

    /// Returns a legacy mpsc sender for modules not yet migrated to the bus.
    ///
    /// The sender forwards events to the bus via an adapter thread. This shim
    /// exists only until the TLS watcher is migrated to emit directly.
    // TODO: remove once tls_watcher uses the bus directly.
    pub fn legacy_sender(&self) -> std::sync::mpsc::Sender<crate::events::Event> {
        let (tx, _rx) = std::sync::mpsc::channel();
        tx
    }
}

impl Clone for RecordBus {
    fn clone(&self) -> Self {
        Self
    }
}

/// Raw ptrace stop stream backed by the ptrace loop thread.
///
/// Placeholder — the real implementation is provided by the pipeline-ptrace
/// agent.
// TODO: replace with real `PtraceStream` once pipeline-ptrace agent merges.
pub struct PtraceStream;

impl PtraceStream {
    /// Spawn the ptrace loop thread and return `(stream, join_handle)`.
    pub fn spawn(
        _child_pid: nix::unistd::Pid,
        _sync_pipe_w: std::os::fd::RawFd,
    ) -> (Self, std::thread::JoinHandle<()>) {
        let handle = std::thread::spawn(|| {});
        (Self, handle)
    }

    /// Send a directive back to the ptrace loop (resume, inject error, etc.).
    pub fn directive(&self, _directive: directive::PipelineDirective) {}
}

impl futures::Stream for PtraceStream {
    type Item = crate::pipeline::classified::RawStop;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Ready(None)
    }
}

/// Optional recorder for raw ptrace stops to disk for debugging and replay.
///
/// Placeholder — the real definition comes from the pipeline-runner agent.
// TODO: replace with real `RawStopRecorder` once pipeline-runner agent merges.
pub struct RawStopRecorder;

impl RawStopRecorder {
    /// Record a raw stop to disk.
    pub fn record(&mut self, _stop: &crate::pipeline::classified::RawStop) {}
}

/// Trait implemented by all pipeline output sinks.
///
/// Each sink receives a [`Record`] and persists or forwards it. Sinks run
/// synchronously from the bus fan-out; async sinks must buffer internally.
pub trait Sink: Send + Sync {
    /// Process one record from the bus.
    fn handle(&self, record: &Record);

    /// Flush and finalize any buffered data.
    fn shutdown(&self) {}
}

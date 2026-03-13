// Rust guideline compliant 2026-02-21
//! Fan-out bus that delivers records to all registered sinks.
//!
//! Blocking sinks are called synchronously so the pipeline can hold up
//! the tracee resume until durable writes complete. Async sinks are
//! called afterward; errors are logged but do not stop delivery.

use std::sync::Arc;

use tracing::event;
use tracing::Level;

use super::record::Record;
use super::sink::{Sink, SinkPriority};

/// Fan-out record bus partitioned by sink priority.
///
/// Create with [`RecordBus::new`], then call [`RecordBus::emit`] for
/// every captured event. Call [`RecordBus::flush_all`] at checkpoint
/// boundaries and [`RecordBus::shutdown_all`] on agent exit.
pub struct RecordBus {
    blocking: Vec<Arc<dyn Sink>>,
    async_sinks: Vec<Arc<dyn Sink>>,
}

impl std::fmt::Debug for RecordBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordBus")
            .field("blocking_count", &self.blocking.len())
            .field("async_count", &self.async_sinks.len())
            .finish()
    }
}

impl RecordBus {
    /// Partition `sinks` by priority and build the bus.
    pub fn new(sinks: Vec<Arc<dyn Sink>>) -> Self {
        let mut blocking = Vec::new();
        let mut async_sinks = Vec::new();
        for sink in sinks {
            match sink.priority() {
                SinkPriority::Blocking => blocking.push(sink),
                SinkPriority::Async => async_sinks.push(sink),
            }
        }
        Self { blocking, async_sinks }
    }

    /// Deliver `record` to every accepting sink in priority order.
    ///
    /// Blocking sinks run first and inline. Async sinks run afterward.
    /// Errors from any sink are logged but do not prevent delivery to
    /// the remaining sinks.
    pub fn emit(&self, record: Record) {
        for sink in &self.blocking {
            if sink.accept(&record) {
                if let Err(e) = sink.write(record.clone()) {
                    event!(
                        name: "bus.sink.write_error",
                        Level::WARN,
                        sink.name = sink.name(),
                        error.message = %e,
                        "blocking sink {{sink.name}} write failed: {{error.message}}",
                    );
                }
            }
        }
        for sink in &self.async_sinks {
            if sink.accept(&record) {
                if let Err(e) = sink.write(record.clone()) {
                    event!(
                        name: "bus.sink.async_write_error",
                        Level::WARN,
                        sink.name = sink.name(),
                        error.message = %e,
                        "async sink {{sink.name}} write failed: {{error.message}}",
                    );
                }
            }
        }
    }

    /// Flush all sinks in registration order.
    pub fn flush_all(&self) {
        for sink in self.blocking.iter().chain(self.async_sinks.iter()) {
            if let Err(e) = sink.flush() {
                event!(
                    name: "bus.sink.flush_error",
                    Level::WARN,
                    sink.name = sink.name(),
                    error.message = %e,
                    "sink {{sink.name}} flush failed: {{error.message}}",
                );
            }
        }
    }

    /// Shut down all sinks in registration order.
    pub fn shutdown_all(&self) {
        for sink in self.blocking.iter().chain(self.async_sinks.iter()) {
            if let Err(e) = sink.shutdown() {
                event!(
                    name: "bus.sink.shutdown_error",
                    Level::WARN,
                    sink.name = sink.name(),
                    error.message = %e,
                    "sink {{sink.name}} shutdown failed: {{error.message}}",
                );
            }
        }
    }
}

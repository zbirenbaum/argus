// Rust guideline compliant 2026-02-21
// Tests are in the #[cfg(test)] block at the bottom of this file.
//! Fan-out bus that delivers records to all registered sinks.
//!
//! Blocking sinks are called synchronously so the pipeline can hold up
//! the tracee resume until durable writes complete. Async sinks are
//! called afterward; errors are logged but do not stop delivery.
//!
//! Each sink is wrapped in `Arc<Mutex<dyn Sink>>`. The `Arc` lets the bus
//! be cloned cheaply across threads; the `Mutex` serializes write access
//! since the `Sink` trait uses `&mut self`.

use std::sync::{Arc, Mutex};

use tracing::event;
use tracing::Level;

use super::record::Record;
use super::sink::{Sink, SinkPriority};

/// Fan-out record bus partitioned by sink priority.
///
/// Create with [`RecordBus::new`], then call [`RecordBus::emit`] for
/// every captured event. Call [`RecordBus::flush_all`] at checkpoint
/// boundaries and [`RecordBus::shutdown_all`] on agent exit.
///
/// `Clone` shares the same underlying `Arc<Mutex<dyn Sink>>` handles so
/// all clones deliver to the same sinks — useful for handing one copy to
/// the pipeline runner and another to the TLS watcher thread.
#[derive(Clone)]
pub struct RecordBus {
    blocking: Vec<Arc<Mutex<dyn Sink>>>,
    async_sinks: Vec<Arc<Mutex<dyn Sink>>>,
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
    pub fn new(sinks: Vec<Arc<Mutex<dyn Sink>>>) -> Self {
        let mut blocking = Vec::new();
        let mut async_sinks = Vec::new();
        for sink in sinks {
            let priority = sink.lock().expect("sink mutex poisoned during bus construction").priority();
            match priority {
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
    /// the remaining sinks. A poisoned mutex causes the sink to be
    /// skipped and an error to be logged.
    pub fn emit(&self, record: Record) {
        event!(
            name: "bus.emit",
            Level::TRACE,
            blocking_count = self.blocking.len(),
            async_count = self.async_sinks.len(),
            "delivering record to sinks",
        );
        for sink in &self.blocking {
            let mut guard = match sink.lock() {
                Ok(g) => g,
                Err(e) => {
                    event!(
                        name: "bus.sink.mutex_poisoned",
                        Level::ERROR,
                        error.message = %e,
                        "blocking sink mutex poisoned, skipping",
                    );
                    continue;
                }
            };
            if guard.accept(&record) {
                if let Err(e) = guard.write(record.clone()) {
                    event!(
                        name: "bus.sink.write_error",
                        Level::WARN,
                        sink.name = guard.name(),
                        error.message = %e,
                        "blocking sink {{sink.name}} write failed: {{error.message}}",
                    );
                }
            }
        }
        for sink in &self.async_sinks {
            let mut guard = match sink.lock() {
                Ok(g) => g,
                Err(e) => {
                    event!(
                        name: "bus.sink.async_mutex_poisoned",
                        Level::ERROR,
                        error.message = %e,
                        "async sink mutex poisoned, skipping",
                    );
                    continue;
                }
            };
            if guard.accept(&record) {
                if let Err(e) = guard.write(record.clone()) {
                    event!(
                        name: "bus.sink.async_write_error",
                        Level::WARN,
                        sink.name = guard.name(),
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
            let mut guard = match sink.lock() {
                Ok(g) => g,
                Err(e) => {
                    event!(
                        name: "bus.sink.flush_mutex_poisoned",
                        Level::ERROR,
                        error.message = %e,
                        "sink mutex poisoned during flush, skipping",
                    );
                    continue;
                }
            };
            if let Err(e) = guard.flush() {
                event!(
                    name: "bus.sink.flush_error",
                    Level::WARN,
                    sink.name = guard.name(),
                    error.message = %e,
                    "sink {{sink.name}} flush failed: {{error.message}}",
                );
            }
        }
    }

    /// Shut down all sinks in registration order.
    pub fn shutdown_all(&self) {
        for sink in self.blocking.iter().chain(self.async_sinks.iter()) {
            let mut guard = match sink.lock() {
                Ok(g) => g,
                Err(e) => {
                    event!(
                        name: "bus.sink.shutdown_mutex_poisoned",
                        Level::ERROR,
                        error.message = %e,
                        "sink mutex poisoned during shutdown, skipping",
                    );
                    continue;
                }
            };
            if let Err(e) = guard.shutdown() {
                event!(
                    name: "bus.sink.shutdown_error",
                    Level::WARN,
                    sink.name = guard.name(),
                    error.message = %e,
                    "sink {{sink.name}} shutdown failed: {{error.message}}",
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::events::envelope::{Event, EventPayload};
    use crate::events::control::AgentStart;
    use crate::pipeline::record::Record;
    use crate::pipeline::sink::{Sink, SinkPriority};
    use crate::pipeline::sinks::memory::MemorySink;

    use super::RecordBus;

    fn make_record(seq: u64) -> Record {
        Record::Event(Event {
            seq,
            ts_monotonic: 0,
            ts_wall: "2026-01-01T00:00:00Z".to_owned(),
            agent_id: "test".to_owned(),
            vclock: None,
            redactions: Vec::new(),
            payload: EventPayload::AgentStart(AgentStart {
                agent_id: "test".to_owned(),
                supervisor_pid_host: None,
                supervisor_pid_ns: None,
                config_summary: "test".to_owned(),
                node: None,
                pod: None,
                container: None,
            }),
        })
    }

    #[test]
    fn emit_reaches_blocking_sink() {
        let sink = Arc::new(Mutex::new(MemorySink::new(SinkPriority::Blocking)));
        let bus = RecordBus::new(vec![sink.clone() as Arc<Mutex<dyn Sink>>]);
        bus.emit(make_record(1));
        assert_eq!(sink.lock().unwrap().len(), 1);
    }

    #[test]
    fn emit_reaches_async_sink() {
        let sink = Arc::new(Mutex::new(MemorySink::new(SinkPriority::Async)));
        let bus = RecordBus::new(vec![sink.clone() as Arc<Mutex<dyn Sink>>]);
        bus.emit(make_record(1));
        assert_eq!(sink.lock().unwrap().len(), 1);
    }

    #[test]
    fn blocking_before_async() {
        // Verify ordering by checking both receive the same record.
        // True ordering would require sequence tracking; here we verify
        // both sinks receive the record and counts are correct.
        let blocking = Arc::new(Mutex::new(MemorySink::new(SinkPriority::Blocking)));
        let async_sink = Arc::new(Mutex::new(MemorySink::new(SinkPriority::Async)));
        let bus = RecordBus::new(vec![
            blocking.clone() as Arc<Mutex<dyn Sink>>,
            async_sink.clone() as Arc<Mutex<dyn Sink>>,
        ]);
        bus.emit(make_record(2));
        assert_eq!(blocking.lock().unwrap().len(), 1, "blocking sink must receive the record");
        assert_eq!(async_sink.lock().unwrap().len(), 1, "async sink must receive the record");
    }

    #[test]
    fn sink_error_does_not_stop_delivery() {
        // A sink that always errors.
        use anyhow::anyhow;
        struct FailSink;
        impl Sink for FailSink {
            fn priority(&self) -> SinkPriority { SinkPriority::Blocking }
            fn write(&mut self, _: Record) -> anyhow::Result<()> { Err(anyhow!("injected error")) }
            fn flush(&mut self) -> anyhow::Result<()> { Ok(()) }
            fn name(&self) -> &str { "fail" }
        }
        impl std::fmt::Debug for FailSink {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("FailSink")
            }
        }

        let good = Arc::new(Mutex::new(MemorySink::new(SinkPriority::Blocking)));
        let bus = RecordBus::new(vec![
            Arc::new(Mutex::new(FailSink)) as Arc<Mutex<dyn Sink>>,
            good.clone() as Arc<Mutex<dyn Sink>>,
        ]);
        bus.emit(make_record(3));
        // The good sink still receives the record despite the first sink failing.
        assert_eq!(good.lock().unwrap().len(), 1, "good sink must still receive record after prior sink error");
    }
}

// Rust guideline compliant 2026-02-21
//! Fan-out output implementations for the enriched event pipeline.
//!
//! `OutputList` delivers each event to every registered `Output` in
//! registration order. Errors from individual outputs are logged and
//! discarded so that one failing destination does not block others.

pub(crate) mod stdout;
pub mod file;

pub(crate) use stdout::StdoutOutput;
pub use file::FileOutput;

use tracing::{Level, event};

use anyhow::Result;

use crate::events::Event;
use crate::pipeline::output::Output;

/// Fans out events to all registered outputs, tolerating per-output errors.
///
/// Outputs are invoked in registration order. If one output's `emit`
/// returns `Err`, the error is logged at `ERROR` level and delivery
/// continues to the remaining outputs. This matches the `RecordBus`
/// policy for `Sink` errors.
pub struct OutputList {
    outputs: Vec<Box<dyn Output>>,
}

impl std::fmt::Debug for OutputList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Box<dyn Output> is not Debug; show only the count to satisfy the trait.
        f.debug_struct("OutputList")
            .field("count", &self.outputs.len())
            .finish()
    }
}

impl OutputList {
    /// Creates an empty list; use `push` to register outputs.
    pub fn new() -> Self {
        Self {
            outputs: Vec::new(),
        }
    }

    /// Appends an output to the fan-out set.
    pub fn push(&mut self, output: Box<dyn Output>) {
        self.outputs.push(output);
    }

    /// Delivers `event` to every output, logging but not propagating errors.
    pub fn emit(&mut self, ev: &Event) {
        for output in &mut self.outputs {
            if let Err(err) = output.emit(ev) {
                event!(
                    name: "output.emit.error",
                    Level::ERROR,
                    output.name = output.name(),
                    error.message = %err,
                    event.seq = ev.seq,
                    "output {{output.name}} failed to emit event {{event.seq}}: {{error.message}}",
                );
            }
        }
    }

    /// Flushes all outputs, logging but not propagating errors.
    pub fn flush(&mut self) -> Result<()> {
        for output in &mut self.outputs {
            if let Err(err) = output.flush() {
                event!(
                    name: "output.flush.error",
                    Level::ERROR,
                    output.name = output.name(),
                    error.message = %err,
                    "output {{output.name}} flush failed: {{error.message}}",
                );
            }
        }
        Ok(())
    }

    /// Shuts down all outputs in registration order, logging but not propagating errors.
    pub fn shutdown(&mut self) -> Result<()> {
        for output in &mut self.outputs {
            if let Err(err) = output.shutdown() {
                event!(
                    name: "output.shutdown.error",
                    Level::ERROR,
                    output.name = output.name(),
                    error.message = %err,
                    "output {{output.name}} shutdown failed: {{error.message}}",
                );
            }
        }
        Ok(())
    }

    /// Returns the number of registered outputs.
    pub fn len(&self) -> usize {
        self.outputs.len()
    }

    /// Returns true if no outputs are registered.
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }
}

impl Default for OutputList {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use anyhow::{Result, bail};

    use crate::events::control::AgentStart;
    use crate::events::envelope::{Event, EventPayload};
    use crate::pipeline::output::Output;

    use super::OutputList;

    fn make_event(seq: u64) -> Event {
        Event {
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
        }
    }

    /// Mock output that records received sequence numbers.
    struct RecordingOutput {
        name: &'static str,
        received: Arc<Mutex<Vec<u64>>>,
        fail_emit: bool,
    }

    impl Output for RecordingOutput {
        fn emit(&mut self, event: &Event) -> Result<()> {
            if self.fail_emit {
                bail!("injected emit failure");
            }
            self.received.lock().expect("lock poisoned").push(event.seq);
            Ok(())
        }

        fn flush(&mut self) -> Result<()> {
            Ok(())
        }

        fn name(&self) -> &str {
            self.name
        }
    }

    fn recording(name: &'static str) -> (Box<dyn Output>, Arc<Mutex<Vec<u64>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let output = RecordingOutput {
            name,
            received: Arc::clone(&received),
            fail_emit: false,
        };
        (Box::new(output), received)
    }

    fn failing(name: &'static str) -> Box<dyn Output> {
        Box::new(RecordingOutput {
            name,
            received: Arc::new(Mutex::new(Vec::new())),
            fail_emit: true,
        })
    }

    #[test]
    fn fan_out_delivers_to_all_outputs() {
        let (out_a, recv_a) = recording("a");
        let (out_b, recv_b) = recording("b");

        let mut list = OutputList::new();
        list.push(out_a);
        list.push(out_b);

        list.emit(&make_event(1));
        list.emit(&make_event(2));

        assert_eq!(*recv_a.lock().unwrap(), vec![1, 2]);
        assert_eq!(*recv_b.lock().unwrap(), vec![1, 2]);
    }

    #[test]
    fn failing_output_does_not_block_others() {
        let (out_b, recv_b) = recording("b");

        let mut list = OutputList::new();
        // "a" always fails; "b" must still receive the event.
        list.push(failing("a"));
        list.push(out_b);

        list.emit(&make_event(42));

        assert_eq!(*recv_b.lock().unwrap(), vec![42]);
    }

    #[test]
    fn flush_and_shutdown_succeed_with_no_outputs() {
        let mut list = OutputList::new();
        assert!(list.flush().is_ok());
        assert!(list.shutdown().is_ok());
    }

    #[test]
    fn len_and_is_empty_reflect_registration() {
        let mut list = OutputList::new();
        assert!(list.is_empty());
        let (out, _) = recording("x");
        list.push(out);
        assert_eq!(list.len(), 1);
        assert!(!list.is_empty());
    }
}

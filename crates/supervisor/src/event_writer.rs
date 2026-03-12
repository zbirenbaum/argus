// Rust guideline compliant 2026-02-21
//! Event writer thread: drains the event channel through pluggable sinks.
//!
//! Each event is dispatched to every registered [`EventSink`]. Sink
//! errors are logged but do not block other sinks or stop the writer.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use argus::events::Event;
use tracing::{Level, event};

use crate::event_sink::EventSink;

/// Maximum time between flushes. Bounds worst-case latency for
/// consumers reading the event stream (tests, streaming pipes).
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// Spawns the event writer thread.
///
/// Returns the sinks on join so callers can finalize them (e.g.
/// shutdown a storage pipeline).
pub fn spawn(
    rx: Receiver<Event>,
    sinks: Vec<Box<dyn EventSink>>,
) -> JoinHandle<Vec<Box<dyn EventSink>>> {
    thread::Builder::new()
        .name("event-writer".into())
        .spawn(move || write_loop(rx, sinks))
        .expect("failed to spawn event writer thread")
}

/// Core loop: receive events and fan out to all sinks.
fn write_loop(
    rx: Receiver<Event>,
    mut sinks: Vec<Box<dyn EventSink>>,
) -> Vec<Box<dyn EventSink>> {
    let mut count: u64 = 0;
    let mut dirty = false;

    loop {
        let recv = if dirty {
            match rx.recv_timeout(FLUSH_INTERVAL) {
                Ok(evt) => Some(evt),
                Err(RecvTimeoutError::Timeout) => {
                    flush_all(&mut sinks);
                    dirty = false;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => None,
            }
        } else {
            rx.recv().ok()
        };

        let Some(evt) = recv else { break };

        write_to_all(&mut sinks, &evt);
        count += 1;
        dirty = true;

        // Drain any queued events without blocking.
        while let Ok(evt) = rx.try_recv() {
            write_to_all(&mut sinks, &evt);
            count += 1;
        }

        drain_confirmations_all(&mut sinks);
    }

    flush_all(&mut sinks);

    event!(
        name: "event_writer.done",
        Level::INFO,
        events.count = count,
        "event writer finished, wrote {{events.count}} events",
    );

    sinks
}

/// Dispatch one event to every sink, logging failures.
fn write_to_all(sinks: &mut [Box<dyn EventSink>], evt: &Event) {
    for sink in sinks.iter_mut() {
        if let Err(e) = sink.write(evt) {
            event!(
                name: "event_writer.sink_error",
                Level::WARN,
                sink.name = sink.name(),
                error.message = %e,
                "sink {{sink.name}} write failed: {{error.message}}",
            );
        }
    }
}

/// Flush every sink, logging failures.
fn flush_all(sinks: &mut [Box<dyn EventSink>]) {
    for sink in sinks.iter_mut() {
        if let Err(e) = sink.flush() {
            event!(
                name: "event_writer.flush_error",
                Level::WARN,
                sink.name = sink.name(),
                error.message = %e,
                "sink {{sink.name}} flush failed: {{error.message}}",
            );
        }
    }
}

/// Let sinks that do async work drain confirmations.
fn drain_confirmations_all(sinks: &mut [Box<dyn EventSink>]) {
    for sink in sinks.iter_mut() {
        if let Err(e) = sink.drain_confirmations() {
            event!(
                name: "event_writer.confirmations_error",
                Level::WARN,
                sink.name = sink.name(),
                error.message = %e,
                "sink {{sink.name}} drain_confirmations failed: {{error.message}}",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use argus::events::{EventPayload, SequenceGenerator};
    use argus::events::process::Exit;

    use super::*;
    use crate::stdout_sink::StdoutSink;

    #[test]
    fn writer_processes_events_and_exits() {
        let (tx, rx) = mpsc::channel();
        let seq = SequenceGenerator::default();

        let evt = Event::new(&seq, "agent-w".into(), EventPayload::Exit(Exit {
            pid: 1,
            exit_code: 0,
            signal: None,
        }));

        tx.send(evt).unwrap();
        drop(tx);

        let sinks: Vec<Box<dyn EventSink>> = vec![Box::new(StdoutSink::new())];
        let handle = spawn(rx, sinks);
        handle.join().expect("writer thread should not panic");
    }

    #[test]
    fn writer_handles_empty_channel() {
        let (tx, rx) = mpsc::channel::<Event>();
        drop(tx);

        let sinks: Vec<Box<dyn EventSink>> = vec![Box::new(StdoutSink::new())];
        let handle = spawn(rx, sinks);
        handle.join().expect("writer thread should not panic on empty channel");
    }

    #[test]
    fn writer_works_with_no_sinks() {
        let (tx, rx) = mpsc::channel::<Event>();
        drop(tx);

        let handle = spawn(rx, vec![]);
        handle.join().expect("writer thread should not panic with no sinks");
    }
}

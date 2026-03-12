// Rust guideline compliant 2026-02-21
//! Event writer thread for JSON lines output.
//!
//! Drains the event channel and writes each event as a single JSON line
//! to stdout. Runs on a dedicated thread so the ptrace loop is never
//! blocked by I/O.

use std::io::{self, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use argus::events::Event;
use tracing::{Level, event};

/// Maximum time between flushes. Bounds worst-case latency for
/// consumers reading the event stream (tests, streaming pipes).
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// Spawns a thread that writes events as JSON lines to stdout.
///
/// The thread runs until the channel sender is dropped (i.e., the
/// tracer loop exits and all senders are cleaned up).
pub fn spawn(rx: Receiver<Event>) -> JoinHandle<()> {
    thread::Builder::new()
        .name("event-writer".into())
        .spawn(move || write_loop(rx))
        .expect("failed to spawn event writer thread")
}

/// Reads events from the channel and writes JSON lines to stdout.
///
/// Uses `recv_timeout` to batch events within a flush window. When
/// the timer fires or the channel empties, the buffer is flushed.
/// This gives O(1) syscalls per batch instead of per event while
/// keeping worst-case read latency at `FLUSH_INTERVAL`.
fn write_loop(rx: Receiver<Event>) {
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let mut count: u64 = 0;
    let mut dirty = false;

    loop {
        let recv = if dirty {
            // Already have buffered writes — wait up to the flush
            // interval for more events before flushing.
            match rx.recv_timeout(FLUSH_INTERVAL) {
                Ok(evt) => Some(evt),
                Err(RecvTimeoutError::Timeout) => {
                    let _ = out.flush();
                    dirty = false;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => None,
            }
        } else {
            // Buffer is clean — block until the next event arrives.
            rx.recv().ok()
        };

        let Some(evt) = recv else { break };

        if write_event(&mut out, &evt) {
            count += 1;
            dirty = true;
        }

        // Drain any queued events without blocking.
        while let Ok(evt) = rx.try_recv() {
            if write_event(&mut out, &evt) {
                count += 1;
            }
        }
    }

    if let Err(e) = out.flush() {
        event!(
            name: "event_writer.flush_error",
            Level::WARN,
            error.message = %e,
            "failed to flush event writer: {{error.message}}",
        );
    }

    event!(
        name: "event_writer.done",
        Level::INFO,
        events.count = count,
        "event writer finished, wrote {{events.count}} events",
    );
}

/// Serializes and writes a single event. Returns `true` on success.
fn write_event(out: &mut impl Write, evt: &Event) -> bool {
    match serde_json::to_string(evt) {
        Ok(json) => {
            if writeln!(out, "{json}").is_err() {
                event!(
                    name: "event_writer.write_error",
                    Level::ERROR,
                    "failed to write event to stdout, stopping writer",
                );
                return false;
            }
            true
        }
        Err(e) => {
            event!(
                name: "event_writer.serialize_error",
                Level::ERROR,
                error.message = %e,
                event.seq = evt.seq,
                "failed to serialize event seq={{event.seq}}: {{error.message}}",
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use argus::events::{EventPayload, SequenceGenerator};
    use argus::events::process::Exit;

    use super::*;

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

        let handle = spawn(rx);
        handle.join().expect("writer thread should not panic");
    }

    #[test]
    fn writer_handles_empty_channel() {
        let (tx, rx) = mpsc::channel::<Event>();
        drop(tx);

        let handle = spawn(rx);
        handle.join().expect("writer thread should not panic on empty channel");
    }
}

// Rust guideline compliant 2026-02-21
//! Event writer thread for JSON lines output.
//!
//! Drains the event channel and writes each event as a single JSON line
//! to stdout. Runs on a dedicated thread so the ptrace loop is never
//! blocked by I/O.

use std::io::{self, Write};
use std::sync::mpsc::Receiver;
use std::thread::{self, JoinHandle};

use sandbox::events::Event;
use tracing::{Level, event};

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
fn write_loop(rx: Receiver<Event>) {
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let mut count: u64 = 0;

    for evt in rx {
        match serde_json::to_string(&evt) {
            Ok(json) => {
                // A partial write here is acceptable; the next event
                // will still be on its own line.
                if writeln!(out, "{json}").is_err() {
                    event!(
                        name: "event_writer.write_error",
                        Level::ERROR,
                        "failed to write event to stdout, stopping writer",
                    );
                    return;
                }
                count += 1;
            }
            Err(e) => {
                event!(
                    name: "event_writer.serialize_error",
                    Level::ERROR,
                    error.message = %e,
                    event.seq = evt.seq,
                    "failed to serialize event seq={{event.seq}}: {{error.message}}",
                );
            }
        }
    }

    // Flush remaining buffered output before the thread exits.
    let _ = out.flush();

    event!(
        name: "event_writer.done",
        Level::INFO,
        events.count = count,
        "event writer finished, wrote {{events.count}} events",
    );
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use sandbox::events::{EventPayload, SequenceGenerator};
    use sandbox::events::process::Exit;

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

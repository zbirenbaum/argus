// Rust guideline compliant 2026-02-21
//! Blocking sink that appends events to the JSONL segment log.

use std::sync::Mutex;

use anyhow::Result;

use crate::events::Event;
use crate::pipeline::record::Record;
use crate::pipeline::sink::{Sink, SinkPriority};
use crate::storage::event_log::EventLog;

/// Blocking sink that appends every event to the JSONL event log.
///
/// Non-event records are silently ignored. The underlying `EventLog`
/// requires mutable access per write, so it is wrapped in a `Mutex`.
/// The sink does not hold an `UploadPool` reference — callers that
/// want segment rotation to trigger uploads should configure the
/// `EventLog` with one before handing it to this sink, or drive
/// rotation externally via finalize/reopen.
#[derive(Debug)]
pub struct EventLogSink {
    log: Mutex<EventLog>,
}

impl EventLogSink {
    /// Creates a sink that writes to `log`.
    pub fn new(log: EventLog) -> Self {
        Self {
            log: Mutex::new(log),
        }
    }

    /// Grants temporary access to the inner log for callers that need
    /// to trigger segment rotation or finalization.
    ///
    /// # Panics
    ///
    /// Panics if the mutex is poisoned (a previous write panicked).
    pub fn with_log<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&mut EventLog) -> T,
    {
        f(&mut self.log.lock().expect("event log mutex poisoned"))
    }
}

impl Sink for EventLogSink {
    fn priority(&self) -> SinkPriority {
        SinkPriority::Blocking
    }

    fn accept(&self, record: &Record) -> bool {
        matches!(record, Record::Event(_))
    }

    fn write(&self, record: Record) -> Result<()> {
        let Record::Event(event) = record else {
            return Ok(());
        };
        // No upload pool here — segment upload is driven by the storage layer
        // separately to keep the sink interface synchronous.
        self.log
            .lock()
            .expect("event log mutex poisoned")
            .append(&event, None)?;
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        self.log
            .lock()
            .expect("event log mutex poisoned")
            .flush()
            .map_err(Into::into)
    }

    fn name(&self) -> &str {
        "event-log"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DurabilityMode;
    use crate::pipeline::record::Record;

    fn make_sink() -> (tempfile::TempDir, EventLogSink) {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = EventLog::new(
            "test-agent".to_owned(),
            dir.path().join("events"),
            DurabilityMode::Memory,
        )
        .expect("EventLog::new");
        (dir, EventLogSink::new(log))
    }

    fn make_event() -> Event {
        use crate::events::{EventPayload, control::AgentStart};
        Event {
            seq: 1,
            ts_monotonic: 0,
            ts_wall: "2026-01-01T00:00:00Z".to_owned(),
            agent_id: "test-agent".to_owned(),
            vclock: None,
            payload: EventPayload::AgentStart(AgentStart {
                agent_id: "test-agent".to_owned(),
                supervisor_pid_host: None,
                supervisor_pid_ns: None,
                config_summary: "test".to_owned(),
                node: None,
                pod: None,
                container: None,
            }),
        }
    }

    #[test]
    fn accept_allows_events() {
        let (_dir, sink) = make_sink();
        let event = make_event();
        assert!(sink.accept(&Record::Event(event)));
    }

    #[test]
    fn accept_rejects_content() {
        let (_dir, sink) = make_sink();
        let hash = crate::cas::ContentHash::from_data(b"x");
        assert!(!sink.accept(&Record::Content { hash, data: vec![] }));
    }

    #[test]
    fn write_event_succeeds() {
        let (_dir, sink) = make_sink();
        sink.write(Record::Event(make_event())).expect("write");
        sink.flush().expect("flush");
    }

    #[test]
    fn write_non_event_is_noop() {
        let (_dir, sink) = make_sink();
        let hash = crate::cas::ContentHash::from_data(b"x");
        sink.write(Record::Content { hash, data: vec![1, 2, 3] })
            .expect("write non-event noop");
    }

    #[test]
    fn name_is_event_log() {
        let (_dir, sink) = make_sink();
        assert_eq!(sink.name(), "event-log");
    }
}

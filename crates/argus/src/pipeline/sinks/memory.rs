// Rust guideline compliant 2026-02-21
//! In-memory sink for testing pipeline wiring.

use std::sync::Mutex;

use anyhow::Result;

use crate::events::Event;
use crate::pipeline::record::Record;
use crate::pipeline::sink::{Sink, SinkPriority};

/// In-memory sink that accumulates all records for test assertions.
///
/// All record variants are accepted regardless of priority. Use `drain`
/// to remove and return all accumulated records, or `events` to extract
/// only the event records.
///
/// The record buffer is protected by an internal `Mutex` so the sink
/// satisfies `&self` on the `Sink` trait while still accumulating state.
/// In tests that use the sink directly (without a bus), no locking ceremony
/// is required since `drain`, `events`, and `len` acquire the lock
/// internally.
#[derive(Debug)]
pub struct MemorySink {
    records: Mutex<Vec<Record>>,
    priority: SinkPriority,
}

impl MemorySink {
    /// Creates a sink with the given priority.
    ///
    /// The priority only affects how a `RecordBus` routes the sink;
    /// it does not change accumulation behavior.
    pub fn new(priority: SinkPriority) -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            priority,
        }
    }

    /// Removes and returns all accumulated records.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn drain(&self) -> Vec<Record> {
        std::mem::take(&mut self.records.lock().expect("MemorySink mutex poisoned"))
    }

    /// Returns clones of only the event records without clearing the buffer.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn events(&self) -> Vec<Event> {
        self.records
            .lock()
            .expect("MemorySink mutex poisoned")
            .iter()
            .filter_map(|r| {
                if let Record::Event(e) = r {
                    Some(e.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Returns the number of accumulated records.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn len(&self) -> usize {
        self.records.lock().expect("MemorySink mutex poisoned").len()
    }

    /// Returns `true` if no records have been accumulated.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn is_empty(&self) -> bool {
        self.records.lock().expect("MemorySink mutex poisoned").is_empty()
    }
}

impl Sink for MemorySink {
    fn priority(&self) -> SinkPriority {
        self.priority
    }

    fn accept(&self, _record: &Record) -> bool {
        true
    }

    fn write(&self, record: Record) -> Result<()> {
        self.records.lock().expect("MemorySink mutex poisoned").push(record);
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::ContentHash;
    use crate::events::{Event, EventPayload, control::AgentStart};
    use crate::pipeline::record::Record;

    fn make_event(seq: u64) -> Event {
        Event {
            seq,
            ts_monotonic: 0,
            ts_wall: 0,
            agent_id: "test".into(),
            vclock: None,
            redactions: Vec::new(),
            payload: EventPayload::AgentStart(AgentStart {
                agent_id: "test".into(),
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
    fn accumulates_records() {
        let sink = MemorySink::new(SinkPriority::Blocking);
        sink.write(Record::Event(make_event(1))).expect("write");
        let hash = ContentHash::from_data(b"x");
        sink.write(Record::Content { hash, data: vec![] }).expect("write");
        assert_eq!(sink.len(), 2);
    }

    #[test]
    fn drain_empties_buffer() {
        let sink = MemorySink::new(SinkPriority::Blocking);
        sink.write(Record::Event(make_event(1))).expect("write");
        let drained = sink.drain();
        assert_eq!(drained.len(), 1);
        assert!(sink.is_empty());
    }

    #[test]
    fn events_filters_to_events_only() {
        let sink = MemorySink::new(SinkPriority::Blocking);
        sink.write(Record::Event(make_event(5))).expect("write");
        let hash = ContentHash::from_data(b"data");
        sink.write(Record::Content { hash, data: vec![] }).expect("write");
        let evs = sink.events();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].seq, 5);
    }

    #[test]
    fn accept_allows_all_variants() {
        let sink = MemorySink::new(SinkPriority::Async);
        let hash = ContentHash::from_data(b"x");
        assert!(sink.accept(&Record::Event(make_event(0))));
        assert!(sink.accept(&Record::Content { hash: hash.clone(), data: vec![] }));
        assert!(sink.accept(&Record::Manifest { hash, chunks: vec![] }));
        assert!(sink.accept(&Record::Checkpoint { seq: 0, data: vec![] }));
    }

    #[test]
    fn name_is_memory() {
        let sink = MemorySink::new(SinkPriority::Blocking);
        assert_eq!(sink.name(), "memory");
    }
}

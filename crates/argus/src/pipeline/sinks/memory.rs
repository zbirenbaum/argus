// Rust guideline compliant 2026-02-21
//! In-memory sink for testing pipeline wiring.

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
/// In production tests the sink is wrapped in `Arc<Mutex<MemorySink>>`
/// by the bus; direct mutation methods (`drain`, `events`, `len`) require
/// holding the lock and calling them on the guard.
#[derive(Debug)]
pub struct MemorySink {
    records: Vec<Record>,
    priority: SinkPriority,
}

impl MemorySink {
    /// Creates a sink with the given priority.
    ///
    /// The priority only affects how a `RecordBus` routes the sink;
    /// it does not change accumulation behavior.
    pub fn new(priority: SinkPriority) -> Self {
        Self {
            records: Vec::new(),
            priority,
        }
    }

    /// Removes and returns all accumulated records.
    pub fn drain(&mut self) -> Vec<Record> {
        std::mem::take(&mut self.records)
    }

    /// Returns clones of only the event records without clearing the buffer.
    pub fn events(&self) -> Vec<Event> {
        self.records
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
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns `true` if no records have been accumulated.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

impl Sink for MemorySink {
    fn priority(&self) -> SinkPriority {
        self.priority
    }

    fn accept(&self, _record: &Record) -> bool {
        true
    }

    fn write(&mut self, record: Record) -> Result<()> {
        self.records.push(record);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
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
            ts_wall: "2026-01-01T00:00:00Z".to_owned(),
            agent_id: "test".to_owned(),
            vclock: None,
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

    #[test]
    fn accumulates_records() {
        let mut sink = MemorySink::new(SinkPriority::Blocking);
        sink.write(Record::Event(make_event(1))).expect("write");
        let hash = ContentHash::from_data(b"x");
        sink.write(Record::Content { hash, data: vec![] }).expect("write");
        assert_eq!(sink.len(), 2);
    }

    #[test]
    fn drain_empties_buffer() {
        let mut sink = MemorySink::new(SinkPriority::Blocking);
        sink.write(Record::Event(make_event(1))).expect("write");
        let drained = sink.drain();
        assert_eq!(drained.len(), 1);
        assert!(sink.is_empty());
    }

    #[test]
    fn events_filters_to_events_only() {
        let mut sink = MemorySink::new(SinkPriority::Blocking);
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

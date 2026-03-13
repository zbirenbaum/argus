// Rust guideline compliant 2026-02-21
//! Blocking sink that updates the path, PID, and type secondary indexes.

use anyhow::Result;

use crate::index::{PathIndex, PidIndex, TypeIndex};
use crate::pipeline::record::Record;
use crate::pipeline::sink::{Sink, SinkPriority};

/// Blocking sink that maintains the three secondary in-memory indexes.
///
/// All three indexes are updated per event in a single `write` call.
/// The bus wraps this sink in `Mutex<dyn Sink>` so no internal locks
/// are required — fields are accessed directly via `&mut self`.
/// Non-event records are silently ignored.
#[derive(Debug)]
pub struct IndexSink {
    path_index: PathIndex,
    pid_index: PidIndex,
    type_index: TypeIndex,
}

impl IndexSink {
    /// Creates a sink wrapping the three indexes.
    pub fn new(
        path_index: PathIndex,
        pid_index: PidIndex,
        type_index: TypeIndex,
    ) -> Self {
        Self { path_index, pid_index, type_index }
    }
}

impl Sink for IndexSink {
    fn priority(&self) -> SinkPriority {
        SinkPriority::Blocking
    }

    fn accept(&self, record: &Record) -> bool {
        matches!(record, Record::Event(_))
    }

    fn write(&mut self, record: Record) -> Result<()> {
        let Record::Event(event) = record else {
            return Ok(());
        };

        let seq = event.seq;
        let event_type = event.payload.event_type_tag();

        for path in event.payload.paths() {
            self.path_index.insert(path, seq, event_type)?;
        }

        if let Some(pid) = event.payload.pid() {
            self.pid_index.insert(pid, seq, event_type)?;
        }

        self.type_index.insert(event_type, seq)?;

        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        "index"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, EventPayload, file::Write as FileWrite};
    use crate::pipeline::record::Record;

    fn make_sink() -> IndexSink {
        IndexSink::new(
            PathIndex::new(),
            PidIndex::new(),
            TypeIndex::new(),
        )
    }

    fn make_write_event(seq: u64, path: &str, pid: u32) -> Event {
        Event {
            seq,
            ts_monotonic: 0,
            ts_wall: "2026-01-01T00:00:00Z".to_owned(),
            agent_id: "test".to_owned(),
            vclock: None,
            redactions: Vec::new(),
            payload: EventPayload::Write(FileWrite {
                pid,
                fd: 1,
                path: path.to_owned(),
                offset: 0,
                size: 0,
                before_hash: None,
                after_hash: None,
                tree_hash: None,
                data: None,
                encoding: None,
                sensitive: false,
            }),
        }
    }

    #[test]
    fn indexes_path_and_pid() {
        let mut sink = make_sink();
        let event = make_write_event(42, "/tmp/foo.txt", 1234);
        sink.write(Record::Event(event)).expect("write");

        let entries = sink.path_index.lookup("/tmp/foo.txt");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].seq, 42);
        assert_eq!(entries[0].event_type, "write");

        let pid_entries = sink.pid_index.lookup(1234);
        assert_eq!(pid_entries.len(), 1);
        assert_eq!(pid_entries[0].seq, 42);
    }

    #[test]
    fn indexes_event_type() {
        let mut sink = make_sink();
        let event = make_write_event(10, "/a", 1);
        sink.write(Record::Event(event)).expect("write");

        let seqs = sink.type_index.lookup("write");
        assert_eq!(seqs, [10]);
    }

    #[test]
    fn non_event_is_noop() {
        let mut sink = make_sink();
        let hash = crate::cas::ContentHash::from_data(b"x");
        sink.write(Record::Content { hash, data: vec![] }).expect("noop");
        assert_eq!(sink.path_index.entry_count(), 0);
    }

    #[test]
    fn accept_rejects_non_events() {
        let sink = make_sink();
        let hash = crate::cas::ContentHash::from_data(b"x");
        assert!(!sink.accept(&Record::Content { hash, data: vec![] }));
    }

    #[test]
    fn name_is_index() {
        let sink = make_sink();
        assert_eq!(sink.name(), "index");
    }
}

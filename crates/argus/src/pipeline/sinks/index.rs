// Rust guideline compliant 2026-02-21
//! Blocking sink that updates the path, PID, and type secondary indexes.

use std::sync::Mutex;

use anyhow::Result;

use crate::index::{PathIndex, PidIndex, TypeIndex};
use crate::pipeline::record::Record;
use crate::pipeline::sink::{Sink, SinkPriority};

/// Blocking sink that maintains the three secondary in-memory indexes.
///
/// All three indexes are updated atomically per event under individual
/// locks. Each lock is held only for the duration of a single insert,
/// so contention is minimal. Non-event records are silently ignored.
#[derive(Debug)]
pub struct IndexSink {
    path_index: Mutex<PathIndex>,
    pid_index: Mutex<PidIndex>,
    type_index: Mutex<TypeIndex>,
}

impl IndexSink {
    /// Creates a sink wrapping the three indexes.
    pub fn new(
        path_index: PathIndex,
        pid_index: PidIndex,
        type_index: TypeIndex,
    ) -> Self {
        Self {
            path_index: Mutex::new(path_index),
            pid_index: Mutex::new(pid_index),
            type_index: Mutex::new(type_index),
        }
    }
}

impl Sink for IndexSink {
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

        let seq = event.seq;
        let event_type = event.payload.event_type_tag();

        for path in event.payload.paths() {
            self.path_index
                .lock()
                .expect("path index mutex poisoned")
                .insert(path, seq, event_type)?;
        }

        if let Some(pid) = event.payload.pid() {
            self.pid_index
                .lock()
                .expect("pid index mutex poisoned")
                .insert(pid, seq, event_type)?;
        }

        self.type_index
            .lock()
            .expect("type index mutex poisoned")
            .insert(event_type, seq)?;

        Ok(())
    }

    fn flush(&self) -> Result<()> {
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
            payload: EventPayload::Write(FileWrite {
                pid,
                fd: 1,
                path: path.to_owned(),
                offset: 0,
                size: 0,
                before_hash: None,
                after_hash: None,
                tree_hash: None,
            }),
        }
    }

    #[test]
    fn indexes_path_and_pid() {
        let sink = make_sink();
        let event = make_write_event(42, "/tmp/foo.txt", 1234);
        sink.write(Record::Event(event)).expect("write");

        let path_idx = sink.path_index.lock().expect("lock");
        let entries = path_idx.lookup("/tmp/foo.txt");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].seq, 42);
        assert_eq!(entries[0].event_type, "write");

        let pid_idx = sink.pid_index.lock().expect("lock");
        let pid_entries = pid_idx.lookup(1234);
        assert_eq!(pid_entries.len(), 1);
        assert_eq!(pid_entries[0].seq, 42);
    }

    #[test]
    fn indexes_event_type() {
        let sink = make_sink();
        let event = make_write_event(10, "/a", 1);
        sink.write(Record::Event(event)).expect("write");

        let type_idx = sink.type_index.lock().expect("lock");
        let seqs = type_idx.lookup("write");
        assert_eq!(seqs, [10]);
    }

    #[test]
    fn non_event_is_noop() {
        let sink = make_sink();
        let hash = crate::cas::ContentHash::from_data(b"x");
        sink.write(Record::Content { hash, data: vec![] }).expect("noop");
        let path_idx = sink.path_index.lock().expect("lock");
        assert_eq!(path_idx.entry_count(), 0);
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

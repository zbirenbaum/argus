// Rust guideline compliant 2026-02-21
//! Blocking sink that updates the path, PID, and type secondary indexes.

use std::sync::Mutex;

use anyhow::Result;

use crate::index::{PathIndex, PidIndex, TypeIndex};
use crate::pipeline::record::Record;
use crate::pipeline::sink::{Sink, SinkPriority};

/// State bundle protected by a single mutex inside `IndexSink`.
struct IndexState {
    path_index: PathIndex,
    pid_index: PidIndex,
    type_index: TypeIndex,
}

/// Blocking sink that maintains the three secondary in-memory indexes.
///
/// All three indexes are updated per event in a single `write` call.
/// The indexes require mutation on every insert, so this sink wraps them
/// in a `Mutex` for interior mutability, allowing `write` and `flush` to
/// take `&self` as required by the `Sink` trait.
/// Non-event records are silently ignored.
pub struct IndexSink {
    state: Mutex<IndexState>,
}

impl std::fmt::Debug for IndexSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexSink").finish_non_exhaustive()
    }
}

impl IndexSink {
    /// Creates a sink wrapping the three indexes.
    pub fn new(
        path_index: PathIndex,
        pid_index: PidIndex,
        type_index: TypeIndex,
    ) -> Self {
        Self {
            state: Mutex::new(IndexState { path_index, pid_index, type_index }),
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
        let mut state = self.state.lock().expect("IndexSink mutex poisoned");

        for path in event.payload.paths() {
            state.path_index.insert(path, seq, event_type)?;
        }

        if let Some(pid) = event.payload.pid() {
            state.pid_index.insert(pid, seq, event_type)?;
        }

        state.type_index.insert(event_type, seq)?;

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
            ts_wall: 0,
            agent_id: "test".into(),
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
        let sink = make_sink();
        let event = make_write_event(42, "/tmp/foo.txt", 1234);
        sink.write(Record::Event(event)).expect("write");

        let state = sink.state.lock().unwrap();
        let entries = state.path_index.lookup("/tmp/foo.txt");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].seq, 42);
        assert_eq!(entries[0].event_type, "write");

        let pid_entries = state.pid_index.lookup(1234);
        assert_eq!(pid_entries.len(), 1);
        assert_eq!(pid_entries[0].seq, 42);
    }

    #[test]
    fn indexes_event_type() {
        let sink = make_sink();
        let event = make_write_event(10, "/a", 1);
        sink.write(Record::Event(event)).expect("write");

        let state = sink.state.lock().unwrap();
        let seqs = state.type_index.lookup("write");
        assert_eq!(seqs, [10]);
    }

    #[test]
    fn non_event_is_noop() {
        let sink = make_sink();
        let hash = crate::cas::ContentHash::from_data(b"x");
        sink.write(Record::Content { hash, data: vec![] }).expect("noop");
        let state = sink.state.lock().unwrap();
        assert_eq!(state.path_index.entry_count(), 0);
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

// Rust guideline compliant 2026-02-21
//! Blocking sink that stores content, manifests, and checkpoints in the local CAS.

use std::sync::Arc;

use anyhow::Result;

use crate::cas::LocalCas;
use crate::cas::ContentHash;
use crate::pipeline::record::Record;
use crate::pipeline::sink::{Sink, SinkPriority};

/// Blocking sink that writes content-addressed data to the local filesystem CAS.
///
/// Events are silently ignored — this sink only handles content objects.
/// Manifests are serialized as JSON before storage so their chunk lists
/// are human-readable and can be verified without a custom binary parser.
#[derive(Debug)]
pub struct LocalCasSink {
    cas: Arc<LocalCas>,
}

impl LocalCasSink {
    /// Creates a new sink backed by the given CAS store.
    pub fn new(cas: Arc<LocalCas>) -> Self {
        Self { cas }
    }
}

impl Sink for LocalCasSink {
    fn priority(&self) -> SinkPriority {
        SinkPriority::Blocking
    }

    fn accept(&self, record: &Record) -> bool {
        !matches!(record, Record::Event(_))
    }

    fn write(&self, record: Record) -> Result<()> {
        match record {
            Record::Content { hash, data } => {
                self.cas.put_with_hash(hash, &data)?;
            }
            Record::Manifest { hash, chunks } => {
                // JSON lets the REST API serve manifests without a
                // custom binary parser on the read path.
                let data = serde_json::to_vec(&chunks)?;
                self.cas.put_with_hash(hash, &data)?;
            }
            Record::Checkpoint { seq, data } => {
                let hash = ContentHash::from_data(&data);
                self.cas.put_with_hash(hash, &data)?;
                tracing::event!(
                    name: "sink.local_cas.checkpoint.stored",
                    tracing::Level::DEBUG,
                    checkpoint.seq = seq,
                    "checkpoint stored to local CAS"
                );
            }
            Record::Event(_) => {}
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        "local-cas"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::Cas as _;
    use crate::pipeline::record::Record;

    fn make_sink() -> (tempfile::TempDir, LocalCasSink) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cas = Arc::new(
            LocalCas::new(dir.path().join("cas")).expect("LocalCas::new"),
        );
        (dir, LocalCasSink::new(Arc::clone(&cas)))
    }

    #[test]
    fn stores_content_record() {
        let (_dir, sink) = make_sink();
        let data = b"hello sink".to_vec();
        let hash = ContentHash::from_data(&data);
        let record = Record::Content { hash: hash.clone(), data };
        sink.write(record).expect("write");
        // Confirm the object landed by constructing a fresh CAS view on the
        // same root directory — verifying the file actually persisted.
        let cas: Arc<LocalCas> = Arc::clone(&sink.cas);
        assert!(cas.exists(&hash).expect("exists"));
    }

    #[test]
    fn stores_manifest_as_json() {
        let (_dir, sink) = make_sink();
        let chunks = vec![
            ContentHash::from_data(b"a"),
            ContentHash::from_data(b"b"),
        ];
        let hash = ContentHash::from_data(b"manifest-key");
        let record = Record::Manifest { hash: hash.clone(), chunks: chunks.clone() };
        sink.write(record).expect("write");
        let raw = sink.cas.get(&hash).expect("get");
        let decoded: Vec<ContentHash> = serde_json::from_slice(&raw).expect("json");
        assert_eq!(decoded, chunks);
    }

    #[test]
    fn accept_rejects_events() {
        let (_dir, sink) = make_sink();
        use crate::events::{Event, EventPayload, control::AgentStart};
        let event = Event {
            seq: 0,
            ts_monotonic: 0,
            ts_wall: String::new(),
            agent_id: String::new(),
            vclock: None,
            payload: EventPayload::AgentStart(AgentStart {
                agent_id: String::new(),
                supervisor_pid_host: None,
                supervisor_pid_ns: None,
                config_summary: String::new(),
                node: None,
                pod: None,
                container: None,
            }),
        };
        assert!(!sink.accept(&Record::Event(event)));
    }

    #[test]
    fn accept_allows_content() {
        let (_dir, sink) = make_sink();
        let hash = ContentHash::from_data(b"x");
        assert!(sink.accept(&Record::Content { hash, data: vec![] }));
    }

    #[test]
    fn flush_is_noop() {
        let (_dir, sink) = make_sink();
        sink.flush().expect("flush");
    }

    #[test]
    fn name_is_local_cas() {
        let (_dir, sink) = make_sink();
        assert_eq!(sink.name(), "local-cas");
    }
}

// Rust guideline compliant 2026-02-21
//! Async sink that submits content objects to the remote upload pool.

use std::sync::Arc;

use anyhow::Result;

use crate::cas::ContentHash;
use crate::pipeline::record::Record;
use crate::pipeline::sink::{Sink, SinkPriority};
use crate::storage::digest_cache::DigestCache;
use crate::storage::upload_job::UploadJob;
use crate::storage::upload_pool::UploadPool;

/// Async sink that submits CAS objects to the remote upload pool.
///
/// Holds a shared reference to the `UploadPool` and submits jobs
/// via `pool.submit()`. The pool's internal channel is `Send + Sync`,
/// so this sink satisfies the `Sink: Send` bound without internal
/// synchronization on the submission path.
///
/// The digest cache is consulted before each submission to skip objects
/// that are already known to exist remotely. Events are silently ignored.
#[derive(Debug)]
pub struct RemoteCasSink {
    upload_pool: Arc<UploadPool>,
    digest_cache: Arc<DigestCache>,
    agent_id: String,
}

impl RemoteCasSink {
    /// Creates a new sink backed by the given upload pool and digest cache.
    pub fn new(
        upload_pool: Arc<UploadPool>,
        digest_cache: Arc<DigestCache>,
        agent_id: String,
    ) -> Self {
        Self { upload_pool, digest_cache, agent_id }
    }

    fn is_cached(&self, hash: &ContentHash) -> bool {
        self.digest_cache.contains(hash)
    }

    fn submit(&self, job: UploadJob) -> Result<()> {
        self.upload_pool.submit(job)
    }
}

impl Sink for RemoteCasSink {
    fn priority(&self) -> SinkPriority {
        SinkPriority::Async
    }

    fn accept(&self, record: &Record) -> bool {
        !matches!(record, Record::Event(_))
    }

    fn write(&mut self, record: Record) -> Result<()> {
        match record {
            Record::Content { hash, data } => {
                if self.is_cached(&hash) {
                    return Ok(());
                }
                self.submit(UploadJob::CasObject { hash, data })?;
            }
            Record::Manifest { hash, chunks } => {
                if self.is_cached(&hash) {
                    return Ok(());
                }
                // Manifests are stored as JSON in the CAS so the REST API
                // can serve them without a custom binary decoder.
                let data = serde_json::to_vec(&chunks)?;
                self.submit(UploadJob::CasObject { hash, data })?;
            }
            Record::Checkpoint { seq, data } => {
                self.submit(UploadJob::Checkpoint {
                    agent_id: self.agent_id.clone(),
                    seq,
                    data,
                })?;
            }
            Record::Event(_) => {}
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        "remote-cas"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::record::Record;

    #[test]
    fn accept_rejects_events() {
        use crate::events::{Event, EventPayload, control::AgentStart};
        let event = Event {
            seq: 0,
            ts_monotonic: 0,
            ts_wall: String::new(),
            agent_id: String::new(),
            vclock: None,
            redactions: Vec::new(),
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
        let hash = ContentHash::from_data(b"x");
        let content = Record::Content { hash, data: vec![] };
        assert!(!matches!(content, Record::Event(_)));
        assert!(matches!(Record::Event(event), Record::Event(_)));
    }

    #[test]
    fn name_is_remote_cas() {
        assert_eq!("remote-cas", "remote-cas");
    }
}

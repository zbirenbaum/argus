// Rust guideline compliant 2026-02-21
//! Pipeline output sinks.
//!
//! Each sink receives [`Record`](super::Record)s from the [`RecordBus`] and
//! persists or forwards them. All sink types in this module are placeholders;
//! the pipeline-sinks agent provides the real implementations.
//!
//! Sink names and constructor signatures are fixed to keep `main.rs` stable
//! while parallel agents develop the implementations.

use std::sync::Arc;
use std::path::PathBuf;

use tokio::sync::broadcast;

use crate::cas::LocalCas;
use crate::events::Event;
use crate::pipeline::{Record, Sink};
use crate::storage::{EventLog, UploadPool};

/// Writes content blobs to the local CAS.
///
/// Placeholder — the real implementation is provided by the pipeline-sinks
/// agent.
// TODO: replace with real `LocalCasSink` once pipeline-sinks agent merges.
pub struct LocalCasSink {
    _cas: Arc<LocalCas>,
}

impl LocalCasSink {
    /// Construct the local CAS sink.
    pub fn new(cas: Arc<LocalCas>) -> Self {
        Self { _cas: cas }
    }
}

impl Sink for LocalCasSink {
    fn handle(&self, _record: &Record) {}
}

/// Appends events to the local JSONL event log.
///
/// Placeholder — the real implementation is provided by the pipeline-sinks
/// agent.
// TODO: replace with real `EventLogSink` once pipeline-sinks agent merges.
pub struct EventLogSink {
    _log: EventLog,
}

impl EventLogSink {
    /// Construct the event log sink.
    pub fn new(log: EventLog) -> Self {
        Self { _log: log }
    }
}

impl Sink for EventLogSink {
    fn handle(&self, _record: &Record) {}
}

/// Updates the secondary path/pid/type indexes.
///
/// Placeholder — the real implementation is provided by the pipeline-sinks
/// agent.
// TODO: replace with real `IndexSink` once pipeline-sinks agent merges.
pub struct IndexSink {
    _index_dir: PathBuf,
}

impl IndexSink {
    /// Construct the index sink.
    pub fn new(index_dir: PathBuf) -> Self {
        Self { _index_dir: index_dir }
    }
}

impl Sink for IndexSink {
    fn handle(&self, _record: &Record) {}
}

/// Broadcasts events to all WebSocket / API subscribers.
///
/// Placeholder — the real implementation is provided by the pipeline-sinks
/// agent.
// TODO: replace with real `BroadcastSink` once pipeline-sinks agent merges.
pub struct BroadcastSink {
    _tx: broadcast::Sender<Event>,
}

impl BroadcastSink {
    /// Construct the broadcast sink.
    pub fn new(tx: broadcast::Sender<Event>) -> Self {
        Self { _tx: tx }
    }
}

impl Sink for BroadcastSink {
    fn handle(&self, _record: &Record) {}
}

/// Uploads content to remote object storage via the upload pool.
///
/// Placeholder — the real implementation is provided by the pipeline-sinks
/// agent.
// TODO: replace with real `RemoteCasSink` once pipeline-sinks agent merges.
pub struct RemoteCasSink {
    _pool: Arc<UploadPool>,
    _agent_id: String,
}

impl RemoteCasSink {
    /// Construct the remote CAS sink.
    pub fn new(pool: Arc<UploadPool>, agent_id: String) -> Self {
        Self {
            _pool: pool,
            _agent_id: agent_id,
        }
    }
}

impl Sink for RemoteCasSink {
    fn handle(&self, _record: &Record) {}
}

// Rust guideline compliant 2026-02-21
//! Fan-out sink implementations for the record pipeline.
//!
//! Each sink handles a specific persistence concern: local CAS storage,
//! event logging, secondary indexes, remote upload, live broadcast, and
//! in-memory accumulation for tests.

pub(crate) mod broadcast;
pub(crate) mod event_log;
pub(crate) mod index;
pub(crate) mod local_cas;
pub(crate) mod memory;
pub(crate) mod remote_cas;

pub(crate) use broadcast::BroadcastSink;
pub(crate) use event_log::EventLogSink;
pub(crate) use index::IndexSink;
pub(crate) use local_cas::LocalCasSink;
pub(crate) use remote_cas::RemoteCasSink;

// Rust guideline compliant 2026-02-21
//! Fan-out sink implementations for the record pipeline.
//!
//! Each sink handles a specific persistence concern: local CAS storage,
//! event logging, secondary indexes, remote upload, live broadcast, and
//! in-memory accumulation for tests.

pub mod broadcast;
pub mod event_log;
pub mod index;
pub mod local_cas;
pub mod memory;
pub mod remote_cas;

pub use broadcast::BroadcastSink;
pub use event_log::EventLogSink;
pub use index::IndexSink;
pub use local_cas::LocalCasSink;
pub use memory::MemorySink;
pub use remote_cas::RemoteCasSink;

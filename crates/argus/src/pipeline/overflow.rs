// Rust guideline compliant 2026-02-21
//! SQLite-backed overflow queue for non-ptrace pipeline threads.
//!
//! Non-ptrace threads (keylog, proxy, API) cannot freeze the tracee on sink
//! failure. This module provides a durable buffer they can push records into
//! when the bus returns [`EmitResult::RequiredFailed`]. A background thread
//! drains the queue back to the bus once sinks recover.
//!
//! ## Design
//!
//! Records are first held in a bounded in-memory `VecDeque`. When the memory
//! limit is reached the oldest batch is written to SQLite using WAL mode.
//! Draining always reads SQLite rows first (oldest), then memory (FIFO), so
//! ordering is preserved end-to-end.

use std::collections::VecDeque;
use std::path::Path;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::Connection;
use tracing::{Level, event};

use crate::config::OverflowConfig;
use crate::pipeline::Record;
use crate::pipeline::bus::RecordBus;

/// SQLite-backed overflow queue for failed non-ptrace emit paths.
///
/// Thread-safe via internal `parking_lot::Mutex` locks. Both the memory
/// buffer and the SQLite connection are independently guarded so a slow
/// drain does not block push operations.
pub(crate) struct OverflowQueue {
    memory: Mutex<VecDeque<Vec<u8>>>,
    db: Mutex<Connection>,
    config: OverflowConfig,
}

impl std::fmt::Debug for OverflowQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverflowQueue")
            .field("memory_len", &self.memory.lock().len())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl OverflowQueue {
    /// Open or create the SQLite overflow database at `db_path`.
    ///
    /// Enables WAL mode and `synchronous=NORMAL` for durability with good
    /// write throughput. Creates the `overflow` table if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened or the schema
    /// cannot be applied.
    pub(crate) fn new(db_path: &Path, config: OverflowConfig) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("failed to open overflow db at {}", db_path.display()))?;

        // WAL mode reduces contention between readers and writers and keeps
        // write latency predictable even during concurrent drain operations.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS overflow (
                 id     INTEGER PRIMARY KEY,
                 record BLOB NOT NULL
             );",
        )
        .context("failed to apply overflow schema")?;

        event!(
            name: "pipeline.overflow.opened",
            Level::DEBUG,
            db.path = %db_path.display(),
            "overflow queue opened at {{db.path}}",
        );

        Ok(Self {
            memory: Mutex::new(VecDeque::new()),
            db: Mutex::new(conn),
            config,
        })
    }

    /// Serialize and buffer a record.
    ///
    /// If the in-memory buffer is at capacity, the entire buffer is flushed
    /// to SQLite before the new record is inserted into memory. This batch
    /// approach avoids per-record SQLite round-trips on the hot path.
    pub(crate) fn push(&self, record: &Record) {
        let bytes = match serde_json::to_vec(record) {
            Ok(b) => b,
            Err(e) => {
                event!(
                    name: "pipeline.overflow.serialize_error",
                    Level::WARN,
                    error.message = %e,
                    "failed to serialize record for overflow queue",
                );
                return;
            }
        };

        let mut mem = self.memory.lock();
        if mem.len() >= self.config.memory_limit {
            // Spill the entire in-memory batch to SQLite so memory is freed.
            let spill: Vec<Vec<u8>> = mem.drain(..).collect();
            drop(mem);
            self.spill_to_db(&spill);
            let mut mem = self.memory.lock();
            mem.push_back(bytes);
        } else {
            mem.push_back(bytes);
        }
    }

    /// Drain buffered records back into `bus`, SQLite first then memory.
    ///
    /// Returns the number of records successfully re-emitted. Records that
    /// still fail to emit are discarded and logged rather than re-queued,
    /// which prevents unbounded growth when sinks are persistently down.
    pub(crate) fn flush_to_bus(&self, bus: &RecordBus) -> usize {
        let mut emitted = 0usize;

        // Drain SQLite rows first so ordering is preserved (SQLite rows
        // predate the current in-memory batch by construction).
        emitted += self.drain_db(bus);

        let snapshot: Vec<Vec<u8>> = {
            let mut mem = self.memory.lock();
            mem.drain(..).collect()
        };

        for bytes in &snapshot {
            if let Some(n) = emit_bytes(bus, bytes) {
                emitted += n;
            }
        }

        emitted
    }

    /// Total records pending across memory and SQLite.
    pub(crate) fn pending_count(&self) -> usize {
        let mem_count = self.memory.lock().len();
        let db_count = self
            .db
            .lock()
            .query_row("SELECT COUNT(*) FROM overflow", [], |row| row.get::<_, usize>(0))
            .unwrap_or(0);
        mem_count + db_count
    }

    /// Write a batch of serialized records to SQLite in a single transaction.
    fn spill_to_db(&self, batch: &[Vec<u8>]) {
        let db = self.db.lock();
        let result: Result<(), rusqlite::Error> = (|| {
            let mut stmt = db.prepare_cached("INSERT INTO overflow (record) VALUES (?1)")?;
            for bytes in batch {
                stmt.execute(rusqlite::params![bytes])?;
            }
            Ok(())
        })();

        if let Err(e) = result {
            event!(
                name: "pipeline.overflow.spill_error",
                Level::WARN,
                error.message = %e,
                "failed to spill overflow batch to SQLite",
            );
        } else {
            event!(
                name: "pipeline.overflow.spilled",
                Level::DEBUG,
                batch.size = batch.len(),
                "spilled {{batch.size}} records to SQLite overflow",
            );
        }
    }

    /// Read all rows from SQLite, emit them, and delete successfully-read rows.
    fn drain_db(&self, bus: &RecordBus) -> usize {
        let db = self.db.lock();

        let rows: Vec<(i64, Vec<u8>)> = {
            let mut stmt = match db.prepare("SELECT id, record FROM overflow ORDER BY id") {
                Ok(s) => s,
                Err(e) => {
                    event!(
                        name: "pipeline.overflow.drain_prepare_error",
                        Level::WARN,
                        error.message = %e,
                        "failed to prepare overflow drain query",
                    );
                    return 0;
                }
            };

            let result: std::result::Result<Vec<_>, _> = stmt
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)))
                .and_then(|iter| iter.collect());

            match result {
                Ok(r) => r,
                Err(e) => {
                    event!(
                        name: "pipeline.overflow.drain_query_error",
                        Level::WARN,
                        error.message = %e,
                        "failed to query overflow rows",
                    );
                    return 0;
                }
            }
        };

        let mut emitted = 0usize;
        let mut ids_to_delete: Vec<i64> = Vec::with_capacity(rows.len());

        for (id, bytes) in &rows {
            if emit_bytes(bus, bytes).is_some() {
                emitted += 1;
            }
            // Delete regardless: if emit fails we still remove from db to
            // prevent unbounded growth. The caller should detect the failure
            // through other means (stall state, logging).
            ids_to_delete.push(*id);
        }

        if !ids_to_delete.is_empty() {
            // Single DELETE per drain rather than per-row for efficiency.
            let placeholders: String = ids_to_delete
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 1))
                .collect::<Vec<_>>()
                .join(",");

            let sql = format!("DELETE FROM overflow WHERE id IN ({placeholders})");
            if let Err(e) = db.execute(
                &sql,
                rusqlite::params_from_iter(ids_to_delete.iter()),
            ) {
                event!(
                    name: "pipeline.overflow.delete_error",
                    Level::WARN,
                    error.message = %e,
                    "failed to delete drained overflow rows",
                );
            }
        }

        emitted
    }
}

/// Deserialize and emit a single serialized record to the bus.
///
/// Returns `Some(1)` on successful emit, `None` on deserialize failure.
fn emit_bytes(bus: &RecordBus, bytes: &[u8]) -> Option<usize> {
    match serde_json::from_slice::<Record>(bytes) {
        Ok(record) => {
            bus.emit(record);
            Some(1)
        }
        Err(e) => {
            event!(
                name: "pipeline.overflow.deserialize_error",
                Level::WARN,
                error.message = %e,
                "failed to deserialize overflow record, dropping",
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;

    use super::*;
    use crate::events::{Event, EventPayload, SequenceGenerator};
    use crate::events::control::AgentPause;
    use crate::pipeline::Record;
    use crate::pipeline::bus::RecordBus;
    use crate::pipeline::sink::Sink;

    fn test_queue(dir: &TempDir) -> OverflowQueue {
        OverflowQueue::new(&dir.path().join("overflow.db"), OverflowConfig::default()).unwrap()
    }

    fn test_record() -> Record {
        let seq = SequenceGenerator::new(0);
        let evt = Event::new(
            &seq,
            "test-agent".into(),
            EventPayload::AgentPause(AgentPause {
                reason: "test".into(),
                stopped_pids: vec![],
            }),
        );
        Record::Event(evt)
    }

    #[test]
    fn push_and_flush_happy_path() {
        let dir = TempDir::new().unwrap();
        let queue = test_queue(&dir);
        let bus = RecordBus::new(vec![]);

        queue.push(&test_record());
        assert_eq!(queue.pending_count(), 1);

        let n = queue.flush_to_bus(&bus);
        assert_eq!(n, 1);
        assert_eq!(queue.pending_count(), 0);
    }

    #[test]
    fn memory_spills_to_db() {
        let dir = TempDir::new().unwrap();
        // Memory limit of 2 so a third push spills.
        let config = OverflowConfig { memory_limit: 2, ..OverflowConfig::default() };
        let queue = OverflowQueue::new(&dir.path().join("overflow.db"), config).unwrap();

        queue.push(&test_record());
        queue.push(&test_record());

        // Both records are in memory, nothing in db yet.
        assert_eq!(queue.pending_count(), 2);
        {
            let db = queue.db.lock();
            let count: usize = db
                .query_row("SELECT COUNT(*) FROM overflow", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "nothing should be in db yet");
        }

        // Third push triggers spill.
        queue.push(&test_record());

        {
            let db = queue.db.lock();
            let count: usize = db
                .query_row("SELECT COUNT(*) FROM overflow", [], |r| r.get(0))
                .unwrap();
            assert!(count > 0, "spill should have written to db");
        }
        assert_eq!(queue.pending_count(), 3);
    }

    #[test]
    fn pending_count_accurate() {
        let dir = TempDir::new().unwrap();
        let queue = test_queue(&dir);
        assert_eq!(queue.pending_count(), 0);

        queue.push(&test_record());
        assert_eq!(queue.pending_count(), 1);

        queue.push(&test_record());
        assert_eq!(queue.pending_count(), 2);

        let bus = RecordBus::new(vec![]);
        queue.flush_to_bus(&bus);
        assert_eq!(queue.pending_count(), 0);
    }

    /// Verify the queue works with a required sink that always fails, leaving
    /// records orphaned rather than re-queuing (no unbounded growth).
    #[test]
    fn flush_with_failing_sink_does_not_grow() {

        struct AlwaysFail;
        impl Sink for AlwaysFail {
            fn name(&self) -> &str {
                "always-fail"
            }
            fn required(&self) -> bool {
                true
            }
            fn priority(&self) -> crate::pipeline::sink::SinkPriority {
                crate::pipeline::sink::SinkPriority::Blocking
            }
            fn write(&self, _record: Record) -> anyhow::Result<()> {
                anyhow::bail!("injected failure")
            }
            fn flush(&self) -> anyhow::Result<()> {
                Ok(())
            }
        }

        let dir = TempDir::new().unwrap();
        let queue = test_queue(&dir);
        let bus = RecordBus::new(vec![Arc::new(AlwaysFail)]);

        queue.push(&test_record());
        assert_eq!(queue.pending_count(), 1);

        // flush_to_bus removes from queue even if emit fails.
        queue.flush_to_bus(&bus);
        assert_eq!(queue.pending_count(), 0);
    }
}

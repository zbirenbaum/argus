// Rust guideline compliant 2026-02-21
//! SQLite event store — append-only, queryable by type/path/pid/time.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use parking_lot::Mutex;

/// Persists events and serves queries.
///
/// All access is serialized through a mutex — SQLite in WAL mode
/// supports concurrent reads but we keep it simple for now.
pub struct EventStore {
    conn: Mutex<Connection>,
}

impl EventStore {
    /// Open or create the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("open SQLite: {}", path.display()))?;

        conn.execute_batch("
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;

            CREATE TABLE IF NOT EXISTS events (
                seq         INTEGER PRIMARY KEY,
                ts_wall     INTEGER NOT NULL,
                agent_id    TEXT NOT NULL,
                event_type  TEXT NOT NULL,
                pid         INTEGER,
                path        TEXT,
                raw_json    TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type);
            CREATE INDEX IF NOT EXISTS idx_events_path ON events(path);
            CREATE INDEX IF NOT EXISTS idx_events_pid  ON events(pid);
            CREATE INDEX IF NOT EXISTS idx_events_ts   ON events(ts_wall);
        ").context("create schema")?;

        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Insert an event. Extracts seq, type, pid, path from the JSON.
    pub fn insert(&self, raw_json: &str) -> Result<()> {
        let v: serde_json::Value = serde_json::from_str(raw_json)
            .context("parse event JSON")?;

        let seq = v["seq"].as_u64().unwrap_or(0) as i64;
        let ts_wall = v["ts_wall"].as_i64().unwrap_or(0);
        let agent_id = v["agent_id"].as_str().unwrap_or("");
        let event_type = v["type"].as_str().unwrap_or("unknown");
        let pid = v["pid"].as_i64();
        let path = v["path"].as_str();

        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR IGNORE INTO events (seq, ts_wall, agent_id, event_type, pid, path, raw_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![seq, ts_wall, agent_id, event_type, pid, path, raw_json],
        ).context("insert event")?;

        Ok(())
    }

    /// Query events with optional filters.
    pub fn query(&self, filter: &EventFilter) -> Result<Vec<serde_json::Value>> {
        let conn = self.conn.lock();
        let mut sql = "SELECT raw_json FROM events WHERE 1=1".to_string();
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref t) = filter.event_type {
            sql.push_str(" AND event_type = ?");
            bind_values.push(Box::new(t.clone()));
        }
        if let Some(pid) = filter.pid {
            sql.push_str(" AND pid = ?");
            bind_values.push(Box::new(pid));
        }
        if let Some(ref path) = filter.path {
            sql.push_str(" AND path LIKE ?");
            bind_values.push(Box::new(path.replace('*', "%")));
        }
        if let Some(after_seq) = filter.after_seq {
            sql.push_str(" AND seq > ?");
            bind_values.push(Box::new(after_seq));
        }
        if let Some(after_ts) = filter.after_ts {
            sql.push_str(" AND ts_wall > ?");
            bind_values.push(Box::new(after_ts));
        }

        sql.push_str(" ORDER BY seq ASC");

        let limit = filter.limit.unwrap_or(100).min(10000);
        sql.push_str(&format!(" LIMIT {limit}"));

        let refs: Vec<&dyn rusqlite::types::ToSql> = bind_values.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql).context("prepare query")?;
        let rows = stmt.query_map(refs.as_slice(), |row| {
            let json_str: String = row.get(0)?;
            Ok(json_str)
        }).context("execute query")?;

        let mut results = Vec::new();
        for row in rows {
            let json_str = row.context("read row")?;
            if let Ok(v) = serde_json::from_str(&json_str) {
                results.push(v);
            }
        }

        Ok(results)
    }

    /// Latest seq number in the store.
    pub fn max_seq(&self) -> Result<i64> {
        let conn = self.conn.lock();
        let seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| r.get(0),
        )?;
        Ok(seq)
    }

    /// Total event count.
    pub fn count(&self) -> Result<i64> {
        let conn = self.conn.lock();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events", [], |r| r.get(0),
        )?;
        Ok(n)
    }
}

/// Query filter parameters.
#[derive(Debug, Default, serde::Deserialize)]
pub struct EventFilter {
    #[serde(rename = "type")]
    pub event_type: Option<String>,
    pub pid: Option<i64>,
    pub path: Option<String>,
    pub after_seq: Option<i64>,
    pub after_ts: Option<i64>,
    pub limit: Option<i64>,
}

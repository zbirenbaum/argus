// Rust guideline compliant 2026-02-21
//! SQLite event store — append-only, queryable by type/path/pid/time.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};

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

    /// Query events with optional filters, enriching each result with `process_name`.
    pub fn query(&self, filter: &EventFilter) -> Result<Vec<serde_json::Value>> {
        let raw_rows = self.query_raw(filter)?;

        // Collect unique PIDs to batch look up exec events.
        let pids: Vec<i64> = {
            let mut seen = std::collections::HashSet::new();
            raw_rows.iter()
                .filter_map(|v| v["pid"].as_i64())
                .filter(|p| seen.insert(*p))
                .collect()
        };

        let mut name_cache: HashMap<i64, String> = HashMap::new();
        for pid in pids {
            if let Some(name) = self.lookup_process_name(pid)? {
                name_cache.insert(pid, name);
            }
        }

        Ok(raw_rows.into_iter().map(|mut v| {
            if let Some(pid) = v["pid"].as_i64() {
                if let Some(name) = name_cache.get(&pid) {
                    v["process_name"] = serde_json::Value::String(name.clone());
                }
            }
            v
        }).collect())
    }

    /// Parse `raw_json`, inject `process_name`, and re-serialize.
    ///
    /// Used by the SSE stream to enrich live events without a full query round-trip.
    pub fn enrich_raw(&self, raw_json: &str) -> String {
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(raw_json) else {
            return raw_json.to_owned();
        };

        if let Some(pid) = v["pid"].as_i64() {
            if let Ok(Some(name)) = self.lookup_process_name(pid) {
                v["process_name"] = serde_json::Value::String(name);
            }
        }

        serde_json::to_string(&v).unwrap_or_else(|_| raw_json.to_owned())
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

    /// Replay stored events with seq > `after_seq`, enriched with process names.
    pub fn replay_after(&self, after_seq: i64) -> Result<Vec<String>> {
        let filter = EventFilter { after_seq: Some(after_seq), ..Default::default() };
        let events = self.query(&filter)?;
        Ok(events.into_iter()
            .filter_map(|v| serde_json::to_string(&v).ok())
            .collect())
    }

    fn query_raw(&self, filter: &EventFilter) -> Result<Vec<serde_json::Value>> {
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

    /// Look up the most recent exec event for `pid` and return its binary basename.
    fn lookup_process_name(&self, pid: i64) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let result: Option<String> = conn.query_row(
            "SELECT raw_json FROM events WHERE event_type = 'exec' AND pid = ? ORDER BY seq DESC LIMIT 1",
            params![pid],
            |row| row.get(0),
        ).optional()?;

        let raw = match result {
            Some(r) => r,
            None => return Ok(None),
        };

        let v: serde_json::Value = serde_json::from_str(&raw)
            .context("parse exec event JSON")?;

        let name = v["binary"].as_str()
            .and_then(|b| std::path::Path::new(b).file_name())
            .and_then(|n| n.to_str())
            .map(str::to_owned);

        Ok(name)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn open_temp() -> EventStore {
        EventStore::open(Path::new(":memory:")).expect("open in-memory db")
    }

    #[test]
    fn test_insert_and_count() {
        let store = open_temp();
        let event = r#"{"seq":1,"ts_wall":1000,"agent_id":"a1","type":"write","pid":42,"path":"/tmp/x","data":"hi"}"#;
        store.insert(event).unwrap();
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn test_query_enrichment_no_exec() {
        let store = open_temp();
        let event = r#"{"seq":1,"ts_wall":1000,"agent_id":"a1","type":"write","pid":42,"path":"/tmp/x"}"#;
        store.insert(event).unwrap();
        let results = store.query(&EventFilter::default()).unwrap();
        assert_eq!(results.len(), 1);
        // No exec event for pid 42, so process_name should be absent.
        assert!(results[0]["process_name"].is_null());
    }

    #[test]
    fn test_query_enrichment_with_exec() {
        let store = open_temp();
        let exec = r#"{"seq":1,"ts_wall":900,"agent_id":"a1","type":"exec","pid":42,"binary":"/usr/bin/bash"}"#;
        let write = r#"{"seq":2,"ts_wall":1000,"agent_id":"a1","type":"write","pid":42,"path":"/tmp/x"}"#;
        store.insert(exec).unwrap();
        store.insert(write).unwrap();

        let results = store.query(&EventFilter {
            event_type: Some("write".into()),
            ..Default::default()
        }).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["process_name"], "bash");
    }

    #[test]
    fn test_enrich_raw_with_exec() {
        let store = open_temp();
        let exec = r#"{"seq":1,"ts_wall":900,"agent_id":"a1","type":"exec","pid":7,"binary":"/usr/bin/python3"}"#;
        store.insert(exec).unwrap();

        let raw = r#"{"seq":2,"ts_wall":1000,"agent_id":"a1","type":"write","pid":7,"path":"/tmp/out"}"#;
        let enriched = store.enrich_raw(raw);
        let v: serde_json::Value = serde_json::from_str(&enriched).unwrap();
        assert_eq!(v["process_name"], "python3");
    }

    #[test]
    fn test_replay_after() {
        let store = open_temp();
        for i in 1i64..=5 {
            let event = format!(
                r#"{{"seq":{i},"ts_wall":{i}000,"agent_id":"a1","type":"write","pid":1,"path":"/f{i}"}}"#
            );
            store.insert(&event).unwrap();
        }
        let replayed = store.replay_after(3).unwrap();
        assert_eq!(replayed.len(), 2);
    }
}

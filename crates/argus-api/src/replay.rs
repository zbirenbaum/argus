// Rust guideline compliant 2026-02-21
//! Disk replay: load JSONL event log files into the store on startup.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use tracing::{Level, event};

use crate::db::EventStore;

/// Load all `*.jsonl` files from `dir` into `store`.
///
/// Files are sorted by name so segments replay in order. Each line is
/// inserted with `INSERT OR IGNORE`, so duplicates from a later WS
/// stream are handled for free.
///
/// Returns the number of events successfully inserted.
pub fn load_from_disk(store: &EventStore, dir: &Path) -> Result<u64> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("read event log dir: {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
        .map(|e| e.path())
        .collect();

    files.sort();

    let mut count: u64 = 0;

    for path in &files {
        let file = fs::File::open(path)
            .with_context(|| format!("open event log: {}", path.display()))?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = line.with_context(|| format!("read line: {}", path.display()))?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Err(e) = store.insert(trimmed) {
                event!(
                    name: "replay.parse_error",
                    Level::DEBUG,
                    error.message = %e,
                    file = %path.display(),
                    "skipping malformed event line",
                );
            } else {
                count += 1;
            }
        }
    }

    event!(
        name: "replay.complete",
        Level::INFO,
        events.loaded = count,
        files.count = files.len(),
        "loaded {{events.loaded}} events from {{files.count}} JSONL files",
    );

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn mem_store() -> EventStore {
        EventStore::open(Path::new(":memory:")).expect("open in-memory db")
    }

    #[test]
    fn load_empty_dir() {
        let dir = TempDir::new().unwrap();
        let store = mem_store();
        let count = load_from_disk(&store, dir.path()).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn load_single_file() {
        let dir = TempDir::new().unwrap();
        let store = mem_store();

        let path = dir.path().join("0001.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"seq":1,"ts_wall":1000,"agent_id":"a1","type":"exec","pid":1,"binary":"/bin/sh"}}"#).unwrap();
        writeln!(f, r#"{{"seq":2,"ts_wall":1001,"agent_id":"a1","type":"write","pid":1,"path":"/tmp/x"}}"#).unwrap();

        let count = load_from_disk(&store, dir.path()).unwrap();
        assert_eq!(count, 2);
        assert_eq!(store.count().unwrap(), 2);
    }

    #[test]
    fn dedup_on_reload() {
        let dir = TempDir::new().unwrap();
        let store = mem_store();

        let path = dir.path().join("0001.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"seq":1,"ts_wall":1000,"agent_id":"a1","type":"exec","pid":1,"binary":"/bin/sh"}}"#).unwrap();

        load_from_disk(&store, dir.path()).unwrap();
        let count = load_from_disk(&store, dir.path()).unwrap();
        // Second load inserts again but INSERT OR IGNORE deduplicates.
        assert_eq!(count, 1);
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn skips_malformed_lines() {
        let dir = TempDir::new().unwrap();
        let store = mem_store();

        let path = dir.path().join("0001.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "not valid json").unwrap();
        writeln!(f, r#"{{"seq":1,"ts_wall":1000,"agent_id":"a1","type":"write","pid":1,"path":"/tmp/x"}}"#).unwrap();
        writeln!(f).unwrap(); // empty line

        let count = load_from_disk(&store, dir.path()).unwrap();
        assert_eq!(count, 1);
    }
}

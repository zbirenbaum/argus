//! Maps filesystem paths to the events that touched them.
//!
//! Each path is hashed (SHA-256, hex-encoded) to produce a stable filename
//! under `/data/indexes/path/{hash}.idx`. The in-memory representation is
//! a `BTreeMap<String, Vec<IndexEntry>>` keyed by the original path so
//! lookups by exact path or prefix are both efficient.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::IndexEntry;

/// Secondary index mapping filesystem paths to event entries.
#[derive(Debug)]
pub struct PathIndex {
    entries: BTreeMap<String, Vec<IndexEntry>>,
    index_dir: Option<PathBuf>,
}

impl PathIndex {
    /// Creates an in-memory-only index with no disk backing.
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            index_dir: None,
        }
    }

    /// Creates an index backed by files under `index_dir`.
    ///
    /// The directory is created if it does not exist. Existing `.idx`
    /// files are **not** loaded; call [`rebuild`](Self::rebuild) for that.
    ///
    /// # Errors
    ///
    /// Returns an error if `index_dir` cannot be created.
    pub fn with_dir(index_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&index_dir).with_context(|| {
            format!(
                "failed to create path index dir: {}",
                index_dir.display()
            )
        })?;
        Ok(Self {
            entries: BTreeMap::new(),
            index_dir: Some(index_dir),
        })
    }

    /// Records that event `seq` of `event_type` touched `path`.
    ///
    /// Appends the entry to both the in-memory map and the on-disk
    /// index file (when a directory is configured).
    ///
    /// # Errors
    ///
    /// Returns an error if the disk append fails.
    pub fn insert(
        &mut self,
        path: &str,
        seq: u64,
        event_type: &str,
    ) -> Result<()> {
        let entry = IndexEntry {
            seq,
            event_type: event_type.to_owned(),
        };
        self.entries
            .entry(path.to_owned())
            .or_default()
            .push(entry.clone());

        if let Some(dir) = &self.index_dir {
            append_to_file(dir, path, &entry)?;
        }
        Ok(())
    }

    /// Returns all entries for an exact `path`.
    pub fn lookup(&self, path: &str) -> &[IndexEntry] {
        self.entries
            .get(path)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Returns entries for every path sharing `prefix`.
    pub fn lookup_prefix(&self, prefix: &str) -> Vec<(&str, &[IndexEntry])> {
        self.entries
            .range(prefix.to_owned()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect()
    }

    /// Rebuilds the in-memory index from on-disk `.idx` files.
    ///
    /// # Errors
    ///
    /// Returns an error if any index file cannot be read.
    pub fn rebuild_from_disk(&mut self) -> Result<()> {
        let Some(dir) = &self.index_dir else {
            return Ok(());
        };
        self.entries.clear();
        let read_dir = fs::read_dir(dir).with_context(|| {
            format!("failed to read path index dir: {}", dir.display())
        })?;
        for entry in read_dir {
            let entry = entry?;
            let file_path = entry.path();
            if file_path.extension().is_some_and(|e| e == "idx") {
                load_index_file(&file_path, &mut self.entries)?;
            }
        }
        Ok(())
    }

    /// Returns the total number of indexed paths.
    pub fn path_count(&self) -> usize {
        self.entries.len()
    }

    /// Returns the total number of index entries across all paths.
    pub fn entry_count(&self) -> usize {
        self.entries.values().map(Vec::len).sum()
    }
}

impl Default for PathIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Deterministic hex hash of `path` used as the index filename.
fn path_hash(path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)
}

/// Appends one entry to the on-disk index file for `path`.
fn append_to_file(
    dir: &Path,
    path: &str,
    entry: &IndexEntry,
) -> Result<()> {
    let hash = path_hash(path);
    let file_path = dir.join(format!("{hash}.idx"));
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
        .with_context(|| {
            format!(
                "failed to open path index file: {}",
                file_path.display()
            )
        })?;
    // Format: path\tseq\tevent_type\n
    writeln!(file, "{path}\t{}\t{}", entry.seq, entry.event_type)?;
    Ok(())
}

/// Loads entries from a single `.idx` file into `map`.
fn load_index_file(
    file_path: &Path,
    map: &mut BTreeMap<String, Vec<IndexEntry>>,
) -> Result<()> {
    let file = File::open(file_path).with_context(|| {
        format!(
            "failed to open path index file: {}",
            file_path.display()
        )
    })?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let path = parts[0].to_owned();
        let Ok(seq) = parts[1].parse::<u64>() else {
            tracing::warn!(
                file = %file_path.display(),
                raw = parts[1],
                "skipping path index entry with malformed seq"
            );
            continue;
        };
        let event_type = parts[2].to_owned();
        map.entry(path)
            .or_default()
            .push(IndexEntry { seq, event_type });
    }
    Ok(())
}

#[cfg(test)]
#[path = "path_index_tests.rs"]
mod tests;

// Rust guideline compliant 2026-02-21

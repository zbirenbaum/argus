//! Maps event type tags to the sequence numbers that carry them.
//!
//! Disk format: `/data/indexes/type/{type}.idx`, one sequence number
//! per line.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Secondary index mapping event type tags to sequence numbers.
#[derive(Debug)]
pub struct TypeIndex {
    entries: BTreeMap<String, Vec<u64>>,
    index_dir: Option<PathBuf>,
}

impl TypeIndex {
    /// Creates an in-memory-only type index.
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            index_dir: None,
        }
    }

    /// Creates a type index backed by files under `index_dir`.
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation fails.
    pub fn with_dir(index_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&index_dir).with_context(|| {
            format!(
                "failed to create type index dir: {}",
                index_dir.display()
            )
        })?;
        Ok(Self {
            entries: BTreeMap::new(),
            index_dir: Some(index_dir),
        })
    }

    /// Records that event `seq` has type `event_type`.
    ///
    /// # Errors
    ///
    /// Returns an error if disk write fails.
    pub fn insert(
        &mut self,
        event_type: &str,
        seq: u64,
    ) -> Result<()> {
        self.entries
            .entry(event_type.to_owned())
            .or_default()
            .push(seq);

        if let Some(dir) = &self.index_dir {
            append_seq(dir, event_type, seq)?;
        }
        Ok(())
    }

    /// Returns all sequence numbers for a given event type.
    pub fn lookup(&self, event_type: &str) -> &[u64] {
        self.entries
            .get(event_type)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Returns the number of distinct event types indexed.
    pub fn type_count(&self) -> usize {
        self.entries.len()
    }

    /// Returns the total number of entries across all types.
    pub fn entry_count(&self) -> usize {
        self.entries.values().map(Vec::len).sum()
    }

    /// Returns an iterator over all `(event_type, seqs)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[u64])> {
        self.entries
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    /// Rebuilds in-memory state from on-disk `.idx` files.
    ///
    /// # Errors
    ///
    /// Returns an error if any file cannot be read.
    pub fn rebuild_from_disk(&mut self) -> Result<()> {
        let Some(dir) = &self.index_dir else {
            return Ok(());
        };
        self.entries.clear();

        let read_dir = fs::read_dir(dir).with_context(|| {
            format!("failed to read type index dir: {}", dir.display())
        })?;
        for entry in read_dir {
            let entry = entry?;
            let path = entry.path();
            if !path.extension().is_some_and(|e| e == "idx") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            else {
                continue;
            };
            let event_type = stem.to_owned();
            load_seqs(&path, &event_type, &mut self.entries)?;
        }
        Ok(())
    }
}

impl Default for TypeIndex {
    fn default() -> Self {
        Self::new()
    }
}

fn append_seq(
    dir: &Path,
    event_type: &str,
    seq: u64,
) -> Result<()> {
    let path = dir.join(format!("{event_type}.idx"));
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| {
            format!("failed to open type index: {}", path.display())
        })?;
    writeln!(file, "{seq}")?;
    Ok(())
}

fn load_seqs(
    path: &Path,
    event_type: &str,
    map: &mut BTreeMap<String, Vec<u64>>,
) -> Result<()> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let seqs = map.entry(event_type.to_owned()).or_default();
    for line in reader.lines() {
        let line = line?;
        if let Ok(seq) = line.trim().parse::<u64>() {
            seqs.push(seq);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "type_index_tests.rs"]
mod tests;

// Rust guideline compliant 2026-02-21

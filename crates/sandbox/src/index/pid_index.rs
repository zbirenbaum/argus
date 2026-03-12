//! Maps process IDs to the events they generated.
//!
//! Maintains two structures: a per-PID event list
//! (`BTreeMap<u32, Vec<IndexEntry>>`) and a process tree
//! (`BTreeMap<u32, ProcessInfo>`) recording parentage, binary,
//! argv, and the sequence range of the process lifetime.
//!
//! Disk format: `/data/indexes/pid/{pid}.idx`, one `seq\tevent_type`
//! line per entry. Process tree entries are stored as JSON in
//! `/data/indexes/pid/{pid}.meta`.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::path_index::IndexEntry;

/// Process lifecycle metadata for the process tree index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    /// Parent process ID.
    pub ppid: u32,
    /// Binary path from the most recent exec.
    pub binary: String,
    /// Command-line arguments from the most recent exec.
    pub argv: Vec<String>,
    /// Sequence number of the fork/exec that started this process.
    pub start_seq: u64,
    /// Sequence number of the exit event, if the process has exited.
    pub end_seq: Option<u64>,
}

/// Secondary index mapping PIDs to event entries and tree metadata.
#[derive(Debug)]
pub struct PidIndex {
    entries: BTreeMap<u32, Vec<IndexEntry>>,
    tree: BTreeMap<u32, ProcessInfo>,
    index_dir: Option<PathBuf>,
}

impl PidIndex {
    /// Creates an in-memory-only PID index.
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            tree: BTreeMap::new(),
            index_dir: None,
        }
    }

    /// Creates a PID index backed by files under `index_dir`.
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation fails.
    pub fn with_dir(index_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&index_dir).with_context(|| {
            format!(
                "failed to create pid index dir: {}",
                index_dir.display()
            )
        })?;
        Ok(Self {
            entries: BTreeMap::new(),
            tree: BTreeMap::new(),
            index_dir: Some(index_dir),
        })
    }

    /// Records that event `seq` of `event_type` was produced by `pid`.
    ///
    /// # Errors
    ///
    /// Returns an error if disk write fails.
    pub fn insert(
        &mut self,
        pid: u32,
        seq: u64,
        event_type: &str,
    ) -> Result<()> {
        let entry = IndexEntry {
            seq,
            event_type: event_type.to_owned(),
        };
        self.entries
            .entry(pid)
            .or_default()
            .push(entry);

        if let Some(dir) = &self.index_dir {
            append_entry(dir, pid, seq, event_type)?;
        }
        Ok(())
    }

    /// Records or updates process tree metadata for `pid`.
    ///
    /// # Errors
    ///
    /// Returns an error if the `.meta` file write fails.
    pub fn upsert_process(
        &mut self,
        pid: u32,
        info: ProcessInfo,
    ) -> Result<()> {
        if let Some(dir) = &self.index_dir {
            write_meta(dir, pid, &info)?;
        }
        self.tree.insert(pid, info);
        Ok(())
    }

    /// Sets the `end_seq` for `pid` if it exists in the tree.
    ///
    /// # Errors
    ///
    /// Returns an error if the `.meta` file write fails.
    pub fn mark_exit(&mut self, pid: u32, end_seq: u64) -> Result<()> {
        if let Some(info) = self.tree.get_mut(&pid) {
            info.end_seq = Some(end_seq);
            if let Some(dir) = &self.index_dir {
                write_meta(dir, pid, info)?;
            }
        }
        Ok(())
    }

    /// Returns all entries for a given PID.
    pub fn lookup(&self, pid: u32) -> &[IndexEntry] {
        self.entries
            .get(&pid)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Returns process tree info for `pid`.
    pub fn process_info(&self, pid: u32) -> Option<&ProcessInfo> {
        self.tree.get(&pid)
    }

    /// Returns the full process tree.
    pub fn process_tree(&self) -> &BTreeMap<u32, ProcessInfo> {
        &self.tree
    }

    /// Returns an iterator over all `(pid, entries)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &[IndexEntry])> {
        self.entries
            .iter()
            .map(|(&pid, entries)| (pid, entries.as_slice()))
    }

    /// Returns the number of distinct PIDs with event entries.
    pub fn pid_count(&self) -> usize {
        self.entries.len()
    }

    /// Returns the total number of index entries across all PIDs.
    pub fn entry_count(&self) -> usize {
        self.entries.values().map(Vec::len).sum()
    }

    /// Rebuilds in-memory state from on-disk files.
    ///
    /// # Errors
    ///
    /// Returns an error if any file cannot be read.
    pub fn rebuild_from_disk(&mut self) -> Result<()> {
        let Some(dir) = &self.index_dir else {
            return Ok(());
        };
        self.entries.clear();
        self.tree.clear();

        let read_dir = fs::read_dir(dir).with_context(|| {
            format!("failed to read pid index dir: {}", dir.display())
        })?;
        for entry in read_dir {
            let entry = entry?;
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            else {
                continue;
            };
            let Ok(pid) = stem.parse::<u32>() else {
                continue;
            };

            if path.extension().is_some_and(|e| e == "idx") {
                load_entries(&path, pid, &mut self.entries)?;
            } else if path.extension().is_some_and(|e| e == "meta") {
                load_meta(&path, pid, &mut self.tree)?;
            }
        }
        Ok(())
    }
}

impl Default for PidIndex {
    fn default() -> Self {
        Self::new()
    }
}

fn append_entry(
    dir: &Path,
    pid: u32,
    seq: u64,
    event_type: &str,
) -> Result<()> {
    let path = dir.join(format!("{pid}.idx"));
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| {
            format!("failed to open pid index: {}", path.display())
        })?;
    writeln!(file, "{seq}\t{event_type}")?;
    Ok(())
}

fn write_meta(
    dir: &Path,
    pid: u32,
    info: &ProcessInfo,
) -> Result<()> {
    let path = dir.join(format!("{pid}.meta"));
    let json = serde_json::to_string(info)
        .context("failed to serialize ProcessInfo")?;
    fs::write(&path, json).with_context(|| {
        format!("failed to write pid meta: {}", path.display())
    })
}

fn load_entries(
    path: &Path,
    pid: u32,
    map: &mut BTreeMap<u32, Vec<IndexEntry>>,
) -> Result<()> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let entries = map.entry(pid).or_default();
    for line in reader.lines() {
        let line = line?;
        let parts: Vec<&str> = line.splitn(2, '\t').collect();
        if parts.len() < 2 {
            continue;
        }
        let seq: u64 = parts[0].parse().unwrap_or(0);
        entries.push(IndexEntry {
            seq,
            event_type: parts[1].to_owned(),
        });
    }
    Ok(())
}

fn load_meta(
    path: &Path,
    pid: u32,
    tree: &mut BTreeMap<u32, ProcessInfo>,
) -> Result<()> {
    let data = fs::read_to_string(path)?;
    let info: ProcessInfo = serde_json::from_str(&data)
        .with_context(|| {
            format!("failed to parse pid meta: {}", path.display())
        })?;
    tree.insert(pid, info);
    Ok(())
}

#[cfg(test)]
#[path = "pid_index_tests.rs"]
mod tests;

// Rust guideline compliant 2026-02-21

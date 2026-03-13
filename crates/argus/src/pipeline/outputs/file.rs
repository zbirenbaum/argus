// Rust guideline compliant 2026-02-21
//! Rotating JSONL file output for the enriched event pipeline.
//!
//! `FileOutput` writes one JSON line per event into a named log file.
//! When the file grows beyond `max_size`, it is rotated: the current file
//! becomes `.1`, a previous `.1` becomes `.2`, and so on up to `max_files`.
//! Files beyond `max_files` are deleted to bound disk usage.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bytesize::ByteSize;

use crate::events::Event;
use crate::pipeline::output::Output;

/// Writes enriched events as JSONL, rotating when the file reaches `max_size`.
///
/// Rotation scheme: on overflow the current file is renamed to `<path>.1`,
/// existing `<path>.N` files are shifted to `<path>.N+1`, and files
/// beyond `max_files` are deleted. A fresh file is opened at `path`.
///
/// # Examples
///
/// ```ignore
/// use std::path::PathBuf;
/// use bytesize::ByteSize;
/// use crate::pipeline::outputs::FileOutput;
///
/// let mut out = FileOutput::new(PathBuf::from("/tmp/events.jsonl"),
///                               ByteSize::mb(100), 5).unwrap();
/// ```
#[derive(Debug)]
pub struct FileOutput {
    path: PathBuf,
    max_size: ByteSize,
    max_files: u32,
    writer: BufWriter<File>,
    current_size: u64,
}

impl FileOutput {
    /// Opens (or creates) the file at `path` in append mode.
    ///
    /// `max_size` is the per-file size cap before rotation is triggered.
    /// `max_files` is the maximum number of rotated files kept (`.1`–`.N`);
    /// older files are deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or its metadata cannot
    /// be read.
    pub fn new(path: PathBuf, max_size: ByteSize, max_files: u32) -> Result<Self> {
        let file = open_append(&path)?;
        let current_size = file
            .metadata()
            .with_context(|| format!("stat {}", path.display()))?
            .len();
        Ok(Self {
            path,
            max_size,
            max_files,
            writer: BufWriter::new(file),
            current_size,
        })
    }

    /// Rotates the log file: shifts `.N`→`.N+1`, deletes oldest, renames
    /// current file to `.1`, then opens a fresh file at `self.path`.
    ///
    /// # Errors
    ///
    /// Returns an error if any filesystem operation fails.
    fn rotate(&mut self) -> Result<()> {
        self.writer
            .flush()
            .with_context(|| format!("flush before rotate {}", self.path.display()))?;

        // Delete the file that would overflow max_files.
        let oldest = rotated_path(&self.path, self.max_files);
        if oldest.exists() {
            fs::remove_file(&oldest)
                .with_context(|| format!("delete {}", oldest.display()))?;
        }

        // Shift .N → .N+1 in reverse order so we never clobber a live file.
        for n in (1..self.max_files).rev() {
            let src = rotated_path(&self.path, n);
            let dst = rotated_path(&self.path, n + 1);
            if src.exists() {
                fs::rename(&src, &dst).with_context(|| {
                    format!("rename {} → {}", src.display(), dst.display())
                })?;
            }
        }

        // Rename the current file to .1.
        let dot1 = rotated_path(&self.path, 1);
        if self.path.exists() {
            fs::rename(&self.path, &dot1).with_context(|| {
                format!("rename {} → {}", self.path.display(), dot1.display())
            })?;
        }

        // Open a fresh file.
        let file = open_append(&self.path)?;
        self.writer = BufWriter::new(file);
        self.current_size = 0;
        Ok(())
    }
}

impl Output for FileOutput {
    fn emit(&mut self, event: &Event) -> Result<()> {
        if self.current_size >= self.max_size.as_u64() {
            self.rotate()?;
        }
        let json = serde_json::to_string(event)
            .with_context(|| format!("serialize event seq={}", event.seq))?;
        // +1 for the trailing newline written below.
        let line_len = (json.len() + 1) as u64;
        writeln!(self.writer, "{json}")
            .with_context(|| format!("write event seq={} to {}", event.seq, self.path.display()))?;
        self.current_size += line_len;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer
            .flush()
            .with_context(|| format!("flush {}", self.path.display()))
    }

    fn name(&self) -> &str {
        "file"
    }
}

/// Returns the rotated path for index `n` (e.g. `/tmp/events.jsonl.1`).
fn rotated_path(base: &Path, n: u32) -> PathBuf {
    let mut p = base.as_os_str().to_owned();
    p.push(format!(".{n}"));
    PathBuf::from(p)
}

/// Opens `path` for appending, creating it if it does not exist.
fn open_append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {} for append", path.display()))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::events::control::AgentStart;
    use crate::events::envelope::{Event, EventPayload};
    use crate::pipeline::output::Output;

    use super::FileOutput;
    use bytesize::ByteSize;

    fn make_event(seq: u64) -> Event {
        Event {
            seq,
            ts_monotonic: 0,
            ts_wall: "2026-01-01T00:00:00Z".to_owned(),
            agent_id: "test".to_owned(),
            vclock: None,
            redactions: Vec::new(),
            payload: EventPayload::AgentStart(AgentStart {
                agent_id: "test".to_owned(),
                supervisor_pid_host: None,
                supervisor_pid_ns: None,
                config_summary: "test".to_owned(),
                node: None,
                pod: None,
                container: None,
            }),
        }
    }

    #[test]
    fn writes_jsonl_line() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut out = FileOutput::new(path.clone(), ByteSize::mb(1), 5).unwrap();

        let event = make_event(1);
        out.emit(&event).unwrap();
        out.flush().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let line = content.lines().next().expect("at least one line");
        // Must parse as valid JSON and contain the sequence number.
        let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
        assert_eq!(v["seq"], 1);
    }

    #[test]
    fn rotates_at_max_size() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        // 100-byte cap forces rotation after the first event (~200+ bytes of JSON).
        let mut out = FileOutput::new(path.clone(), ByteSize::b(100), 5).unwrap();

        out.emit(&make_event(1)).unwrap();
        out.emit(&make_event(2)).unwrap();
        out.flush().unwrap();

        let mut dot1_os = path.as_os_str().to_owned();
        dot1_os.push(".1");
        let dot1 = std::path::PathBuf::from(dot1_os);
        assert!(
            dot1.exists(),
            ".1 rotated file must exist at {}",
            dot1.display()
        );
    }

    #[test]
    fn respects_max_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        // max_files=2: only .1 and .2 survive; .3 must not exist.
        let mut out = FileOutput::new(path.clone(), ByteSize::b(1), 2).unwrap();

        // Each emit triggers rotation (1-byte cap), so after 4 emits we have
        // rotated 4 times. max_files=2 means .3 and beyond are deleted.
        for seq in 1..=4 {
            out.emit(&make_event(seq)).unwrap();
        }
        out.flush().unwrap();

        let dot3_ext = format!("{}.3", path.display());
        let dot3 = std::path::PathBuf::from(&dot3_ext);
        assert!(
            !dot3.exists(),
            ".3 file must not exist when max_files=2 (found {})",
            dot3.display()
        );
    }
}

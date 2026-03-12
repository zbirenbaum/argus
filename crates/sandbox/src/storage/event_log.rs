//! Append-only JSONL event log with segment rotation.
//!
//! [`EventLog`] writes serialized [`Event`](crate::events::Event) records
//! as newline-delimited JSON to segment files under a configurable
//! directory. Segments rotate when they exceed a size threshold.
//! Completed segments are submitted to the upload pool for S3 upload.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use tracing::event;

use crate::config::DurabilityMode;
use crate::events::Event;
use crate::storage::upload_job::UploadJob;
use crate::storage::upload_pool::UploadPool;

/// 64 MiB default segment size threshold before rotation.
const DEFAULT_SEGMENT_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Append-only JSONL event log with automatic segment rotation.
///
/// Each segment is a JSONL file named `{seq}.jsonl`. When the current
/// segment exceeds `max_segment_bytes`, it is finalized (fsynced),
/// submitted for upload, and a new segment is opened.
#[derive(Debug)]
pub struct EventLog {
    agent_id: String,
    event_dir: PathBuf,
    max_segment_bytes: u64,
    segment_seq: AtomicU64,
    writer: Option<BufWriter<File>>,
    current_size: u64,
    current_path: PathBuf,
    durability: DurabilityMode,
}

impl EventLog {
    /// Create a new event log writing segments to `event_dir`.
    ///
    /// Creates `event_dir` if it does not exist. Opens the first
    /// segment file immediately.
    ///
    /// # Errors
    ///
    /// Returns an error if `event_dir` cannot be created or the
    /// initial segment file cannot be opened.
    pub fn new(
        agent_id: String,
        event_dir: PathBuf,
        durability: DurabilityMode,
    ) -> Result<Self> {
        Self::with_max_segment_bytes(
            agent_id,
            event_dir,
            durability,
            DEFAULT_SEGMENT_MAX_BYTES,
        )
    }

    /// Create with a custom segment size threshold.
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation or file open fails.
    pub fn with_max_segment_bytes(
        agent_id: String,
        event_dir: PathBuf,
        durability: DurabilityMode,
        max_segment_bytes: u64,
    ) -> Result<Self> {
        fs::create_dir_all(&event_dir).with_context(|| {
            format!("failed to create event dir: {}", event_dir.display())
        })?;

        let mut log = Self {
            agent_id,
            event_dir,
            max_segment_bytes,
            segment_seq: AtomicU64::new(0),
            writer: None,
            current_size: 0,
            current_path: PathBuf::new(),
            durability,
        };
        log.open_segment()?;
        Ok(log)
    }

    /// Append a serialized event to the current segment.
    ///
    /// Serializes `event` as a single JSON line followed by a newline.
    /// If `DurabilityMode::Local`, fsyncs after each append. Rotates
    /// the segment when the size threshold is exceeded.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization, writing, or fsyncing fails.
    pub fn append(
        &mut self,
        event_record: &Event,
        upload_pool: Option<&UploadPool>,
    ) -> Result<()> {
        let line = serde_json::to_string(event_record)
            .context("failed to serialize event to JSON")?;
        let line_bytes = line.len() as u64 + 1; // +1 for newline

        let writer = self
            .writer
            .as_mut()
            .context("event log writer not initialized")?;

        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        self.current_size += line_bytes;

        if self.durability == DurabilityMode::Local {
            self.fsync_current()?;
        }

        if self.current_size >= self.max_segment_bytes {
            self.rotate(upload_pool)?;
        }

        Ok(())
    }

    /// Force-flush and fsync the current segment to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the flush or fsync syscall fails.
    pub fn flush(&mut self) -> Result<()> {
        self.fsync_current()
    }

    /// Return the byte count written to the current segment.
    pub fn current_segment_size(&self) -> u64 {
        self.current_size
    }

    /// Return the current segment sequence number.
    pub fn current_segment_seq(&self) -> u64 {
        self.segment_seq.load(Ordering::Relaxed)
    }

    /// Finalize the current segment and submit it for upload.
    ///
    /// Called on explicit close or shutdown. Does not open a new
    /// segment afterward.
    ///
    /// # Errors
    ///
    /// Returns an error if fsyncing or reading the segment fails.
    pub fn finalize(
        &mut self,
        upload_pool: Option<&UploadPool>,
    ) -> Result<()> {
        self.fsync_current()?;
        self.submit_segment(upload_pool)?;
        self.writer = None;
        Ok(())
    }

    fn open_segment(&mut self) -> Result<()> {
        let seq = self.segment_seq.load(Ordering::Relaxed);
        let path = self.event_dir.join(format!("{seq}.jsonl"));

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| {
                format!("failed to open segment: {}", path.display())
            })?;

        self.writer = Some(BufWriter::new(file));
        self.current_size = 0;
        self.current_path = path;
        Ok(())
    }

    fn rotate(
        &mut self,
        upload_pool: Option<&UploadPool>,
    ) -> Result<()> {
        self.fsync_current()?;
        self.submit_segment(upload_pool)?;

        self.segment_seq.fetch_add(1, Ordering::Relaxed);
        self.open_segment()?;

        event!(
            name: "event_log.segment.rotated",
            tracing::Level::INFO,
            event_log.segment_seq = self.segment_seq.load(Ordering::Relaxed),
            event_log.agent_id = %self.agent_id,
            "segment rotated to {{event_log.segment_seq}}"
        );

        Ok(())
    }

    fn fsync_current(&mut self) -> Result<()> {
        let writer = self
            .writer
            .as_mut()
            .context("event log writer not initialized")?;
        writer.flush()?;
        writer.get_ref().sync_all().context("fsync segment failed")
    }

    fn submit_segment(
        &self,
        upload_pool: Option<&UploadPool>,
    ) -> Result<()> {
        let Some(pool) = upload_pool else {
            return Ok(());
        };

        if self.current_size == 0 {
            return Ok(());
        }

        let data = fs::read(&self.current_path).with_context(|| {
            format!(
                "failed to read segment for upload: {}",
                self.current_path.display()
            )
        })?;

        let seq = self.segment_seq.load(Ordering::Relaxed);
        let job = UploadJob::EventSegment {
            agent_id: self.agent_id.clone(),
            seq,
            data,
        };

        pool.submit(job).with_context(|| {
            format!("failed to submit segment {seq} for upload")
        })
    }
}

#[cfg(test)]
#[path = "event_log_tests.rs"]
mod tests;

// Rust guideline compliant 2026-02-21

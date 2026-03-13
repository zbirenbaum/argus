// Rust guideline compliant 2026-02-21
//! JSONL-based recorder and replay stream for raw ptrace stops.
//!
//! `RawStopRecorder` writes stops to a file as one JSON object per line.
//! `ReplayStream` reads that file back as a `futures::Stream` for
//! deterministic testing without a live ptrace session.

use std::io::{BufRead, BufReader, BufWriter, Lines, Write};
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::Result;
use futures::Stream;

use super::raw_stop::RawSyscallStop;

/// Appends serialized `RawSyscallStop` records to a JSONL file.
pub struct RawStopRecorder {
    writer: BufWriter<std::fs::File>,
}

impl RawStopRecorder {
    /// Open (or create) the file at `path` for append-only recording.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub fn new(path: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    /// Serialize `stop` as a JSON line. Silently skips serialization failures.
    pub fn record(&mut self, stop: &RawSyscallStop) {
        if let Ok(line) = serde_json::to_string(stop) {
            let _ = writeln!(self.writer, "{line}");
        }
    }

    /// Flush the underlying writer.
    ///
    /// # Errors
    ///
    /// Returns an error if the flush fails.
    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }
}

/// Reads a JSONL stop file back as a stream.
pub struct ReplayStream {
    lines: Lines<BufReader<std::fs::File>>,
}

impl ReplayStream {
    /// Open the JSONL file at `path` for replay.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        Ok(Self {
            lines: BufReader::new(file).lines(),
        })
    }
}

impl Stream for ReplayStream {
    type Item = RawSyscallStop;

    /// Reads lines synchronously; invalid JSON lines are skipped.
    ///
    /// Using `Poll::Ready` for every line is correct here because
    /// `ReplayStream` wraps a blocking file I/O source — it should
    /// never be used in a latency-sensitive async context.
    fn poll_next(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        loop {
            match self.lines.next() {
                Some(Ok(line)) => {
                    if let Ok(stop) = serde_json::from_str(&line) {
                        return Poll::Ready(Some(stop));
                    }
                    // Skip malformed lines silently.
                }
                Some(Err(_)) => continue,
                None => return Poll::Ready(None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use nix::unistd::Pid;

    use crate::pipeline::raw_stop::{StopType, SyscallArgs};

    fn make_stop(pid: i32, nr: u64) -> RawSyscallStop {
        RawSyscallStop {
            pid: Pid::from_raw(pid),
            stop_type: StopType::SyscallEntry {
                syscall_nr: nr,
                args: SyscallArgs::from_array([0; 6]),
            },
        }
    }

    #[test]
    fn record_and_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stops.jsonl");

        let mut rec = RawStopRecorder::new(&path).unwrap();
        rec.record(&make_stop(1, 10));
        rec.record(&make_stop(2, 20));
        rec.flush().unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let stops: Vec<_> = rt.block_on(async {
            let stream = ReplayStream::open(&path).unwrap();
            stream.collect().await
        });

        assert_eq!(stops.len(), 2);
        assert_eq!(stops[0].pid, Pid::from_raw(1));
        assert_eq!(stops[1].pid, Pid::from_raw(2));
    }
}

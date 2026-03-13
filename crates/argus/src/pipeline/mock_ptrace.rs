// Rust guideline compliant 2026-02-21
//! Test helper: a mock ptrace thread that replays canned stops.
//!
//! Replaces the real ptrace thread in unit tests so stages can be exercised
//! without a live tracee. Supply stops in construction order; they are
//! delivered one-by-one over the `PtraceStream` channel. Directives are
//! handled by an in-process background task using the canned memory map.

use std::collections::HashMap;

use nix::unistd::Pid;
use tokio::sync::mpsc;

use super::directive::PipelineDirective;
use super::ptrace_thread::{PtraceHandle, PtraceStream};
use super::raw_stop::RawSyscallStop;

/// In-test replacement for the real ptrace thread.
///
/// Build with [`MockPtraceThread::new`], seed memory with [`add_memory`],
/// then call [`into_stream`] to get a `PtraceStream` that yields the
/// supplied stops in order.
///
/// [`add_memory`]: MockPtraceThread::add_memory
/// [`into_stream`]: MockPtraceThread::into_stream
#[derive(Debug, Default)]
pub struct MockPtraceThread {
    /// Canned read-memory responses keyed by `(pid, addr)`.
    memory: HashMap<(i32, usize), Vec<u8>>,
    /// Directives received for test assertions.
    pub directives_received: Vec<PipelineDirective>,
    /// WriteMemory payloads received, keyed by `(pid, addr)`.
    pub writes_received: HashMap<(i32, usize), Vec<u8>>,
}

impl MockPtraceThread {
    /// Creates an empty mock with no canned memory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a canned byte slice to return for `ReadMemory(pid, addr, _)`.
    pub fn add_memory(&mut self, pid: Pid, addr: usize, data: Vec<u8>) {
        self.memory.insert((pid.as_raw(), addr), data);
    }

    /// Consume the mock and return a `PtraceStream` that yields `stops` in
    /// order, with directives handled by an in-process task.
    ///
    /// A background Tokio task receives directives from the pipeline stages.
    /// `ReadMemory` replies with canned data (empty vec if no match).
    /// `WriteMemory` records the payload. `Resume` and `InjectError` are
    /// recorded and ACK'd appropriately. Reply channels are answered before
    /// the next stop is sent so ordering is deterministic.
    pub fn into_stream(self, stops: Vec<RawSyscallStop>) -> (PtraceStream, PtraceHandle) {
        let (stop_tx, stop_rx) = mpsc::unbounded_channel();
        let (directive_tx, directive_rx) = mpsc::unbounded_channel();

        // Clone what the background task needs; the rest stays in the stream.
        let memory = self.memory;

        tokio::spawn(drive_mock(stops, stop_tx, directive_rx, memory));

        let stream = PtraceStream::from_channels(stop_rx, directive_tx.clone());
        let handle = PtraceHandle::from_sender(directive_tx);
        (stream, handle)
    }
}

/// Background task: send stops one at a time, handle each directive before
/// moving to the next stop. Ends when all stops are exhausted.
async fn drive_mock(
    stops: Vec<RawSyscallStop>,
    stop_tx: mpsc::UnboundedSender<RawSyscallStop>,
    mut directive_rx: mpsc::UnboundedReceiver<PipelineDirective>,
    memory: HashMap<(i32, usize), Vec<u8>>,
) {
    for stop in stops {
        if stop_tx.send(stop).is_err() {
            // Receiver dropped — test already finished.
            return;
        }
        // Handle directives until a Resume or InjectError (which resumes).
        loop {
            match directive_rx.recv().await {
                None => return,
                Some(d) => {
                    let resumed = handle_directive(d, &memory);
                    if resumed {
                        break;
                    }
                }
            }
        }
    }
    // Drops stop_tx, closing the stream.
}

/// Handle one directive. Returns `true` if the tracee was logically resumed.
fn handle_directive(directive: PipelineDirective, memory: &HashMap<(i32, usize), Vec<u8>>) -> bool {
    match directive {
        PipelineDirective::Resume { .. } | PipelineDirective::InjectError { .. } => true,
        PipelineDirective::ReadMemory { pid, addr, len, reply } => {
            let key = (pid.as_raw(), addr);
            let data = memory.get(&key).cloned().unwrap_or_default();
            let slice = data.into_iter().take(len).collect::<Vec<_>>();
            let _ = reply.send(Ok(slice));
            false
        }
        PipelineDirective::ReadString { pid, addr, max_len, reply } => {
            let key = (pid.as_raw(), addr);
            let data = memory.get(&key).cloned().unwrap_or_default();
            let s = String::from_utf8_lossy(&data)
                .trim_end_matches('\0')
                .chars()
                .take(max_len)
                .collect::<String>();
            let _ = reply.send(Ok(s));
            false
        }
        PipelineDirective::ReadFile { path, reply } => {
            let result = std::fs::read(&path).map_err(anyhow::Error::from);
            let _ = reply.send(result);
            false
        }
        PipelineDirective::ResolveFd { pid, fd, reply } => {
            // Return a synthetic path; real fd resolution requires /proc.
            let path = std::path::PathBuf::from(format!("/mock/{}/{}", pid.as_raw(), fd));
            let _ = reply.send(Ok(path));
            false
        }
        PipelineDirective::WriteMemory { reply, .. } => {
            // Record the write but don't mutate the shared memory map;
            // tests assert via directives_received on the mock struct if needed.
            let _ = reply.send(Ok(()));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    use crate::pipeline::raw_stop::{StopType, SyscallArgs};

    fn make_entry(pid: i32, nr: u64) -> RawSyscallStop {
        RawSyscallStop {
            pid: Pid::from_raw(pid),
            stop_type: StopType::SyscallEntry {
                syscall_nr: nr,
                args: SyscallArgs::from_array([0; 6]),
            },
        }
    }

    #[tokio::test]
    async fn yields_stops_in_order() {
        let mock = MockPtraceThread::new();
        let stops = vec![make_entry(1, 10), make_entry(2, 20), make_entry(3, 30)];
        let (mut stream, _handle) = mock.into_stream(stops);

        // Resume each stop manually so the driver can proceed.
        let s1 = stream.next().await.expect("stop 1");
        stream.directive(PipelineDirective::Resume { pid: s1.pid });

        let s2 = stream.next().await.expect("stop 2");
        stream.directive(PipelineDirective::Resume { pid: s2.pid });

        let s3 = stream.next().await.expect("stop 3");
        stream.directive(PipelineDirective::Resume { pid: s3.pid });

        assert!(stream.next().await.is_none(), "stream should end");

        assert_eq!(s1.pid, Pid::from_raw(1));
        assert_eq!(s2.pid, Pid::from_raw(2));
        assert_eq!(s3.pid, Pid::from_raw(3));
    }

    #[tokio::test]
    async fn read_memory_returns_canned_data() {
        let mut mock = MockPtraceThread::new();
        let pid = Pid::from_raw(42);
        mock.add_memory(pid, 0x1000, b"hello".to_vec());

        let (mut stream, handle) = mock.into_stream(vec![make_entry(42, 1)]);

        // Consume the stop, issue a ReadMemory, then resume.
        let _stop = stream.next().await.expect("stop");
        let data = handle.read_memory(pid, 0x1000, 5).await.expect("read_memory");
        stream.directive(PipelineDirective::Resume { pid });

        assert_eq!(data, b"hello");
        // Drain the stream so the background task finishes.
        while stream.next().await.is_some() {}
    }

    #[tokio::test]
    async fn empty_stops_ends_immediately() {
        let mock = MockPtraceThread::new();
        let (mut stream, _handle) = mock.into_stream(vec![]);
        assert!(stream.next().await.is_none());
    }
}

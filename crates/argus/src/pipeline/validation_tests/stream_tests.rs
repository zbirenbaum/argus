// Rust guideline compliant 2026-02-21
//! Mock-stream tests for validate.sh test 13 (child process reaping).
//!
//! These tests drive `MockPtraceThread` directly (without the full runner)
//! to verify fork/exit symmetry and that all stops are delivered in order.

use std::collections::HashSet;

use futures::StreamExt;
use nix::unistd::Pid;

use crate::pipeline::directive::PipelineDirective;
use crate::pipeline::mock_ptrace::MockPtraceThread;
use crate::pipeline::raw_stop::{RawSyscallStop, StopType, SyscallArgs};

fn entry(pid: i32, nr: u64) -> RawSyscallStop {
    RawSyscallStop {
        pid: Pid::from_raw(pid),
        stop_type: StopType::SyscallEntry {
            syscall_nr: nr,
            args: SyscallArgs::from_array([0; 6]),
        },
    }
}

/// All stops are delivered in order; the driver does not hang on Resume.
#[tokio::test]
async fn test_all_stops_delivered_in_order() {
    let parent = 100i32;
    let children = [201i32, 202, 203];
    let mut stops = vec![entry(parent, 0)];

    for &child in &children {
        stops.push(RawSyscallStop {
            pid: Pid::from_raw(parent),
            stop_type: StopType::Fork { parent: Pid::from_raw(parent), child: Pid::from_raw(child) },
        });
        stops.push(entry(child, 0));
        stops.push(RawSyscallStop {
            pid: Pid::from_raw(child),
            stop_type: StopType::Exit { pid: Pid::from_raw(child), exit_code: 0 },
        });
        stops.push(RawSyscallStop {
            pid: Pid::from_raw(parent),
            stop_type: StopType::Signal {
                pid: Pid::from_raw(parent),
                signal: nix::sys::signal::Signal::SIGCHLD as i32,
            },
        });
    }
    stops.push(RawSyscallStop {
        pid: Pid::from_raw(parent),
        stop_type: StopType::Exit { pid: Pid::from_raw(parent), exit_code: 0 },
    });

    let expected = stops.len();
    let (mut stream, _handle) = MockPtraceThread::new().into_stream(stops);

    let mut received = 0usize;
    while let Some(stop) = stream.next().await {
        received += 1;
        let signal = match &stop.stop_type {
            StopType::Signal { signal, .. } => nix::sys::signal::Signal::try_from(*signal).ok(),
            _ => None,
        };
        stream.directive(PipelineDirective::Resume {
            pid: stop.pid, trace_exit: false, signal,
        });
    }
    assert_eq!(received, expected, "all {} stops must be delivered without hanging", expected);
}

/// Every forked child PID has a matching exit stop — no zombies.
#[tokio::test]
async fn test_fork_exit_symmetry() {
    let children = [301i32, 302, 303, 304, 305];
    let mut stops = Vec::new();
    for &child in &children {
        stops.push(RawSyscallStop {
            pid: Pid::from_raw(10),
            stop_type: StopType::Fork { parent: Pid::from_raw(10), child: Pid::from_raw(child) },
        });
    }
    for &child in &children {
        stops.push(RawSyscallStop {
            pid: Pid::from_raw(child),
            stop_type: StopType::Exit { pid: Pid::from_raw(child), exit_code: 0 },
        });
    }

    let (mut stream, _handle) = MockPtraceThread::new().into_stream(stops);
    let mut forked: HashSet<i32> = HashSet::new();
    let mut exited: HashSet<i32> = HashSet::new();

    while let Some(stop) = stream.next().await {
        match &stop.stop_type {
            StopType::Fork { child, .. } => { forked.insert(child.as_raw()); }
            StopType::Exit { pid, .. } => { exited.insert(pid.as_raw()); }
            _ => {}
        }
        stream.directive(PipelineDirective::Resume { pid: stop.pid, trace_exit: false, signal: None });
    }
    assert_eq!(forked, exited, "every forked child must have a matching exit stop");
}

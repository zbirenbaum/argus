// Rust guideline compliant 2026-02-21
//! Whole-agent freeze: stop every traced process on demand.
//!
//! The pipeline holds one tracee at a time — the one stopped at the
//! syscall it is currently deciding on. Its siblings keep running until
//! they happen to trap. That is not a freeze: while an operator (or a
//! judge) decides whether a dangerous syscall may proceed, the rest of
//! the agent must not make progress.
//!
//! [`freeze_all`] closes that gap by sending `PTRACE_INTERRUPT` to every
//! live tracee. Interrupt-stops are left unreaped on purpose: the kernel
//! keeps each process stopped (zero CPU) until the ptrace thread reaps
//! the notification, and when it does the stop flows through the normal
//! pipeline path and is resumed like any other passthrough. Thawing is
//! therefore implicit — there is no separate "resume all" ptrace step.
//!
//! All functions here must run on the ptrace thread: ptrace requests are
//! only valid from the thread that attached to the tracee.

use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use nix::sys::ptrace;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use nix::unistd::Pid;
use tracing::{Level, event};

use super::tracee_registry::TraceeRegistry;

/// How long to wait for a single tracee to reach a stopped state.
const STOP_TIMEOUT: Duration = Duration::from_millis(500);

/// Poll interval while waiting for a tracee to stop.
const POLL_INTERVAL: Duration = Duration::from_micros(200);

/// Signal used to interrupt the ptrace thread's blocking `waitpid`.
const WAKE_SIGNAL: Signal = Signal::SIGUSR1;

/// Thread ID of the ptrace thread, published by [`register_wake_target`].
///
/// Zero means "not registered yet" — [`wake`] is then a no-op.
static PTRACE_TID: AtomicI32 = AtomicI32::new(0);

/// Publish the calling thread as the wake target and install the handler.
///
/// Called once from the ptrace thread before it enters its wait loop.
/// The handler is installed *without* `SA_RESTART` so that a delivered
/// [`WAKE_SIGNAL`] makes a blocking `waitpid` return `EINTR` instead of
/// silently restarting — that `EINTR` is what lets the thread notice
/// queued directives while no tracee stop is in flight.
pub(crate) fn register_wake_target() {
    PTRACE_TID.store(nix::unistd::gettid().as_raw(), Ordering::Release);

    let action = SigAction::new(
        SigHandler::Handler(wake_handler),
        SaFlags::empty(),
        SigSet::empty(),
    );
    // SAFETY: the handler body is empty, which is async-signal-safe.
    if let Err(e) = unsafe { sigaction(WAKE_SIGNAL, &action) } {
        event!(
            name: "ptrace_thread.wake_handler_failed",
            Level::WARN,
            error.message = %e,
            "failed to install wake handler; pause may be delayed until the next syscall stop",
        );
    }
}

/// Interrupt the ptrace thread's `waitpid` so it drains queued directives.
///
/// Safe to call from any thread and at any time — a spurious wake only
/// costs one extra loop iteration.
pub(crate) fn wake() {
    let tid = PTRACE_TID.load(Ordering::Acquire);
    if tid == 0 {
        return;
    }
    // SAFETY: tgkill with a live tid and a valid signal number; the
    // handler is a no-op. A stale tid returns ESRCH, which we ignore.
    unsafe {
        libc::syscall(
            libc::SYS_tgkill,
            libc::getpid(),
            tid,
            WAKE_SIGNAL as libc::c_int,
        );
    }
}

/// No-op handler: its only job is to make `waitpid` return `EINTR`.
extern "C" fn wake_handler(_sig: std::ffi::c_int) {}

/// Stop every live tracee and return those confirmed stopped.
///
/// `in_flight` is the tracee already held at a syscall stop by the
/// pipeline, if any — it is counted as stopped without being
/// interrupted. Tracees that have already exited are dropped from the
/// registry. Returns the PIDs known to be stopped, ascending.
pub(crate) fn freeze_all(registry: &TraceeRegistry, in_flight: Option<Pid>) -> Vec<Pid> {
    let mut stopped = Vec::new();

    for pid in registry.pids() {
        if Some(pid) == in_flight || is_stopped(pid) {
            stopped.push(pid);
            continue;
        }

        if let Err(e) = ptrace::interrupt(pid) {
            event!(
                name: "ptrace_thread.interrupt_failed",
                Level::DEBUG,
                pid = pid.as_raw(),
                error.message = %e,
                "PTRACE_INTERRUPT failed, dropping tracee from registry",
            );
            registry.remove(pid);
            continue;
        }

        if wait_until_stopped(pid, STOP_TIMEOUT) {
            stopped.push(pid);
        } else {
            event!(
                name: "ptrace_thread.freeze_timeout",
                Level::WARN,
                pid = pid.as_raw(),
                timeout_ms = STOP_TIMEOUT.as_millis() as u64,
                "tracee did not stop within the freeze timeout",
            );
        }
    }

    event!(
        name: "ptrace_thread.frozen",
        Level::INFO,
        stopped.count = stopped.len(),
        "froze all traced processes",
    );

    stopped
}

/// Whether `pid` is in a stopped state according to `/proc`.
///
/// `t` is ptrace-stop and `T` is job-control stop; both mean the
/// process consumes no CPU. A process that has exited reads as gone and
/// is reported as not stopped so callers can prune it.
pub(crate) fn is_stopped(pid: Pid) -> bool {
    matches!(proc_state(pid), Some('t') | Some('T'))
}

/// Read the single-character process state from `/proc/<pid>/stat`.
///
/// The state is the third whitespace-separated field, but the second
/// field (`comm`) is parenthesized and may itself contain spaces, so
/// the scan starts after the final `)`.
pub(crate) fn proc_state(pid: Pid) -> Option<char> {
    let raw = std::fs::read_to_string(format!("/proc/{}/stat", pid.as_raw())).ok()?;
    let after_comm = raw.rfind(')').map(|i| &raw[i + 1..])?;
    after_comm.split_whitespace().next()?.chars().next()
}

/// Poll `/proc` until `pid` is stopped or `timeout` elapses.
fn wait_until_stopped(pid: Pid, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if is_stopped(pid) {
            return true;
        }
        if proc_state(pid).is_none() {
            // Exited while we were waiting — nothing left to freeze.
            return false;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Answer `Freeze` directives so tests can exercise stages that freeze.
///
/// Spawns a task that replies to every [`PipelineDirective::Freeze`]
/// with an empty stopped-PID list — no real tracees exist in unit tests
/// — and forwards every other directive to the returned receiver, which
/// tests assert on as before.
#[cfg(test)]
pub(crate) fn spawn_freeze_responder(
    mut directives: tokio::sync::mpsc::UnboundedReceiver<super::directive::PipelineDirective>,
) -> tokio::sync::mpsc::UnboundedReceiver<super::directive::PipelineDirective> {
    use super::directive::PipelineDirective;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(directive) = directives.recv().await {
            match directive {
                PipelineDirective::Freeze { reply } => {
                    let _ = reply.send(Vec::new());
                }
                other => {
                    if tx.send(other).is_err() {
                        return;
                    }
                }
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn proc_state_of_self_is_not_stopped() {
        let me = Pid::from_raw(std::process::id() as i32);
        // Running or sleeping depending on what the test harness threads
        // are doing; either way the process is not stopped.
        assert!(matches!(proc_state(me), Some('R') | Some('S')));
        assert!(!is_stopped(me));
    }

    #[test]
    fn proc_state_of_missing_pid_is_none() {
        // PID 0 never has a /proc/0/stat entry.
        assert!(proc_state(Pid::from_raw(0)).is_none());
        assert!(!is_stopped(Pid::from_raw(0)));
    }

    #[test]
    fn sigstopped_child_reads_as_stopped() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = Pid::from_raw(child.id() as i32);

        nix::sys::signal::kill(pid, Signal::SIGSTOP).expect("SIGSTOP");
        assert!(wait_until_stopped(pid, STOP_TIMEOUT), "child should report stopped");
        assert!(is_stopped(pid));

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn freeze_all_prunes_dead_tracees() {
        let registry = TraceeRegistry::new();
        // A PID that cannot be interrupted (never a tracee of ours).
        registry.insert(Pid::from_raw(0x7FFF_FFFE));
        let stopped = freeze_all(&registry, None);
        assert!(stopped.is_empty());
        assert!(registry.is_empty(), "unreachable tracee should be pruned");
    }

    #[test]
    fn freeze_all_counts_in_flight_without_interrupting() {
        let registry = TraceeRegistry::new();
        let me = Pid::from_raw(std::process::id() as i32);
        registry.insert(me);
        let stopped = freeze_all(&registry, Some(me));
        assert_eq!(stopped, vec![me], "in-flight tracee counts as stopped");
    }

    #[test]
    fn wake_without_registered_target_is_noop() {
        // PTRACE_TID may be set by another test in this binary; either way
        // wake() must not panic or signal an unrelated process.
        wake();
    }

    #[test]
    fn register_wake_target_publishes_tid() {
        register_wake_target();
        assert_eq!(PTRACE_TID.load(Ordering::Acquire), nix::unistd::gettid().as_raw());
        wake();
    }
}

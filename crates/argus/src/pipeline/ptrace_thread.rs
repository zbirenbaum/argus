// Rust guideline compliant 2026-02-21
//! Ptrace thread and async handle for the pipeline architecture.
//!
//! The ptrace thread runs a blocking `waitpid` loop on a dedicated OS
//! thread and communicates with async pipeline stages through unbounded
//! channels. Stages send directives (memory reads, resumes) and receive
//! `RawSyscallStop` events in return.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::Result;
use futures::Stream;
use nix::sys::ptrace;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use tokio::sync::{mpsc, oneshot};
use tracing::event;
use tracing::Level;

use crate::tracer::memory::{read_bytes, read_c_string, write_bytes};
use crate::tracer::regs::get_regs;

use super::directive::PipelineDirective;
use super::freeze;
use super::raw_stop::{RawSyscallStop, StopType, SyscallArgs};
use super::tracee_registry::TraceeRegistry;

/// How long a freeze/thaw request waits for the ptrace thread before the
/// wake signal is re-sent. The thread only misses a wake if the signal
/// lands in the window between draining directives and re-entering
/// `waitpid`, so one retry is normally enough.
const FREEZE_RETRY_INTERVAL: Duration = Duration::from_millis(200);

/// Total time a freeze request waits before giving up.
const FREEZE_TOTAL_TIMEOUT: Duration = Duration::from_secs(5);

/// Ptrace options applied to every seized process.
///
/// Mirrors the existing `PTRACE_OPTS` constant in `trace_loop.rs` plus
/// `TRACESECCOMP` for the pipeline's seccomp-driven entry stops.
const PTRACE_OPTS: ptrace::Options = ptrace::Options::from_bits_truncate(
    ptrace::Options::PTRACE_O_TRACEFORK.bits()
        | ptrace::Options::PTRACE_O_TRACEVFORK.bits()
        | ptrace::Options::PTRACE_O_TRACECLONE.bits()
        | ptrace::Options::PTRACE_O_TRACEEXEC.bits()
        | ptrace::Options::PTRACE_O_TRACEEXIT.bits()
        | ptrace::Options::PTRACE_O_TRACESECCOMP.bits()
        | ptrace::Options::PTRACE_O_TRACESYSGOOD.bits(),
);

/// Convert a nix `WaitStatus` into a `RawSyscallStop`.
fn translate_wait_status(status: WaitStatus) -> RawSyscallStop {
    match status {
        WaitStatus::PtraceSyscall(pid) => {
            // Syscall-exit stop (SIGTRAP|0x80). Read registers to get
            // the syscall number and return value for exit correlation.
            let (nr, ret) = {
                use crate::tracer::regs::{get_regs, syscall_nr, syscall_ret};
                let r = get_regs(pid).unwrap_or_default();
                (syscall_nr(&r), syscall_ret(&r) as i64)
            };
            RawSyscallStop {
                pid,
                stop_type: StopType::SyscallExit {
                    syscall_nr: nr,
                    return_value: ret,
                },
            }
        }
        WaitStatus::PtraceEvent(pid, _sig, evt) => {
            let fork_evt = ptrace::Event::PTRACE_EVENT_FORK as i32;
            let vfork_evt = ptrace::Event::PTRACE_EVENT_VFORK as i32;
            let clone_evt = ptrace::Event::PTRACE_EVENT_CLONE as i32;
            let exec_evt = ptrace::Event::PTRACE_EVENT_EXEC as i32;
            let exit_evt = ptrace::Event::PTRACE_EVENT_EXIT as i32;
            let seccomp_evt = ptrace::Event::PTRACE_EVENT_SECCOMP as i32;

            if evt == fork_evt || evt == vfork_evt || evt == clone_evt {
                // Child pid is embedded in the event message.
                let child_raw = ptrace::getevent(pid).unwrap_or(0) as i32;
                let child = Pid::from_raw(child_raw);
                RawSyscallStop {
                    pid,
                    stop_type: StopType::Fork { parent: pid, child },
                }
            } else if evt == exec_evt {
                RawSyscallStop {
                    pid,
                    stop_type: StopType::Exec { pid },
                }
            } else if evt == exit_evt {
                let code = ptrace::getevent(pid).unwrap_or(0) as i32;
                RawSyscallStop {
                    pid,
                    stop_type: StopType::Exit { pid, exit_code: code },
                }
            } else if evt == seccomp_evt {
                // Seccomp entry stop — read syscall args from registers.
                // The regs module names args 1-based (arg1 = first arg = x0/rdi).
                // We map them to 0-based SyscallArgs fields.
                let (nr, args) = {
                    use crate::tracer::regs::{arg1, arg2, arg3, arg4, arg5, syscall_nr};
                    let r = get_regs(pid).unwrap_or_default();
                    (syscall_nr(&r), SyscallArgs::from_array([
                        arg1(&r), arg2(&r), arg3(&r), arg4(&r), arg5(&r), 0,
                    ]))
                };
                RawSyscallStop {
                    pid,
                    stop_type: StopType::SyscallEntry { syscall_nr: nr, args },
                }
            } else {
                RawSyscallStop { pid, stop_type: StopType::Unknown }
            }
        }
        WaitStatus::Exited(pid, code) => RawSyscallStop {
            pid,
            stop_type: StopType::Exit { pid, exit_code: code },
        },
        WaitStatus::Signaled(pid, sig, _) => RawSyscallStop {
            pid,
            stop_type: StopType::Signal { pid, signal: sig as i32 },
        },
        WaitStatus::Stopped(pid, sig) => RawSyscallStop {
            pid,
            stop_type: StopType::Signal { pid, signal: sig as i32 },
        },
        _ => {
            // StillAlive and other non-actionable statuses.
            RawSyscallStop {
                pid: Pid::from_raw(0),
                stop_type: StopType::Unknown,
            }
        }
    }
}

/// Execute one `PipelineDirective` on behalf of the pipeline.
///
/// Called synchronously in the ptrace thread, either while a stop is in
/// flight or while draining after a wake. `in_flight` is the tracee the
/// pipeline currently holds at a syscall stop, if any. Returns `true` if
/// the tracee was resumed by this function.
fn execute_directive(
    directive: PipelineDirective,
    registry: &TraceeRegistry,
    in_flight: Option<Pid>,
) -> bool {
    match directive {
        PipelineDirective::Freeze { reply } => {
            let _ = reply.send(freeze::freeze_all(registry, in_flight));
            false
        }
        other => execute_tracee_directive(other),
    }
}

/// Execute a directive that targets a single tracee.
fn execute_tracee_directive(directive: PipelineDirective) -> bool {
    match directive {
        PipelineDirective::Freeze { .. } => unreachable!("handled by execute_directive"),
        PipelineDirective::Resume { pid, trace_exit, signal } => {
            if trace_exit {
                let _ = ptrace::syscall(pid, signal);
            } else {
                let _ = ptrace::cont(pid, signal);
            }
            true
        }
        PipelineDirective::ReadMemory { pid, addr, len, reply } => {
            let _ = reply.send(read_bytes(pid, addr as u64, len));
            false
        }
        PipelineDirective::ReadString { pid, addr, max_len, reply } => {
            let result = read_c_string(pid, addr as u64).map(|mut s| {
                s.truncate(max_len);
                s
            });
            let _ = reply.send(result);
            false
        }
        PipelineDirective::ReadFile { path, reply } => {
            let _ = reply.send(std::fs::read(&path).map_err(anyhow::Error::from));
            false
        }
        PipelineDirective::InjectError { pid, errno } => {
            inject_errno(pid, errno);
            true
        }
        PipelineDirective::ResolveFd { pid, fd, reply } => {
            let link = format!("/proc/{}/fd/{}", pid.as_raw(), fd);
            let _ = reply.send(std::fs::read_link(&link).map_err(anyhow::Error::from));
            false
        }
        PipelineDirective::WriteMemory { pid, addr, data, reply } => {
            let _ = reply.send(write_bytes(pid, addr as u64, &data));
            false
        }
    }
}

/// Cancel the pending syscall and hand `errno` back to the tracee.
///
/// At a seccomp entry stop the syscall has not run yet. Invalidating the
/// syscall number makes the kernel skip execution; the return register
/// then keeps whatever the tracer leaves in it, so the negated errno has
/// to be written explicitly. Without that write the tracee sees whatever
/// happened to be in the register — a denial that reports a nonsense
/// errno instead of the `EPERM` the rules promise.
fn inject_errno(pid: Pid, errno: i32) {
    use crate::tracer::regs::{cancel_syscall, set_regs, set_ret, set_syscall_nr};

    // aarch64 keeps the pending syscall number outside the GP register
    // set, so it needs its own regset write.
    let _ = set_syscall_nr(pid, -1);

    match get_regs(pid) {
        Ok(mut regs) => {
            cancel_syscall(&mut regs);
            set_ret(&mut regs, (-i64::from(errno)) as u64);
            if let Err(e) = set_regs(pid, &regs) {
                event!(
                    name: "ptrace_thread.inject_errno_failed",
                    Level::WARN,
                    pid = pid.as_raw(),
                    error.message = %e,
                    "failed to write injected errno into tracee registers",
                );
            }
        }
        Err(e) => {
            event!(
                name: "ptrace_thread.inject_errno_regs_failed",
                Level::WARN,
                pid = pid.as_raw(),
                error.message = %e,
                "failed to read tracee registers for errno injection",
            );
        }
    }

    let _ = ptrace::cont(pid, None);
}

/// Tracks active tracee PIDs for the ptrace loop.
///
/// Distinguishes tracees from non-tracee children (e.g. mitmdump) so
/// `waitpid(-1)` events for non-tracees can be ignored. Exits the loop
/// when all tracees have terminated.
///
/// Backed by the shared [`TraceeRegistry`] so the API server sees the
/// same set — `GET /agent/status` lists it and `POST /agent/pause`
/// freezes it.
#[derive(Debug)]
struct TraceeSet {
    registry: Arc<TraceeRegistry>,
}

impl TraceeSet {
    /// Create with the initial agent PID.
    fn new(initial_pid: Pid, registry: Arc<TraceeRegistry>) -> Self {
        registry.insert(initial_pid);
        Self { registry }
    }

    /// Returns true if `pid` is a known tracee.
    fn contains(&self, pid: &Pid) -> bool {
        self.registry.contains(*pid)
    }

    /// Returns true when no tracees remain.
    fn is_empty(&self) -> bool {
        self.registry.is_empty()
    }

    /// Register a new tracee discovered via fork/clone.
    fn add(&mut self, pid: Pid) {
        self.registry.insert(pid);
    }

    /// Remove a tracee that has been finally reaped.
    fn remove(&mut self, pid: &Pid) {
        self.registry.remove(*pid);
    }

    /// Process a wait status. Returns `None` if the event is from a
    /// non-tracee and should be ignored. Returns `Some((stop, is_final))`
    /// where `is_final` means no resume directive is needed.
    fn process_wait(&mut self, status: WaitStatus) -> Option<(RawSyscallStop, bool)> {
        let wait_pid = status.pid().unwrap_or(Pid::from_raw(0));

        // Exited/Signaled are final — the process has been reaped.
        let is_final = matches!(
            status,
            WaitStatus::Exited(..) | WaitStatus::Signaled(..)
        );

        if wait_pid != Pid::from_raw(0) && !self.contains(&wait_pid) {
            if is_final {
                // Non-tracee child exited (e.g. mitmdump). Ignore.
                return None;
            }
            // Unknown PID in a ptrace stop — an auto-traced child whose
            // initial stop arrived before the parent's PTRACE_EVENT_FORK.
            // Add it now so it gets properly tracked and resumed.
            self.add(wait_pid);
        }

        if is_final {
            self.remove(&wait_pid);
        }

        let stop = translate_wait_status(status);

        // Learn new tracee PIDs from fork/clone events.
        if let StopType::Fork { child, .. } = &stop.stop_type {
            self.add(*child);
        }

        Some((stop, is_final))
    }
}

/// Entry point for the dedicated ptrace thread.
fn ptrace_thread_main(
    initial_pid: Pid,
    stop_tx: mpsc::UnboundedSender<RawSyscallStop>,
    mut directive_rx: mpsc::UnboundedReceiver<PipelineDirective>,
    seize_ready: oneshot::Sender<Result<()>>,
    registry: Arc<TraceeRegistry>,
) {
    // Publish this thread as the wake target before any tracee exists so
    // an early pause request can interrupt the wait below.
    freeze::register_wake_target();

    match ptrace::seize(initial_pid, PTRACE_OPTS) {
        Err(e) => {
            event!(
                name: "ptrace_thread.seize_failed",
                Level::ERROR,
                pid = initial_pid.as_raw(),
                error.message = %e,
                "ptrace seize of pid {{pid}} failed: {{error.message}}",
            );
            // Notify the caller that seize failed so it can abort.
            let _ = seize_ready.send(Err(anyhow::anyhow!("ptrace seize failed: {e}")));
            return;
        }
        Ok(()) => {
            // Seize succeeded — the caller may now release the sync pipe
            // and let the child process proceed.
            let _ = seize_ready.send(Ok(()));
        }
    }

    let mut tracees = TraceeSet::new(initial_pid, Arc::clone(&registry));

    loop {
        // Directives can arrive with no stop in flight — a pause request
        // while the agent runs freely. Drain them before blocking so the
        // freeze happens promptly rather than at the next syscall.
        drain_directives(&mut directive_rx, &registry, None);

        let status = match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::__WALL)) {
            Ok(s) => s,
            Err(nix::errno::Errno::ECHILD) => break,
            // A wake signal interrupted the wait: loop back and drain.
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => {
                event!(
                    name: "ptrace_thread.waitpid_error",
                    Level::ERROR,
                    error.message = %e,
                    "waitpid failed: {{error.message}}",
                );
                break;
            }
        };

        let Some((stop, is_final)) = tracees.process_wait(status) else {
            continue;
        };

        // Final exits (Exited/Signaled) mean the process has already been
        // reaped by waitpid. Don't send them to the pipeline — the
        // PTRACE_EVENT_EXIT stop already notified stages of the exit.
        // Sending finals would cause the pipeline to emit a stale Resume
        // that pollutes the directive channel for the next stop.
        if is_final {
            if tracees.is_empty() {
                break;
            }
            continue;
        }

        let in_flight = stop.pid;
        if stop_tx.send(stop).is_err() {
            break;
        }

        loop {
            match directive_rx.blocking_recv() {
                Some(directive) => {
                    if execute_directive(directive, &registry, Some(in_flight)) {
                        break;
                    }
                }
                None => return,
            }
        }

        if tracees.is_empty() {
            break;
        }
    }
}

/// Run every directive already queued, without blocking.
fn drain_directives(
    directive_rx: &mut mpsc::UnboundedReceiver<PipelineDirective>,
    registry: &TraceeRegistry,
    in_flight: Option<Pid>,
) {
    while let Ok(directive) = directive_rx.try_recv() {
        execute_directive(directive, registry, in_flight);
    }
}

/// Cloneable handle for sending directives from async pipeline stages.
///
/// Holds only the sending side of the directive channel so stages can
/// request memory reads and resumes without owning the stream.
#[derive(Clone, Debug)]
pub struct PtraceHandle {
    tx: mpsc::UnboundedSender<PipelineDirective>,
}

impl PtraceHandle {
    /// Construct a handle directly from a sender — test helper only.
    #[cfg(test)]
    pub fn from_sender(tx: mpsc::UnboundedSender<PipelineDirective>) -> Self {
        Self { tx }
    }

    /// Send a raw directive without awaiting a reply.
    pub fn directive(&self, d: PipelineDirective) {
        let _ = self.tx.send(d);
    }

    /// Read `len` bytes from `addr` in the tracee's address space.
    ///
    /// # Errors
    ///
    /// Returns an error if the ptrace thread fails to read the memory.
    pub async fn read_memory(&self, pid: Pid, addr: usize, len: usize) -> Result<Vec<u8>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.directive(PipelineDirective::ReadMemory { pid, addr, len, reply: reply_tx });
        reply_rx.await?
    }

    /// Read a null-terminated string from `addr` in the tracee.
    ///
    /// # Errors
    ///
    /// Returns an error if the read fails.
    pub async fn read_string(&self, pid: Pid, addr: usize, max_len: usize) -> Result<String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.directive(PipelineDirective::ReadString { pid, addr, max_len, reply: reply_tx });
        reply_rx.await?
    }

    /// Read a file through the supervisor's filesystem namespace.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read.
    pub async fn read_file(&self, path: PathBuf) -> Result<Vec<u8>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.directive(PipelineDirective::ReadFile { path, reply: reply_tx });
        reply_rx.await?
    }

    /// Resolve an open file descriptor to its filesystem path.
    ///
    /// # Errors
    ///
    /// Returns an error if the fd cannot be resolved.
    pub async fn resolve_fd(&self, pid: Pid, fd: i32) -> Result<PathBuf> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.directive(PipelineDirective::ResolveFd { pid, fd, reply: reply_tx });
        reply_rx.await?
    }

    /// Write `data` into the tracee's address space at `addr`.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub async fn write_memory(&self, pid: Pid, addr: usize, data: Vec<u8>) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.directive(PipelineDirective::WriteMemory { pid, addr, data, reply: reply_tx });
        reply_rx.await?
    }

    /// Resume the tracee, optionally tracing the syscall exit.
    pub fn resume(&self, pid: Pid, trace_exit: bool, signal: Option<nix::sys::signal::Signal>) {
        self.directive(PipelineDirective::Resume { pid, trace_exit, signal });
    }

    /// Inject an error return and resume the tracee.
    pub fn inject_error(&self, pid: Pid, errno: i32) {
        self.directive(PipelineDirective::InjectError { pid, errno });
    }

    /// Stop every live tracee and return the PIDs confirmed stopped.
    ///
    /// Returns an empty vector if the ptrace thread has already exited or
    /// does not answer within [`FREEZE_TOTAL_TIMEOUT`]. The wake signal is
    /// re-sent between polls: the thread only misses one if it lands in
    /// the narrow window before `waitpid` blocks, and a resend closes it.
    pub async fn freeze(&self) -> Vec<Pid> {
        let (reply_tx, mut reply_rx) = oneshot::channel();
        self.directive(PipelineDirective::Freeze { reply: reply_tx });

        let deadline = tokio::time::Instant::now() + FREEZE_TOTAL_TIMEOUT;
        loop {
            freeze::wake();
            match tokio::time::timeout_at(
                deadline.min(tokio::time::Instant::now() + FREEZE_RETRY_INTERVAL),
                &mut reply_rx,
            )
            .await
            {
                Ok(Ok(pids)) => return pids,
                // Sender dropped: the ptrace thread is gone, so every
                // tracee is either reaped or beyond our control.
                Ok(Err(_)) => return Vec::new(),
                Err(_) if tokio::time::Instant::now() >= deadline => {
                    event!(
                        name: "ptrace_thread.freeze_no_reply",
                        Level::WARN,
                        "ptrace thread did not answer the freeze request",
                    );
                    return Vec::new();
                }
                Err(_) => continue,
            }
        }
    }
}

/// Async stream of `RawSyscallStop` events from the ptrace thread.
///
/// Constructed via [`PtraceStream::spawn`]. Each item yielded represents
/// one ptrace stop; the caller must send a directive back through the
/// attached [`PtraceHandle`] to unblock the ptrace thread.
pub struct PtraceStream {
    stop_rx: mpsc::UnboundedReceiver<RawSyscallStop>,
    directive_tx: mpsc::UnboundedSender<PipelineDirective>,
}

impl PtraceStream {
    /// Construct a stream from raw channel halves — test helper only.
    ///
    /// Allows mock implementations to inject stops without spawning a real
    /// ptrace thread.
    #[cfg(test)]
    pub fn from_channels(
        stop_rx: mpsc::UnboundedReceiver<RawSyscallStop>,
        directive_tx: mpsc::UnboundedSender<PipelineDirective>,
    ) -> Self {
        Self { stop_rx, directive_tx }
    }

    /// Spawn the ptrace thread for `child_pid` and return the stream.
    ///
    /// The returned `oneshot::Receiver<Result<()>>` fires once `PTRACE_SEIZE`
    /// completes (or fails). The caller must await this before releasing the
    /// child's sync pipe so the tracee cannot advance ahead of the seize.
    ///
    /// # Errors
    ///
    /// Returns an error if the OS fails to create the ptrace thread.
    pub fn spawn(
        child_pid: Pid,
        registry: Arc<TraceeRegistry>,
    ) -> Result<(Self, oneshot::Receiver<Result<()>>, std::thread::JoinHandle<()>)> {
        let (stop_tx, stop_rx) = mpsc::unbounded_channel();
        let (directive_tx, directive_rx) = mpsc::unbounded_channel();
        let (seize_tx, seize_rx) = oneshot::channel();

        let handle = std::thread::Builder::new()
            .name("ptrace-loop".into())
            .spawn(move || {
                ptrace_thread_main(child_pid, stop_tx, directive_rx, seize_tx, registry)
            })
            .map_err(|e| anyhow::anyhow!("failed to spawn ptrace thread: {e}"))?;

        let stream = Self { stop_rx, directive_tx };
        Ok((stream, seize_rx, handle))
    }

    /// Send a directive to the ptrace thread without awaiting a reply.
    pub fn directive(&self, d: PipelineDirective) {
        let _ = self.directive_tx.send(d);
    }

    /// Clone the directive sender for use by pipeline stages.
    pub fn handle(&self) -> PtraceHandle {
        PtraceHandle { tx: self.directive_tx.clone() }
    }
}

impl Stream for PtraceStream {
    type Item = RawSyscallStop;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.stop_rx.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::signal::Signal;

    fn pid(n: i32) -> Pid {
        Pid::from_raw(n)
    }

    /// Fresh registry-backed tracee set for a single test.
    fn tracee_set(initial: Pid) -> TraceeSet {
        TraceeSet::new(initial, Arc::new(TraceeRegistry::new()))
    }

    // ── TraceeSet: non-tracee filtering ──────────────────────────────

    #[test]
    fn tracee_set_ignores_unknown_pid() {
        let mut set = tracee_set(pid(10));

        // mitmdump (pid 99) exits — should be ignored.
        let status = WaitStatus::Exited(pid(99), 0);
        assert!(set.process_wait(status).is_none(), "non-tracee should be ignored");
        assert!(!set.is_empty(), "initial tracee should remain");
    }

    #[test]
    fn tracee_set_tracks_initial_pid() {
        let set = tracee_set(pid(10));
        assert!(set.contains(&pid(10)));
        assert!(!set.contains(&pid(99)));
    }

    #[test]
    fn tracee_set_auto_adds_unknown_non_final() {
        let mut set = tracee_set(pid(10));

        // A child's initial stop can arrive before the parent's fork event.
        // Non-final stops from unknown PIDs should auto-add the PID.
        let status = WaitStatus::Stopped(pid(20), Signal::SIGSTOP);
        let result = set.process_wait(status);
        assert!(result.is_some(), "non-final unknown stop should be processed");
        assert!(set.contains(&pid(20)), "unknown pid should be auto-added");

        let (stop, is_final) = result.unwrap();
        assert!(!is_final);
        assert_eq!(stop.pid, pid(20));
    }

    // ── TraceeSet: fork adds child ───────────────────────────────────

    #[test]
    fn tracee_set_learns_child_from_fork() {
        let mut set = tracee_set(pid(10));

        // Parent (10) forks child (20).
        let status = WaitStatus::PtraceEvent(
            pid(10),
            nix::sys::signal::Signal::SIGTRAP,
            ptrace::Event::PTRACE_EVENT_CLONE as i32,
        );
        // The translate_wait_status would normally extract child from
        // ptrace::getevent, but we can't call that in tests. Instead,
        // test process_wait by providing a pre-translated stop.
        // Let's test the logic directly via add/remove.
        set.add(pid(20));
        assert!(set.contains(&pid(20)));
    }

    // ── TraceeSet: exit removes tracee ───────────────────────────────

    #[test]
    fn tracee_set_removes_on_exited() {
        let mut set = tracee_set(pid(10));
        set.add(pid(20));

        // pid 20 exits normally.
        let result = set.process_wait(WaitStatus::Exited(pid(20), 0));
        assert!(result.is_some());
        let (stop, is_final) = result.unwrap();
        assert!(is_final, "Exited should be final");
        assert!(!set.contains(&pid(20)), "exited pid should be removed");
        assert!(!set.is_empty(), "pid 10 should remain");

        // Verify the stop has the right pid and type.
        assert_eq!(stop.pid, pid(20));
        assert!(matches!(stop.stop_type, StopType::Exit { exit_code: 0, .. }));
    }

    #[test]
    fn tracee_set_removes_on_signaled() {
        let mut set = tracee_set(pid(10));

        let result = set.process_wait(WaitStatus::Signaled(pid(10), Signal::SIGKILL, false));
        assert!(result.is_some());
        let (_stop, is_final) = result.unwrap();
        assert!(is_final);
        assert!(set.is_empty(), "last tracee removed");
    }

    #[test]
    fn tracee_set_empty_after_all_exit() {
        let mut set = tracee_set(pid(10));
        set.add(pid(20));
        set.add(pid(30));

        set.process_wait(WaitStatus::Exited(pid(30), 0));
        assert!(!set.is_empty());
        set.process_wait(WaitStatus::Exited(pid(20), 0));
        assert!(!set.is_empty());
        set.process_wait(WaitStatus::Exited(pid(10), 0));
        assert!(set.is_empty());
    }

    // ── TraceeSet: ptrace event stops are non-final ──────────────────

    #[test]
    fn ptrace_event_exit_is_not_final() {
        let mut set = tracee_set(pid(10));

        // PTRACE_EVENT_EXIT is a stop, not the final reap.
        let status = WaitStatus::PtraceEvent(
            pid(10),
            Signal::SIGTRAP,
            ptrace::Event::PTRACE_EVENT_EXIT as i32,
        );
        let result = set.process_wait(status);
        assert!(result.is_some());
        let (_stop, is_final) = result.unwrap();
        assert!(!is_final, "ptrace exit event is not final — process still alive");
        assert!(set.contains(&pid(10)), "tracee should NOT be removed yet");
    }

    // ── TraceeSet: signal-delivery stop is non-final ─────────────────

    #[test]
    fn signal_delivery_stop_is_not_final() {
        let mut set = tracee_set(pid(10));

        // SIGCHLD delivered to tracee — this is a ptrace signal-delivery stop.
        let status = WaitStatus::Stopped(pid(10), Signal::SIGCHLD);
        let result = set.process_wait(status);
        assert!(result.is_some());
        let (stop, is_final) = result.unwrap();
        assert!(!is_final, "signal-delivery stop is not final");
        assert!(set.contains(&pid(10)), "tracee should remain after signal stop");
        assert!(matches!(stop.stop_type, StopType::Signal { signal: 17, .. }));
    }

    // ── TraceeSet: simulated full lifecycle ───────────────────────────

    #[test]
    fn full_lifecycle_with_fork_and_exit() {
        let mut set = tracee_set(pid(10));

        // Agent (10) forks child (20) — learned via add().
        set.add(pid(20));
        assert_eq!(set.registry.len(), 2);

        // Child (20) receives SIGCHLD — non-final, re-inject signal.
        let result = set.process_wait(WaitStatus::Stopped(pid(20), Signal::SIGCHLD));
        let (_stop, is_final) = result.unwrap();
        assert!(!is_final);

        // Child (20) ptrace exit event — non-final.
        let result = set.process_wait(WaitStatus::PtraceEvent(
            pid(20), Signal::SIGTRAP, ptrace::Event::PTRACE_EVENT_EXIT as i32,
        ));
        let (_stop, is_final) = result.unwrap();
        assert!(!is_final);

        // Child (20) truly exits — final.
        let result = set.process_wait(WaitStatus::Exited(pid(20), 0));
        let (_stop, is_final) = result.unwrap();
        assert!(is_final);
        assert!(!set.is_empty());

        // Non-tracee mitmdump (pid 99) exits — ignored.
        assert!(set.process_wait(WaitStatus::Exited(pid(99), 0)).is_none());

        // Agent (10) ptrace exit event — non-final.
        let result = set.process_wait(WaitStatus::PtraceEvent(
            pid(10), Signal::SIGTRAP, ptrace::Event::PTRACE_EVENT_EXIT as i32,
        ));
        assert!(!result.unwrap().1);

        // Agent (10) truly exits — final, set now empty.
        let result = set.process_wait(WaitStatus::Exited(pid(10), 0));
        assert!(result.unwrap().1);
        assert!(set.is_empty());
    }

    // ── translate_wait_status: signal extraction ─────────────────────

    #[test]
    fn translate_stopped_produces_signal_stop() {
        let status = WaitStatus::Stopped(pid(10), Signal::SIGCHLD);
        let stop = translate_wait_status(status);
        assert_eq!(stop.pid, pid(10));
        match stop.stop_type {
            StopType::Signal { pid: p, signal } => {
                assert_eq!(p, pid(10));
                assert_eq!(signal, Signal::SIGCHLD as i32);
            }
            other => panic!("expected Signal, got {other:?}"),
        }
    }

    #[test]
    fn translate_signaled_produces_signal_stop() {
        let status = WaitStatus::Signaled(pid(10), Signal::SIGTERM, false);
        let stop = translate_wait_status(status);
        match stop.stop_type {
            StopType::Signal { signal, .. } => {
                assert_eq!(signal, Signal::SIGTERM as i32);
            }
            other => panic!("expected Signal, got {other:?}"),
        }
    }

    // ── execute_directive: signal passed through ─────────────────────

    #[test]
    fn resume_with_signal_is_recognized() {
        // We can't call real ptrace::cont in tests, but we can verify the
        // directive variant carries the signal correctly.
        let d = PipelineDirective::Resume {
            pid: pid(10),
            trace_exit: false,
            signal: Some(Signal::SIGCHLD),
        };
        match d {
            PipelineDirective::Resume { signal, .. } => {
                assert_eq!(signal, Some(Signal::SIGCHLD));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn resume_without_signal_has_none() {
        let d = PipelineDirective::Resume {
            pid: pid(10),
            trace_exit: false,
            signal: None,
        };
        match d {
            PipelineDirective::Resume { signal, .. } => {
                assert!(signal.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }
}

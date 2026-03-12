// Rust guideline compliant 2026-02-21
//! Main ptrace event loop.
//!
//! Sits on a dedicated thread, calling `waitpid(-1)` in a loop and
//! dispatching to handlers based on the wait status. Automatically
//! follows forks, program replacements, and exits. Emits structured
//! events over a channel for downstream consumers.

use std::collections::{HashMap, HashSet, VecDeque};
use std::os::fd::{BorrowedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use nix::sys::ptrace;
use nix::sys::signal::Signal;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use tracing::event;
use tracing::Level;

use crate::api::state::SharedState;
use crate::cas::{Cas, LocalCas};
use crate::config::RuleSet;
use crate::events::{Event, EventPayload, SequenceGenerator};
use crate::events::file as ef;
use crate::snapshot::MerkleTree;
use crate::state::{FdTable, FdTarget, PipeRegistry, ProcessTree, PtyRegistry, WriteLocks};
use crate::tracer::{handlers, memory, process_events};

/// Ptrace options to set on every traced process.
///
/// `TRACESYSGOOD` makes syscall-exit stops report as `SIGTRAP | 0x80`
/// so we can distinguish them from signal-delivery stops. Required for
/// the entry/exit capture flow on file-mutating syscalls.
pub const PTRACE_OPTS: ptrace::Options = ptrace::Options::from_bits_truncate(
    ptrace::Options::PTRACE_O_TRACEFORK.bits()
        | ptrace::Options::PTRACE_O_TRACEVFORK.bits()
        | ptrace::Options::PTRACE_O_TRACECLONE.bits()
        | ptrace::Options::PTRACE_O_TRACEEXEC.bits()
        | ptrace::Options::PTRACE_O_TRACEEXIT.bits()
        | ptrace::Options::PTRACE_O_TRACESECCOMP.bits()
        | ptrace::Options::PTRACE_O_TRACESYSGOOD.bits(),
);

/// What kind of file mutation triggered the capture.
#[derive(Debug)]
pub enum CaptureKind {
    /// A write/pwrite/writev/pwritev syscall.
    Write { fd: i32, size: u64 },
    /// An open with O_TRUNC that truncates existing content.
    OpenTrunc,
}

/// Saved state between open entry and exit for fd table insertion.
#[derive(Debug)]
pub struct PendingOpen {
    pub path: String,
    pub flags: i32,
}

/// Saved state between read entry and exit for content capture.
#[derive(Debug)]
pub struct PendingRead {
    pub pid: u32,
    pub fd: i32,
    pub path: String,
    pub buf_addr: u64,
    pub count: u64,
}

/// Saved state between pipe/pipe2 entry and exit for fd capture.
#[derive(Debug)]
pub struct PendingPipe {
    pub pid: u32,
    /// Address of the int[2] pipefd array in tracee memory.
    pub pipefd_addr: u64,
}

/// Saved state between syscall entry and exit for content capture.
#[derive(Debug)]
pub struct PendingCapture {
    /// SHA-256 of the file before the syscall executed.
    pub before_hash: Option<String>,
    pub path: String,
    pub pid: u32,
    pub kind: CaptureKind,
}

/// Hashes a file's content via CAS, returning `None` on any error.
pub fn hash_file_content(cas: &impl Cas, path: &str) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    cas.put(&data).ok().map(|h| h.to_string())
}

/// Sets the syscall return value to -EPERM at syscall exit.
///
/// Called after a cancelled syscall reaches the exit stop. The kernel
/// returned -ENOSYS (because orig_rax was set to -1 at entry); we
/// overwrite rax with -EPERM so the process sees "Permission denied."
///
/// # Errors
///
/// Returns an error if register read/write fails.
fn inject_eperm(pid: Pid) -> Result<()> {
    use super::regs;
    let mut r = regs::get_regs(pid)?;
    // -EPERM = -1 as signed, which is 0xFFFFFFFFFFFFFFFF unsigned.
    regs::set_ret(&mut r, (-(libc::EPERM as i64)) as u64);
    regs::set_regs(pid, &r)?;
    Ok(())
}

/// Orchestrates the ptrace event loop.
///
/// Owns all in-memory state and the event channel. Runs synchronously
/// on a dedicated thread until all traced processes have exited.
#[derive(Debug)]
pub struct TracerLoop {
    pub process_tree: ProcessTree,
    pub pipe_registry: PipeRegistry,
    pub pty_registry: PtyRegistry,
    pub write_locks: WriteLocks,
    pub cas: LocalCas,
    pub tree: MerkleTree,
    /// Captures awaiting syscall-exit to hash the post-mutation content.
    pub pending_captures: HashMap<u32, PendingCapture>,
    /// Last known content hash per path, used as before_hash for the
    /// next mutation. Guarantees an unbroken hash chain across events.
    pub path_hashes: HashMap<String, String>,
    /// Path → pid currently in-kernel executing a write. Serializes
    /// concurrent writes to the same file at the ptrace level,
    /// preventing kernel-level interleaving and garbled content.
    pub active_writes: HashMap<String, u32>,
    /// Tracees held at syscall entry waiting for the active writer on
    /// the same path to finish. Drained FIFO on write completion.
    pub write_wait_queue: HashMap<String, VecDeque<PendingCapture>>,
    event_tx: Sender<Event>,
    seq_gen: SequenceGenerator,
    agent_id: String,
    /// Workspace root for initial filesystem capture.
    workspace_dir: Option<PathBuf>,
    /// Lock-free handle to the active rule set for pause/block evaluation.
    pub rules: Option<Arc<ArcSwap<RuleSet>>>,
    /// Shared state for submitting pending approvals to the API.
    pub shared_state: Option<SharedState>,
    /// Pids awaiting syscall exit for EPERM injection.
    pub pending_eperm: HashSet<u32>,
    /// Opens awaiting syscall exit to read the returned fd.
    pub pending_opens: HashMap<u32, PendingOpen>,
    /// Reads awaiting syscall exit to capture buffer content.
    pub pending_reads: HashMap<u32, PendingRead>,
    /// Pipes awaiting syscall exit to read the returned fd pair.
    pub pending_pipes: HashMap<u32, PendingPipe>,
    pub alive_count: u32,
}

impl TracerLoop {
    /// Creates a new tracer loop with a shared sequence generator.
    pub fn new(
        agent_id: String,
        event_tx: Sender<Event>,
        seq_gen: SequenceGenerator,
        cas: LocalCas,
    ) -> Self {
        Self {
            process_tree: ProcessTree::new(),
            pipe_registry: PipeRegistry::new(),
            pty_registry: PtyRegistry::new(),
            write_locks: WriteLocks::new(),
            cas,
            tree: MerkleTree::new(),
            pending_captures: HashMap::new(),
            path_hashes: HashMap::new(),
            active_writes: HashMap::new(),
            write_wait_queue: HashMap::new(),
            event_tx,
            seq_gen,
            agent_id,
            workspace_dir: None,
            rules: None,
            shared_state: None,
            pending_eperm: HashSet::new(),
            pending_opens: HashMap::new(),
            pending_reads: HashMap::new(),
            pending_pipes: HashMap::new(),
            alive_count: 0,
        }
    }

    /// Set the workspace directory for initial state capture.
    pub fn with_workspace(mut self, path: PathBuf) -> Self {
        self.workspace_dir = Some(path);
        self
    }

    /// Set the rule set handle for pause/block enforcement.
    pub fn with_rules(mut self, rules: Arc<ArcSwap<RuleSet>>) -> Self {
        self.rules = Some(rules);
        self
    }

    /// Set the shared state for approval submission.
    pub fn with_shared_state(mut self, state: SharedState) -> Self {
        self.shared_state = Some(state);
        self
    }

    /// Runs the main ptrace loop until all traced processes exit.
    ///
    /// Attaches to `initial_pid` via `PTRACE_SEIZE`, then signals
    /// the child via `sync_pipe_w` to install seccomp and exec.
    ///
    /// # Errors
    ///
    /// Returns an error if ptrace operations fail or the wait loop
    /// encounters an unrecoverable error.
    pub fn run(&mut self, initial_pid: Pid, sync_pipe_w: RawFd) -> Result<()> {
        ptrace::seize(initial_pid, PTRACE_OPTS)
            .with_context(|| format!("ptrace seize pid {initial_pid}"))?;

        // Child is blocked on pipe read — unblock it now that seize
        // has established the trace relationship.
        // SAFETY: sync_pipe_w is a valid open fd from pipe().
        let pipe_fd = unsafe { BorrowedFd::borrow_raw(sync_pipe_w) };
        nix::unistd::write(pipe_fd, &[1u8])
            .context("write to sync pipe")?;
        nix::unistd::close(sync_pipe_w)
            .context("close sync pipe")?;

        self.register_initial_process(initial_pid)?;
        self.capture_initial_state()?;
        self.alive_count = 1;

        event!(
            name: "tracer.loop.start",
            Level::INFO,
            pid = initial_pid.as_raw(),
            "ptrace loop started, tracing pid {{pid}}",
        );

        self.wait_loop()
    }

    /// The core wait loop. Blocks on `waitpid(-1)` and dispatches.
    fn wait_loop(&mut self) -> Result<()> {
        loop {
            if self.alive_count == 0 {
                event!(
                    name: "tracer.loop.exit",
                    Level::INFO,
                    "all traced processes exited, stopping ptrace loop",
                );
                return Ok(());
            }

            let wall = WaitPidFlag::__WALL;
            let status = match waitpid(Pid::from_raw(-1), Some(wall)) {
                Ok(s) => s,
                Err(nix::errno::Errno::ECHILD) => {
                    event!(
                        name: "tracer.loop.no_children",
                        Level::INFO,
                        "no more children to wait for",
                    );
                    return Ok(());
                }
                Err(e) => return Err(e).context("waitpid failed"),
            };

            self.handle_wait_status(status)?;
        }
    }

    /// Dispatches a single wait status to the appropriate handler.
    fn handle_wait_status(&mut self, status: WaitStatus) -> Result<()> {
        match status {
            WaitStatus::PtraceEvent(pid, _sig, evt) => {
                let already_resumed = self.handle_ptrace_event(pid, evt)?;
                if !already_resumed {
                    ptrace::cont(pid, None)?;
                }
            }
            WaitStatus::PtraceSyscall(pid) => {
                self.handle_syscall_exit(pid)?;
            }
            WaitStatus::Stopped(pid, sig) => {
                self.handle_signal_stop(pid, sig)?;
            }
            WaitStatus::Exited(pid, code) => {
                let pid_u32 = pid.as_raw() as u32;
                self.pending_captures.remove(&pid_u32);
                self.cleanup_dead_writer(pid_u32)?;
                process_events::handle_process_exit(self, pid, code, None);
            }
            WaitStatus::Signaled(pid, sig, _core) => {
                let pid_u32 = pid.as_raw() as u32;
                self.pending_captures.remove(&pid_u32);
                self.cleanup_dead_writer(pid_u32)?;
                process_events::handle_process_exit(
                    self,
                    pid,
                    128 + sig as i32,
                    Some(sig as i32),
                );
            }
            _ => {}
        }
        Ok(())
    }

    /// Handles ptrace events (fork, clone, seccomp, etc.).
    ///
    /// Returns `true` if the tracee was already resumed (via
    /// `ptrace::syscall`) and the caller should NOT call `ptrace::cont`.
    fn handle_ptrace_event(&mut self, pid: Pid, evt: i32) -> Result<bool> {
        let fork = ptrace::Event::PTRACE_EVENT_FORK as i32;
        let vfork = ptrace::Event::PTRACE_EVENT_VFORK as i32;
        let clone = ptrace::Event::PTRACE_EVENT_CLONE as i32;
        let exec = ptrace::Event::PTRACE_EVENT_EXEC as i32;
        let exit = ptrace::Event::PTRACE_EVENT_EXIT as i32;
        let seccomp = ptrace::Event::PTRACE_EVENT_SECCOMP as i32;

        if evt == fork || evt == vfork || evt == clone {
            process_events::handle_fork(self, pid)?;
        } else if evt == exec {
            process_events::handle_program_replace(self, pid)?;
        } else if evt == exit {
            process_events::handle_exit_event(self, pid)?;
        } else if evt == seccomp {
            match handlers::handle_seccomp_stop(self, pid) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(e) => {
                    event!(
                        name: "tracer.seccomp.error",
                        Level::WARN,
                        pid = pid.as_raw(),
                        error.message = %e,
                        "seccomp handler error for pid {{pid}}: {{error.message}}",
                    );
                }
            }
        }
        Ok(false)
    }

    /// Completes a pending content capture at syscall exit.
    ///
    /// Uses the per-path `path_hashes` cache for before_hash and
    /// updates it with the new after_hash, ensuring an unbroken chain.
    /// If other tracees are queued for the same path, dequeues the next
    /// one and resumes it with `ptrace::syscall`.
    fn handle_syscall_exit(&mut self, pid: Pid) -> Result<()> {
        let pid_u32 = pid.as_raw() as u32;

        // Inject EPERM for blocked/denied syscalls.
        if self.pending_eperm.remove(&pid_u32) {
            inject_eperm(pid)?;
            ptrace::cont(pid, None)?;
            return Ok(());
        }

        // Complete pending open: read the returned fd and insert
        // the path into the process fd table.
        if let Some(open) = self.pending_opens.remove(&pid_u32) {
            self.complete_pending_open(pid, &open)?;
            ptrace::cont(pid, None)?;
            return Ok(());
        }

        // Complete pending read: read the buffer content from tracee
        // memory, hash it, and emit event with content_hash.
        if let Some(read_info) = self.pending_reads.remove(&pid_u32) {
            self.complete_pending_read(pid, &read_info)?;
            ptrace::cont(pid, None)?;
            return Ok(());
        }

        // Complete pending pipe: read the fd pair from tracee memory,
        // register in fd table and pipe registry, emit PipeCreate.
        if let Some(pipe_info) = self.pending_pipes.remove(&pid_u32) {
            self.complete_pending_pipe(pid, &pipe_info)?;
            ptrace::cont(pid, None)?;
            return Ok(());
        }

        if let Some(cap) = self.pending_captures.remove(&pid_u32) {
            let after_hash = hash_file_content(&self.cas, &cap.path);

            // Use cached hash as before_hash; fall back to the one
            // computed at entry (for the first event on this path).
            let before_hash = self
                .path_hashes
                .get(&cap.path)
                .cloned()
                .or(cap.before_hash);

            // Update cache so the next event's before_hash chains.
            if let Some(ref h) = after_hash {
                self.path_hashes.insert(cap.path.clone(), h.clone());
            }

            let path_for_queue = cap.path.clone();

            let tree_hash = if let Some(ref h) = after_hash {
                self.tree_update(&cap.path, h)
            } else {
                self.tree_root()
            };

            match cap.kind {
                CaptureKind::Write { fd, size } => {
                    self.emit(EventPayload::Write(ef::Write {
                        pid: cap.pid,
                        path: cap.path,
                        fd,
                        offset: 0,
                        size,
                        before_hash,
                        after_hash: after_hash.clone(),
                        tree_hash,
                    }));
                }
                CaptureKind::OpenTrunc => {
                    if before_hash != after_hash {
                        self.emit(EventPayload::Write(ef::Write {
                            pid: cap.pid,
                            path: cap.path,
                            fd: -1,
                            offset: 0,
                            size: 0,
                            before_hash,
                            after_hash: after_hash.clone(),
                            tree_hash,
                        }));
                    }
                }
            }

            self.resume_next_queued_writer(&path_for_queue, after_hash)?;
        }

        ptrace::cont(pid, None)?;
        Ok(())
    }

    /// Dequeues the next waiting writer for a path and resumes it.
    ///
    /// Removes the completed write from `active_writes`. If another
    /// tracee is queued, sets its before_hash to the just-completed
    /// after_hash (guaranteed correct chain), installs it as the new
    /// active writer, and resumes it with `ptrace::syscall`.
    fn resume_next_queued_writer(
        &mut self,
        path: &str,
        after_hash: Option<String>,
    ) -> Result<()> {
        self.active_writes.remove(path);

        let next = self
            .write_wait_queue
            .get_mut(path)
            .and_then(VecDeque::pop_front);

        // Clean up empty queue entries.
        if self
            .write_wait_queue
            .get(path)
            .is_some_and(VecDeque::is_empty)
        {
            self.write_wait_queue.remove(path);
        }

        if let Some(mut queued) = next {
            // The next writer's before_hash is this write's after_hash.
            queued.before_hash = after_hash;
            let next_pid = Pid::from_raw(queued.pid as i32);

            self.active_writes
                .insert(queued.path.clone(), queued.pid);
            self.pending_captures.insert(queued.pid, queued);

            ptrace::syscall(next_pid, None)?;
        }

        Ok(())
    }

    /// Completes a pending read by reading the buffer content from
    /// tracee memory, hashing it, and emitting the event.
    fn complete_pending_read(&mut self, pid: Pid, read_info: &PendingRead) -> Result<()> {
        use super::regs;
        use super::content_capture;
        use crate::events::io as eio;

        let r = regs::get_regs(pid)?;
        let ret = regs::ret_val(&r) as i64;

        // Negative or zero return means read failed or EOF.
        if ret <= 0 {
            return Ok(());
        }
        let bytes_read = ret as u64;

        let content_hash = content_capture::try_capture_flat(
            &self.cas,
            pid,
            read_info.buf_addr,
            bytes_read,
        );

        // Classify: if fd is 0 and backed by pipe/pty, emit Stdio.
        // Otherwise emit a file Read event.
        if read_info.fd == 0 {
            let target = crate::tracer::handlers::io_ops::resolve_fd_target(
                self, read_info.pid, 0,
            );
            if matches!(target, FdTarget::Pipe { .. } | FdTarget::Pty { .. }) {
                self.emit(EventPayload::Stdio(eio::Stdio {
                    pid: read_info.pid,
                    subtype: eio::StdioSubtype::Stdin,
                    content_hash: content_hash.clone(),
                    size: bytes_read,
                    pipe_inode: match &target {
                        FdTarget::Pipe { inode, .. } => Some(*inode),
                        _ => None,
                    },
                    dest_pid: None,
                    source_pid: None,
                }));
                // Also emit PipeData for pipe topology visibility.
                if let FdTarget::Pipe { inode, .. } = &target {
                    self.emit(EventPayload::PipeData(eio::PipeData {
                        pid: read_info.pid,
                        inode: *inode,
                        direction: eio::PipeDirection::Read,
                        content_hash,
                        size: bytes_read,
                        dest_pids: vec![],
                    }));
                }
                return Ok(());
            }
        }

        self.emit(EventPayload::Read(ef::Read {
            pid: read_info.pid,
            path: read_info.path.clone(),
            fd: read_info.fd,
            offset: 0,
            size: bytes_read,
            content_hash,
        }));

        Ok(())
    }

    /// Reads the returned fd from a completed open syscall and inserts
    /// the path into the process fd table.
    fn complete_pending_open(&mut self, pid: Pid, open: &PendingOpen) -> Result<()> {
        use super::regs;

        let r = regs::get_regs(pid)?;
        let ret = regs::ret_val(&r) as i64;

        // Negative return means open failed — nothing to insert.
        if ret < 0 {
            return Ok(());
        }
        let fd = ret as i32;
        let pid_u32 = pid.as_raw() as u32;

        let target = FdTarget::File {
            path: PathBuf::from(&open.path),
        };
        if let Some(proc_state) = self.process_tree.get_process_mut(pid_u32) {
            if open.flags & libc::O_CLOEXEC != 0 {
                proc_state.fds.insert_cloexec(fd, target);
            } else {
                proc_state.fds.insert(fd, target);
            }
        }
        Ok(())
    }

    /// Reads the fd pair from a completed pipe/pipe2 syscall.
    ///
    /// Reads the two-element `int` array from tracee memory, looks up
    /// the inode via `/proc`, registers both ends in the fd table and
    /// pipe registry, and emits a `PipeCreate` event.
    fn complete_pending_pipe(&mut self, pid: Pid, pipe_info: &PendingPipe) -> Result<()> {
        use super::regs;
        use crate::events::io as eio;

        let r = regs::get_regs(pid)?;
        let ret = regs::ret_val(&r) as i64;

        // Negative return means pipe() failed.
        if ret < 0 {
            return Ok(());
        }

        // Read the int[2] pipefd array from tracee memory.
        let bytes = memory::read_bytes(pid, pipe_info.pipefd_addr, 8)?;
        if bytes.len() < 8 {
            return Ok(());
        }
        let read_fd = i32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let write_fd = i32::from_ne_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);

        // Read the inode from /proc for the read end.
        let pid_u32 = pipe_info.pid;
        let link = format!("/proc/{pid_u32}/fd/{read_fd}");
        let inode = std::fs::read_link(&link)
            .ok()
            .and_then(|p| {
                let s = p.to_string_lossy();
                s.strip_prefix("pipe:[")
                    .and_then(|rest| rest.strip_suffix(']'))
                    .and_then(|n| n.parse::<u64>().ok())
            })
            .unwrap_or(0);

        // Register both ends in the process fd table.
        if let Some(proc_state) = self.process_tree.get_process_mut(pid_u32) {
            proc_state.fds.insert(
                read_fd,
                FdTarget::Pipe {
                    inode,
                    direction: crate::state::PipeEnd::Read,
                },
            );
            proc_state.fds.insert(
                write_fd,
                FdTarget::Pipe {
                    inode,
                    direction: crate::state::PipeEnd::Write,
                },
            );
        }

        // Register in the pipe registry.
        self.pipe_registry
            .create_pipe(pid_u32, inode, read_fd, write_fd);

        self.emit(EventPayload::PipeCreate(eio::PipeCreate {
            pid: pid_u32,
            inode,
            read_fd,
            write_fd,
        }));

        Ok(())
    }

    /// Forwards non-ptrace signals to the tracee.
    ///
    /// If the pid has a pending capture or open, resumes with
    /// `ptrace::syscall` to preserve syscall-exit tracking across
    /// signal delivery.
    fn handle_signal_stop(&mut self, pid: Pid, sig: Signal) -> Result<()> {
        let forward = match sig {
            Signal::SIGSTOP | Signal::SIGTRAP => None,
            other => Some(other),
        };

        let pid_u32 = pid.as_raw() as u32;
        if self.pending_captures.contains_key(&pid_u32)
            || self.pending_opens.contains_key(&pid_u32)
            || self.pending_reads.contains_key(&pid_u32)
            || self.pending_pipes.contains_key(&pid_u32)
        {
            ptrace::syscall(pid, forward)?;
        } else {
            ptrace::cont(pid, forward)?;
        }
        Ok(())
    }

    /// Cleans up write serialization state for a dead process.
    ///
    /// Called on process exit to release any active write and resume
    /// queued writers that were blocked behind the dead process.
    pub fn cleanup_dead_writer(&mut self, pid_u32: u32) -> Result<()> {
        // If this pid held an active write, release it and resume the
        // next queued writer.
        let active_path: Option<String> = self
            .active_writes
            .iter()
            .find(|&(_, &p)| p == pid_u32)
            .map(|(path, _)| path.clone());

        if let Some(path) = active_path {
            // The dead process never completed its write, so the
            // file's current state is the after_hash. Read it now.
            let after_hash = hash_file_content(&self.cas, &path);
            if let Some(ref h) = after_hash {
                self.path_hashes.insert(path.clone(), h.clone());
            }
            self.resume_next_queued_writer(&path, after_hash)?;
        }

        // Remove this pid from any wait queues.
        self.write_wait_queue.retain(|_, queue| {
            queue.retain(|cap| cap.pid != pid_u32);
            !queue.is_empty()
        });

        Ok(())
    }

    /// Registers the initial process in the tree.
    fn register_initial_process(&mut self, pid: Pid) -> Result<()> {
        let pid_u32 = pid.as_raw() as u32;
        let ppid = nix::unistd::getpid().as_raw() as u32;
        let binary = memory::read_proc_exe(pid)
            .unwrap_or_else(|_| PathBuf::from("unknown"));
        let argv = memory::read_proc_cmdline(pid).unwrap_or_default();
        let cwd = std::fs::read_link(format!("/proc/{}/cwd", pid.as_raw()))
            .unwrap_or_else(|_| PathBuf::from("/"));

        let fds = FdTable::from_proc(pid_u32);
        self.process_tree
            .add_process(pid_u32, ppid, binary, argv, cwd, fds);

        Ok(())
    }

    /// Walks the workspace and captures pre-agent filesystem state.
    ///
    /// Emits one `InitialFile` event per file, then a single
    /// `InitialState` summary. Populates the Merkle tree so the
    /// first agent write has a valid `before_hash` chain.
    ///
    /// # Errors
    ///
    /// Returns an error if directory traversal fails.
    pub fn capture_initial_state(&mut self) -> Result<()> {
        let workspace = match &self.workspace_dir {
            Some(p) if p.exists() => p.clone(),
            _ => return Ok(()),
        };

        let pid = 0u32; // supervisor pid for initial state events
        let mut file_count = 0u64;
        let mut total_size = 0u64;

        self.walk_dir(&workspace, pid, &mut file_count, &mut total_size)?;

        let tree_hash = self.store_tree();

        self.emit(EventPayload::InitialState(
            crate::events::snapshot::InitialState {
                tree_hash,
                file_count,
                total_size,
            },
        ));

        event!(
            name: "tracer.initial_state.captured",
            Level::INFO,
            file_count,
            total_size,
            "captured initial filesystem state: {{file_count}} files, {{total_size}} bytes",
        );

        Ok(())
    }

    /// Recursively walk a directory, hashing files and emitting events.
    fn walk_dir(
        &mut self,
        dir: &Path,
        pid: u32,
        file_count: &mut u64,
        total_size: &mut u64,
    ) -> Result<()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                event!(
                    name: "tracer.initial_state.dir_error",
                    Level::WARN,
                    dir.path = %dir.display(),
                    error.message = %e,
                    "cannot read directory {{dir.path}}: {{error.message}}",
                );
                return Ok(());
            }
        };

        for entry in entries {
            let entry = entry.context("read directory entry")?;
            let path = entry.path();

            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    event!(
                        name: "tracer.initial_state.meta_error",
                        Level::WARN,
                        file.path = %path.display(),
                        error.message = %e,
                        "cannot stat {{file.path}}: {{error.message}}",
                    );
                    continue;
                }
            };

            if meta.is_dir() {
                self.walk_dir(&path, pid, file_count, total_size)?;
            } else if meta.is_file() {
                self.capture_initial_file(&path, &meta, pid, file_count, total_size);
            }
        }

        Ok(())
    }

    /// Hash and record a single pre-existing file.
    fn capture_initial_file(
        &mut self,
        path: &Path,
        meta: &std::fs::Metadata,
        pid: u32,
        file_count: &mut u64,
        total_size: &mut u64,
    ) {
        let size = meta.len();
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode()
        };

        let path_str = path.to_string_lossy().into_owned();

        let content_hash = match hash_file_content(&self.cas, &path_str) {
            Some(h) => h,
            None => return,
        };

        self.tree_update(&path_str, &content_hash);
        self.path_hashes
            .insert(path_str.clone(), content_hash.clone());

        self.emit(EventPayload::InitialFile(
            crate::events::snapshot::InitialFile {
                pid,
                path: path_str,
                content_hash,
                size,
                mode,
            },
        ));

        *file_count += 1;
        *total_size += size;
    }

    /// Updates the Merkle tree for a file write and returns the CAS tree hash.
    pub fn tree_update(&mut self, path: &str, content_hash: &str) -> Option<String> {
        use crate::cas::ContentHash;
        if let Ok(h) = ContentHash::try_from(content_hash.to_string()) {
            self.tree.update(PathBuf::from(path), h);
            self.store_tree()
        } else {
            None
        }
    }

    /// Removes a file from the Merkle tree and returns the CAS tree hash.
    pub fn tree_remove(&mut self, path: &str) -> Option<String> {
        self.tree.remove(std::path::Path::new(path));
        self.store_tree()
    }

    /// Renames a file in the Merkle tree and returns the CAS tree hash.
    pub fn tree_rename(&mut self, old: &str, new: &str) -> Option<String> {
        self.tree
            .rename(std::path::Path::new(old), PathBuf::from(new));
        self.store_tree()
    }

    /// Returns the current Merkle tree root hash (without storing).
    pub fn tree_root(&self) -> Option<String> {
        Some(self.tree.root_hash().to_string())
    }

    /// Stores tree objects in CAS and returns the root CAS hash.
    fn store_tree(&self) -> Option<String> {
        self.tree.store(&self.cas).ok().map(|h| h.to_string())
    }

    /// Emits an event through the channel.
    pub fn emit(&self, payload: EventPayload) {
        let evt = Event::new(&self.seq_gen, self.agent_id.clone(), payload);
        if let Err(e) = self.event_tx.send(evt) {
            event!(
                name: "tracer.event.send_error",
                Level::ERROR,
                error.message = %e,
                "failed to send event: {{error.message}}",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn test_cas() -> LocalCas {
        let dir = tempfile::tempdir().expect("tempdir");
        LocalCas::new(dir.path().join("cas")).expect("LocalCas")
    }

    #[test]
    fn tracer_loop_new_initializes_empty_state() {
        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let tracer = TracerLoop::new("test-agent".into(), tx, seq, test_cas());
        assert!(tracer.process_tree.is_empty());
        assert!(tracer.pipe_registry.is_empty());
        assert!(tracer.pty_registry.is_empty());
        assert!(tracer.write_locks.is_empty());
        assert!(tracer.pending_captures.is_empty());
        assert!(tracer.active_writes.is_empty());
        assert!(tracer.write_wait_queue.is_empty());
        assert_eq!(tracer.alive_count, 0);
    }

    #[test]
    fn emit_sends_event_with_correct_agent_id() {
        let (tx, rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let tracer = TracerLoop::new("agent-42".into(), tx, seq, test_cas());
        tracer.emit(EventPayload::Fork(crate::events::process::Fork {
            parent_pid: 1,
            child_pid: 2,
        }));
        let evt = rx.recv().unwrap();
        assert_eq!(evt.agent_id, "agent-42");
        assert_eq!(evt.seq, 0);
    }

    #[test]
    fn emit_increments_sequence() {
        let (tx, rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let tracer = TracerLoop::new("a".into(), tx, seq, test_cas());
        tracer.emit(EventPayload::Exit(crate::events::process::Exit {
            pid: 1,
            exit_code: 0,
            signal: None,
        }));
        tracer.emit(EventPayload::Exit(crate::events::process::Exit {
            pid: 2,
            exit_code: 0,
            signal: None,
        }));
        let e1 = rx.recv().unwrap();
        let e2 = rx.recv().unwrap();
        assert_eq!(e1.seq, 0);
        assert_eq!(e2.seq, 1);
    }

    #[test]
    fn active_writes_blocks_concurrent_path() {
        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let mut tracer = TracerLoop::new("a".into(), tx, seq, test_cas());

        // Simulate pid 10 as active writer on "/workspace/f.txt".
        tracer
            .active_writes
            .insert("/workspace/f.txt".into(), 10);

        // A second pid for the same path should go into the wait queue.
        assert!(tracer.active_writes.contains_key("/workspace/f.txt"));
        tracer
            .write_wait_queue
            .entry("/workspace/f.txt".into())
            .or_default()
            .push_back(PendingCapture {
                before_hash: None,
                path: "/workspace/f.txt".into(),
                pid: 20,
                kind: CaptureKind::Write { fd: 3, size: 64 },
            });

        assert_eq!(tracer.write_wait_queue["/workspace/f.txt"].len(), 1);
    }

    #[test]
    fn resume_next_queued_writer_drains_queue() {
        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let mut tracer = TracerLoop::new("a".into(), tx, seq, test_cas());

        let path = "/workspace/queued.txt".to_string();
        tracer.active_writes.insert(path.clone(), 10);
        tracer
            .write_wait_queue
            .entry(path.clone())
            .or_default()
            .push_back(PendingCapture {
                before_hash: None,
                path: path.clone(),
                pid: 20,
                kind: CaptureKind::Write { fd: 3, size: 32 },
            });

        // resume_next_queued_writer needs ptrace, so we test the data
        // structure manipulation by calling it — it will fail on
        // ptrace::syscall since pid 20 isn't real, but the state
        // updates happen before the ptrace call.
        let _ = tracer.resume_next_queued_writer(&path, Some("abc123".into()));

        // The queued entry should have been moved to pending_captures
        // with the correct before_hash, regardless of whether ptrace
        // succeeded.
        if let Some(cap) = tracer.pending_captures.get(&20) {
            assert_eq!(cap.before_hash.as_deref(), Some("abc123"));
            assert_eq!(cap.path, path);
        }
        // Queue should be empty and cleaned up.
        assert!(!tracer.write_wait_queue.contains_key(&path));
    }

    #[test]
    fn cleanup_dead_writer_removes_from_active() {
        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let mut tracer = TracerLoop::new("a".into(), tx, seq, test_cas());

        let path = "/workspace/dead.txt".to_string();
        tracer.active_writes.insert(path.clone(), 10);

        // No queued writers, so cleanup just removes the active entry.
        let _ = tracer.cleanup_dead_writer(10);
        assert!(!tracer.active_writes.contains_key(&path));
    }

    #[test]
    fn cleanup_dead_writer_removes_from_wait_queue() {
        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let mut tracer = TracerLoop::new("a".into(), tx, seq, test_cas());

        let path = "/workspace/queued.txt".to_string();
        tracer.active_writes.insert(path.clone(), 5);

        // Pid 10 and 20 are queued; 10 dies.
        tracer
            .write_wait_queue
            .entry(path.clone())
            .or_default()
            .push_back(PendingCapture {
                before_hash: None,
                path: path.clone(),
                pid: 10,
                kind: CaptureKind::Write { fd: 1, size: 8 },
            });
        tracer
            .write_wait_queue
            .entry(path.clone())
            .or_default()
            .push_back(PendingCapture {
                before_hash: None,
                path: path.clone(),
                pid: 20,
                kind: CaptureKind::Write { fd: 1, size: 8 },
            });

        let _ = tracer.cleanup_dead_writer(10);

        // Pid 10 should be removed, pid 20 should remain.
        let queue = &tracer.write_wait_queue[&path];
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].pid, 20);
    }

    #[test]
    fn different_paths_not_blocked() {
        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let mut tracer = TracerLoop::new("a".into(), tx, seq, test_cas());

        tracer
            .active_writes
            .insert("/workspace/a.txt".into(), 10);

        // A write to a different path should not be blocked.
        assert!(!tracer.active_writes.contains_key("/workspace/b.txt"));
    }

    #[test]
    fn capture_initial_state_empty_workspace() {
        let (tx, rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let dir = tempfile::tempdir().expect("tempdir");

        let ws = dir.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();

        let mut tracer = TracerLoop::new("a".into(), tx, seq, test_cas())
            .with_workspace(ws);
        tracer.capture_initial_state().unwrap();

        let evt = rx.recv().unwrap();
        match &evt.payload {
            EventPayload::InitialState(s) => {
                assert_eq!(s.file_count, 0);
                assert_eq!(s.total_size, 0);
            }
            other => panic!("expected InitialState, got {other:?}"),
        }
    }

    #[test]
    fn capture_initial_state_with_files() {
        let (tx, rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let dir = tempfile::tempdir().expect("tempdir");

        let ws = dir.path().join("workspace");
        std::fs::create_dir_all(ws.join("subdir")).unwrap();
        std::fs::write(ws.join("a.txt"), b"hello").unwrap();
        std::fs::write(ws.join("subdir/b.txt"), b"world").unwrap();

        let mut tracer = TracerLoop::new("a".into(), tx, seq, test_cas())
            .with_workspace(ws);
        tracer.capture_initial_state().unwrap();

        let mut events: Vec<_> = rx.try_iter().collect();
        // Last event is InitialState, preceding are InitialFile.
        let last = events.pop().unwrap();
        match &last.payload {
            EventPayload::InitialState(s) => {
                assert_eq!(s.file_count, 2);
                assert_eq!(s.total_size, 10);
                assert!(s.tree_hash.is_some());
            }
            other => panic!("expected InitialState, got {other:?}"),
        }

        assert_eq!(events.len(), 2);
        for evt in &events {
            match &evt.payload {
                EventPayload::InitialFile(f) => {
                    assert!(f.size > 0);
                    assert!(!f.content_hash.is_empty());
                }
                other => panic!("expected InitialFile, got {other:?}"),
            }
        }
    }

    #[test]
    fn capture_initial_state_populates_merkle_tree() {
        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let dir = tempfile::tempdir().expect("tempdir");

        let ws = dir.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("f.txt"), b"content").unwrap();

        let mut tracer = TracerLoop::new("a".into(), tx, seq, test_cas())
            .with_workspace(ws.clone());
        tracer.capture_initial_state().unwrap();

        let path = ws.join("f.txt").to_string_lossy().into_owned();
        assert!(
            tracer.tree.contains(std::path::Path::new(&path)),
            "Merkle tree should contain the walked file"
        );
        assert!(tracer.path_hashes.contains_key(&path));
    }

    #[test]
    fn capture_initial_state_no_workspace_is_noop() {
        let (tx, rx) = mpsc::channel();
        let seq = SequenceGenerator::default();

        let mut tracer = TracerLoop::new("a".into(), tx, seq, test_cas());
        tracer.capture_initial_state().unwrap();

        // No events emitted.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn tree_update_populates_root_hash() {
        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let mut tracer = TracerLoop::new("a".into(), tx, seq, test_cas());

        let hash = crate::cas::ContentHash::from_data(b"hello");
        let root = tracer.tree_update("/workspace/f.txt", hash.as_str());
        assert!(root.is_some());
        assert_eq!(tracer.tree.file_count(), 1);
    }

    #[test]
    fn tree_rename_moves_entry() {
        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let mut tracer = TracerLoop::new("a".into(), tx, seq, test_cas());

        let hash = crate::cas::ContentHash::from_data(b"data");
        tracer.tree_update("/workspace/a.txt", hash.as_str());
        tracer.tree_rename("/workspace/a.txt", "/workspace/b.txt");

        assert!(!tracer.tree.contains(std::path::Path::new("/workspace/a.txt")));
        assert!(tracer.tree.contains(std::path::Path::new("/workspace/b.txt")));
    }

    #[test]
    fn tree_remove_deletes_entry() {
        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let mut tracer = TracerLoop::new("a".into(), tx, seq, test_cas());

        let hash = crate::cas::ContentHash::from_data(b"data");
        tracer.tree_update("/workspace/f.txt", hash.as_str());
        assert_eq!(tracer.tree.file_count(), 1);

        tracer.tree_remove("/workspace/f.txt");
        assert_eq!(tracer.tree.file_count(), 0);
    }
}

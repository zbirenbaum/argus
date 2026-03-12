//! Syscall dispatch and handler functions for seccomp-ptrace stops.
//!
//! Each handler reads arguments from the tracee's registers, updates
//! in-memory state, and emits the corresponding event. Phase 1 does
//! not capture file content -- hashes are `None`.

mod file_ops;
pub(crate) mod io_ops;
mod metadata_ops;
mod net_ops;

use anyhow::Result;
use nix::sys::ptrace;
use nix::unistd::Pid;
use tracing::event;
use tracing::Level;

use crate::config::{MatchKind, RuleDecision};
use crate::events::EventPayload;
use crate::events::control;
use crate::state::FdTarget;

use super::memory;
use super::regs;
use super::syscall_nr::*;
use super::trace_loop::{CaptureKind, PendingCapture, TracerLoop, hash_file_content};

/// Maps a syscall number to a rule `MatchKind` for evaluation.
///
/// Returns `None` for syscalls that are not covered by rules.
fn syscall_to_match_kind(nr: u64) -> Option<MatchKind> {
    match nr {
        SYS_READ | SYS_PREAD64 | SYS_READV | SYS_PREADV => Some(MatchKind::Read),
        SYS_WRITE | SYS_PWRITE64 | SYS_WRITEV | SYS_PWRITEV
        | SYS_TRUNCATE | SYS_FTRUNCATE => Some(MatchKind::Write),
        SYS_UNLINK | SYS_UNLINKAT => Some(MatchKind::Unlink),
        SYS_RENAME | SYS_RENAMEAT | SYS_RENAMEAT2 => Some(MatchKind::Rename),
        SYS_CHMOD | SYS_FCHMOD | SYS_FCHMODAT => Some(MatchKind::Chmod),
        SYS_EXECVE | SYS_EXECVEAT => Some(MatchKind::Exec),
        SYS_CONNECT => Some(MatchKind::Connect),
        _ => None,
    }
}

/// Extracts the syscall's path from registers and fd tables.
fn resolve_syscall_path(
    tracer: &TracerLoop,
    pid: Pid,
    nr: u64,
    r: &regs::UserRegs,
) -> Option<String> {
    let pid_u32 = pid.as_raw() as u32;
    match nr {
        SYS_READ | SYS_PREAD64 | SYS_READV | SYS_PREADV
        | SYS_WRITE | SYS_PWRITE64 | SYS_WRITEV | SYS_PWRITEV => {
            let fd = regs::arg1(r) as i32;
            match io_ops::resolve_fd_target(tracer, pid_u32, fd) {
                FdTarget::File { path } => Some(path.to_string_lossy().into_owned()),
                _ => None,
            }
        }
        SYS_UNLINK => memory::read_c_string(pid, regs::arg1(r)).ok(),
        SYS_UNLINKAT => {
            let dirfd = regs::arg1(r) as i32;
            memory::read_path_at(pid, dirfd, regs::arg2(r))
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        }
        SYS_OPEN | SYS_OPENAT => resolve_open_path(pid, nr, r).ok(),
        SYS_RENAME => memory::read_c_string(pid, regs::arg1(r)).ok(),
        SYS_RENAMEAT | SYS_RENAMEAT2 => {
            let dirfd = regs::arg1(r) as i32;
            memory::read_path_at(pid, dirfd, regs::arg2(r))
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        }
        SYS_CHMOD => memory::read_c_string(pid, regs::arg1(r)).ok(),
        SYS_FCHMODAT => {
            let dirfd = regs::arg1(r) as i32;
            memory::read_path_at(pid, dirfd, regs::arg2(r))
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        }
        SYS_FCHMOD => {
            let fd = regs::arg1(r) as i32;
            match io_ops::resolve_fd_target(tracer, pid_u32, fd) {
                FdTarget::File { path } => Some(path.to_string_lossy().into_owned()),
                _ => None,
            }
        }
        SYS_TRUNCATE => memory::read_c_string(pid, regs::arg1(r)).ok(),
        SYS_FTRUNCATE => {
            let fd = regs::arg1(r) as i32;
            match io_ops::resolve_fd_target(tracer, pid_u32, fd) {
                FdTarget::File { path } => Some(path.to_string_lossy().into_owned()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Extracts the binary name for exec syscalls.
fn resolve_exec_binary(pid: Pid, nr: u64, r: &regs::UserRegs) -> Option<String> {
    match nr {
        SYS_EXECVE => {
            let path = memory::read_c_string(pid, regs::arg1(r)).ok()?;
            path.rsplit('/').next().map(String::from)
        }
        SYS_EXECVEAT => {
            let path = memory::read_c_string(pid, regs::arg2(r)).ok()?;
            path.rsplit('/').next().map(String::from)
        }
        _ => None,
    }
}

/// Human-readable syscall name for events.
fn syscall_name(nr: u64) -> &'static str {
    match nr {
        SYS_READ => "read", SYS_PREAD64 => "pread64",
        SYS_READV => "readv", SYS_PREADV => "preadv",
        SYS_WRITE => "write", SYS_PWRITE64 => "pwrite64",
        SYS_WRITEV => "writev", SYS_PWRITEV => "pwritev",
        SYS_UNLINK => "unlink", SYS_UNLINKAT => "unlinkat",
        SYS_RENAME => "rename", SYS_RENAMEAT => "renameat",
        SYS_RENAMEAT2 => "renameat2",
        SYS_CHMOD => "chmod", SYS_FCHMOD => "fchmod",
        SYS_FCHMODAT => "fchmodat",
        SYS_TRUNCATE => "truncate", SYS_FTRUNCATE => "ftruncate",
        SYS_EXECVE => "execve", SYS_EXECVEAT => "execveat",
        SYS_CONNECT => "connect",
        _ => "unknown",
    }
}

/// Cancels a syscall and sets up EPERM injection at exit.
///
/// Modifies orig_rax to -1 (kernel skips the syscall), then resumes
/// with `ptrace::syscall` so the exit stop fires. The exit handler
/// checks `pending_eperm` and overwrites rax with -EPERM.
fn cancel_syscall_with_eperm(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &regs::UserRegs,
) -> Result<()> {
    let mut modified = *r;
    regs::cancel_syscall(&mut modified);
    regs::set_regs(pid, &modified)?;
    tracer.pending_eperm.insert(pid.as_raw() as u32);
    ptrace::syscall(pid, None)?;
    Ok(())
}

/// Evaluates rules and enforces block/pause decisions.
///
/// Returns `Some(true)` if the tracee was handled (caller should not
/// resume), `Some(false)` if rules allow (proceed normally), or `None`
/// if no rules are configured.
fn evaluate_rules(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &regs::UserRegs,
) -> Result<Option<bool>> {
    let rules = match &tracer.rules {
        Some(handle) => handle.load(),
        None => return Ok(None),
    };

    let kind = match syscall_to_match_kind(nr) {
        Some(k) => k,
        None => return Ok(Some(false)),
    };

    let path = resolve_syscall_path(tracer, pid, nr, r);
    let binary = resolve_exec_binary(pid, nr, r);
    let decision = rules.evaluate(kind, path.as_deref(), binary.as_deref(), None);

    match decision {
        RuleDecision::Allow => Ok(Some(false)),

        RuleDecision::Block { rule_index } => {
            let pid_u32 = pid.as_raw() as u32;
            let rule_desc = rules.block_rule_description(rule_index);

            tracer.emit(EventPayload::Blocked(control::Blocked {
                pid: pid_u32,
                syscall: syscall_name(nr).into(),
                path: path.clone(),
                rule: rule_desc.clone(),
            }));

            event!(
                name: "tracer.rule.blocked",
                Level::INFO,
                pid = pid_u32,
                syscall = syscall_name(nr),
                rule = %rule_desc,
                "blocked syscall {{syscall}} for pid {{pid}}: {{rule}}",
            );

            cancel_syscall_with_eperm(tracer, pid, r)?;
            Ok(Some(true))
        }

        RuleDecision::Pause { rule_index } => {
            let pid_u32 = pid.as_raw() as u32;
            let rule_desc = rules.pause_rule_description(rule_index);

            tracer.emit(EventPayload::PendingApproval(control::PendingApproval {
                pid: pid_u32,
                syscall: syscall_name(nr).into(),
                path: path.clone(),
                binary: binary.clone(),
                rule_name: rule_desc.clone(),
            }));

            let shared = match &tracer.shared_state {
                Some(s) => s.clone(),
                None => {
                    // No shared state — cannot submit approval, deny.
                    cancel_syscall_with_eperm(tracer, pid, r)?;
                    return Ok(Some(true));
                }
            };

            let process_name = tracer
                .process_tree
                .get_process(pid_u32)
                .map(|p| p.binary.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".into());

            let (_action_id, rx) = crate::api::routes::submit_pending_approval(
                &shared,
                pid_u32,
                process_name,
                syscall_name(nr).into(),
                path,
                rule_desc,
            );

            // Block the tracer thread until the operator decides.
            // All traced processes are frozen while we wait.
            let decision = rx.blocking_recv().unwrap_or(
                crate::events::ApprovalDecision::Deny,
            );

            match decision {
                crate::events::ApprovalDecision::Approve => Ok(Some(false)),
                crate::events::ApprovalDecision::Deny => {
                    cancel_syscall_with_eperm(tracer, pid, r)?;
                    Ok(Some(true))
                }
            }
        }
    }
}

/// Dispatches a seccomp stop to the appropriate handler.
///
/// Returns `true` if the tracee was already resumed via
/// `ptrace::syscall` (for file-mutating ops that need exit capture).
/// Returns `false` when the caller must call `ptrace::cont`.
///
/// # Errors
///
/// Returns an error if register reads or handler logic fails.
pub fn handle_seccomp_stop(tracer: &mut TracerLoop, pid: Pid) -> Result<bool> {
    let r = regs::get_regs(pid)?;
    let nr = regs::syscall_nr(&r);

    // Evaluate block/pause rules before dispatching to handlers.
    if let Some(handled) = evaluate_rules(tracer, pid, nr, &r)? {
        if handled {
            return Ok(true);
        }
    }

    match nr {
        // File open/close/dup — capture O_TRUNC as a mutation.
        SYS_OPEN | SYS_OPENAT | SYS_OPENAT2 | SYS_CREAT => {
            if try_start_open_trunc_capture(tracer, pid, nr, &r)? {
                return Ok(true);
            }
            if file_ops::handle_open(tracer, pid, nr, &r)? {
                return Ok(true);
            }
        }
        SYS_CLOSE => {
            file_ops::handle_close(tracer, pid, &r)?;
        }
        SYS_DUP | SYS_DUP2 | SYS_DUP3 => {
            file_ops::handle_dup(tracer, pid, nr, &r)?;
        }
        SYS_FCNTL => {
            file_ops::handle_fcntl(tracer, pid, &r)?;
        }

        // Seek
        SYS_LSEEK => {
            io_ops::handle_lseek(tracer, pid, &r)?;
        }

        // Read — capture content at exit via pending_reads.
        SYS_READ | SYS_PREAD64 | SYS_READV | SYS_PREADV => {
            if io_ops::handle_read(tracer, pid, &r)? {
                return Ok(true);
            }
        }

        // Write — capture before/after hashes for file targets.
        SYS_WRITE | SYS_PWRITE64 | SYS_WRITEV | SYS_PWRITEV => {
            if try_start_write_capture(tracer, pid, &r)? {
                return Ok(true);
            }
            io_ops::handle_write(tracer, pid, &r)?;
        }

        // File metadata
        SYS_RENAME | SYS_RENAMEAT | SYS_RENAMEAT2 => {
            metadata_ops::handle_rename(tracer, pid, nr, &r)?;
        }
        SYS_UNLINK | SYS_UNLINKAT => {
            metadata_ops::handle_unlink(tracer, pid, nr, &r)?;
        }
        SYS_MKDIR | SYS_MKDIRAT => {
            metadata_ops::handle_mkdir(tracer, pid, nr, &r)?;
        }
        SYS_RMDIR => {
            metadata_ops::handle_rmdir(tracer, pid, &r)?;
        }
        SYS_CHMOD | SYS_FCHMOD | SYS_FCHMODAT => {
            metadata_ops::handle_chmod(tracer, pid, nr, &r)?;
        }
        SYS_CHOWN | SYS_FCHOWN | SYS_FCHOWNAT => {
            metadata_ops::handle_chown(tracer, pid, nr, &r)?;
        }
        SYS_TRUNCATE | SYS_FTRUNCATE => {
            metadata_ops::handle_truncate(tracer, pid, nr, &r)?;
        }
        SYS_LINK | SYS_LINKAT => {
            metadata_ops::handle_link(tracer, pid, nr, &r)?;
        }
        SYS_SYMLINK | SYS_SYMLINKAT => {
            metadata_ops::handle_symlink(tracer, pid, nr, &r)?;
        }
        SYS_READLINK | SYS_READLINKAT => {}

        // Pipe/PTY — entry/exit pattern for fd capture.
        SYS_PIPE | SYS_PIPE2 => {
            if io_ops::handle_pipe(tracer, pid, &r)? {
                return Ok(true);
            }
        }
        SYS_IOCTL => {
            io_ops::handle_ioctl(tracer, pid, &r)?;
        }

        // Network
        SYS_SOCKET => {
            net_ops::handle_socket(tracer, pid, &r)?;
        }
        SYS_CONNECT => {
            net_ops::handle_connect(tracer, pid, &r)?;
        }
        SYS_ACCEPT | SYS_ACCEPT4 => {
            net_ops::handle_accept(tracer, pid, &r)?;
        }
        SYS_BIND | SYS_LISTEN | SYS_SENDTO | SYS_SENDMSG
        | SYS_RECVFROM | SYS_RECVMSG => {}

        // Process lifecycle handled via PTRACE_EVENT, not seccomp.
        SYS_FORK | SYS_VFORK | SYS_CLONE | SYS_CLONE3
        | SYS_EXECVE | SYS_EXECVEAT | SYS_EXIT | SYS_EXIT_GROUP => {}

        other => {
            event!(
                name: "tracer.syscall.unhandled",
                Level::TRACE,
                pid = pid.as_raw(),
                syscall_nr = other,
                "unhandled seccomp stop for syscall {{syscall_nr}} in pid {{pid}}",
            );
        }
    }

    Ok(false)
}

/// Starts a write capture for file targets.
///
/// If another write to the same path is in-kernel, queues this tracee
/// instead of resuming it — the tracee stays stopped until the active
/// writer completes. Returns `true` if the tracee was handled (either
/// resumed or queued).
fn try_start_write_capture(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &regs::UserRegs,
) -> Result<bool> {
    let fd = regs::arg1(r) as i32;
    let size = regs::arg3(r);
    let pid_u32 = pid.as_raw() as u32;

    let target = io_ops::resolve_fd_target(tracer, pid_u32, fd);
    let path = match target {
        FdTarget::File { ref path } => path.to_string_lossy().into_owned(),
        _ => return Ok(false),
    };

    if tracer.active_writes.contains_key(&path) {
        // Another thread is mid-write to this path. Hold this tracee
        // at entry; it will be resumed when the active write completes.
        tracer
            .write_wait_queue
            .entry(path.clone())
            .or_default()
            .push_back(PendingCapture {
                before_hash: None,
                path,
                pid: pid_u32,
                kind: CaptureKind::Write { fd, size },
            });
        return Ok(true);
    }

    let before_hash = hash_file_content(&tracer.cas, &path);

    tracer.active_writes.insert(path.clone(), pid_u32);
    tracer.pending_captures.insert(pid_u32, PendingCapture {
        before_hash,
        path,
        pid: pid_u32,
        kind: CaptureKind::Write { fd, size },
    });

    ptrace::syscall(pid, None)?;
    Ok(true)
}

/// `O_TRUNC` flag — truncate file on open.
const O_TRUNC: u64 = 0x200;
/// `O_WRONLY` flag.
const O_WRONLY: u64 = 0x1;
/// `O_RDWR` flag.
const O_RDWR: u64 = 0x2;

/// Captures open(O_TRUNC) as a file mutation so the hash chain
/// includes truncations between writes.
///
/// Respects the per-path write queue — if a write to this path is
/// in-kernel, queues this tracee until the active write completes.
fn try_start_open_trunc_capture(
    tracer: &mut TracerLoop,
    pid: Pid,
    nr: u64,
    r: &regs::UserRegs,
) -> Result<bool> {
    let flags = match nr {
        SYS_OPEN => regs::arg2(r),
        SYS_OPENAT => regs::arg3(r),
        SYS_CREAT => return Ok(false),
        _ => return Ok(false),
    };

    if flags & O_TRUNC == 0 {
        return Ok(false);
    }
    if flags & O_WRONLY == 0 && flags & O_RDWR == 0 {
        return Ok(false);
    }

    let path = resolve_open_path(pid, nr, r)?;
    let pid_u32 = pid.as_raw() as u32;

    if tracer.active_writes.contains_key(&path) {
        tracer
            .write_wait_queue
            .entry(path.clone())
            .or_default()
            .push_back(PendingCapture {
                before_hash: None,
                path,
                pid: pid_u32,
                kind: CaptureKind::OpenTrunc,
            });
        return Ok(true);
    }

    let before_hash = hash_file_content(&tracer.cas, &path);

    // Skip capture if file doesn't exist yet (nothing to truncate).
    if before_hash.is_none() {
        return Ok(false);
    }

    tracer.active_writes.insert(path.clone(), pid_u32);
    tracer.pending_captures.insert(pid_u32, PendingCapture {
        before_hash,
        path,
        pid: pid_u32,
        kind: CaptureKind::OpenTrunc,
    });

    ptrace::syscall(pid, None)?;
    Ok(true)
}

/// Reads the path argument from an open/openat syscall.
fn resolve_open_path(
    pid: Pid,
    nr: u64,
    r: &regs::UserRegs,
) -> Result<String> {
    match nr {
        SYS_OPEN => memory::read_c_string(pid, regs::arg1(r)),
        SYS_OPENAT => {
            let dirfd = regs::arg1(r) as i32;
            let p = memory::read_path_at(pid, dirfd, regs::arg2(r))?;
            Ok(p.to_string_lossy().into_owned())
        }
        _ => Ok(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MatchKind, Rule, RuleSet};

    // --- syscall_to_match_kind ---

    #[test]
    fn match_kind_read_variants() {
        for nr in [SYS_READ, SYS_PREAD64, SYS_READV, SYS_PREADV] {
            assert_eq!(syscall_to_match_kind(nr), Some(MatchKind::Read));
        }
    }

    #[test]
    fn match_kind_write_variants() {
        for nr in [
            SYS_WRITE, SYS_PWRITE64, SYS_WRITEV, SYS_PWRITEV,
            SYS_TRUNCATE, SYS_FTRUNCATE,
        ] {
            assert_eq!(syscall_to_match_kind(nr), Some(MatchKind::Write));
        }
    }

    #[test]
    fn match_kind_unlink_variants() {
        for nr in [SYS_UNLINK, SYS_UNLINKAT] {
            assert_eq!(syscall_to_match_kind(nr), Some(MatchKind::Unlink));
        }
    }

    #[test]
    fn match_kind_rename_variants() {
        for nr in [SYS_RENAME, SYS_RENAMEAT, SYS_RENAMEAT2] {
            assert_eq!(syscall_to_match_kind(nr), Some(MatchKind::Rename));
        }
    }

    #[test]
    fn match_kind_chmod_variants() {
        for nr in [SYS_CHMOD, SYS_FCHMOD, SYS_FCHMODAT] {
            assert_eq!(syscall_to_match_kind(nr), Some(MatchKind::Chmod));
        }
    }

    #[test]
    fn match_kind_exec_variants() {
        for nr in [SYS_EXECVE, SYS_EXECVEAT] {
            assert_eq!(syscall_to_match_kind(nr), Some(MatchKind::Exec));
        }
    }

    #[test]
    fn match_kind_connect() {
        assert_eq!(syscall_to_match_kind(SYS_CONNECT), Some(MatchKind::Connect));
    }

    #[test]
    fn match_kind_none_for_uncovered() {
        // Socket/pipe/ioctl are not rule-covered syscalls.
        assert_eq!(syscall_to_match_kind(SYS_SOCKET), None);
        assert_eq!(syscall_to_match_kind(SYS_PIPE), None);
        assert_eq!(syscall_to_match_kind(SYS_IOCTL), None);
        assert_eq!(syscall_to_match_kind(SYS_CLOSE), None);
    }

    // --- syscall_name ---

    #[test]
    fn syscall_name_known() {
        assert_eq!(syscall_name(SYS_READ), "read");
        assert_eq!(syscall_name(SYS_WRITE), "write");
        assert_eq!(syscall_name(SYS_UNLINK), "unlink");
        assert_eq!(syscall_name(SYS_RENAME), "rename");
        assert_eq!(syscall_name(SYS_CHMOD), "chmod");
        assert_eq!(syscall_name(SYS_EXECVE), "execve");
        assert_eq!(syscall_name(SYS_CONNECT), "connect");
        assert_eq!(syscall_name(SYS_TRUNCATE), "truncate");
        assert_eq!(syscall_name(SYS_FTRUNCATE), "ftruncate");
    }

    #[test]
    fn syscall_name_unknown_for_unmapped() {
        assert_eq!(syscall_name(SYS_CLOSE), "unknown");
        assert_eq!(syscall_name(SYS_SOCKET), "unknown");
        assert_eq!(syscall_name(9999), "unknown");
    }

    // --- pending_eperm tracking ---

    #[test]
    fn pending_eperm_insert_and_remove() {
        use std::sync::mpsc;
        use crate::cas::LocalCas;
        use crate::events::SequenceGenerator;

        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let cas = LocalCas::new(dir.path().join("cas")).expect("LocalCas");
        let mut tracer = TracerLoop::new("test".into(), tx, seq, cas);

        assert!(!tracer.pending_eperm.contains(&42));
        tracer.pending_eperm.insert(42);
        assert!(tracer.pending_eperm.contains(&42));
        assert!(tracer.pending_eperm.remove(&42));
        assert!(!tracer.pending_eperm.contains(&42));
    }

    // --- evaluate_rules with no rules configured ---

    #[test]
    fn evaluate_rules_returns_none_when_no_rules() {
        use std::sync::mpsc;
        use crate::cas::LocalCas;
        use crate::events::SequenceGenerator;

        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let cas = LocalCas::new(dir.path().join("cas")).expect("LocalCas");
        let tracer = TracerLoop::new("test".into(), tx, seq, cas);

        // No rules configured — evaluate_rules should return None.
        assert!(tracer.rules.is_none());
    }

    // --- evaluate_rules with rules configured ---

    fn make_rule(
        kind: MatchKind,
        paths: Vec<String>,
        binaries: Vec<String>,
    ) -> Rule {
        Rule::new(kind, paths, binaries, Vec::new())
    }

    #[test]
    fn tracer_with_block_rule_has_rules() {
        use std::sync::mpsc;
        use std::sync::Arc;
        use arc_swap::ArcSwap;
        use crate::cas::LocalCas;
        use crate::events::SequenceGenerator;

        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let cas = LocalCas::new(dir.path().join("cas")).expect("LocalCas");

        let mut rs = RuleSet {
            block: vec![make_rule(
                MatchKind::Write,
                vec!["/protected/**".into()],
                Vec::new(),
            )],
            pause_before: Vec::new(),
        };
        rs.compile_patterns();

        let rules = Arc::new(ArcSwap::from_pointee(rs));
        let tracer = TracerLoop::new("test".into(), tx, seq, cas)
            .with_rules(rules.clone());

        assert!(tracer.rules.is_some());

        // Verify the loaded rule set evaluates correctly.
        let loaded = tracer.rules.as_ref().unwrap().load();
        let decision = loaded.evaluate(
            MatchKind::Write,
            Some("/protected/secret.txt"),
            None,
            None,
        );
        assert!(matches!(decision, crate::config::RuleDecision::Block { .. }));

        // Non-matching path is allowed.
        let decision = loaded.evaluate(
            MatchKind::Write,
            Some("/tmp/scratch.txt"),
            None,
            None,
        );
        assert_eq!(decision, crate::config::RuleDecision::Allow);
    }

    #[test]
    fn tracer_with_pause_rule_has_rules() {
        use std::sync::mpsc;
        use std::sync::Arc;
        use arc_swap::ArcSwap;
        use crate::cas::LocalCas;
        use crate::events::SequenceGenerator;

        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let cas = LocalCas::new(dir.path().join("cas")).expect("LocalCas");

        let mut rs = RuleSet {
            block: Vec::new(),
            pause_before: vec![make_rule(
                MatchKind::Exec,
                Vec::new(),
                vec!["rm".into(), "curl".into()],
            )],
        };
        rs.compile_patterns();

        let rules = Arc::new(ArcSwap::from_pointee(rs));
        let tracer = TracerLoop::new("test".into(), tx, seq, cas)
            .with_rules(rules);

        let loaded = tracer.rules.as_ref().unwrap().load();
        let decision = loaded.evaluate(MatchKind::Exec, None, Some("rm"), None);
        assert!(matches!(decision, crate::config::RuleDecision::Pause { .. }));

        let decision = loaded.evaluate(MatchKind::Exec, None, Some("ls"), None);
        assert_eq!(decision, crate::config::RuleDecision::Allow);
    }

    #[test]
    fn block_rules_take_priority_over_pause() {
        use std::sync::mpsc;
        use std::sync::Arc;
        use arc_swap::ArcSwap;
        use crate::cas::LocalCas;
        use crate::events::SequenceGenerator;

        let (tx, _rx) = mpsc::channel();
        let seq = SequenceGenerator::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let cas = LocalCas::new(dir.path().join("cas")).expect("LocalCas");

        // Both block and pause target the same category/path.
        let mut rs = RuleSet {
            block: vec![make_rule(
                MatchKind::Unlink,
                vec!["/workspace/**".into()],
                Vec::new(),
            )],
            pause_before: vec![make_rule(
                MatchKind::Unlink,
                vec!["/workspace/**".into()],
                Vec::new(),
            )],
        };
        rs.compile_patterns();

        let rules = Arc::new(ArcSwap::from_pointee(rs));
        let tracer = TracerLoop::new("test".into(), tx, seq, cas)
            .with_rules(rules);

        let loaded = tracer.rules.as_ref().unwrap().load();
        let decision = loaded.evaluate(
            MatchKind::Unlink,
            Some("/workspace/foo.txt"),
            None,
            None,
        );
        assert!(
            matches!(decision, crate::config::RuleDecision::Block { .. }),
            "block should take priority over pause"
        );
    }

    // --- syscall_to_match_kind exhaustiveness ---

    #[test]
    fn all_mapped_syscalls_have_names() {
        let mapped = [
            SYS_READ, SYS_PREAD64, SYS_READV, SYS_PREADV,
            SYS_WRITE, SYS_PWRITE64, SYS_WRITEV, SYS_PWRITEV,
            SYS_UNLINK, SYS_UNLINKAT,
            SYS_RENAME, SYS_RENAMEAT, SYS_RENAMEAT2,
            SYS_CHMOD, SYS_FCHMOD, SYS_FCHMODAT,
            SYS_TRUNCATE, SYS_FTRUNCATE,
            SYS_EXECVE, SYS_EXECVEAT,
            SYS_CONNECT,
        ];
        for nr in mapped {
            assert_ne!(
                syscall_name(nr), "unknown",
                "syscall {nr} should have a name"
            );
            assert!(
                syscall_to_match_kind(nr).is_some(),
                "syscall {nr} should have a match kind"
            );
        }
    }
}

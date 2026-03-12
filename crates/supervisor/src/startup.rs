// Rust guideline compliant 2026-02-21
//! Startup helpers for directory creation and agent process spawning.
//!
//! Handles creating required data directories and forking the agent
//! child process with seccomp filter installation and ptrace setup.

use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::Path;

use anyhow::{Context, Result, bail};
use argus::config::RunAs;
use nix::unistd::{ForkResult, Pid, execvpe, pipe, read};
use tracing::{Level, event};

/// Handles returned from `spawn_agent` for stdio forwarding.
#[derive(Debug)]
pub struct SpawnResult {
    pub child_pid: Pid,
    pub sync_pipe_w: RawFd,
    /// Read end of the pipe connected to the child's stdout.
    pub stdout_r: OwnedFd,
    /// Read end of the pipe connected to the child's stderr.
    pub stderr_r: OwnedFd,
}

/// Subdirectories under `data_dir` required at startup.
const DATA_SUBDIRS: &[&str] = &["cas", "checkpoints", "events", "indexes"];

/// Creates the required data directory tree.
///
/// # Errors
///
/// Returns an error if any directory cannot be created.
pub fn create_data_dirs(data_dir: &Path) -> Result<()> {
    for sub in DATA_SUBDIRS {
        let dir = data_dir.join(sub);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create directory {}", dir.display()))?;
    }
    event!(
        name: "supervisor.dirs.created",
        Level::INFO,
        data.dir = %data_dir.display(),
        "created data directories under {{data.dir}}",
    );
    Ok(())
}

/// Forks a child process that waits for ptrace attachment, then
/// installs seccomp and execs the agent command.
///
/// Returns `(child_pid, sync_pipe_write_fd)`. The caller must pass
/// `sync_pipe_write_fd` to the tracer loop which writes to it after
/// completing `PTRACE_SEIZE` on the child.
///
/// # Errors
///
/// Returns an error if `fork()` or pipe creation fails, or if the
/// command path cannot be converted to a C string.
pub fn spawn_agent(
    command: &[String],
    env_vars: &HashMap<String, String>,
    working_dir: &Path,
    run_as: Option<&RunAs>,
) -> Result<SpawnResult> {
    if command.is_empty() {
        bail!("agent command must not be empty");
    }

    let c_program = to_cstring(&command[0])?;
    let c_argv: Vec<CString> = command
        .iter()
        .map(|a| to_cstring(a))
        .collect::<Result<_>>()?;

    // Merge current environment with agent-specific overrides.
    let mut full_env: HashMap<String, String> = std::env::vars().collect();
    full_env.extend(env_vars.iter().map(|(k, v)| (k.clone(), v.clone())));
    let c_env: Vec<CString> = full_env
        .iter()
        .map(|(k, v)| to_cstring(&format!("{k}={v}")))
        .collect::<Result<_>>()?;

    // Sync pipe: child blocks on read until parent completes PTRACE_SEIZE.
    let (pipe_r, pipe_w) = pipe().context("pipe() for sync failed")?;

    // Pipes for capturing the child's stdout and stderr.
    let (stdout_r, stdout_w) = pipe().context("pipe() for stdout failed")?;
    let (stderr_r, stderr_w) = pipe().context("pipe() for stderr failed")?;

    // SAFETY: fork() is called once; child blocks on pipe then execs,
    // parent returns immediately. No shared mutable state between
    // fork and exec.
    match unsafe { nix::unistd::fork() }.context("fork() failed")? {
        ForkResult::Child => {
            drop(pipe_w);
            drop(stdout_r);
            drop(stderr_r);

            // Redirect child stdout/stderr to pipes before exec.
            // SAFETY: dup2 is async-signal-safe; called between
            // fork and exec in the child process.
            unsafe {
                libc::dup2(stdout_w.as_raw_fd(), libc::STDOUT_FILENO);
                libc::dup2(stderr_w.as_raw_fd(), libc::STDERR_FILENO);
            }
            drop(stdout_w);
            drop(stderr_w);

            child_setup(&c_program, &c_argv, &c_env, working_dir, pipe_r, run_as);
        }
        ForkResult::Parent { child } => {
            // Extract the raw fd before dropping — the tracer loop
            // takes ownership and will close it after PTRACE_SEIZE.
            let raw_w = pipe_w.as_raw_fd();
            // Leak ownership so the fd stays open for the tracer.
            std::mem::forget(pipe_w);
            drop(pipe_r);
            // Close write ends — only the child writes to these.
            drop(stdout_w);
            drop(stderr_w);

            event!(
                name: "supervisor.agent.spawned",
                Level::INFO,
                agent.pid = child.as_raw(),
                "spawned agent process with pid {{agent.pid}}",
            );

            Ok(SpawnResult {
                child_pid: child,
                sync_pipe_w: raw_w,
                stdout_r,
                stderr_r,
            })
        }
    }
}

/// Child-side setup: wait for parent's seize, install seccomp, exec.
///
/// This function never returns on success; it aborts on failure.
fn child_setup(
    program: &CString,
    argv: &[CString],
    envp: &[CString],
    working_dir: &Path,
    pipe_r: OwnedFd,
    run_as: Option<&RunAs>,
) -> ! {
    if std::env::set_current_dir(working_dir).is_err() {
        // SAFETY: _exit is async-signal-safe and avoids running
        // destructors in the forked child.
        unsafe { libc::_exit(71) };
    }

    // Block until parent completes PTRACE_SEIZE on us.
    let mut buf = [0u8; 1];
    let _ = read(&pipe_r, &mut buf);
    drop(pipe_r);

    // Install seccomp filter — parent has already seized us so
    // SECCOMP_RET_TRACE will produce ptrace stops, not SIGSYS.
    // Seccomp is unavailable under Rosetta/QEMU emulation; warn
    // and continue so the supervisor can still track process
    // lifecycle via ptrace fork/exec/exit events.
    if let Err(e) = argus::tracer::seccomp::install_seccomp_filter() {
        let msg = format!("seccomp install failed (syscall tracing disabled): {e}\n");
        let _ = nix::unistd::write(std::io::stderr(), msg.as_bytes());
    }

    // Drop privileges before exec if configured. Must happen after
    // seccomp install (which may need CAP_SYS_ADMIN) and after
    // ptrace seize (which the parent does as root).
    if let Some(ra) = run_as {
        let gid = ra.gid.unwrap_or(ra.uid);
        // SAFETY: setgid/setgroups/setuid are async-signal-safe.
        unsafe {
            if libc::setgid(gid) != 0 {
                libc::_exit(74);
            }
            // Clear supplementary groups (initgroups needs a username
            // which we don't have; setgroups(0, []) drops them all).
            libc::setgroups(0, std::ptr::null());
            if libc::setuid(ra.uid) != 0 {
                libc::_exit(75);
            }
        }
    }

    // execvpe replaces the process image on success; unreachable
    // on success. nix's execvpe takes &CStr references.
    let argv_refs: Vec<&CString> = argv.iter().collect();
    let envp_refs: Vec<&CString> = envp.iter().collect();
    let _err = execvpe(program, &argv_refs, &envp_refs);
    unsafe { libc::_exit(73) }
}

/// Converts a string to a `CString`, adding context on failure.
fn to_cstring(s: &str) -> Result<CString> {
    CString::new(s.as_bytes())
        .with_context(|| format!("string contains null byte: {s}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_data_dirs_builds_subdirectories() {
        let tmp = tempfile::TempDir::new().unwrap();
        create_data_dirs(tmp.path()).unwrap();

        for sub in DATA_SUBDIRS {
            let dir = tmp.path().join(sub);
            assert!(dir.is_dir(), "{} should be a directory", dir.display());
        }
    }

    #[test]
    fn create_data_dirs_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        create_data_dirs(tmp.path()).unwrap();
        create_data_dirs(tmp.path()).unwrap();
    }

    #[test]
    fn to_cstring_valid() {
        let cs = to_cstring("hello").unwrap();
        assert_eq!(cs.to_str().unwrap(), "hello");
    }

    #[test]
    fn to_cstring_rejects_null() {
        let result = to_cstring("hel\0lo");
        assert!(result.is_err());
    }

    #[test]
    fn spawn_agent_rejects_empty_command() {
        let result = spawn_agent(&[], &HashMap::new(), Path::new("/"), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }
}

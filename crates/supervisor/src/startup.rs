// Rust guideline compliant 2026-02-21
//! Startup helpers for directory creation and agent process spawning.
//!
//! Handles creating required data directories and forking the agent
//! child process with seccomp filter installation and ptrace setup.

use std::collections::HashMap;
use std::ffi::CString;
use std::path::Path;

use anyhow::{Context, Result, bail};
use nix::unistd::Pid;
use tracing::{Level, event};

/// Subdirectories under `data_dir` required at startup.
const DATA_SUBDIRS: &[&str] = &["cas", "events", "indexes"];

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

/// Forks a child process that installs seccomp, signals readiness via
/// `PTRACE_TRACEME` + `SIGSTOP`, then execs the agent command.
///
/// The parent receives the child PID for ptrace attachment.
///
/// # Errors
///
/// Returns an error if `fork()` fails or the command path cannot be
/// converted to a C string.
///
/// # Safety
///
/// Uses `fork()` which is inherently unsafe. The child calls only
/// async-signal-safe functions before `execvp`.
pub fn spawn_agent(
    command: &[String],
    env_vars: &HashMap<String, String>,
    working_dir: &Path,
) -> Result<Pid> {
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

    // SAFETY: `fork()` is called once; child immediately sets up ptrace
    // and execs, parent returns the child PID. No shared mutable state
    // is accessed between fork and exec.
    let fork_result = unsafe { libc::fork() };

    if fork_result < 0 {
        return Err(std::io::Error::last_os_error()).context("fork() failed");
    }

    if fork_result == 0 {
        // Child process — async-signal-safe only from here to exec.
        child_setup(&c_program, &c_argv, &c_env, working_dir);
    }

    let child_pid = Pid::from_raw(fork_result);
    event!(
        name: "supervisor.agent.spawned",
        Level::INFO,
        agent.pid = fork_result,
        "spawned agent process with pid {{agent.pid}}",
    );

    // Wait for the child's initial SIGSTOP before returning, so the
    // caller can safely attach ptrace options.
    wait_for_child_stop(child_pid)?;

    Ok(child_pid)
}

/// Child-side setup: change directory, install seccomp, signal parent
/// via `PTRACE_TRACEME` + `SIGSTOP`, then exec.
///
/// This function never returns on success; it calls `_exit(1)` on failure.
fn child_setup(
    program: &CString,
    argv: &[CString],
    envp: &[CString],
    working_dir: &Path,
) -> ! {
    // Change to workspace directory before exec.
    if let Err(_e) = std::env::set_current_dir(working_dir) {
        unsafe { libc::_exit(1) };
    }

    // Request ptrace attachment from the parent.
    // SAFETY: standard ptrace setup in forked child.
    unsafe {
        if libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) < 0 {
            libc::_exit(1);
        }
    }

    // Stop ourselves so the parent can set ptrace options.
    // SAFETY: SIGSTOP is safe to raise in child after PTRACE_TRACEME.
    unsafe {
        libc::raise(libc::SIGSTOP);
    }

    // Install seccomp filter after ptrace is established.
    if sandbox::tracer::seccomp::install_seccomp_filter().is_err() {
        unsafe { libc::_exit(1) };
    }

    // Build null-terminated pointer arrays for execvpe.
    let argv_ptrs: Vec<*const libc::c_char> = argv
        .iter()
        .map(|a| a.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let envp_ptrs: Vec<*const libc::c_char> = envp
        .iter()
        .map(|e| e.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    // SAFETY: argv and envp are valid null-terminated arrays.
    // execve replaces the process image on success.
    unsafe {
        libc::execvpe(
            program.as_ptr(),
            argv_ptrs.as_ptr(),
            envp_ptrs.as_ptr(),
        );
        // execvpe only returns on error.
        libc::_exit(1);
    }
}

/// Waits for the child to hit its initial `SIGSTOP`.
fn wait_for_child_stop(pid: Pid) -> Result<()> {
    let mut status: libc::c_int = 0;
    // SAFETY: waiting on our own child process.
    let ret = unsafe { libc::waitpid(pid.as_raw(), &mut status, 0) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error())
            .context("waitpid for child SIGSTOP failed");
    }
    Ok(())
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
        let result = spawn_agent(&[], &HashMap::new(), Path::new("/"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }
}

//! Process lifecycle event handlers (fork, program replacement, exit).
//!
//! Extracted from the trace loop to keep file sizes manageable.

use std::path::PathBuf;

use anyhow::Result;
use nix::sys::ptrace;
use nix::unistd::Pid;

use crate::events::EventPayload;
use crate::events::process as ep;
use crate::tracer::memory;
use crate::tracer::trace_loop::TracerLoop;

/// Handles fork/vfork/clone by registering the child process.
pub fn handle_fork(tracer: &mut TracerLoop, parent_pid: Pid) -> Result<()> {
    let child_raw = ptrace::getevent(parent_pid)? as i32;
    let parent_u32 = parent_pid.as_raw() as u32;
    let child_u32 = child_raw as u32;

    let child_fds = tracer
        .process_tree
        .get_process(parent_u32)
        .map(|p| p.fds.clone_for_fork())
        .unwrap_or_default();

    let (binary, argv, cwd) = tracer
        .process_tree
        .get_process(parent_u32)
        .map(|p| (p.binary.clone(), p.argv.clone(), p.cwd.clone()))
        .unwrap_or_else(|| (PathBuf::from("unknown"), vec![], PathBuf::from("/")));

    tracer.pipe_registry.on_fork(child_u32, &child_fds);

    tracer
        .process_tree
        .add_process(child_u32, parent_u32, binary, argv, cwd, child_fds);
    tracer.alive_count += 1;

    // Options propagate automatically from PTRACE_SEIZE — no
    // explicit setoptions needed on auto-traced children.

    tracer.emit(EventPayload::Fork(ep::Fork {
        parent_pid: parent_u32,
        child_pid: child_u32,
    }));

    Ok(())
}

/// Handles program replacement by updating binary/argv and closing cloexec fds.
pub fn handle_program_replace(tracer: &mut TracerLoop, pid: Pid) -> Result<()> {
    let pid_u32 = pid.as_raw() as u32;

    let binary = memory::read_proc_exe(pid)
        .unwrap_or_else(|_| PathBuf::from("unknown"));
    let argv = memory::read_proc_cmdline(pid).unwrap_or_default();
    let cwd = std::fs::read_link(format!("/proc/{}/cwd", pid.as_raw()))
        .unwrap_or_else(|_| PathBuf::from("/"));

    let ppid = tracer
        .process_tree
        .get_process(pid_u32)
        .map(|p| p.ppid)
        .unwrap_or(0);

    tracer
        .process_tree
        .update_on_program_replace(pid_u32, binary.clone(), argv.clone());

    if let Some(proc_state) = tracer.process_tree.get_process_mut(pid_u32) {
        proc_state.cwd = cwd.clone();
    }

    tracer.emit(EventPayload::Exec(ep::Exec {
        pid: pid_u32,
        ppid,
        binary: binary.to_string_lossy().into_owned(),
        argv,
        envp: vec![], // Phase 1: envp capture not implemented.
        cwd: cwd.to_string_lossy().into_owned(),
    }));

    Ok(())
}

/// Handles PTRACE_EVENT_EXIT (process about to exit).
pub fn handle_exit_event(tracer: &mut TracerLoop, pid: Pid) -> Result<()> {
    let exit_status = ptrace::getevent(pid)? as i32;
    let pid_u32 = pid.as_raw() as u32;
    let exit_code = (exit_status >> 8) & 0xFF;
    let signal = exit_status & 0x7F;

    tracer.process_tree.mark_exited(pid_u32);
    tracer.alive_count = tracer.alive_count.saturating_sub(1);

    let sig_opt = if signal != 0 { Some(signal) } else { None };

    tracer.emit(EventPayload::Exit(ep::Exit {
        pid: pid_u32,
        exit_code,
        signal: sig_opt,
    }));

    Ok(())
}

/// Handles actual process exit (Exited/Signaled wait status).
pub fn handle_process_exit(
    tracer: &mut TracerLoop,
    pid: Pid,
    code: i32,
    signal: Option<i32>,
) {
    let pid_u32 = pid.as_raw() as u32;

    // Only emit if we haven't already via PTRACE_EVENT_EXIT.
    if tracer
        .process_tree
        .get_process(pid_u32)
        .is_some_and(|p| p.alive)
    {
        tracer.process_tree.mark_exited(pid_u32);
        tracer.alive_count = tracer.alive_count.saturating_sub(1);

        tracer.emit(EventPayload::Exit(ep::Exit {
            pid: pid_u32,
            exit_code: code,
            signal,
        }));
    }
}

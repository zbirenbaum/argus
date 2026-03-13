// Rust guideline compliant 2026-02-21
//! Thin supervisor wiring: delegates all initialization to `SupervisorRuntime`.
//!
//! Owns process lifecycle, API server, and shutdown coordination only.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;

use anyhow::Result;
use tracing::{Level, event};

use argus::api;
use argus::api::state::SharedState;
use argus::config::SupervisorConfig;
use argus::net;
use argus::runtime::SupervisorRuntime;

/// Top-level async entry point: initializes the runtime and runs the pipeline.
///
/// # Errors
///
/// Returns an error if any subsystem fails to initialize.
pub async fn run(
    config: SupervisorConfig,
    agent_env: std::collections::HashMap<String, String>,
    mut mitmdump: Option<net::MitmdumpHandle>,
) -> Result<()> {
    let flow_path = mitmdump.as_ref().and_then(|m| m.flow_output_path().cloned());

    let runtime = SupervisorRuntime::new(config.clone()).await?;

    let shared = runtime.shared_state();
    let (api_shutdown_tx, api_shutdown_rx) = tokio::sync::watch::channel(false);
    spawn_api_server(shared, config.listen_addr, api_shutdown_rx);

    // AgentStart is emitted before spawn so the event log records
    // the supervisor config before any tracee events arrive.
    runtime.emit_agent_start();

    let spawn = crate::startup::spawn_agent(
        &config.agent_command,
        &agent_env,
        &config.workspace_dir,
        config.run_as.as_ref(),
    )?;
    let _stdout_drain = crate::spawn_drain_thread("stdout", spawn.stdout_r);
    let _stderr_drain = crate::spawn_drain_thread("stderr", spawn.stderr_r);

    crate::signals::install_handler();

    let (keylog_handle, keylog_stop) = runtime.spawn_keylog_pipeline();
    let proxy = runtime.spawn_proxy_pipeline(flow_path);

    runtime.emit_initial_state();

    // Spawn the pipeline (which starts the ptrace thread and calls PTRACE_SEIZE)
    // before releasing the sync pipe. The child is still blocked on the pipe read,
    // so seize is guaranteed to happen before the child executes any syscalls.
    let (runner, seize_rx, ptrace_thread) = runtime.into_pipeline(spawn.child_pid);

    // Wait for PTRACE_SEIZE to complete before letting the child proceed.
    // Closing the write end of the sync pipe unblocks the child.
    if let Ok(Err(e)) = seize_rx.await {
        event!(Level::ERROR, error.message = %e, "ptrace seize failed, aborting");
        return Err(e);
    }
    let _ = nix::unistd::close(spawn.sync_pipe_w);

    event!(Level::DEBUG, "wiring: entering pipeline.run()");
    runner.run().await;
    event!(Level::DEBUG, "wiring: pipeline.run() returned, beginning shutdown");

    shutdown(
        keylog_stop,
        keylog_handle,
        proxy,
        mitmdump.as_mut(),
        ptrace_thread,
        api_shutdown_tx,
    )?;

    Ok(())
}

/// Spawns the axum API server as a tokio task.
fn spawn_api_server(
    shared: SharedState,
    listen_addr: std::net::SocketAddr,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        if let Err(e) = api::serve(shared, listen_addr, shutdown_rx).await {
            event!(
                name: "supervisor.api.error",
                Level::ERROR,
                error.message = %e,
                "API server failed: {{error.message}}",
            );
        }
    });

    event!(
        name: "supervisor.api.started",
        Level::INFO,
        listen.addr = %listen_addr,
        "API server listening on {{listen.addr}}",
    );
}

/// Shuts down all subsystems in dependency order.
fn shutdown(
    keylog_stop: Arc<AtomicBool>,
    keylog_handle: JoinHandle<()>,
    proxy: Option<(JoinHandle<()>, Arc<AtomicBool>)>,
    mitmdump: Option<&mut net::MitmdumpHandle>,
    ptrace_thread: JoinHandle<()>,
    api_shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> Result<()> {
    use std::sync::atomic::Ordering;

    // Stop TLS pipelines before mitmdump exits so they can drain final data.
    keylog_stop.store(true, Ordering::Release);
    if let Some((_, ref proxy_stop)) = proxy {
        proxy_stop.store(true, Ordering::Release);
    }
    let _ = keylog_handle.join();
    if let Some((proxy_handle, _)) = proxy {
        let _ = proxy_handle.join();
    }
    event!(Level::DEBUG, "shutdown: keylog and proxy pipelines stopped");

    if let Some(m) = mitmdump {
        event!(Level::DEBUG, "shutdown: stopping mitmdump");
        let _ = m.stop();
        event!(Level::DEBUG, "shutdown: mitmdump stopped");
    }

    // Bus shutdown is performed inside PipelineRunner::run() after the
    // pipeline loop exits, before control returns here.
    let _ = api_shutdown_tx.send(true);
    ptrace_thread.join().ok();

    event!(Level::DEBUG, "shutdown: all subsystems stopped");
    Ok(())
}

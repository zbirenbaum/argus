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

    // Close the write end of the sync pipe — the ptrace thread signals the
    // tracee after attaching; we don't write to it from the async context.
    let _ = nix::unistd::close(spawn.sync_pipe_w);

    let (keylog_handle, keylog_stop) = runtime.spawn_keylog_pipeline();
    let (proxy_handle, proxy_stop) = runtime.spawn_proxy_pipeline(flow_path);

    runtime.emit_initial_state();

    let (runner, ptrace_thread) = runtime.into_pipeline(spawn.child_pid);

    event!(Level::DEBUG, "wiring: entering pipeline.run()");
    runner.run().await;
    event!(Level::DEBUG, "wiring: pipeline.run() returned, beginning shutdown");

    shutdown(
        keylog_stop,
        keylog_handle,
        proxy_stop,
        proxy_handle,
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
    proxy_stop: Arc<AtomicBool>,
    proxy_handle: JoinHandle<()>,
    mitmdump: Option<&mut net::MitmdumpHandle>,
    ptrace_thread: JoinHandle<()>,
    api_shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> Result<()> {
    use std::sync::atomic::Ordering;

    // Stop TLS pipelines before mitmdump exits so they can drain final data.
    keylog_stop.store(true, Ordering::Release);
    proxy_stop.store(true, Ordering::Release);
    let _ = keylog_handle.join();
    let _ = proxy_handle.join();
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

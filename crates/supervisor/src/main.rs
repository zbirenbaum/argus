// Rust guideline compliant 2026-02-21
//! Argus supervisor PID 1 entrypoint.
//!
//! Parses CLI arguments, loads configuration, initializes all subsystems
//! (CA generation, mitmdump proxy, event writer), spawns the traced agent
//! process, and enters the ptrace loop.

mod event_writer;
mod signals;
mod startup;
mod tls_watcher;

use std::fs;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{Level, event};
use tracing_subscriber::EnvFilter;

use argus::api;
use argus::api::state::new_shared_state;
use argus::cas::LocalCas;
use argus::config::SupervisorConfig;
use argus::events::{Event, EventPayload, SequenceGenerator};
use argus::net;
use argus::tracer::TracerLoop;

/// Argus supervisor -- ptrace-based filesystem versioning.
#[derive(Debug, Parser)]
#[command(name = "supervisor", version, about)]
struct Cli {
    /// Unique agent identifier for this session (auto-generated UUID v4 if omitted).
    #[arg(long)]
    agent_id: Option<String>,

    /// Path to YAML configuration file.
    #[arg(long, default_value = "supervisor.yaml")]
    config: PathBuf,

    /// Command and arguments to run as the traced agent.
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    init_tracing();
    let mut config = load_config(&cli)?;
    config.validate()?;

    event!(
        name: "supervisor.start",
        Level::INFO,
        agent.id = %config.agent_id,
        "supervisor starting for agent {{agent.id}}",
    );

    startup::create_data_dirs(&config.data_dir)?;

    // Embed the mitmdump addon so the binary is self-contained.
    const ADDON_SCRIPT: &str = include_str!("../../../scripts/argus_addon.py");
    let addon_script = config.data_dir.join("argus_addon.py");
    fs::write(&addon_script, ADDON_SCRIPT)
        .context("failed to write embedded addon script")?;

    let ca_paths = net::generate_ca(&config.tls.ca_dir)?;
    let agent_env = net::agent_env_vars(&config.tls, &ca_paths);

    let flow_output = config.data_dir.join("flows.jsonl");

    let addon = net::AddonConfig {
        script: Some(addon_script),
        output_file: Some(flow_output),
    };

    let proxy_mode = config.tls.proxy_mode;

    // In off mode, skip mitmdump entirely — only SSLKEYLOGFILE captures keys.
    let upstream = config.tls.upstream_verify();
    let mut mitmdump = if proxy_mode == argus::config::ProxyMode::Off {
        event!(
            name: "supervisor.mitmdump.off",
            Level::INFO,
            "proxy_mode=off, skipping mitmdump",
        );
        None
    } else {
        // mitmdump is optional; log a warning if it fails to start but
        // continue so the supervisor works without mitmproxy installed.
        match net::start_mitmdump_with_flow_capture(
            &ca_paths,
            config.tls.mitm_proxy_port,
            &addon,
            &upstream,
            proxy_mode,
        ) {
        Ok(handle) => Some(handle),
        Err(e) => {
            event!(
                name: "supervisor.mitmdump.skip",
                Level::WARN,
                error.message = %e,
                "mitmdump unavailable, no TLS interception: {{error.message}}",
            );
            None
        }
        }
    };

    let cas_path = config.data_dir.join("cas");
    let cas = LocalCas::new(cas_path.clone())
        .context("failed to initialize CAS store")?;

    // Second CAS handle for the API Bridge (same directory, safe
    // because CAS is append-only with content-addressed dedup).
    let api_cas: std::sync::Arc<dyn argus::cas::Cas> = std::sync::Arc::new(
        LocalCas::new(cas_path.clone()).context("failed to initialize API CAS handle")?,
    );

    let (event_tx, event_rx) = mpsc::channel::<Event>();
    let tracer_seq = SequenceGenerator::default();
    // Separate sequence space so TLS watcher sequences never collide
    // with tracer sequences while both generators are lock-free.
    let tls_seq = SequenceGenerator::new(1_000_000);

    let writer_handle = event_writer::spawn(event_rx);

    emit_agent_start(&event_tx, &config, &tracer_seq);

    let tls_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let tls_cas = LocalCas::new(cas_path.clone())
        .context("failed to initialize TLS watcher CAS handle")?;
    let flow_path = mitmdump.as_ref().and_then(|m| m.flow_output_path().cloned());
    let tls_watcher_handle = tls_watcher::spawn(
        config.tls.keylog_path.clone(),
        flow_path,
        tls_cas,
        event_tx.clone(),
        tls_seq,
        config.agent_id.clone(),
        tls_stop.clone(),
    );

    // Build lock-free bridge for API server + tracer.
    let shared = new_shared_state(config.agent_id.clone(), api_cas);
    shared.store_rules(config.build_ruleset());
    let rules_handle = shared.rules_handle();

    // Spawn the API server on a background tokio runtime.
    let listen_addr = config.listen_addr;
    let api_shared = shared.clone();
    let (api_shutdown_tx, api_shutdown_rx) = tokio::sync::watch::channel(false);
    let api_thread = std::thread::Builder::new()
        .name("api-server".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime for API server");
            rt.block_on(async {
                if let Err(e) = api::serve(api_shared, listen_addr, api_shutdown_rx).await {
                    event!(
                        name: "supervisor.api.error",
                        Level::ERROR,
                        error.message = %e,
                        "API server failed: {{error.message}}",
                    );
                }
            });
        })
        .context("failed to spawn API server thread")?;

    event!(
        name: "supervisor.api.started",
        Level::INFO,
        listen.addr = %listen_addr,
        "API server listening on {{listen.addr}}",
    );

    // Forward API-originated events (pause, resume, approvals) to the
    // main event channel so they appear in the JSONL output.
    let api_event_tx = event_tx.clone();
    let mut api_event_rx = shared.subscribe_events();
    let bridge_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let bridge_stop_flag = bridge_stop.clone();
    let api_event_bridge = std::thread::Builder::new()
        .name("api-event-bridge".into())
        .spawn(move || {
            use tokio::sync::broadcast::error::TryRecvError;
            loop {
                if bridge_stop_flag.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                match api_event_rx.try_recv() {
                    Ok(evt) => {
                        if api_event_tx.send(evt).is_err() {
                            break;
                        }
                    }
                    Err(TryRecvError::Empty) => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(TryRecvError::Closed) => break,
                    Err(TryRecvError::Lagged(_)) => continue,
                }
            }
        })
        .context("failed to spawn API event bridge thread")?;

    let spawn = startup::spawn_agent(
        &config.agent_command,
        &agent_env,
        &config.workspace_dir,
    )?;

    // Drain agent stdout/stderr to supervisor's stderr so stdout
    // stays clean JSONL. Ptrace already captures stdio content.
    let stdout_drain = spawn_drain_thread("stdout", spawn.stdout_r);
    let stderr_drain = spawn_drain_thread("stderr", spawn.stderr_r);

    signals::install_handler();

    let mut tracer = TracerLoop::new(
        config.agent_id.clone(),
        event_tx,
        tracer_seq,
        cas,
    )
    .with_workspace(config.workspace_dir.clone())
    .with_rules(rules_handle)
    .with_shared_state(shared)
    .with_proxy(proxy_mode, config.tls.mitm_proxy_port);
    event!(Level::DEBUG, "main: entering tracer.run()");
    tracer.run(spawn.child_pid, spawn.sync_pipe_w)?;
    event!(Level::DEBUG, "main: tracer.run() returned");

    event!(
        name: "supervisor.shutdown",
        Level::DEBUG,
        "shutdown: tracer.run() returned",
    );

    // Stop TLS watcher first so it can drain final data before
    // mitmdump exits and the flow file stops being written.
    event!(Level::DEBUG, "shutdown: stopping tls-watcher");
    tls_stop.store(true, std::sync::atomic::Ordering::Release);
    let _ = tls_watcher_handle.join();
    event!(Level::DEBUG, "shutdown: tls-watcher stopped");

    if let Some(ref mut m) = mitmdump {
        event!(Level::DEBUG, "shutdown: stopping mitmdump");
        let _ = m.stop();
        event!(Level::DEBUG, "shutdown: mitmdump stopped");
    }

    event!(Level::DEBUG, "shutdown: signalling bridge stop");
    bridge_stop.store(true, std::sync::atomic::Ordering::Release);
    event!(Level::DEBUG, "shutdown: joining bridge thread");
    let _ = api_event_bridge.join();
    event!(Level::DEBUG, "shutdown: bridge thread joined");

    event!(Level::DEBUG, "shutdown: dropping tracer");
    drop(tracer);
    event!(Level::DEBUG, "shutdown: joining writer thread");
    writer_handle.join().expect("event writer thread panicked");
    event!(Level::DEBUG, "shutdown: writer thread joined");

    event!(Level::DEBUG, "shutdown: joining stdout drain");
    let _ = stdout_drain.join();
    event!(Level::DEBUG, "shutdown: joining stderr drain");
    let _ = stderr_drain.join();
    event!(Level::DEBUG, "shutdown: stopping API server");
    let _ = api_shutdown_tx.send(true);
    event!(Level::DEBUG, "shutdown: joining API server thread");
    let _ = api_thread.join();
    event!(Level::DEBUG, "shutdown: all threads joined");

    Ok(())
}

/// Forwards data from an agent pipe to supervisor stderr.
///
/// Agent stdout/stderr are piped so they don't mix with JSONL on
/// stdout. This thread just drains the pipe to stderr.
fn spawn_drain_thread(
    name: &str,
    pipe_fd: OwnedFd,
) -> std::thread::JoinHandle<()> {
    let label = name.to_string();
    std::thread::Builder::new()
        .name(format!("drain-{name}"))
        .spawn(move || {
            let mut pipe = std::fs::File::from(pipe_fd);
            if let Err(e) = std::io::copy(&mut pipe, &mut std::io::stderr()) {
                event!(
                    name: "supervisor.drain.error",
                    Level::WARN,
                    stream = %label,
                    error.message = %e,
                    "drain thread for {{stream}} failed: {{error.message}}",
                );
            }
        })
        .expect("failed to spawn drain thread")
}

/// Initializes the tracing subscriber with JSON output to stderr.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_writer(std::io::stderr)
        .init();
}

/// Loads and merges config from the YAML file with CLI overrides.
fn load_config(cli: &Cli) -> Result<SupervisorConfig> {
    let mut config = if cli.config.exists() {
        let file = fs::File::open(&cli.config).with_context(|| {
            format!("failed to open config file {}", cli.config.display())
        })?;
        SupervisorConfig::load(file)?
    } else {
        event!(
            name: "supervisor.config.default",
            Level::INFO,
            config.path = %cli.config.display(),
            "config not found at {{config.path}}, using defaults",
        );
        SupervisorConfig::default()
    };

    // CLI args take precedence over the config file.
    if let Some(ref id) = cli.agent_id {
        config.agent_id = id.clone();
    }
    config.agent_command = cli.command.clone();

    Ok(config)
}

/// Emits the `AgentStart` control event.
fn emit_agent_start(
    tx: &mpsc::Sender<Event>,
    config: &SupervisorConfig,
    seq_gen: &SequenceGenerator,
) {
    let nspid = argus::config::read_nspid_pair();

    let payload = EventPayload::AgentStart(argus::events::control::AgentStart {
        agent_id: config.agent_id.clone(),
        supervisor_pid_host: nspid.map(|(h, _)| h),
        supervisor_pid_ns: nspid.map(|(_, n)| n),
        config_summary: format!(
            "data_dir={}, workspace={}",
            config.data_dir.display(),
            config.workspace_dir.display(),
        ),
        node: std::env::var("NODE_NAME").ok(),
        pod: std::env::var("POD_NAME").ok(),
        container: std::env::var("CONTAINER_NAME").ok(),
    });

    let evt = Event::new(seq_gen, config.agent_id.clone(), payload);

    if let Err(e) = tx.send(evt) {
        event!(
            name: "supervisor.event.send_error",
            Level::ERROR,
            error.message = %e,
            "failed to send AgentStart event: {{error.message}}",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_basic_args() {
        let args = [
            "supervisor",
            "--agent-id", "test-agent",
            "--", "/bin/echo", "hello",
        ];
        let cli = Cli::try_parse_from(args).unwrap();
        assert_eq!(cli.agent_id.as_deref(), Some("test-agent"));
        assert_eq!(cli.command, vec!["/bin/echo", "hello"]);
        assert_eq!(cli.config, PathBuf::from("supervisor.yaml"));
    }

    #[test]
    fn cli_parses_custom_config() {
        let args = [
            "supervisor",
            "--agent-id", "a1",
            "--config", "/etc/argus.yaml",
            "--", "bash",
        ];
        let cli = Cli::try_parse_from(args).unwrap();
        assert_eq!(cli.config, PathBuf::from("/etc/argus.yaml"));
    }

    #[test]
    fn cli_agent_id_is_optional() {
        let args = ["supervisor", "--", "bash"];
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(cli.agent_id.is_none());
    }

    #[test]
    fn cli_requires_command() {
        let args = ["supervisor", "--agent-id", "x"];
        assert!(Cli::try_parse_from(args).is_err());
    }

    #[test]
    fn load_config_uses_defaults_when_file_missing() {
        init_tracing_for_test();
        let cli = Cli {
            agent_id: Some("test".into()),
            config: PathBuf::from("/nonexistent/config.yaml"),
            command: vec!["echo".into()],
        };
        let config = load_config(&cli).unwrap();
        assert_eq!(config.agent_id, "test");
        assert_eq!(config.agent_command, vec!["echo"]);
        assert_eq!(config.data_dir, PathBuf::from("/data"));
    }

    #[test]
    fn load_config_reads_yaml_file() {
        init_tracing_for_test();
        let dir = tempfile::TempDir::new().unwrap();
        let yaml = "agent_id: yaml-agent\nagent_command: [\"python\", \"run.py\"]\ndata_dir: /custom/data\nworkspace_dir: /custom/ws\n";
        let config_path = dir.path().join("test.yaml");
        fs::write(&config_path, yaml).unwrap();

        let cli = Cli {
            agent_id: Some("yaml-agent".into()),
            config: config_path,
            command: vec!["python".into(), "run.py".into()],
        };
        let config = load_config(&cli).unwrap();
        assert_eq!(config.agent_id, "yaml-agent");
        assert_eq!(config.data_dir, PathBuf::from("/custom/data"));
        assert_eq!(config.workspace_dir, PathBuf::from("/custom/ws"));
    }

    #[test]
    fn emit_agent_start_sends_event() {
        let (tx, rx) = mpsc::channel();
        let seq_gen = SequenceGenerator::default();
        let config = SupervisorConfig {
            agent_id: "start-test".into(),
            agent_command: vec!["echo".into()],
            ..SupervisorConfig::default()
        };
        emit_agent_start(&tx, &config, &seq_gen);
        let evt = rx.recv().unwrap();
        assert_eq!(evt.agent_id, "start-test");
        match &evt.payload {
            EventPayload::AgentStart(s) => {
                assert_eq!(s.agent_id, "start-test");
                assert!(s.config_summary.contains("data_dir"));
            }
            other => panic!("expected AgentStart, got {other:?}"),
        }
    }

    /// Tracing can only be initialized once per process.
    fn init_tracing_for_test() {
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = tracing_subscriber::fmt()
                .with_env_filter("off")
                .with_writer(std::io::sink)
                .try_init();
        });
    }
}

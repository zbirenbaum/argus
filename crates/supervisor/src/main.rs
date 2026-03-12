// Rust guideline compliant 2026-02-21
//! Argus supervisor PID 1 entrypoint.
//!
//! Parses CLI arguments, loads configuration, initializes all subsystems
//! (CA generation, mitmdump proxy, event writer), spawns the traced agent
//! process, and enters the ptrace loop.

mod event_writer;
mod signals;
mod startup;

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{Level, event};
use tracing_subscriber::EnvFilter;

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

    let ca_paths = net::generate_ca(&config.tls.ca_dir)?;
    let agent_env = net::agent_env_vars(&config.tls, &ca_paths);

    // mitmdump is optional; log a warning if it fails to start but
    // continue so the supervisor works without mitmproxy installed.
    let mut mitmdump = match net::start_mitmdump(&ca_paths, config.tls.mitm_proxy_port) {
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
    };

    let cas = LocalCas::new(config.data_dir.join("cas"))
        .context("failed to initialize CAS store")?;

    let (event_tx, event_rx) = mpsc::channel::<Event>();
    let seq_gen = SequenceGenerator::default();

    let writer_handle = event_writer::spawn(event_rx);

    emit_agent_start(&event_tx, &config, &seq_gen);

    let (child_pid, sync_pipe) = startup::spawn_agent(
        &config.agent_command,
        &agent_env,
        &config.workspace_dir,
    )?;

    signals::install_handler();

    let mut tracer = TracerLoop::new(
        config.agent_id.clone(),
        event_tx,
        seq_gen,
        cas,
    );
    tracer.run(child_pid, sync_pipe)?;

    event!(
        name: "supervisor.shutdown",
        Level::INFO,
        "agent exited, shutting down supervisor",
    );

    if let Some(ref mut m) = mitmdump {
        let _ = m.stop();
    }

    // Drop the sender so the writer thread sees channel close.
    drop(tracer);
    writer_handle.join().expect("event writer thread panicked");

    Ok(())
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

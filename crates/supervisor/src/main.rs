// Rust guideline compliant 2026-02-21
//! Argus supervisor PID 1 entrypoint.
//!
//! Parses CLI arguments, loads configuration, initializes TLS/proxy
//! subsystems, then delegates async startup to [`wiring::run`].

mod signals;
mod startup;
mod wiring;

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{Level, event};
use tracing_subscriber::EnvFilter;

use argus::config::SupervisorConfig;
use argus::net;

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

    let mut config = load_config(&cli)?;
    config.validate()?;
    init_tracing(&config.data_dir)?;

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

    let flow_output = config.data_dir.join("flows.jsonl");
    let addon = net::AddonConfig {
        script: Some(addon_script),
        output_file: Some(flow_output),
    };
    let proxy_mode = config.tls.proxy_mode;

    // In off mode, skip mitmdump entirely — only SSLKEYLOGFILE captures keys.
    let upstream = config.tls.upstream_verify();
    let mitmdump = if proxy_mode == argus::config::ProxyMode::Off {
        event!(
            name: "supervisor.mitmdump.off",
            Level::INFO,
            "proxy_mode=off, skipping mitmdump",
        );
        None
    } else {
        // mitmdump is optional — log a warning and continue if unavailable.
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
                // Downgrade to Off so HTTPS_PROXY isn't set for a
                // non-existent proxy.
                config.tls.proxy_mode = argus::config::ProxyMode::Off;
                None
            }
        }
    };

    // Build agent env AFTER mitmdump resolution so proxy_mode reflects
    // actual availability.
    let agent_env = net::agent_env_vars(&config.tls, &ca_paths);

    // Tokio runtime for the pipeline, API server, and S3 upload pool.
    // 4 worker threads balance ptrace-loop overhead against async I/O concurrency.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    rt.block_on(wiring::run(config, agent_env, mitmdump))?;

    Ok(())
}


/// Initializes the tracing subscriber with JSON output to a log file.
///
/// Writes to `{data_dir}/supervisor.log`. Creates the directory if needed.
///
/// # Errors
///
/// Returns an error if the log file cannot be created.
fn init_tracing(data_dir: &std::path::Path) -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let log_path = data_dir.join("supervisor.log");
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open log file {}", log_path.display()))?;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_writer(log_file)
        .init();

    Ok(())
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

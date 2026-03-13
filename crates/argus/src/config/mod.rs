//! Supervisor configuration types and validation.
//!
//! All configuration is loaded from a YAML file (default: `supervisor.yaml`).
//! Every struct derives `Serialize`, `Deserialize`, `Debug`, and `Clone` for
//! round-trip fidelity and diagnostic output.
//!
//! Use [`SupervisorConfig::load`] to parse from a reader, or
//! [`SupervisorConfig::default`] for sensible defaults suitable for local
//! development.

mod capture;
mod durability;
mod enrich;
mod output;
mod pause_rules;
mod redact;
mod storage;
mod tls;

pub use capture::{CaptureConfig, CapturePathConfig};
pub use durability::{DurabilityConfig, DurabilityMode, DurabilityOverride};
pub use enrich::{Category, CategoryConfig, EnrichConfig};
pub use output::OutputConfig;
pub use pause_rules::{
    MatchKind, PauseAction, PauseMatchKind, PauseRule, Rule, RuleDecision, RuleSet,
};
pub use redact::{BuiltinRedactions, RedactConfig, RedactPattern};
pub use storage::{DigestCacheConfig, LocalBufferConfig, S3Config, StorageConfig, UploadConfig};
pub use tls::{ProxyMode, TlsConfig, UpstreamVerify};

use std::io::Read;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

/// Top-level supervisor configuration.
///
/// Combines agent identity, storage, durability, networking, and
/// pause-before-action rules into a single validated bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupervisorConfig {
    /// Unique identifier for this agent instance.
    #[serde(default)]
    pub agent_id: String,

    /// Command and arguments to exec as the traced agent process.
    #[serde(default)]
    pub agent_command: Vec<String>,

    /// Root directory for local CAS, event segments, and indexes.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,

    /// Agent working directory (mounted volume).
    #[serde(default = "default_workspace_dir")]
    pub workspace_dir: PathBuf,

    /// Filesystem subtrees to snapshot and watch for changes.
    #[serde(default = "default_watched_paths")]
    pub watched_paths: Vec<PathBuf>,

    /// Object storage configuration. `None` disables remote uploads.
    #[serde(default)]
    pub storage: StorageConfig,

    /// REST API / WebSocket listen address.
    #[serde(default = "default_listen_addr")]
    pub listen_addr: SocketAddr,

    /// Durability mode and per-path overrides.
    #[serde(default)]
    pub durability: DurabilityConfig,

    /// TLS interception and MITM proxy settings.
    #[serde(default)]
    pub tls: TlsConfig,

    /// Drop privileges to this UID:GID before exec'ing the agent.
    ///
    /// When set, the supervisor calls `setgid()`/`setuid()` in the
    /// forked child before `execvpe()`. The supervisor itself stays
    /// root (required for ptrace). Omit or set to `null` to run the
    /// agent as root.
    #[serde(default)]
    pub run_as: Option<RunAs>,

    /// Rules that immediately deny syscalls with EPERM.
    #[serde(default)]
    pub block: Vec<Rule>,

    /// Rules that pause syscalls for operator approval.
    #[serde(default)]
    pub pause_before: Vec<PauseRule>,

    /// Content capture policy — which paths get full content vs metadata-only vs ignored.
    #[serde(default)]
    pub capture: CaptureConfig,

    /// When true, record raw ptrace stops to raw_stops.jsonl for offline replay.
    #[serde(default)]
    pub record_raw_stops: bool,

    /// Controls which event data categories are inlined and at what size.
    #[serde(default)]
    pub enrich: EnrichConfig,

    /// Three-tier PII scrubbing: path exclusion, field drop, value scan.
    #[serde(default)]
    pub redact: RedactConfig,

    /// Destinations that receive every enriched event record.
    #[serde(default = "default_outputs")]
    pub outputs: Vec<OutputConfig>,
}

/// UID/GID to drop to before exec'ing the agent process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunAs {
    pub uid: u32,
    #[serde(default)]
    pub gid: Option<u32>,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            agent_id: String::new(),
            agent_command: Vec::new(),
            data_dir: default_data_dir(),
            workspace_dir: default_workspace_dir(),
            watched_paths: default_watched_paths(),
            storage: StorageConfig::default(),
            listen_addr: default_listen_addr(),
            durability: DurabilityConfig::default(),
            tls: TlsConfig::default(),
            run_as: None,
            block: Vec::new(),
            pause_before: Vec::new(),
            capture: CaptureConfig::default(),
            record_raw_stops: false,
            enrich: EnrichConfig::default(),
            redact: RedactConfig::default(),
            outputs: default_outputs(),
        }
    }
}

impl SupervisorConfig {
    /// Build a [`RuleSet`] from the config's block and pause rules.
    ///
    /// The returned rule set has all glob patterns pre-compiled.
    pub fn build_ruleset(&self) -> RuleSet {
        let mut rs = RuleSet {
            block: self.block.clone(),
            pause_before: self.pause_before.clone(),
        };
        rs.compile_patterns();
        rs
    }
}

impl SupervisorConfig {
    /// Parse configuration from a YAML reader.
    ///
    /// Compiles all glob patterns after deserialization so that
    /// pattern errors surface at load time rather than at match time.
    ///
    /// # Errors
    ///
    /// Returns an error if the YAML is malformed or deserialization fails.
    pub fn load(reader: impl Read) -> anyhow::Result<Self> {
        let mut config: Self =
            serde_yaml::from_reader(reader).context("failed to parse supervisor config YAML")?;
        config.compile_patterns();
        Ok(config)
    }

    /// Validate semantic invariants beyond what serde enforces.
    ///
    /// # Errors
    ///
    /// Returns an error when `agent_command` is empty,
    /// or `data_dir` / `workspace_dir` are empty strings.
    pub fn validate(&mut self) -> anyhow::Result<()> {
        if self.agent_id.is_empty() {
            self.agent_id = generate_agent_id();
        }
        if self.agent_command.is_empty() {
            bail!("agent_command must not be empty");
        }
        if self.data_dir.as_os_str().is_empty() {
            bail!("data_dir must not be empty");
        }
        if self.workspace_dir.as_os_str().is_empty() {
            bail!("workspace_dir must not be empty");
        }
        self.storage.validate()?;
        Ok(())
    }

    /// Pre-compile all glob patterns in durability overrides and rules.
    fn compile_patterns(&mut self) {
        self.durability.validate_patterns();
        for rule in &mut self.block {
            rule.validate_patterns();
        }
        for rule in &mut self.pause_before {
            rule.validate_patterns();
        }
    }
}

fn default_outputs() -> Vec<OutputConfig> {
    vec![OutputConfig::Stdout { flush_every_event: true }]
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("/data")
}

fn default_workspace_dir() -> PathBuf {
    PathBuf::from("/workspace")
}

fn default_watched_paths() -> Vec<PathBuf> {
    vec![PathBuf::from("/workspace")]
}

/// Bind only to localhost; the API is not intended for external access.
fn default_listen_addr() -> SocketAddr {
    "127.0.0.1:9090".parse().expect("hardcoded listen address is valid")
}

/// Generates an agent ID from hostname and host-visible PID.
///
/// Produces IDs like `gke-pool-1-abc-14523` so operators can map from
/// agent identity to a specific process on a specific node. Falls back
/// to `AGENT_ID` env var, then `HOSTNAME` env var, then `/etc/hostname`.
fn generate_agent_id() -> String {
    if let Ok(id) = std::env::var("AGENT_ID")
        && !id.is_empty() {
            return id;
        }

    let hostname = std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_owned())
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into());

    let host_pid = read_host_pid().unwrap_or_else(std::process::id);

    format!("{hostname}-{host_pid}")
}

/// Reads the host-visible PID from `/proc/1/status` NSpid line.
///
/// Inside a PID namespace the supervisor runs as PID 1, but the host
/// sees a different PID. The `NSpid` line lists PIDs from outermost
/// to innermost namespace: `NSpid: 14523 1`.
pub(crate) fn read_host_pid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/1/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("NSpid:") {
            // First field is the outermost (host) PID.
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// Reads all namespace PID layers from `/proc/1/status` NSpid line.
///
/// Returns `(host_pid, namespace_pid)` if both are present.
pub(crate) fn read_nspid_pair() -> Option<(u32, u32)> {
    let status = std::fs::read_to_string("/proc/1/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("NSpid:") {
            let mut pids = rest.split_whitespace();
            let host = pids.next()?.parse().ok()?;
            let ns = pids.next()?.parse().ok()?;
            return Some((host, ns));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_values() {
        let cfg = SupervisorConfig::default();
        assert_eq!(cfg.data_dir, PathBuf::from("/data"));
        assert_eq!(cfg.workspace_dir, PathBuf::from("/workspace"));
        assert_eq!(cfg.listen_addr.port(), 9090);
        assert_eq!(cfg.durability.default, DurabilityMode::Local);
        assert!(cfg.block.is_empty());
        assert!(cfg.pause_before.is_empty());
        assert!(cfg.storage.s3.is_none());
    }

    #[test]
    fn validation_generates_agent_id_when_empty() {
        let mut cfg = SupervisorConfig {
            agent_command: vec!["bash".into()],
            ..SupervisorConfig::default()
        };
        assert!(cfg.agent_id.is_empty());
        cfg.validate().unwrap();
        assert!(!cfg.agent_id.is_empty());
        // Generated ID is hostname-pid format.
        assert!(cfg.agent_id.contains('-'), "expected hostname-pid format, got: {}", cfg.agent_id);
    }

    #[test]
    fn validation_rejects_empty_command() {
        let mut cfg = SupervisorConfig {
            agent_id: "test-agent".into(),
            ..SupervisorConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("agent_command"));
    }

    #[test]
    fn validation_passes_for_valid_config() {
        let mut cfg = SupervisorConfig {
            agent_id: "agent-1".into(),
            agent_command: vec!["python".into(), "main.py".into()],
            ..SupervisorConfig::default()
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn parse_minimal_yaml() {
        let yaml = r#"
agent_id: my-agent
agent_command: ["python", "run.py"]
"#;
        let cfg = SupervisorConfig::load(yaml.as_bytes()).unwrap();
        assert_eq!(cfg.agent_id, "my-agent");
        assert_eq!(cfg.agent_command, vec!["python", "run.py"]);
        assert_eq!(cfg.data_dir, PathBuf::from("/data"));
    }

    #[test]
    fn parse_full_yaml() {
        let yaml = r#"
agent_id: full-agent
agent_command: ["bash", "-c", "echo hello"]
data_dir: /custom/data
workspace_dir: /custom/workspace
watched_paths:
  - /custom/workspace
  - /home/user
listen_addr: "0.0.0.0:8080"
storage:
  s3:
    bucket: my-bucket
    prefix: "agents/{agent_id}/"
    region: us-west-2
  upload:
    max_concurrent: 8
    retry_max: 3
  local_buffer:
    cas_dir: /custom/cas
    event_dir: /custom/events
  digest_cache:
    path: /custom/digest-cache.bin
    rebuild_on_start: false
durability:
  default: memory
  overrides:
    - paths: ["*.key", "*.pem"]
      mode: remote
tls:
  ca_dir: /custom/ca
  keylog_path: /custom/keylog.txt
  mitm_proxy_port: 9999
block:
  - type: read
    paths: ["*.env", "*.key"]
pause_before:
  - type: unlink
    paths: ["/workspace/**"]
  - type: exec
    binaries: ["rm", "curl"]
"#;
        let cfg = SupervisorConfig::load(yaml.as_bytes()).unwrap();
        assert_eq!(cfg.data_dir, PathBuf::from("/custom/data"));
        assert_eq!(cfg.listen_addr.port(), 8080);
        assert_eq!(cfg.durability.default, DurabilityMode::Memory);
        assert_eq!(cfg.durability.overrides.len(), 1);
        assert_eq!(cfg.block.len(), 1);
        assert_eq!(cfg.pause_before.len(), 2);
        assert!(cfg.storage.s3.is_some());
        let s3 = cfg.storage.s3.as_ref().unwrap();
        assert_eq!(s3.bucket, "my-bucket");
    }

    #[test]
    fn serde_round_trip() {
        let cfg = SupervisorConfig {
            agent_id: "rt-agent".into(),
            agent_command: vec!["node".into(), "index.js".into()],
            ..SupervisorConfig::default()
        };
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        let parsed: SupervisorConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn build_ruleset_combines_block_and_pause() {
        let yaml = r#"
agent_id: rs-agent
agent_command: ["bash"]
block:
  - type: read
    paths: ["*.env"]
pause_before:
  - type: exec
    binaries: ["rm"]
"#;
        let cfg = SupervisorConfig::load(yaml.as_bytes()).unwrap();
        let rs = cfg.build_ruleset();
        assert_eq!(rs.block.len(), 1);
        assert_eq!(rs.pause_before.len(), 1);
        assert_eq!(rs.rule_count(), 2);

        let decision = rs.evaluate(MatchKind::Read, Some(".env"), None, None);
        assert!(matches!(decision, RuleDecision::Block { .. }));
    }
}

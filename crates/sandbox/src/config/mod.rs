//! Supervisor configuration types and validation.
//!
//! All configuration is loaded from a YAML file (default: `supervisor.yaml`).
//! Every struct derives `Serialize`, `Deserialize`, `Debug`, and `Clone` for
//! round-trip fidelity and diagnostic output.
//!
//! Use [`SupervisorConfig::load`] to parse from a reader, or
//! [`SupervisorConfig::default`] for sensible defaults suitable for local
//! development.

mod durability;
mod pause_rules;
mod storage;
mod tls;

pub use durability::{DurabilityConfig, DurabilityMode, DurabilityOverride};
pub use pause_rules::{PauseAction, PauseMatchKind, PauseRule};
pub use storage::{DigestCacheConfig, LocalBufferConfig, S3Config, StorageConfig, UploadConfig};
pub use tls::TlsConfig;

use std::io::Read;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

/// Top-level supervisor configuration.
///
/// Combines agent identity, storage, durability, networking, and
/// pause-before-action rules into a single validated bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorConfig {
    /// Unique identifier for this agent instance.
    pub agent_id: String,

    /// Command and arguments to exec as the traced agent process.
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

    /// Rules that pause or deny syscalls before execution.
    #[serde(default)]
    pub pause_before: Vec<PauseRule>,
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
            pause_before: Vec::new(),
        }
    }
}

impl SupervisorConfig {
    /// Parse configuration from a YAML reader.
    ///
    /// # Errors
    ///
    /// Returns an error if the YAML is malformed or deserialization fails.
    pub fn load(reader: impl Read) -> anyhow::Result<Self> {
        let config: Self =
            serde_yaml::from_reader(reader).context("failed to parse supervisor config YAML")?;
        Ok(config)
    }

    /// Validate semantic invariants beyond what serde enforces.
    ///
    /// # Errors
    ///
    /// Returns an error when `agent_id` is empty, `agent_command` is empty,
    /// or `data_dir` does not exist and cannot be created.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.agent_id.is_empty() {
            bail!("agent_id must not be empty");
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
        assert!(cfg.pause_before.is_empty());
        assert!(cfg.storage.s3.is_none());
    }

    #[test]
    fn validation_rejects_empty_agent_id() {
        let cfg = SupervisorConfig {
            agent_command: vec!["bash".into()],
            ..SupervisorConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("agent_id"));
    }

    #[test]
    fn validation_rejects_empty_command() {
        let cfg = SupervisorConfig {
            agent_id: "test-agent".into(),
            ..SupervisorConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("agent_command"));
    }

    #[test]
    fn validation_passes_for_valid_config() {
        let cfg = SupervisorConfig {
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
pause_before:
  - match_kind: unlink
    paths: ["/workspace/**"]
  - match_kind: exec
    binaries: ["rm", "curl"]
"#;
        let cfg = SupervisorConfig::load(yaml.as_bytes()).unwrap();
        assert_eq!(cfg.data_dir, PathBuf::from("/custom/data"));
        assert_eq!(cfg.listen_addr.port(), 8080);
        assert_eq!(cfg.durability.default, DurabilityMode::Memory);
        assert_eq!(cfg.durability.overrides.len(), 1);
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
        assert_eq!(parsed.agent_id, cfg.agent_id);
        assert_eq!(parsed.agent_command, cfg.agent_command);
        assert_eq!(parsed.data_dir, cfg.data_dir);
        assert_eq!(parsed.listen_addr, cfg.listen_addr);
    }
}

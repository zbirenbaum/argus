//! TLS interception and MITM proxy configuration.
//!
//! Controls where the generated CA keypair is stored, where TLS key-log
//! data is written, and which port the `mitmdump` child process listens on.

// Rust guideline compliant 2026-02-21

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// TLS interception settings for the MITM proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// Directory for the generated CA certificate and private key.
    #[serde(default = "default_ca_dir")]
    pub ca_dir: PathBuf,

    /// Path where `SSLKEYLOGFILE` data is written for TLS decryption.
    #[serde(default = "default_keylog_path")]
    pub keylog_path: PathBuf,

    /// Local port for the `mitmdump` transparent proxy.
    #[serde(default = "default_mitm_proxy_port")]
    pub mitm_proxy_port: u16,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            ca_dir: default_ca_dir(),
            keylog_path: default_keylog_path(),
            mitm_proxy_port: default_mitm_proxy_port(),
        }
    }
}

fn default_ca_dir() -> PathBuf {
    PathBuf::from("/data/tls")
}

fn default_keylog_path() -> PathBuf {
    PathBuf::from("/data/tls/keylog.txt")
}

/// Port 8080 matches the default in the supervisor startup sequence.
fn default_mitm_proxy_port() -> u16 {
    8080
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let cfg = TlsConfig::default();
        assert_eq!(cfg.ca_dir, PathBuf::from("/data/tls"));
        assert_eq!(cfg.keylog_path, PathBuf::from("/data/tls/keylog.txt"));
        assert_eq!(cfg.mitm_proxy_port, 8080);
    }

    #[test]
    fn yaml_round_trip() {
        let cfg = TlsConfig {
            ca_dir: PathBuf::from("/custom/ca"),
            keylog_path: PathBuf::from("/custom/keylog.txt"),
            mitm_proxy_port: 9999,
        };
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        let parsed: TlsConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.ca_dir, cfg.ca_dir);
        assert_eq!(parsed.keylog_path, cfg.keylog_path);
        assert_eq!(parsed.mitm_proxy_port, cfg.mitm_proxy_port);
    }

    #[test]
    fn deserialize_with_defaults() {
        let yaml = "ca_dir: /my/ca\n";
        let cfg: TlsConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.ca_dir, PathBuf::from("/my/ca"));
        assert_eq!(cfg.mitm_proxy_port, 8080);
    }
}

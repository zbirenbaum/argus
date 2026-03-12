//! TLS interception and MITM proxy configuration.
//!
//! Controls where the generated CA keypair is stored, where TLS key-log
//! data is written, which port the `mitmdump` child process listens on,
//! and how traffic is routed to it.


use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How the supervisor routes agent traffic through the MITM proxy.
///
/// - `Env` — sets `HTTPS_PROXY`/`HTTP_PROXY` env vars on the agent.
///   Covers most HTTP libraries but can be overridden by the agent.
/// - `Transparent` — rewrites `connect()` sockaddr via ptrace to
///   redirect TLS traffic through mitmdump. Works on statically
///   linked binaries and programs that ignore proxy env vars.
/// - `Off` — no proxy routing. `SSLKEYLOGFILE` still captures keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    /// Route via HTTPS_PROXY/HTTP_PROXY env vars (default).
    #[default]
    Env,
    /// Rewrite connect() destinations at the syscall level.
    Transparent,
    /// Disable proxy routing entirely.
    Off,
}

/// TLS interception settings for the MITM proxy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    /// How agent traffic reaches the proxy.
    #[serde(default)]
    pub proxy_mode: ProxyMode,

    /// CA bundle mitmdump trusts for upstream (backend) connections.
    ///
    /// When set, mitmdump verifies upstream TLS certs against this CA
    /// instead of the system trust store. Required when agents call
    /// internal services signed by a private CA (corporate PKI,
    /// service mesh certs). Without this, those calls get 502s.
    ///
    /// ```yaml
    /// tls:
    ///   upstream_ca: /etc/argus/internal-ca.pem
    /// ```
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_ca: Option<PathBuf>,

    /// Skip all upstream TLS certificate verification.
    ///
    /// **Dev/test escape hatch only.** When `true`, mitmdump accepts
    /// any upstream certificate including self-signed. Do not enable
    /// in production — use `upstream_ca` to trust specific CAs instead.
    ///
    /// ```yaml
    /// tls:
    ///   upstream_insecure: true   # only for dev/test!
    /// ```
    #[serde(default)]
    pub upstream_insecure: bool,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            ca_dir: default_ca_dir(),
            keylog_path: default_keylog_path(),
            mitm_proxy_port: default_mitm_proxy_port(),
            proxy_mode: ProxyMode::default(),
            upstream_ca: None,
            upstream_insecure: false,
        }
    }
}

/// How mitmdump should verify upstream (backend) TLS certificates.
///
/// Built from `TlsConfig::upstream_ca` and `TlsConfig::upstream_insecure`.
#[derive(Debug, Clone, PartialEq)]
pub enum UpstreamVerify {
    /// Verify using the system trust store (default).
    SystemStore,
    /// Verify using a specific CA bundle.
    CustomCa(PathBuf),
    /// Skip all verification (dev/test only).
    Insecure,
}

impl TlsConfig {
    /// Derive the upstream verification mode from config fields.
    ///
    /// `upstream_insecure` takes precedence over `upstream_ca`.
    pub fn upstream_verify(&self) -> UpstreamVerify {
        if self.upstream_insecure {
            UpstreamVerify::Insecure
        } else if let Some(ref ca) = self.upstream_ca {
            UpstreamVerify::CustomCa(ca.clone())
        } else {
            UpstreamVerify::SystemStore
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
        assert_eq!(cfg.proxy_mode, ProxyMode::Env);
        assert_eq!(cfg.upstream_ca, None);
        assert!(!cfg.upstream_insecure);
    }

    #[test]
    fn yaml_round_trip() {
        let cfg = TlsConfig {
            ca_dir: PathBuf::from("/custom/ca"),
            keylog_path: PathBuf::from("/custom/keylog.txt"),
            mitm_proxy_port: 9999,
            proxy_mode: ProxyMode::Transparent,
            upstream_ca: Some(PathBuf::from("/custom/internal-ca.pem")),
            upstream_insecure: false,
        };
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        let parsed: TlsConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.ca_dir, cfg.ca_dir);
        assert_eq!(parsed.keylog_path, cfg.keylog_path);
        assert_eq!(parsed.mitm_proxy_port, cfg.mitm_proxy_port);
        assert_eq!(parsed.proxy_mode, cfg.proxy_mode);
        assert_eq!(parsed.upstream_ca, cfg.upstream_ca);
        assert_eq!(parsed.upstream_insecure, cfg.upstream_insecure);
    }

    #[test]
    fn deserialize_with_defaults() {
        let yaml = "ca_dir: /my/ca\n";
        let cfg: TlsConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.ca_dir, PathBuf::from("/my/ca"));
        assert_eq!(cfg.mitm_proxy_port, 8080);
        assert_eq!(cfg.proxy_mode, ProxyMode::Env);
        assert_eq!(cfg.upstream_ca, None);
        assert!(!cfg.upstream_insecure);
    }

    #[test]
    fn deserialize_proxy_mode_transparent() {
        let yaml = "proxy_mode: transparent\n";
        let cfg: TlsConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.proxy_mode, ProxyMode::Transparent);
    }

    #[test]
    fn deserialize_proxy_mode_off() {
        let yaml = "proxy_mode: off\n";
        let cfg: TlsConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.proxy_mode, ProxyMode::Off);
    }

    #[test]
    fn upstream_verify_default_is_system_store() {
        let cfg = TlsConfig::default();
        assert_eq!(cfg.upstream_verify(), UpstreamVerify::SystemStore);
    }

    #[test]
    fn upstream_verify_custom_ca() {
        let cfg = TlsConfig {
            upstream_ca: Some(PathBuf::from("/pki/ca.pem")),
            ..TlsConfig::default()
        };
        assert_eq!(
            cfg.upstream_verify(),
            UpstreamVerify::CustomCa(PathBuf::from("/pki/ca.pem")),
        );
    }

    #[test]
    fn upstream_verify_insecure_wins_over_custom_ca() {
        let cfg = TlsConfig {
            upstream_ca: Some(PathBuf::from("/pki/ca.pem")),
            upstream_insecure: true,
            ..TlsConfig::default()
        };
        assert_eq!(cfg.upstream_verify(), UpstreamVerify::Insecure);
    }
}

//! Agent environment variables for TLS interception.
//!
//! Builds the set of environment variables that make an agent process
//! route HTTP/HTTPS traffic through the MITM proxy and trust the
//! sandbox CA certificate. These variables are injected into the
//! agent's process environment at spawn time.

use std::collections::HashMap;

use crate::config::TlsConfig;
use crate::net::CaPaths;

/// Build the environment variables for the traced agent process.
///
/// Returns a map containing proxy settings, the TLS key-log path,
/// and CA certificate paths for common TLS libraries (OpenSSL,
/// Node.js, Python requests).
pub fn agent_env_vars(config: &TlsConfig, ca: &CaPaths) -> HashMap<String, String> {
    let proxy = format!("http://127.0.0.1:{}", config.mitm_proxy_port);
    let keylog = config.keylog_path.display().to_string();

    // The agent must trust the mitmdump CA, not the argus sandbox CA.
    // mitmdump generates its own CA at <confdir>/mitmproxy-ca-cert.pem
    // when started with --set confdir=<dir>.
    let mitm_cert = ca
        .cert
        .parent()
        .map(|dir| dir.join("mitmproxy-ca-cert.pem"))
        .unwrap_or_else(|| ca.cert.clone());
    let cert = mitm_cert.display().to_string();

    let mut env = HashMap::with_capacity(6);
    env.insert("HTTPS_PROXY".into(), proxy.clone());
    env.insert("HTTP_PROXY".into(), proxy);
    env.insert("SSLKEYLOGFILE".into(), keylog);
    env.insert("SSL_CERT_FILE".into(), cert.clone());
    env.insert("REQUESTS_CA_BUNDLE".into(), cert.clone());
    env.insert("NODE_EXTRA_CA_CERTS".into(), cert);
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_config() -> TlsConfig {
        TlsConfig {
            ca_dir: PathBuf::from("/data/tls"),
            keylog_path: PathBuf::from("/data/tls/keylog.txt"),
            mitm_proxy_port: 8080,
        }
    }

    fn test_ca_paths() -> CaPaths {
        CaPaths {
            cert: PathBuf::from("/data/tls/ca-cert.pem"),
            key: PathBuf::from("/data/tls/ca-key.pem"),
        }
    }

    #[test]
    fn returns_all_six_required_keys() {
        let env = agent_env_vars(&test_config(), &test_ca_paths());

        let expected_keys = [
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "SSLKEYLOGFILE",
            "SSL_CERT_FILE",
            "REQUESTS_CA_BUNDLE",
            "NODE_EXTRA_CA_CERTS",
        ];

        assert_eq!(env.len(), 6, "must have exactly 6 env vars");
        for key in &expected_keys {
            assert!(env.contains_key(*key), "missing key: {key}");
        }
    }

    #[test]
    fn proxy_urls_use_configured_port() {
        let mut config = test_config();
        config.mitm_proxy_port = 9999;

        let env = agent_env_vars(&config, &test_ca_paths());

        assert_eq!(env["HTTPS_PROXY"], "http://127.0.0.1:9999");
        assert_eq!(env["HTTP_PROXY"], "http://127.0.0.1:9999");
    }

    #[test]
    fn cert_paths_point_to_mitmproxy_ca() {
        let env = agent_env_vars(&test_config(), &test_ca_paths());
        let expected = "/data/tls/mitmproxy-ca-cert.pem";

        assert_eq!(env["SSL_CERT_FILE"], expected);
        assert_eq!(env["REQUESTS_CA_BUNDLE"], expected);
        assert_eq!(env["NODE_EXTRA_CA_CERTS"], expected);
    }

    #[test]
    fn keylog_path_matches_config() {
        let env = agent_env_vars(&test_config(), &test_ca_paths());
        assert_eq!(env["SSLKEYLOGFILE"], "/data/tls/keylog.txt");
    }

    #[test]
    fn custom_keylog_path() {
        let mut config = test_config();
        config.keylog_path = PathBuf::from("/custom/keylog.txt");

        let env = agent_env_vars(&config, &test_ca_paths());
        assert_eq!(env["SSLKEYLOGFILE"], "/custom/keylog.txt");
    }
}

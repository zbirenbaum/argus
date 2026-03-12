//! Self-signed CA certificate generation for TLS interception.
//!
//! Uses the `rcgen` crate to produce an ECDSA P-256 CA keypair. The
//! certificate is written to `ca-cert.pem` and the private key to
//! `ca-key.pem` inside the configured CA directory. Generation is
//! idempotent: if both files already exist, their paths are returned
//! without overwriting.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rcgen::{CertificateParams, IsCa, KeyPair};
use tracing::{event, Level};

/// File paths to the generated CA certificate and key.
#[derive(Debug, Clone)]
pub struct CaPaths {
    /// PEM-encoded CA certificate.
    pub cert: PathBuf,
    /// PEM-encoded CA private key.
    pub key: PathBuf,
}

/// CA certificate common name used for all generated certificates.
const CA_COMMON_NAME: &str = "argus-sandbox-ca";

/// Validity period prevents re-generation during long-running sessions.
const CA_VALIDITY_DAYS: u32 = 3650;

const CERT_FILENAME: &str = "ca-cert.pem";
const KEY_FILENAME: &str = "ca-key.pem";

/// Generate a self-signed CA keypair if not already present.
///
/// Creates `ca_dir` if it does not exist, then writes `ca-cert.pem` and
/// `ca-key.pem`. When both files already exist, returns their paths
/// without regenerating.
///
/// # Errors
///
/// Returns an error if directory creation fails, certificate generation
/// fails, or the PEM files cannot be written.
pub fn generate_ca(ca_dir: &Path) -> Result<CaPaths> {
    let cert_path = ca_dir.join(CERT_FILENAME);
    let key_path = ca_dir.join(KEY_FILENAME);

    // Both files must exist and be non-empty; a crash mid-write could
    // leave one file present but the other missing or zero-length.
    let cert_ok = fs::metadata(&cert_path).is_ok_and(|m| m.len() > 0);
    let key_ok = fs::metadata(&key_path).is_ok_and(|m| m.len() > 0);

    if cert_ok && key_ok {
        event!(
            name: "net.ca.reuse",
            Level::INFO,
            ca.dir = %ca_dir.display(),
            "reusing existing CA keypair at {{ca.dir}}",
        );
        return Ok(CaPaths {
            cert: cert_path,
            key: key_path,
        });
    }

    fs::create_dir_all(ca_dir)
        .with_context(|| format!("failed to create CA directory {}", ca_dir.display()))?;

    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .context("failed to generate ECDSA P-256 key pair")?;

    let mut params = CertificateParams::new(Vec::new())
        .context("failed to create certificate params")?;
    params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        rcgen::DnValue::Utf8String(CA_COMMON_NAME.into()),
    );
    params.not_before = time::OffsetDateTime::now_utc();
    params.not_after =
        params.not_before + time::Duration::days(i64::from(CA_VALIDITY_DAYS));
    params
        .key_usages
        .push(rcgen::KeyUsagePurpose::KeyCertSign);
    params
        .key_usages
        .push(rcgen::KeyUsagePurpose::CrlSign);

    let cert = params
        .self_signed(&key_pair)
        .context("failed to self-sign CA certificate")?;

    fs::write(&cert_path, cert.pem())
        .with_context(|| format!("failed to write CA cert to {}", cert_path.display()))?;
    fs::write(&key_path, key_pair.serialize_pem())
        .with_context(|| format!("failed to write CA key to {}", key_path.display()))?;

    event!(
        name: "net.ca.generated",
        Level::INFO,
        ca.dir = %ca_dir.display(),
        "generated new CA keypair at {{ca.dir}}",
    );

    Ok(CaPaths {
        cert: cert_path,
        key: key_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generate_creates_cert_and_key() {
        let tmp = TempDir::new().unwrap();
        let ca_dir = tmp.path().join("ca");

        let paths = generate_ca(&ca_dir).unwrap();

        assert!(paths.cert.exists(), "cert file must exist");
        assert!(paths.key.exists(), "key file must exist");

        let cert_pem = fs::read_to_string(&paths.cert).unwrap();
        assert!(
            cert_pem.contains("BEGIN CERTIFICATE"),
            "cert must be PEM-encoded"
        );

        let key_pem = fs::read_to_string(&paths.key).unwrap();
        assert!(
            key_pem.contains("BEGIN PRIVATE KEY"),
            "key must be PEM-encoded"
        );
    }

    #[test]
    fn generate_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let ca_dir = tmp.path().join("ca");

        let first = generate_ca(&ca_dir).unwrap();
        let first_cert = fs::read_to_string(&first.cert).unwrap();
        let first_key = fs::read_to_string(&first.key).unwrap();

        let second = generate_ca(&ca_dir).unwrap();
        let second_cert = fs::read_to_string(&second.cert).unwrap();
        let second_key = fs::read_to_string(&second.key).unwrap();

        assert_eq!(first.cert, second.cert);
        assert_eq!(first.key, second.key);
        assert_eq!(first_cert, second_cert, "cert must not be overwritten");
        assert_eq!(first_key, second_key, "key must not be overwritten");
    }

    #[test]
    fn regenerates_when_cert_is_empty() {
        let tmp = TempDir::new().unwrap();
        let ca_dir = tmp.path().join("ca");
        fs::create_dir_all(&ca_dir).unwrap();

        // Simulate a crash that left an empty cert file
        fs::write(ca_dir.join(CERT_FILENAME), "").unwrap();
        fs::write(ca_dir.join(KEY_FILENAME), "valid-key-content").unwrap();

        let paths = generate_ca(&ca_dir).unwrap();
        let cert_pem = fs::read_to_string(&paths.cert).unwrap();
        assert!(
            cert_pem.contains("BEGIN CERTIFICATE"),
            "empty cert should trigger regeneration"
        );
    }

    #[test]
    fn regenerates_when_key_missing() {
        let tmp = TempDir::new().unwrap();
        let ca_dir = tmp.path().join("ca");
        fs::create_dir_all(&ca_dir).unwrap();

        // Only cert exists — key was never written
        fs::write(ca_dir.join(CERT_FILENAME), "cert-content").unwrap();

        let paths = generate_ca(&ca_dir).unwrap();
        let key_pem = fs::read_to_string(&paths.key).unwrap();
        assert!(
            key_pem.contains("BEGIN PRIVATE KEY"),
            "missing key should trigger regeneration"
        );
    }

    #[test]
    fn generate_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let ca_dir = tmp.path().join("deeply").join("nested").join("ca");

        let paths = generate_ca(&ca_dir).unwrap();
        assert!(paths.cert.exists());
        assert!(paths.key.exists());
    }
}

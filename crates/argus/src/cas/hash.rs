//! Algorithm-prefixed content hash for CAS addressing.
//!
//! Each hash is serialized as `{algorithm}:{hex_digest}` (e.g.
//! `blake3:a1b2c3...`). CAS storage paths use the algorithm as a
//! top-level directory: `{algorithm}/{digest[0:2]}/{digest[2:]}`.
//!
//! The default algorithm is BLAKE3. SHA-256 is supported for
//! backward compatibility and compliance requirements.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Hash algorithm used for content addressing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HashAlgorithm {
    /// BLAKE3 — fast, parallelizable, default.
    Blake3,
    /// SHA-256 — legacy and compliance.
    Sha256,
}

impl HashAlgorithm {
    /// Algorithm label used in serialized hashes and CAS paths.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blake3 => "blake3",
            Self::Sha256 => "sha256",
        }
    }

    /// Expected hex-encoded digest length for this algorithm.
    fn hex_len(self) -> usize {
        // Both BLAKE3 and SHA-256 produce 32-byte (256-bit) digests,
        // which encode to exactly 64 lowercase hex characters.
        64
    }
}

/// Algorithm-prefixed content hash used as a CAS key.
///
/// Stores a raw 32-byte digest alongside the algorithm tag. This keeps
/// the type `Copy` (33 bytes, no heap allocation) while `Display` and
/// `Serialize` produce the canonical `{algorithm}:{hex_digest}` form.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentHash {
    algorithm: HashAlgorithm,
    /// Raw digest bytes — both BLAKE3 and SHA-256 are 32 bytes.
    digest: [u8; 32],
}

impl Serialize for ContentHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        ContentHash::try_from(s).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for ContentHash {
    type Error = InvalidHashError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let (algo_str, hex) = s
            .split_once(':')
            .ok_or(InvalidHashError::MissingAlgorithm)?;

        let algorithm = match algo_str {
            "blake3" => HashAlgorithm::Blake3,
            "sha256" => HashAlgorithm::Sha256,
            other => return Err(InvalidHashError::UnknownAlgorithm(other.to_owned())),
        };

        let expected_len = algorithm.hex_len();
        if hex.len() != expected_len {
            return Err(InvalidHashError::BadLength {
                expected: expected_len,
                got: hex.len(),
            });
        }
        if !hex
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(InvalidHashError::BadCharacters);
        }

        let digest = hex_decode_32(hex).map_err(|_| InvalidHashError::BadCharacters)?;
        Ok(Self { algorithm, digest })
    }
}

/// Reasons a string cannot be interpreted as a [`ContentHash`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum InvalidHashError {
    /// No `algorithm:` prefix found.
    #[error("hash must be prefixed with algorithm (e.g. blake3:...)")]
    MissingAlgorithm,
    /// Unrecognized algorithm name.
    #[error("unknown hash algorithm: {0}")]
    UnknownAlgorithm(String),
    /// Hex digest is not the expected length.
    #[error("expected {expected}-char hex digest, got {got}")]
    BadLength { expected: usize, got: usize },
    /// Hex digest contains non-hex or uppercase characters.
    #[error("digest must be lowercase hex only")]
    BadCharacters,
}

impl ContentHash {
    /// Compute a BLAKE3 hash from raw bytes (default algorithm).
    pub fn from_data(data: &[u8]) -> Self {
        Self::blake3(data)
    }

    /// Compute a BLAKE3 hash from raw bytes.
    pub fn blake3(data: &[u8]) -> Self {
        let raw = blake3::hash(data);
        Self {
            algorithm: HashAlgorithm::Blake3,
            digest: *raw.as_bytes(),
        }
    }

    /// Compute a SHA-256 hash from raw bytes.
    pub fn sha256(data: &[u8]) -> Self {
        let raw = Sha256::digest(data);
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&raw);
        Self {
            algorithm: HashAlgorithm::Sha256,
            digest,
        }
    }

    /// Hex-encoded digest, without algorithm prefix.
    ///
    /// Allocates on each call; use `Display` for logging to avoid the
    /// allocation in disabled tracing levels.
    pub fn digest(&self) -> String {
        hex_encode(&self.digest)
    }

    /// Algorithm used for this hash.
    pub fn algorithm(&self) -> HashAlgorithm {
        self.algorithm
    }

    /// Algorithm label for use as a CAS directory prefix.
    pub fn algorithm_dir(&self) -> &str {
        self.algorithm.as_str()
    }

    /// First two hex characters of the digest, used as the CAS directory prefix.
    pub fn prefix(&self) -> String {
        hex_encode(&self.digest[..1])
    }

    /// Remaining 62 hex characters after the 2-char prefix, used as filename.
    pub fn suffix(&self) -> String {
        // Encode all 32 bytes then skip the first 2 hex chars (1 byte).
        let full = hex_encode(&self.digest);
        full[2..].to_owned()
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm.as_str(), hex_encode(&self.digest))
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({}:{})", self.algorithm.as_str(), hex_encode(&self.digest))
    }
}

/// Format a byte slice as lowercase hex.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX_CHARS[(b >> 4) as usize]);
        s.push(HEX_CHARS[(b & 0x0f) as usize]);
    }
    s
}

/// Decode exactly 64 lowercase hex chars into a 32-byte array.
fn hex_decode_32(hex: &str) -> Result<[u8; 32], ()> {
    let mut out = [0u8; 32];
    let bytes = hex.as_bytes();
    for (i, chunk) in bytes.chunks(2).enumerate() {
        let hi = hex_val(chunk[0]).ok_or(())?;
        let lo = hex_val(chunk[1]).ok_or(())?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

const HEX_CHARS: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd',
    'e', 'f',
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_hash() {
        let a = ContentHash::from_data(b"hello world");
        let b = ContentHash::from_data(b"hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn different_input_different_hash() {
        let a = ContentHash::from_data(b"hello");
        let b = ContentHash::from_data(b"world");
        assert_ne!(a, b);
    }

    #[test]
    fn default_is_blake3() {
        let h = ContentHash::from_data(b"test");
        assert_eq!(h.algorithm(), HashAlgorithm::Blake3);
        assert!(format!("{h}").starts_with("blake3:"));
    }

    #[test]
    fn sha256_variant() {
        let h = ContentHash::sha256(b"");
        assert_eq!(h.algorithm(), HashAlgorithm::Sha256);
        assert_eq!(
            h.digest(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            format!("{h}"),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn digest_length_is_64() {
        let h = ContentHash::from_data(b"test");
        assert_eq!(h.digest().len(), 64);
    }

    #[test]
    fn prefix_suffix_split() {
        let h = ContentHash::from_data(b"test");
        assert_eq!(h.prefix().len(), 2);
        assert_eq!(h.suffix().len(), 62);
        let reassembled = format!("{}{}", h.prefix(), h.suffix());
        assert_eq!(reassembled, h.digest());
    }

    #[test]
    fn lowercase_hex_digest() {
        let h = ContentHash::from_data(b"test");
        assert!(h.digest().chars().all(|c| c.is_ascii_hexdigit()));
        assert!(h
            .digest()
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .all(|c| c.is_ascii_lowercase()));
    }

    #[test]
    fn display_is_canonical_form() {
        let h = ContentHash::from_data(b"display test");
        let s = format!("{h}");
        assert!(s.starts_with("blake3:"));
        assert_eq!(s.len(), "blake3:".len() + 64);
    }

    #[test]
    fn debug_contains_hash() {
        let h = ContentHash::from_data(b"debug test");
        let dbg = format!("{h:?}");
        assert!(dbg.contains("ContentHash("));
        assert!(dbg.contains(&format!("{h}")));
    }

    #[test]
    fn serde_round_trip() {
        let h = ContentHash::from_data(b"serde");
        let json = serde_json::to_string(&h).expect("serialize");
        let deserialized: ContentHash =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(h, deserialized);
    }

    #[test]
    fn serde_round_trip_sha256() {
        let h = ContentHash::sha256(b"serde");
        let json = serde_json::to_string(&h).expect("serialize");
        let deserialized: ContentHash =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(h, deserialized);
        assert_eq!(deserialized.algorithm(), HashAlgorithm::Sha256);
    }

    #[test]
    fn deserialize_rejects_no_prefix() {
        let hex = "a".repeat(64);
        let json = format!("\"{hex}\"");
        let result: Result<ContentHash, _> = serde_json::from_str(&json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_rejects_wrong_length() {
        let json = "\"blake3:abcd\"";
        let result: Result<ContentHash, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_rejects_unknown_algorithm() {
        let hex = "a".repeat(64);
        let json = format!("\"md5:{hex}\"");
        let result: Result<ContentHash, _> = serde_json::from_str(&json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_rejects_uppercase() {
        let hex = "AAAA".to_string() + &"a".repeat(60);
        let json = format!("\"blake3:{hex}\"");
        let result: Result<ContentHash, _> = serde_json::from_str(&json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_rejects_non_hex() {
        let hex = "zzzz".to_string() + &"0".repeat(60);
        let json = format!("\"blake3:{hex}\"");
        let result: Result<ContentHash, _> = serde_json::from_str(&json);
        assert!(result.is_err());
    }

    #[test]
    fn try_from_valid_string() {
        let h = ContentHash::from_data(b"test");
        let s = format!("{h}");
        let h2 = ContentHash::try_from(s).expect("valid hash");
        assert_eq!(h, h2);
    }

    #[test]
    fn algorithm_dir_matches() {
        let b = ContentHash::from_data(b"x");
        assert_eq!(b.algorithm_dir(), "blake3");

        let s = ContentHash::sha256(b"x");
        assert_eq!(s.algorithm_dir(), "sha256");
    }

    #[test]
    fn known_blake3_vector() {
        let h = ContentHash::from_data(b"");
        assert_eq!(
            h.digest(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn copy_semantics() {
        let a = ContentHash::from_data(b"copy test");
        let b = a; // copy, not move
        assert_eq!(a, b);
    }
}

// Rust guideline compliant 2026-02-21

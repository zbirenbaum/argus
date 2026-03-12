//! SHA-256 content hash newtype for CAS addressing.
//!
//! Wraps a 64-character lowercase hex SHA-256 digest and provides
//! accessors for the two-character prefix used in the storage path
//! layout (`{prefix}/{suffix}`).

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Expected length of a SHA-256 hex digest.
const SHA256_HEX_LEN: usize = 64;

/// Lowercase hex SHA-256 digest used as a CAS key.
///
/// The hash is split into a 2-char prefix and 62-char suffix to
/// avoid placing too many entries in a single directory.
///
/// Deserialization validates that the string is exactly 64 lowercase
/// hex characters, rejecting malformed hashes early.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ContentHash(String);

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
        if s.len() != SHA256_HEX_LEN {
            return Err(InvalidHashError::BadLength(s.len()));
        }
        if !s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
            return Err(InvalidHashError::BadCharacters);
        }
        Ok(Self(s))
    }
}

/// Reasons a string cannot be interpreted as a `ContentHash`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum InvalidHashError {
    /// String is not exactly 64 characters.
    #[error("expected {SHA256_HEX_LEN}-char hex string, got {0} chars")]
    BadLength(usize),
    /// String contains non-hex or uppercase characters.
    #[error("hash must be lowercase hex only")]
    BadCharacters,
}

impl ContentHash {
    /// Compute a SHA-256 hash from raw bytes.
    pub fn from_data(data: &[u8]) -> Self {
        let digest = Sha256::digest(data);
        let hex = hex_encode(&digest);
        Self(hex)
    }

    /// Return the full 64-character hex string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// First two hex characters, used as the directory prefix.
    pub fn prefix(&self) -> &str {
        &self.0[..2]
    }

    /// Remaining 62 hex characters, used as the filename.
    pub fn suffix(&self) -> &str {
        &self.0[2..]
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({hash})", hash = &self.0)
    }
}

/// Format a 32-byte digest as 64-char lowercase hex without allocating
/// an intermediate `Vec`.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX_CHARS[(b >> 4) as usize]);
        s.push(HEX_CHARS[(b & 0x0f) as usize]);
    }
    s
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
    fn hash_length_is_64() {
        let h = ContentHash::from_data(b"test");
        assert_eq!(h.as_str().len(), 64);
    }

    #[test]
    fn prefix_suffix_split() {
        let h = ContentHash::from_data(b"test");
        assert_eq!(h.prefix().len(), 2);
        assert_eq!(h.suffix().len(), 62);
        let reassembled = format!("{}{}", h.prefix(), h.suffix());
        assert_eq!(reassembled, h.as_str());
    }

    #[test]
    fn lowercase_hex() {
        let h = ContentHash::from_data(b"test");
        assert!(h.as_str().chars().all(|c| c.is_ascii_hexdigit()));
        assert!(h
            .as_str()
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .all(|c| c.is_ascii_lowercase()));
    }

    #[test]
    fn known_sha256_vector() {
        // SHA-256 of empty string is well-known.
        let h = ContentHash::from_data(b"");
        assert_eq!(
            h.as_str(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn display_matches_as_str() {
        let h = ContentHash::from_data(b"display test");
        assert_eq!(format!("{h}"), h.as_str());
    }

    #[test]
    fn debug_contains_hash() {
        let h = ContentHash::from_data(b"debug test");
        let dbg = format!("{h:?}");
        assert!(dbg.contains("ContentHash("));
        assert!(dbg.contains(h.as_str()));
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
    fn deserialize_rejects_wrong_length() {
        let json = "\"abcd\"";
        let result: Result<ContentHash, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_rejects_uppercase() {
        let hex = "AAAA".to_string()
            + "a".repeat(60).as_str();
        let json = format!("\"{hex}\"");
        let result: Result<ContentHash, _> = serde_json::from_str(&json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_rejects_non_hex() {
        let hex = "zzzz".to_string()
            + "0".repeat(60).as_str();
        let json = format!("\"{hex}\"");
        let result: Result<ContentHash, _> = serde_json::from_str(&json);
        assert!(result.is_err());
    }

    #[test]
    fn try_from_valid_string() {
        let h = ContentHash::from_data(b"test");
        let s = h.as_str().to_owned();
        let h2 = ContentHash::try_from(s).expect("valid hash");
        assert_eq!(h, h2);
    }
}

// Rust guideline compliant 2026-02-21
//! Output destination configuration.
//!
//! An `OutputConfig` describes one sink for enriched event records.
//! Multiple outputs can be listed; each receives every event.

use std::path::PathBuf;
use std::time::Duration;

use bytesize::ByteSize;
use serde::{Deserialize, Serialize};

/// A single event output destination.
///
/// Tagged with `type` in YAML so the variant is self-describing:
///
/// ```yaml
/// outputs:
///   - type: stdout
///   - type: file
///     path: /data/events/out.jsonl
///   - type: unix_socket
///     path: /run/argus.sock
///   - type: http
///     endpoint: "http://ingest.example.com/events"
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputConfig {
    /// Deprecated — accepted for config compatibility but ignored.
    ///
    /// Events go through the bus (event log, WebSocket). The agent
    /// process inherits the terminal's stdout directly.
    Stdout {
        #[serde(default = "default_flush_every_event")]
        flush_every_event: bool,
    },

    /// Write newline-delimited JSON to a rotating set of files.
    File {
        /// Absolute path prefix for output files.
        path: PathBuf,

        /// Maximum size per file before rotation.
        ///
        /// 64 MiB keeps individual files manageable for post-processing
        /// tools that load entire files into memory.
        #[serde(default = "default_max_size")]
        max_size: ByteSize,

        /// Number of rotated files to retain before the oldest is deleted.
        #[serde(default = "default_max_files")]
        max_files: u32,
    },

    /// Write newline-delimited JSON to a Unix domain socket.
    ///
    /// Used to hand events to a local Vector/Fluent Bit sidecar without
    /// going through the filesystem.
    UnixSocket {
        /// Absolute path to the Unix socket.
        path: PathBuf,
    },

    /// POST newline-delimited JSON batches to an HTTP endpoint.
    Http {
        /// Full URL of the ingest endpoint.
        endpoint: String,

        /// Per-request timeout.
        ///
        /// 5 s is generous for LAN/localhost; raise for WAN endpoints.
        #[serde(default = "default_http_timeout", with = "humantime_serde")]
        timeout: Duration,

        /// Maximum retry attempts on transient errors before giving up.
        #[serde(default = "default_retry_max")]
        retry_max: u32,
    },
}

/// 64 MiB — fits comfortably on any modern filesystem and is a common
/// log-rotation target for downstream tooling.
fn default_max_size() -> ByteSize {
    ByteSize::mib(64)
}

fn default_max_files() -> u32 {
    10
}

/// 5 s — sufficient for local or fast-LAN endpoints.
fn default_http_timeout() -> Duration {
    Duration::from_secs(5)
}

fn default_retry_max() -> u32 {
    3
}

fn default_flush_every_event() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stdout() {
        let yaml = "type: stdout";
        let out: OutputConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(out, OutputConfig::Stdout { flush_every_event: true });
    }

    #[test]
    fn parse_file() {
        let yaml = r#"
type: file
path: /data/events/out.jsonl
"#;
        let out: OutputConfig = serde_yaml::from_str(yaml).unwrap();
        match out {
            OutputConfig::File { path, max_size, max_files } => {
                assert_eq!(path, PathBuf::from("/data/events/out.jsonl"));
                assert_eq!(max_size, ByteSize::mib(64));
                assert_eq!(max_files, 10);
            }
            _ => panic!("expected File variant"),
        }
    }

    #[test]
    fn parse_unix_socket() {
        let yaml = r#"
type: unix_socket
path: /run/argus.sock
"#;
        let out: OutputConfig = serde_yaml::from_str(yaml).unwrap();
        match out {
            OutputConfig::UnixSocket { path } => {
                assert_eq!(path, PathBuf::from("/run/argus.sock"));
            }
            _ => panic!("expected UnixSocket variant"),
        }
    }

    #[test]
    fn parse_http() {
        let yaml = r#"
type: http
endpoint: "http://ingest.example.com/events"
"#;
        let out: OutputConfig = serde_yaml::from_str(yaml).unwrap();
        match out {
            OutputConfig::Http { endpoint, timeout, retry_max } => {
                assert_eq!(endpoint, "http://ingest.example.com/events");
                assert_eq!(timeout, Duration::from_secs(5));
                assert_eq!(retry_max, 3);
            }
            _ => panic!("expected Http variant"),
        }
    }

    #[test]
    fn parse_output_list() {
        let yaml = r#"
- type: stdout
- type: file
  path: /data/events/out.jsonl
- type: unix_socket
  path: /run/argus.sock
- type: http
  endpoint: "http://ingest.example.com/events"
  retry_max: 5
"#;
        let outputs: Vec<OutputConfig> = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(outputs.len(), 4);
        assert_eq!(outputs[0], OutputConfig::Stdout { flush_every_event: true });
    }
}

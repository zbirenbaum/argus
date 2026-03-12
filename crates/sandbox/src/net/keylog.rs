//! SSLKEYLOGFILE line parsing and CAS storage.
//!
//! Reads TLS key log lines (NSS Key Log Format), stores each unique
//! line in the CAS, and produces `TlsKeys` events. The watcher is
//! designed to be driven from a polling loop or inotify callback;
//! it tracks the last-read byte offset to avoid re-processing lines.
// Rust guideline compliant 2026-02-21

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing::{event, Level};

use crate::cas::CasStore;
use crate::events::network::TlsKeys;

/// Parsed NSS Key Log line with label, client random, and secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeylogLine {
    /// Label identifying the secret type (e.g. `CLIENT_RANDOM`).
    pub label: String,
    /// Hex-encoded client random value.
    pub client_random: String,
    /// Hex-encoded secret value.
    pub secret: String,
}

/// Watches an SSLKEYLOGFILE and emits events for new lines.
///
/// Maintains read offset and a set of seen client_random values to
/// avoid emitting duplicate events when the file is re-read.
#[derive(Debug)]
pub struct KeylogWatcher {
    path: PathBuf,
    offset: u64,
    seen: HashSet<String>,
}

impl KeylogWatcher {
    /// Create a watcher for the given keylog file path.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            seen: HashSet::new(),
        }
    }

    /// Read new lines from the keylog file since the last call.
    ///
    /// Returns parsed lines that have not been seen before. Updates
    /// the internal offset so subsequent calls only read new data.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or read.
    pub fn read_new_lines(&mut self) -> Result<Vec<KeylogLine>> {
        let file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(e) => {
                return Err(e).context("open keylog file");
            }
        };

        let mut reader = BufReader::new(file);
        reader
            .seek(SeekFrom::Start(self.offset))
            .context("seek in keylog file")?;

        let mut new_lines = Vec::new();
        let mut buf = String::new();

        loop {
            buf.clear();
            let bytes_read = reader
                .read_line(&mut buf)
                .context("read keylog line")?;
            if bytes_read == 0 {
                break;
            }
            self.offset += bytes_read as u64;

            if let Some(parsed) = parse_keylog_line(&buf) {
                if self.seen.insert(parsed.client_random.clone()) {
                    new_lines.push(parsed);
                }
            }
        }

        Ok(new_lines)
    }

    /// Store new keylog lines in the CAS and build TlsKeys events.
    ///
    /// Each line is stored individually so the content hash can be
    /// referenced in the event. Returns one `TlsKeys` per new line.
    ///
    /// # Errors
    ///
    /// Returns an error if CAS storage fails.
    pub fn process_new_lines(
        &mut self,
        cas: &CasStore,
        pid: u32,
        fd: i32,
    ) -> Result<Vec<TlsKeys>> {
        let lines = self.read_new_lines()?;
        let mut events = Vec::with_capacity(lines.len());

        for line in &lines {
            let raw = format!("{} {} {}", line.label, line.client_random, line.secret);
            let hash = cas.store(raw.as_bytes())?;

            event!(
                name: "net.keylog.captured",
                Level::DEBUG,
                keylog.label = %line.label,
                keylog.hash = hash.as_str(),
                "captured TLS key material",
            );

            events.push(TlsKeys {
                pid,
                fd,
                sni: None,
                keylog_line_hash: Some(hash.as_str().to_owned()),
            });
        }

        Ok(events)
    }
}

/// Parse a single NSS Key Log Format line.
///
/// Format: `<label> <client_random_hex> <secret_hex>`
/// Lines starting with `#` or that are blank are ignored.
pub fn parse_keylog_line(line: &str) -> Option<KeylogLine> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let parts: Vec<&str> = trimmed.splitn(3, ' ').collect();
    if parts.len() != 3 {
        return None;
    }

    // Validate hex encoding on both fields.
    if !is_hex(parts[1]) || !is_hex(parts[2]) {
        return None;
    }

    Some(KeylogLine {
        label: parts[0].to_owned(),
        client_random: parts[1].to_owned(),
        secret: parts[2].to_owned(),
    })
}

/// Check that a string consists entirely of hexadecimal characters.
fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parse_valid_client_random_line() {
        let line = "CLIENT_RANDOM aabbccdd00112233 \
                    deadbeefcafebabe00112233445566778899aabb";
        let parsed = parse_keylog_line(line).expect("should parse");
        assert_eq!(parsed.label, "CLIENT_RANDOM");
        assert_eq!(parsed.client_random, "aabbccdd00112233");
    }

    #[test]
    fn parse_ignores_comments() {
        assert!(parse_keylog_line("# comment line").is_none());
    }

    #[test]
    fn parse_ignores_blank_lines() {
        assert!(parse_keylog_line("").is_none());
        assert!(parse_keylog_line("   \n").is_none());
    }

    #[test]
    fn parse_rejects_malformed_line() {
        assert!(parse_keylog_line("ONLY_TWO_PARTS abc").is_none());
    }

    #[test]
    fn parse_rejects_non_hex() {
        assert!(parse_keylog_line("CLIENT_RANDOM not_hex secret").is_none());
    }

    #[test]
    fn watcher_reads_new_lines_incrementally() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keylog.txt");

        fs::write(
            &path,
            "CLIENT_RANDOM aa11 bb22\nCLIENT_RANDOM cc33 dd44\n",
        )
        .unwrap();

        let mut watcher = KeylogWatcher::new(path.clone());
        let first = watcher.read_new_lines().unwrap();
        assert_eq!(first.len(), 2);

        // Append another line.
        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "CLIENT_RANDOM ee55 ff66").unwrap();

        let second = watcher.read_new_lines().unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].client_random, "ee55");
    }

    #[test]
    fn watcher_deduplicates_by_client_random() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keylog.txt");

        fs::write(&path, "CLIENT_RANDOM aa11 bb22\n").unwrap();
        let mut watcher = KeylogWatcher::new(path.clone());
        let first = watcher.read_new_lines().unwrap();
        assert_eq!(first.len(), 1);

        // Write the same client_random again with a different secret.
        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "CLIENT_RANDOM aa11 cc33").unwrap();

        let second = watcher.read_new_lines().unwrap();
        assert_eq!(second.len(), 0, "duplicate client_random should be skipped");
    }

    #[test]
    fn watcher_handles_missing_file() {
        let mut watcher = KeylogWatcher::new(PathBuf::from("/nonexistent/keylog.txt"));
        let lines = watcher.read_new_lines().unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn process_stores_in_cas_and_builds_events() {
        let dir = TempDir::new().unwrap();
        let keylog_path = dir.path().join("keylog.txt");
        let cas_path = dir.path().join("cas");

        fs::write(
            &keylog_path,
            "CLIENT_RANDOM aa11 bb22\nCLIENT_RANDOM cc33 dd44\n",
        )
        .unwrap();

        let cas = CasStore::new(cas_path).unwrap();
        let mut watcher = KeylogWatcher::new(keylog_path);
        let events = watcher.process_new_lines(&cas, 100, 5).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].pid, 100);
        assert_eq!(events[0].fd, 5);
        assert!(events[0].keylog_line_hash.is_some());

        // Verify CAS actually has the content.
        let hash_str = events[0].keylog_line_hash.as_ref().unwrap();
        let hash = crate::cas::ContentHash::try_from(hash_str.clone()).unwrap();
        let stored = cas.read(&hash).unwrap();
        assert_eq!(
            String::from_utf8(stored).unwrap(),
            "CLIENT_RANDOM aa11 bb22"
        );
    }
}

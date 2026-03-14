//! SSLKEYLOGFILE line parsing and CAS storage.
//!
//! Reads TLS key log lines (NSS Key Log Format), stores each unique
//! line in the CAS, and produces `TlsKeys` events. The watcher is
//! designed to be driven from a polling loop or inotify callback;
//! it tracks the last-read byte offset to avoid re-processing lines.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing::{event, Level};

use crate::cas::ContentHash;
use crate::events::network::TlsKeys;
use crate::pipeline::EmitResult;
use crate::pipeline::bus::RecordBus;
use crate::pipeline::record::Record;

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
/// Maintains read offset and a set of seen `(label, client_random)` pairs
/// to avoid emitting duplicate events when the file is re-read. Multiple
/// TLS secret types share the same client_random, so the label is needed.
#[derive(Debug)]
pub struct KeylogWatcher {
    path: PathBuf,
    offset: u64,
    seen: HashSet<(String, String)>,
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
                let key = (parsed.label.clone(), parsed.client_random.clone());
                if self.seen.insert(key) {
                    new_lines.push(parsed);
                }
            }
        }

        Ok(new_lines)
    }

    /// Build TlsKeys events from new lines without bus interaction.
    ///
    /// Returns each event paired with its content hash and raw bytes
    /// for the caller to persist through the pipeline.
    ///
    /// # Errors
    ///
    /// Returns an error if reading the keylog file fails.
    pub fn poll_new_lines(
        &mut self,
        pid: u32,
        fd: i32,
    ) -> Result<Vec<(TlsKeys, ContentHash, Vec<u8>)>> {
        let lines = self.read_new_lines()?;
        let mut results = Vec::with_capacity(lines.len());

        for line in &lines {
            let raw = format!("{} {} {}", line.label, line.client_random, line.secret);
            let data = raw.into_bytes();
            let hash = ContentHash::from_data(&data);
            event!(
                name: "net.keylog.captured",
                Level::DEBUG,
                keylog.label = %line.label,
                keylog.hash = %hash,
                "captured TLS key material",
            );

            results.push((
                TlsKeys {
                    pid,
                    fd,
                    sni: None,
                    keylog_line_hash: Some(hash.to_string()),
                },
                hash,
                data,
            ));
        }

        Ok(results)
    }

    /// Emit new keylog lines as Content records to the bus and build TlsKeys events.
    ///
    /// Each line is hashed and emitted as a Content record so the bus can
    /// route it to CAS sinks. Returns one `TlsKeys` per new line.
    ///
    /// # Errors
    ///
    /// Returns an error if reading the keylog file fails.
    pub fn process_new_lines(
        &mut self,
        bus: &RecordBus,
        pid: u32,
        fd: i32,
    ) -> Result<Vec<TlsKeys>> {
        let lines = self.read_new_lines()?;
        let mut events = Vec::with_capacity(lines.len());

        for line in &lines {
            let raw = format!("{} {} {}", line.label, line.client_random, line.secret);
            let data = raw.into_bytes();
            let hash = ContentHash::from_data(&data);
            if let EmitResult::RequiredFailed(failures) = bus.emit(Record::Content { hash, data }) {
                for (sink_name, err) in &failures {
                    event!(
                        name: "pipeline.emit.required_sink_failed",
                        Level::ERROR,
                        sink.name = sink_name.as_str(),
                        error.message = %err,
                        "required sink failed emitting TLS key content, data may be lost",
                    );
                }
            }

            event!(
                name: "net.keylog.captured",
                Level::DEBUG,
                keylog.label = %line.label,
                keylog.hash = %hash,
                "captured TLS key material",
            );

            events.push(TlsKeys {
                pid,
                fd,
                sni: None,
                keylog_line_hash: Some(hash.to_string()),
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

    // client_random must be exactly 32 bytes (64 hex characters).
    if parts[1].len() != 64 {
        event!(
            name: "net.keylog.malformed",
            Level::WARN,
            keylog.label = parts[0],
            keylog.client_random_len = parts[1].len(),
            "skipping keylog entry: client_random is not 64 hex characters",
        );
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
    use crate::pipeline::bus::RecordBus;
    use std::fs;
    use tempfile::TempDir;

    fn noop_bus() -> RecordBus {
        RecordBus::new(vec![])
    }

    /// 64 hex character client_random for tests (32 bytes).
    const TEST_CR: &str = "aabbccdd00112233aabbccdd00112233aabbccdd00112233aabbccdd00112233";

    #[test]
    fn parse_valid_client_random_line() {
        let line = format!("CLIENT_RANDOM {TEST_CR} deadbeef");
        let parsed = parse_keylog_line(&line).expect("should parse");
        assert_eq!(parsed.label, "CLIENT_RANDOM");
        assert_eq!(parsed.client_random, TEST_CR);
    }

    #[test]
    fn parse_rejects_short_client_random() {
        assert!(
            parse_keylog_line("CLIENT_RANDOM aabbccdd deadbeef").is_none(),
            "client_random shorter than 64 hex chars should be rejected"
        );
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

    const TEST_CR2: &str = "ccddaabb00112233ccddaabb00112233ccddaabb00112233ccddaabb00112233";
    const TEST_CR3: &str = "eeff001122334455eeff001122334455eeff001122334455eeff001122334455";

    #[test]
    fn watcher_reads_new_lines_incrementally() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keylog.txt");

        fs::write(
            &path,
            format!("CLIENT_RANDOM {TEST_CR} bb22\nCLIENT_RANDOM {TEST_CR2} dd44\n"),
        )
        .unwrap();

        let mut watcher = KeylogWatcher::new(path.clone());
        let first = watcher.read_new_lines().unwrap();
        assert_eq!(first.len(), 2);

        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "CLIENT_RANDOM {TEST_CR3} ff66").unwrap();

        let second = watcher.read_new_lines().unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].client_random, TEST_CR3);
    }

    #[test]
    fn watcher_deduplicates_by_label_and_client_random() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keylog.txt");

        fs::write(&path, format!("CLIENT_RANDOM {TEST_CR} bb22\n")).unwrap();
        let mut watcher = KeylogWatcher::new(path.clone());
        let first = watcher.read_new_lines().unwrap();
        assert_eq!(first.len(), 1);

        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        // Same label + client_random should be skipped.
        writeln!(f, "CLIENT_RANDOM {TEST_CR} cc33").unwrap();
        let second = watcher.read_new_lines().unwrap();
        assert_eq!(second.len(), 0, "duplicate (label, client_random) should be skipped");

        // Different label with same client_random should NOT be skipped.
        writeln!(f, "CLIENT_HANDSHAKE_TRAFFIC_SECRET {TEST_CR} dd44").unwrap();
        let third = watcher.read_new_lines().unwrap();
        assert_eq!(third.len(), 1, "different label should not be deduped");
    }

    #[test]
    fn watcher_handles_missing_file() {
        let mut watcher = KeylogWatcher::new(PathBuf::from("/nonexistent/keylog.txt"));
        let lines = watcher.read_new_lines().unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn process_emits_hashes_and_builds_events() {
        let dir = TempDir::new().unwrap();
        let keylog_path = dir.path().join("keylog.txt");

        fs::write(
            &keylog_path,
            format!("CLIENT_RANDOM {TEST_CR} bb22\nCLIENT_RANDOM {TEST_CR2} dd44\n"),
        )
        .unwrap();

        let bus = noop_bus();
        let mut watcher = KeylogWatcher::new(keylog_path);
        let events = watcher.process_new_lines(&bus, 100, 5).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].pid, 100);
        assert_eq!(events[0].fd, 5);
        assert!(events[0].keylog_line_hash.is_some());
    }
}

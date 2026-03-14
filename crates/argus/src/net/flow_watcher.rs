// Rust guideline compliant 2026-02-21
//! Incremental HTTP flow reader and event emitter.
//!
//! Reads newline-delimited JSON from a mitmdump addon output file,
//! processes each flow through [`process_flow`], and returns
//! `HttpRequest`/`HttpResponse` event payloads ready for emission.
//!
//! Designed to be polled from a background thread, mirroring the
//! [`KeylogWatcher`](super::KeylogWatcher) pattern.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing::{event, Level};

use crate::events::EventPayload;
use crate::events::network::{HttpRequest, HttpResponse};
use crate::pipeline::bus::RecordBus;

use super::flow_parser::{parse_flow_line, process_flow, process_flow_detached};

/// Result of processing a single flow into event payloads.
#[derive(Debug)]
pub struct FlowEvents {
    /// The HTTP request event payload.
    pub request: HttpRequest,
    /// The HTTP response event payload, if a response was captured.
    pub response: Option<HttpResponse>,
}

/// Watches a mitmdump flow output file for new HTTP flows.
///
/// Maintains read offset to avoid re-processing lines on each poll.
/// Each call to [`process_new_flows`] reads only new data appended
/// since the last call.
#[derive(Debug)]
pub struct FlowWatcher {
    path: PathBuf,
    offset: u64,
}

impl FlowWatcher {
    /// Create a watcher for the given flow output file path.
    pub fn new(path: PathBuf) -> Self {
        Self { path, offset: 0 }
    }

    /// Read and process new flows without bus interaction.
    ///
    /// Returns flow events and their associated content blobs for the
    /// caller to persist through the pipeline.
    ///
    /// # Errors
    ///
    /// Returns an error if file I/O fails.
    pub fn poll_new_flows(
        &mut self,
        pid: u32,
    ) -> Result<Vec<(FlowEvents, Vec<super::flow_parser::FlowContent>)>> {
        let file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(e) => {
                return Err(e).context("open flow output file");
            }
        };

        let mut reader = BufReader::new(file);
        reader
            .seek(SeekFrom::Start(self.offset))
            .context("seek in flow output file")?;

        let mut results = Vec::new();
        let mut buf = String::new();

        loop {
            buf.clear();
            let bytes_read = reader
                .read_line(&mut buf)
                .context("read flow output line")?;
            if bytes_read == 0 {
                break;
            }
            self.offset += bytes_read as u64;

            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }

            let flow = match parse_flow_line(trimmed) {
                Ok(f) => f,
                Err(e) => {
                    event!(
                        name: "net.flow_watcher.parse_error",
                        Level::WARN,
                        error.message = %e,
                        "skipping malformed flow line: {{error.message}}",
                    );
                    continue;
                }
            };

            match process_flow_detached(&flow, pid) {
                Ok((processed, content)) => {
                    results.push((
                        FlowEvents {
                            request: processed.request,
                            response: processed.response,
                        },
                        content,
                    ));
                }
                Err(e) => {
                    event!(
                        name: "net.flow_watcher.process_error",
                        Level::WARN,
                        error.message = %e,
                        "failed to process flow: {{error.message}}",
                    );
                }
            }
        }

        Ok(results)
    }

    /// Read and process new flows from the file.
    ///
    /// Returns event payloads ready for emission. Skips malformed
    /// lines with a warning. Bodies are decoded from base64 and
    /// emitted as Content records to the bus.
    ///
    /// # Errors
    ///
    /// Returns an error if file I/O fails.
    pub fn process_new_flows(
        &mut self,
        bus: &RecordBus,
        pid: u32,
    ) -> Result<Vec<FlowEvents>> {
        let file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(e) => {
                return Err(e).context("open flow output file");
            }
        };

        let mut reader = BufReader::new(file);
        reader
            .seek(SeekFrom::Start(self.offset))
            .context("seek in flow output file")?;

        let mut results = Vec::new();
        let mut buf = String::new();

        loop {
            buf.clear();
            let bytes_read = reader
                .read_line(&mut buf)
                .context("read flow output line")?;
            if bytes_read == 0 {
                break;
            }
            self.offset += bytes_read as u64;

            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }

            let flow = match parse_flow_line(trimmed) {
                Ok(f) => f,
                Err(e) => {
                    event!(
                        name: "net.flow_watcher.parse_error",
                        Level::WARN,
                        error.message = %e,
                        "skipping malformed flow line: {{error.message}}",
                    );
                    continue;
                }
            };

            match process_flow(&flow, bus, pid) {
                Ok(processed) => {
                    results.push(FlowEvents {
                        request: processed.request,
                        response: processed.response,
                    });
                }
                Err(e) => {
                    event!(
                        name: "net.flow_watcher.process_error",
                        Level::WARN,
                        error.message = %e,
                        "failed to process flow: {{error.message}}",
                    );
                }
            }
        }

        Ok(results)
    }

    /// Convert flow events into `EventPayload` variants for emission.
    pub fn into_event_payloads(flows: Vec<FlowEvents>) -> Vec<EventPayload> {
        let mut payloads = Vec::with_capacity(flows.len() * 2);
        for flow in flows {
            payloads.push(EventPayload::HttpRequest(flow.request));
            if let Some(resp) = flow.response {
                payloads.push(EventPayload::HttpResponse(resp));
            }
        }
        payloads
    }
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

    fn flow_json(method: &str, url: &str, status: u16) -> String {
        format!(
            r#"{{"request":{{"method":"{method}","url":"{url}","headers":[["Host","example.com"]],"body":"aGVsbG8="}},"response":{{"status_code":{status},"headers":[["Content-Type","text/plain"]],"body":"d29ybGQ="}}}}"#,
        )
    }

    #[test]
    fn process_new_flows_reads_incrementally() {
        let dir = TempDir::new().unwrap();
        let flow_path = dir.path().join("flows.jsonl");
        let bus = noop_bus();

        fs::write(
            &flow_path,
            format!("{}\n", flow_json("GET", "https://a.com/1", 200)),
        )
        .unwrap();

        let mut watcher = FlowWatcher::new(flow_path.clone());
        let first = watcher.process_new_flows(&bus, 42).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].request.method, "GET");
        assert_eq!(first[0].request.pid, 42);
        assert!(first[0].response.is_some());

        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&flow_path)
            .unwrap();
        writeln!(f, "{}", flow_json("POST", "https://b.com/2", 201)).unwrap();

        let second = watcher.process_new_flows(&bus, 42).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].request.method, "POST");
    }

    #[test]
    fn process_new_flows_skips_bad_lines() {
        let dir = TempDir::new().unwrap();
        let flow_path = dir.path().join("flows.jsonl");
        let bus = noop_bus();

        let content = format!(
            "{}\nnot valid json\n{}\n",
            flow_json("GET", "https://a.com", 200),
            flow_json("POST", "https://b.com", 201),
        );
        fs::write(&flow_path, content).unwrap();

        let mut watcher = FlowWatcher::new(flow_path);
        let flows = watcher.process_new_flows(&bus, 1).unwrap();
        assert_eq!(flows.len(), 2);
    }

    #[test]
    fn process_new_flows_handles_missing_file() {
        let mut watcher = FlowWatcher::new(PathBuf::from("/nonexistent/flows.jsonl"));
        let bus = noop_bus();
        let flows = watcher.process_new_flows(&bus, 1).unwrap();
        assert!(flows.is_empty());
    }

    #[test]
    fn body_hash_is_present() {
        let dir = TempDir::new().unwrap();
        let flow_path = dir.path().join("flows.jsonl");
        let bus = noop_bus();

        fs::write(
            &flow_path,
            format!("{}\n", flow_json("POST", "https://api.com", 200)),
        )
        .unwrap();

        let mut watcher = FlowWatcher::new(flow_path);
        let flows = watcher.process_new_flows(&bus, 1).unwrap();
        assert!(flows[0].request.body_hash.is_some());
    }

    #[test]
    fn into_event_payloads_creates_correct_variants() {
        let events = vec![FlowEvents {
            request: HttpRequest {
                pid: 1,
                method: "GET".into(),
                url: "https://x.com".into(),
                headers_hash: None,
                body_hash: None,
                headers: None,
                body: None,
            },
            response: Some(HttpResponse {
                pid: 1,
                status: 200,
                headers_hash: None,
                body_hash: None,
                headers: None,
                body: None,
            }),
        }];

        let payloads = FlowWatcher::into_event_payloads(events);
        assert_eq!(payloads.len(), 2);
        assert!(matches!(&payloads[0], EventPayload::HttpRequest(_)));
        assert!(matches!(&payloads[1], EventPayload::HttpResponse(_)));
    }

    #[test]
    fn no_response_yields_request_only() {
        let events = vec![FlowEvents {
            request: HttpRequest {
                pid: 1,
                method: "GET".into(),
                url: "https://x.com".into(),
                headers_hash: None,
                body_hash: None,
                headers: None,
                body: None,
            },
            response: None,
        }];

        let payloads = FlowWatcher::into_event_payloads(events);
        assert_eq!(payloads.len(), 1);
    }

    #[test]
    fn skips_blank_lines() {
        let dir = TempDir::new().unwrap();
        let flow_path = dir.path().join("flows.jsonl");
        let bus = noop_bus();

        let content = format!(
            "\n\n{}\n\n",
            flow_json("GET", "https://x.com", 200),
        );
        fs::write(&flow_path, content).unwrap();

        let mut watcher = FlowWatcher::new(flow_path);
        let flows = watcher.process_new_flows(&bus, 1).unwrap();
        assert_eq!(flows.len(), 1);
    }
}

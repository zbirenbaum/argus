//! Mitmdump flow JSON parsing and HTTP event construction.
//!
//! Parses the JSON output produced by a mitmdump addon script that
//! serializes each HTTP flow as a JSON object with request and response
//! fields. Extracts headers and bodies, emits Content records to the bus,
//! and produces `HttpRequest` and `HttpResponse` event payloads.

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{event, Level};

use crate::cas::ContentHash;
use crate::events::network::{HttpRequest, HttpResponse};
use crate::pipeline::bus::RecordBus;
use crate::pipeline::record::Record;

/// Raw flow structure deserialized from mitmdump addon JSON output.
///
/// The addon script emits one JSON object per completed HTTP flow,
/// containing the request and optional response.
#[derive(Debug, Clone, Deserialize)]
pub struct MitmdumpFlow {
    pub request: FlowRequest,
    #[serde(default)]
    pub response: Option<FlowResponse>,
}

/// HTTP request portion of a mitmdump flow.
#[derive(Debug, Clone, Deserialize)]
pub struct FlowRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Base64-encoded request body, if present.
    #[serde(default)]
    pub body: Option<String>,
}

/// HTTP response portion of a mitmdump flow.
#[derive(Debug, Clone, Deserialize)]
pub struct FlowResponse {
    pub status_code: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Base64-encoded response body, if present.
    #[serde(default)]
    pub body: Option<String>,
}

/// Result of processing a single mitmdump flow.
#[derive(Debug)]
pub struct ProcessedFlow {
    /// The HTTP request event payload.
    pub request: HttpRequest,
    /// The HTTP response event payload, if a response was captured.
    pub response: Option<HttpResponse>,
}

/// Parse a single line of JSON into a `MitmdumpFlow`.
///
/// # Errors
///
/// Returns an error if the JSON is malformed or missing required fields.
pub fn parse_flow_line(line: &str) -> Result<MitmdumpFlow> {
    serde_json::from_str(line).context("parse mitmdump flow JSON")
}

/// Parse and process a mitmdump flow, emitting Content records to the bus.
///
/// Headers are serialized as JSON and emitted as Content records. Bodies are
/// decoded from base64 and also emitted. The returned events reference
/// content by hash.
///
/// # Errors
///
/// Returns an error if JSON parsing or base64 decoding fails.
pub fn process_flow(
    flow: &MitmdumpFlow,
    bus: &RecordBus,
    pid: u32,
) -> Result<ProcessedFlow> {
    let req_headers_hash = store_headers(bus, &flow.request.headers)?;
    let req_body_hash = store_body(bus, flow.request.body.as_deref())?;

    let resp_event = match &flow.response {
        Some(resp) => {
            let resp_headers_hash = store_headers(bus, &resp.headers)?;
            let resp_body_hash = store_body(bus, resp.body.as_deref())?;

            Some(HttpResponse {
                pid,
                status: resp.status_code,
                headers_hash: resp_headers_hash,
                body_hash: resp_body_hash,
            })
        }
        None => None,
    };

    event!(
        name: "net.flow.processed",
        Level::DEBUG,
        flow.method = %flow.request.method,
        flow.url = %flow.request.url,
        "processed HTTP flow",
    );

    let http_req = HttpRequest {
        pid,
        method: flow.request.method.clone(),
        url: flow.request.url.clone(),
        headers_hash: req_headers_hash,
        body_hash: req_body_hash,
    };

    Ok(ProcessedFlow {
        request: http_req,
        response: resp_event,
    })
}

/// Serialize headers as JSON, emit a Content record to the bus.
fn store_headers(
    bus: &RecordBus,
    headers: &[(String, String)],
) -> Result<Option<String>> {
    if headers.is_empty() {
        return Ok(None);
    }
    let data = serde_json::to_vec(headers).context("serialize headers")?;
    let hash = emit_content(bus, data);
    Ok(Some(hash.to_string()))
}

/// Decode base64 body, emit a Content record to the bus.
fn store_body(
    bus: &RecordBus,
    body_b64: Option<&str>,
) -> Result<Option<String>> {
    let Some(encoded) = body_b64 else {
        return Ok(None);
    };
    if encoded.is_empty() {
        return Ok(None);
    }

    // The addon script uses standard base64 encoding.
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("decode base64 body")?;

    let hash = emit_content(bus, decoded);
    Ok(Some(hash.to_string()))
}

/// Hash data, emit a Content record to the bus, and return the hash.
fn emit_content(bus: &RecordBus, data: Vec<u8>) -> ContentHash {
    let hash = ContentHash::from_data(&data);
    bus.emit(Record::Content { hash, data });
    hash
}

/// Parse multiple newline-delimited JSON flow lines.
///
/// Skips blank lines and lines that fail to parse (logging a warning).
/// Returns successfully parsed flows.
pub fn parse_flow_lines(input: &str) -> Vec<MitmdumpFlow> {
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            parse_flow_line(line)
                .inspect_err(|e| {
                    event!(
                        name: "net.flow.parse_error",
                        Level::WARN,
                        error = %e,
                        "skipping malformed flow line",
                    );
                })
                .ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::bus::RecordBus;

    fn noop_bus() -> RecordBus {
        RecordBus::new(vec![])
    }

    fn flow_json(method: &str, url: &str, status: u16) -> String {
        // Body "hello" in base64 is "aGVsbG8="
        format!(
            r#"{{"request":{{"method":"{method}","url":"{url}","headers":[["Host","example.com"],["Content-Type","application/json"]],"body":"aGVsbG8="}},"response":{{"status_code":{status},"headers":[["Content-Type","text/plain"]],"body":"d29ybGQ="}}}}"#,
        )
    }

    #[test]
    fn parse_complete_flow() {
        let json = flow_json("POST", "https://api.example.com/data", 200);
        let flow = parse_flow_line(&json).unwrap();
        assert_eq!(flow.request.method, "POST");
        assert_eq!(flow.request.url, "https://api.example.com/data");
        assert_eq!(flow.request.headers.len(), 2);
        assert!(flow.response.is_some());
        assert_eq!(flow.response.unwrap().status_code, 200);
    }

    #[test]
    fn parse_flow_without_response() {
        let json = r#"{"request":{"method":"GET","url":"https://example.com"}}"#;
        let flow = parse_flow_line(json).unwrap();
        assert_eq!(flow.request.method, "GET");
        assert!(flow.response.is_none());
    }

    #[test]
    fn parse_rejects_invalid_json() {
        assert!(parse_flow_line("not json").is_err());
    }

    #[test]
    fn process_flow_emits_hashes() {
        let bus = noop_bus();
        let json = flow_json("POST", "https://api.example.com/data", 200);
        let flow = parse_flow_line(&json).unwrap();
        let result = process_flow(&flow, &bus, 42).unwrap();

        assert_eq!(result.request.method, "POST");
        assert_eq!(result.request.pid, 42);
        assert!(result.request.body_hash.is_some());
        assert!(result.request.headers_hash.is_some());

        let resp = result.response.unwrap();
        assert_eq!(resp.status, 200);
        assert!(resp.body_hash.is_some());
    }

    #[test]
    fn process_flow_without_body() {
        let bus = noop_bus();
        let json = r#"{"request":{"method":"GET","url":"https://example.com","headers":[]}}"#;
        let flow = parse_flow_line(json).unwrap();
        let result = process_flow(&flow, &bus, 1).unwrap();

        assert!(result.request.body_hash.is_none());
        assert!(result.request.headers_hash.is_none());
        assert!(result.response.is_none());
    }

    #[test]
    fn parse_flow_lines_skips_bad_lines() {
        let input = format!(
            "{}\nnot valid json\n{}\n",
            r#"{"request":{"method":"GET","url":"https://a.com","headers":[]}}"#,
            r#"{"request":{"method":"POST","url":"https://b.com","headers":[]}}"#,
        );
        let flows = parse_flow_lines(&input);
        assert_eq!(flows.len(), 2);
    }

    #[test]
    fn process_flow_response_body_decoded() {
        let bus = noop_bus();
        let json = flow_json("GET", "https://example.com", 200);
        let flow = parse_flow_line(&json).unwrap();
        let result = process_flow(&flow, &bus, 1).unwrap();

        let resp = result.response.unwrap();
        assert_eq!(resp.status, 200);
        assert!(resp.body_hash.is_some());
    }

    #[test]
    fn empty_body_string_yields_none() {
        let bus = noop_bus();
        let json = r#"{"request":{"method":"GET","url":"https://x.com","headers":[],"body":""}}"#;
        let flow = parse_flow_line(json).unwrap();
        let result = process_flow(&flow, &bus, 1).unwrap();
        assert!(result.request.body_hash.is_none());
    }
}

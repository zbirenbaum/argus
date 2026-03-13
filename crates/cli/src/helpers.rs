// Rust guideline compliant 2026-02-21
//! Argument parsing and request-building helpers.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::Utc;

use crate::types::RestoreRequest;

/// Convert a human-readable duration like "5m" to an ISO8601 timestamp.
///
/// Passes through strings that already look like timestamps (contain
/// 'T' or '-'). Otherwise parses `<number><unit>` where unit is one
/// of s, m, h, d.
pub fn resolve_since(since: &str) -> Result<String> {
    if since.contains('T') || since.contains('-') {
        return Ok(since.to_owned());
    }
    let split = since
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(since.len());
    let (digits, suffix) = since.split_at(split);
    let n: i64 = digits.parse().context("invalid duration number")?;
    let seconds = match suffix {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        other => bail!("unknown duration suffix: {other} (use s, m, h, d)"),
    };
    let ts = Utc::now() - chrono::Duration::seconds(seconds);
    Ok(ts.to_rfc3339())
}

/// Build query parameters for the events endpoint.
pub fn build_event_params(
    since: Option<&str>,
    path: Option<&str>,
    pid: Option<u32>,
    event_type: Option<&str>,
    limit: u64,
) -> Result<Vec<(&'static str, String)>> {
    let mut params: Vec<(&str, String)> = Vec::new();
    if let Some(s) = since {
        params.push(("since", resolve_since(s)?));
    }
    if let Some(p) = path {
        params.push(("path", p.to_owned()));
    }
    if let Some(p) = pid {
        params.push(("pid", p.to_string()));
    }
    if let Some(t) = event_type {
        params.push(("type", t.to_owned()));
    }
    params.push(("limit", limit.to_string()));
    Ok(params)
}

/// Build the restore request body from CLI flags.
pub fn build_restore_request(
    timestamp: Option<String>,
    seq: Option<u64>,
    target: Option<String>,
    in_place: bool,
    force: bool,
    path: Option<String>,
) -> Result<RestoreRequest> {
    let mode = if path.is_some() {
        "selective".to_owned()
    } else if target.is_some() {
        "new_directory".to_owned()
    } else if in_place {
        "in_place".to_owned()
    } else {
        bail!("specify --target <dir>, --in-place, or --path <file>");
    };

    Ok(RestoreRequest {
        timestamp,
        seq,
        mode,
        target,
        path,
        force: if force { Some(true) } else { None },
        in_place: if in_place { Some(true) } else { None },
    })
}

/// Build query parameters for the timeline endpoint.
pub fn build_timeline_params<'a>(
    agents: &'a str,
    since: Option<&str>,
    event_type: Option<&'a str>,
) -> Vec<(&'a str, String)> {
    let mut params: Vec<(&str, String)> = vec![("agents", agents.to_owned())];
    if let Some(s) = since
        && let Ok(resolved) = resolve_since(s) {
            params.push(("since", resolved));
        }
    if let Some(t) = event_type {
        params.push(("type", t.to_owned()));
    }
    params
}

/// Read a rules file and parse it as JSON.
pub fn read_rules_file(path: &PathBuf) -> Result<serde_json::Value> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read rules file {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("parse rules file {} as JSON", path.display()))
}

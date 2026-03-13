// Rust guideline compliant 2026-02-21
//! Three-tier redaction stage — path exclusion, field drop, and value scrub.
//!
//! Tier 1 (glob): any file event whose path matches an `exclude_paths` pattern
//! has all inline content stripped and `sensitive` set.
//! Tier 2 (set): named dot-path fields are nulled before the event leaves.
//! Tier 3 (regex): fields listed in `scan_fields` are searched for secrets
//! and matching substrings are replaced with a redaction token.
//!
//! The tiers are ordered cheapest-first: most events are clean and exit after
//! the O(1) path check without ever touching the regex engine.

use std::collections::HashSet;

use glob::Pattern;
use regex::Regex;

use crate::config::RedactConfig;
use crate::events::envelope::{Event, EventPayload, Redaction};

/// A compiled regex rule ready for field scanning.
struct CompiledPattern {
    name: String,
    regex: Regex,
    replacement: String,
}

/// Name of a rule that triggered during scrubbing.
struct ScrubMatch {
    rule: String,
}

/// Three-tier PII redaction filter applied to each event in the pipeline.
///
/// Constructed once at startup from a [`RedactConfig`] and reused across
/// all events. All tier lookups are designed to be O(1) or O(p) where `p`
/// is the number of compiled patterns.
pub(crate) struct RedactStage {
    exclude_paths: Vec<Pattern>,
    drop_fields: HashSet<String>,
    scan_fields: HashSet<String>,
    patterns: Vec<CompiledPattern>,
}

impl RedactStage {
    /// Construct a stage by compiling all glob patterns and regexes.
    ///
    /// Invalid glob patterns are silently skipped; invalid regex patterns
    /// are silently skipped. Both cases emit a tracing warning.
    ///
    /// # Errors
    ///
    /// This constructor never returns an error — bad patterns are warned
    /// about and dropped so that the stage degrades gracefully.
    pub(crate) fn new(config: &RedactConfig) -> Self {
        let exclude_paths = config
            .exclude_paths
            .iter()
            .filter_map(|p| match Pattern::new(p) {
                Ok(pat) => Some(pat),
                Err(e) => {
                    tracing::warn!(
                        name: "redact.glob.invalid",
                        pattern = p,
                        error = %e,
                        "skipping invalid exclude_paths glob {{pattern}}: {{error}}"
                    );
                    None
                }
            })
            .collect();

        let drop_fields = config.drop_fields.iter().cloned().collect();
        let scan_fields = config.scan_fields.iter().cloned().collect();
        let patterns = build_patterns(config);

        Self {
            exclude_paths,
            drop_fields,
            scan_fields,
            patterns,
        }
    }

    /// Returns `true` if the path matches any Tier-1 exclusion glob.
    ///
    /// Checks the full path, the basename, and every path suffix starting from
    /// a `/` boundary, so that patterns like `*.env` match `/workspace/.env`
    /// and patterns like `.ssh/**` match `/home/user/.ssh/id_rsa`.
    pub(crate) fn should_exclude_path(&self, path: &str) -> bool {
        // Walk every suffix starting at a slash boundary so that relative
        // glob patterns (e.g. ".ssh/**") match against absolute paths.
        let mut search = path;
        loop {
            if self.exclude_paths.iter().any(|p| p.matches(search)) {
                return true;
            }
            match search.find('/') {
                Some(idx) => search = &search[idx + 1..],
                None => break,
            }
        }
        false
    }

    /// Returns `true` if the dot-path field name should be dropped (Tier 2).
    pub(crate) fn should_drop_field(&self, field: &str) -> bool {
        self.drop_fields.contains(field)
    }

    /// Returns `true` if the dot-path field name is eligible for scanning (Tier 3).
    pub(crate) fn should_scan(&self, field: &str) -> bool {
        self.scan_fields.contains(field)
    }

    /// Apply all compiled patterns to `input`, replacing matches in order.
    ///
    /// Returns the scrubbed string and one [`ScrubMatch`] per pattern that
    /// found at least one match. When no patterns match, the returned `Vec`
    /// is empty and the string is unchanged.
    fn scrub_string(&self, input: &str) -> (String, Vec<ScrubMatch>) {
        let mut current = input.to_owned();
        let mut matches = Vec::new();

        for cp in &self.patterns {
            if cp.regex.is_match(&current) {
                current = cp
                    .regex
                    .replace_all(&current, cp.replacement.as_str())
                    .into_owned();
                matches.push(ScrubMatch {
                    rule: cp.name.clone(),
                });
            }
        }

        (current, matches)
    }

    /// Run the full three-tier pipeline on a mutable event in place.
    pub(crate) fn redact(&self, event: &mut Event) {
        // Tier 1: path exclusion — strip all inline content.
        if let Some(path) = extract_path(&event.payload) {
            if self.should_exclude_path(path) {
                apply_path_exclusion(event);
                return;
            }
        }

        // Tier 2 + 3: field-level operations.
        apply_field_redactions(self, event);
    }
}

/// Extract the primary file path from a payload, if one exists.
fn extract_path(payload: &EventPayload) -> Option<&str> {
    match payload {
        EventPayload::Write(w) => Some(&w.path),
        EventPayload::Read(r) => Some(&r.path),
        EventPayload::Unlink(u) => Some(&u.path),
        EventPayload::Truncate(t) => Some(&t.path),
        _ => None,
    }
}

/// Tier 1: strip inline content and mark the event sensitive.
fn apply_path_exclusion(event: &mut Event) {
    let field = match &event.payload {
        EventPayload::Write(_) => "write.data",
        EventPayload::Read(_) => "read.data",
        EventPayload::Unlink(_) => "unlink.data",
        EventPayload::Truncate(_) => "truncate.before_data+after_data",
        _ => "unknown",
    };

    match &mut event.payload {
        EventPayload::Write(w) => {
            w.sensitive = true;
            w.data = None;
            w.encoding = None;
        }
        EventPayload::Read(r) => {
            r.sensitive = true;
            r.data = None;
            r.encoding = None;
        }
        EventPayload::Unlink(u) => {
            u.sensitive = true;
            u.data = None;
            u.encoding = None;
        }
        EventPayload::Truncate(t) => {
            t.sensitive = true;
            t.before_data = None;
            t.after_data = None;
            t.encoding = None;
        }
        _ => {}
    }

    event.redactions.push(Redaction {
        field: field.to_owned(),
        value: "[excluded by path]".to_owned(),
        rule: "exclude_paths".to_owned(),
    });
}

/// Tier 2 and Tier 3: apply drop-field and scan-field rules.
fn apply_field_redactions(stage: &RedactStage, event: &mut Event) {
    match &mut event.payload {
        EventPayload::HttpRequest(r) => {
            maybe_drop(
                stage,
                &mut r.headers,
                "http_request.headers",
                &mut event.redactions,
            );
            maybe_drop(
                stage,
                &mut r.body,
                "http_request.body",
                &mut event.redactions,
            );
            maybe_scrub(
                stage,
                &mut r.headers,
                "http_request.headers",
                &mut event.redactions,
            );
            maybe_scrub(
                stage,
                &mut r.body,
                "http_request.body",
                &mut event.redactions,
            );
        }
        EventPayload::HttpResponse(r) => {
            maybe_drop(
                stage,
                &mut r.headers,
                "http_response.headers",
                &mut event.redactions,
            );
            maybe_drop(
                stage,
                &mut r.body,
                "http_response.body",
                &mut event.redactions,
            );
            maybe_scrub(
                stage,
                &mut r.headers,
                "http_response.headers",
                &mut event.redactions,
            );
            maybe_scrub(
                stage,
                &mut r.body,
                "http_response.body",
                &mut event.redactions,
            );
        }
        EventPayload::Stdio(s) => {
            maybe_scrub(stage, &mut s.text, "stdio.text", &mut event.redactions);
        }
        EventPayload::Exec(e) => {
            // envp is Vec<String>; join for scanning, then replace if scrubbed.
            scrub_envp(stage, &mut e.envp, &mut event.redactions);
        }
        _ => {}
    }
}

/// Tier 2: null `field_val` if its dot-path is in `drop_fields`.
fn maybe_drop(
    stage: &RedactStage,
    field_val: &mut Option<String>,
    field_name: &str,
    redactions: &mut Vec<Redaction>,
) {
    if field_val.is_some() && stage.should_drop_field(field_name) {
        *field_val = None;
        redactions.push(Redaction {
            field: field_name.to_owned(),
            value: "[dropped]".to_owned(),
            rule: "drop_fields".to_owned(),
        });
    }
}

/// Tier 3: run regex scrubbing on `field_val` if it's in `scan_fields`.
fn maybe_scrub(
    stage: &RedactStage,
    field_val: &mut Option<String>,
    field_name: &str,
    redactions: &mut Vec<Redaction>,
) {
    if !stage.should_scan(field_name) {
        return;
    }
    let Some(value) = field_val.as_deref() else {
        return;
    };
    let (scrubbed, matches) = stage.scrub_string(value);
    if !matches.is_empty() {
        *field_val = Some(scrubbed.clone());
        for m in &matches {
            redactions.push(Redaction {
                field: field_name.to_owned(),
                value: scrubbed.clone(),
                rule: m.rule.clone(),
            });
        }
    }
}

/// Scan each envp entry individually and replace in-place if scrubbed.
fn scrub_envp(
    stage: &RedactStage,
    envp: &mut Vec<String>,
    redactions: &mut Vec<Redaction>,
) {
    if !stage.should_scan("exec.envp") {
        return;
    }
    for entry in envp.iter_mut() {
        let (scrubbed, matches) = stage.scrub_string(entry);
        if !matches.is_empty() {
            *entry = scrubbed.clone();
            for m in &matches {
                redactions.push(Redaction {
                    field: "exec.envp".to_owned(),
                    value: scrubbed.clone(),
                    rule: m.rule.clone(),
                });
            }
        }
    }
}

/// Build the ordered list of compiled patterns from config.
///
/// Built-ins are prepended before user-defined patterns so they run first.
fn build_patterns(config: &RedactConfig) -> Vec<CompiledPattern> {
    let mut patterns = Vec::new();

    if config.builtins.api_keys {
        // Covers Anthropic keys, generic short-lived bearer tokens, and
        // common `sk-` prefixed service keys (OpenAI, Stripe, etc.).
        for (name, pattern) in &[
            ("api_keys.sk_ant", r"sk-ant-[A-Za-z0-9_-]+"),
            ("api_keys.sk", r"sk-[A-Za-z0-9_-]{20,}"),
            (
                "api_keys.bearer",
                r"Bearer\s+[A-Za-z0-9_.\-]+",
            ),
        ] {
            push_pattern(&mut patterns, name, pattern, "[REDACTED]");
        }
    }

    if config.builtins.credentials {
        push_pattern(
            &mut patterns,
            "credentials",
            r"(?i)(password|secret|token|api_key)\s*[=:]\s*\S+",
            "[REDACTED]",
        );
    }

    if config.builtins.private_keys {
        // Dot-matches-newline required for multi-line PEM blocks.
        if let Ok(re) = Regex::new(
            r"(?s)-----BEGIN\s+\S+\s+PRIVATE KEY-----[\s\S]*?-----END\s+\S+\s+PRIVATE KEY-----",
        ) {
            patterns.push(CompiledPattern {
                name: "private_keys".to_owned(),
                regex: re,
                replacement: "[REDACTED-PRIVATE-KEY]".to_owned(),
            });
        }
    }

    if config.builtins.aws_keys {
        // AWS access key IDs are always 20 characters starting with AKIA.
        push_pattern(&mut patterns, "aws_keys", r"AKIA[A-Z0-9]{16}", "[REDACTED-AWS-KEY]");
    }

    // Append user-defined patterns after built-ins.
    for rp in &config.patterns {
        push_pattern(&mut patterns, &rp.name, &rp.regex, &rp.replacement);
    }

    patterns
}

/// Compile a single named regex pattern and push it onto `out`.
///
/// Logs a warning and skips on compile failure so a bad custom regex does not
/// crash the supervisor.
fn push_pattern(out: &mut Vec<CompiledPattern>, name: &str, pattern: &str, replacement: &str) {
    match Regex::new(pattern) {
        Ok(re) => out.push(CompiledPattern {
            name: name.to_owned(),
            regex: re,
            replacement: replacement.to_owned(),
        }),
        Err(e) => {
            tracing::warn!(
                name: "redact.regex.invalid",
                rule = name,
                error = %e,
                "skipping invalid redaction regex {{rule}}: {{error}}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BuiltinRedactions;
    use crate::events::{
        envelope::SequenceGenerator,
        file,
        io::{Stdio, StdioSubtype},
        network::{HttpRequest, HttpResponse},
        process::Exec,
    };

    fn default_stage() -> RedactStage {
        RedactStage::new(&RedactConfig::default())
    }

    fn make_write_event(path: &str, data: Option<&str>) -> Event {
        let seq = SequenceGenerator::default();
        let payload = EventPayload::Write(file::Write {
            pid: 1,
            path: path.to_owned(),
            fd: 3,
            offset: 0,
            size: data.map_or(0, |d| d.len() as u64),
            before_hash: None,
            after_hash: None,
            tree_hash: None,
            data: data.map(str::to_owned),
            encoding: None,
            sensitive: false,
        });
        Event::new(&seq, "test".into(), payload)
    }

    fn make_stdio_event(text: &str) -> Event {
        let seq = SequenceGenerator::default();
        let payload = EventPayload::Stdio(Stdio {
            pid: 1,
            subtype: StdioSubtype::Stdout,
            content_hash: None,
            size: text.len() as u64,
            pipe_inode: None,
            dest_pid: None,
            source_pid: None,
            text: Some(text.to_owned()),
            encoding: None,
        });
        Event::new(&seq, "test".into(), payload)
    }

    fn make_http_request_event(headers: Option<&str>, body: Option<&str>) -> Event {
        let seq = SequenceGenerator::default();
        let payload = EventPayload::HttpRequest(HttpRequest {
            pid: 1,
            method: "POST".to_owned(),
            url: "https://api.example.com/v1/data".to_owned(),
            headers_hash: None,
            body_hash: None,
            headers: headers.map(str::to_owned),
            body: body.map(str::to_owned),
        });
        Event::new(&seq, "test".into(), payload)
    }

    fn make_http_response_event(headers: Option<&str>, body: Option<&str>) -> Event {
        let seq = SequenceGenerator::default();
        let payload = EventPayload::HttpResponse(HttpResponse {
            pid: 1,
            status: 200,
            headers_hash: None,
            body_hash: None,
            headers: headers.map(str::to_owned),
            body: body.map(str::to_owned),
        });
        Event::new(&seq, "test".into(), payload)
    }

    // -------------------------------------------------------------------------
    // Tier 1
    // -------------------------------------------------------------------------

    #[test]
    fn tier1_path_exclusion_strips_all_inline() {
        let stage = default_stage();
        assert!(stage.should_exclude_path("/workspace/.env"));
        assert!(stage.should_exclude_path("/home/user/.env"));
        assert!(stage.should_exclude_path("production.env"));
        assert!(stage.should_exclude_path("/etc/ssl/private/server.pem"));
        assert!(stage.should_exclude_path("/home/user/.ssh/id_rsa"));
        assert!(!stage.should_exclude_path("/workspace/main.rs"));
    }

    #[test]
    fn path_exclusion_sets_sensitive_and_clears_data() {
        let stage = default_stage();
        let mut event = make_write_event("/workspace/.env", Some("SECRET=abc123"));

        stage.redact(&mut event);

        let EventPayload::Write(w) = &event.payload else {
            panic!("wrong variant");
        };
        assert!(w.sensitive, "sensitive must be set");
        assert!(w.data.is_none(), "inline data must be stripped");
        assert_eq!(event.redactions.len(), 1);
        assert_eq!(event.redactions[0].rule, "exclude_paths");
        assert_eq!(event.redactions[0].field, "write.data");
    }

    // -------------------------------------------------------------------------
    // Tier 2
    // -------------------------------------------------------------------------

    #[test]
    fn tier2_field_drop_nullifies() {
        let stage = default_stage();
        assert!(stage.should_drop_field("http_request.headers.authorization"));
        assert!(stage.should_drop_field("http_request.headers.cookie"));
        assert!(stage.should_drop_field("http_request.headers.x-api-key"));
        assert!(!stage.should_drop_field("http_request.body"));
    }

    // -------------------------------------------------------------------------
    // Tier 3
    // -------------------------------------------------------------------------

    #[test]
    fn tier3_only_scans_eligible_fields() {
        let stage = default_stage();
        // stdio.text is in default scan_fields
        assert!(stage.should_scan("stdio.text"));
        // write.data (file data) is NOT in default scan_fields
        assert!(!stage.should_scan("write.data"));
        assert!(!stage.should_scan("read.data"));
    }

    #[test]
    fn tier3_redacts_api_key_in_stdio() {
        let stage = default_stage();
        let mut event = make_stdio_event("token: sk-ant-api03-supersecret-value-xyz");

        stage.redact(&mut event);

        let EventPayload::Stdio(s) = &event.payload else {
            panic!("wrong variant");
        };
        let text = s.text.as_deref().unwrap();
        assert!(
            !text.contains("supersecret"),
            "secret must be replaced, got: {text}"
        );
        assert!(
            !event.redactions.is_empty(),
            "redactions must be populated"
        );
    }

    #[test]
    fn tier3_redacts_bearer_token() {
        let stage = default_stage();
        let (scrubbed, matches) =
            stage.scrub_string("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig");
        assert!(!matches.is_empty(), "bearer token must be detected");
        assert!(!scrubbed.contains("eyJhbGciOiJIUzI1NiJ9"), "token must be replaced");
    }

    #[test]
    fn redacts_aws_key() {
        let stage = default_stage();
        let input = "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE";
        let (scrubbed, matches) = stage.scrub_string(input);
        assert!(!matches.is_empty(), "AWS key must be detected");
        assert!(
            !scrubbed.contains("AKIAIOSFODNN7EXAMPLE"),
            "AWS key must be replaced, got: {scrubbed}"
        );
    }

    // -------------------------------------------------------------------------
    // Audit trail
    // -------------------------------------------------------------------------

    #[test]
    fn redaction_audit_trail_populated() {
        let stage = default_stage();
        let mut event = make_stdio_event("key=sk-ant-api03-AAABBBCCC");

        stage.redact(&mut event);

        assert!(
            !event.redactions.is_empty(),
            "audit trail must be non-empty"
        );
        let r = &event.redactions[0];
        assert!(!r.field.is_empty());
        assert!(!r.rule.is_empty());
        assert!(!r.value.contains("AAABBBCCC"), "scrubbed value stored, not original");
    }

    #[test]
    fn clean_event_no_redactions() {
        let stage = default_stage();
        let mut event = make_stdio_event("hello world, nothing sensitive here");

        stage.redact(&mut event);

        assert!(
            event.redactions.is_empty(),
            "no redactions must be added for clean events"
        );
    }

    // -------------------------------------------------------------------------
    // HTTP request/response field handling
    // -------------------------------------------------------------------------

    #[test]
    fn http_request_authorization_header_dropped() {
        let stage = default_stage();
        // The drop_fields list targets "http_request.headers.authorization" — a
        // sub-path. The stage operates on the whole "http_request.headers" field.
        // Verify that the authorization sub-key drop does NOT match the headers
        // blob key (different dot-path), but that the scrub pass catches bearer
        // tokens within the blob.
        let mut event = make_http_request_event(
            Some("Authorization: Bearer my-secret-token-abcdefghij"),
            None,
        );
        stage.redact(&mut event);

        let EventPayload::HttpRequest(r) = &event.payload else {
            panic!("wrong variant");
        };
        // headers field itself is not in drop_fields (only sub-key is), so it
        // remains present but its contents are scrubbed by Tier 3.
        if let Some(h) = &r.headers {
            assert!(
                !h.contains("my-secret-token"),
                "bearer token must be scrubbed from headers"
            );
        }
    }

    #[test]
    fn http_response_body_scrubbed() {
        let stage = default_stage();
        let mut event =
            make_http_response_event(None, Some("{\"key\":\"sk-validkeyvalue123456789\"}"));

        stage.redact(&mut event);

        let EventPayload::HttpResponse(r) = &event.payload else {
            panic!("wrong variant");
        };
        if let Some(body) = &r.body {
            assert!(
                !body.contains("sk-validkeyvalue123456789"),
                "sk- key must be scrubbed from response body"
            );
        }
    }

    // -------------------------------------------------------------------------
    // Custom pattern
    // -------------------------------------------------------------------------

    #[test]
    fn custom_pattern_scrubs_field() {
        use crate::config::RedactPattern;
        let mut config = RedactConfig {
            builtins: BuiltinRedactions {
                api_keys: false,
                credentials: false,
                private_keys: false,
                aws_keys: false,
            },
            patterns: vec![RedactPattern {
                name: "github_pat".to_owned(),
                regex: r"ghp_[A-Za-z0-9]{36}".to_owned(),
                replacement: "[GH-TOKEN]".to_owned(),
            }],
            ..RedactConfig::default()
        };
        // Make stdio.text scan-eligible.
        config.scan_fields = vec!["stdio.text".to_owned()];

        let stage = RedactStage::new(&config);
        let mut event =
            make_stdio_event("pushing with token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij");

        stage.redact(&mut event);

        let EventPayload::Stdio(s) = &event.payload else {
            panic!("wrong variant");
        };
        let text = s.text.as_deref().unwrap();
        assert!(
            !text.contains("ghp_"),
            "github PAT must be scrubbed, got: {text}"
        );
        assert_eq!(event.redactions[0].rule, "github_pat");
        assert_eq!(event.redactions[0].value, text);
    }

    // -------------------------------------------------------------------------
    // Exec envp scanning
    // -------------------------------------------------------------------------

    #[test]
    fn exec_envp_scrubbed() {
        let stage = default_stage();
        let seq = SequenceGenerator::default();
        let payload = EventPayload::Exec(Exec {
            pid: 42,
            ppid: 1,
            binary: "/usr/bin/curl".to_owned(),
            argv: vec!["curl".to_owned()],
            envp: vec![
                "PATH=/usr/bin".to_owned(),
                "API_KEY=sk-ant-api03-mysupersecretkey".to_owned(),
            ],
            cwd: "/workspace".to_owned(),
        });
        let mut event = Event::new(&seq, "test".into(), payload);

        stage.redact(&mut event);

        let EventPayload::Exec(e) = &event.payload else {
            panic!("wrong variant");
        };
        assert!(
            !e.envp[1].contains("mysupersecretkey"),
            "secret in envp must be redacted"
        );
        assert!(!event.redactions.is_empty());
    }
}

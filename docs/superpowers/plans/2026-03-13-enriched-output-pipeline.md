# Enriched Output Pipeline Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking. **Always activate ms-rust skill before writing Rust code.**

**Goal:** Separate internal durability (CAS) from user-facing outputs, enrich events with inline content, add built-in redaction, and provide lightweight configurable outputs.

**Architecture:** Capture stage retains raw bytes alongside hashes. Stamp stage inlines content into events per enrich config. Redaction stage scrubs sensitive data. DurabilityLayer handles CAS internally. Output trait delivers enriched JSONL to stdout/file/socket/http.

**Tech Stack:** Rust 2024, serde/serde_json, tokio, regex, glob patterns, anyhow/thiserror

**Spec:** `docs/superpowers/specs/2026-03-13-enriched-output-pipeline-design.md`

**Build/test command:** `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus`

**Build command:** `docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor`

**Validation:** `docker exec argus-arm64 ./tests/validate.sh`

---

## File Map

### New files

| File | Responsibility |
|-|-|
| `crates/argus/src/config/enrich.rs` | `EnrichConfig` + per-category `CategoryConfig` |
| `crates/argus/src/config/redact.rs` | `RedactConfig`, builtin patterns, custom patterns, path/field exclusions |
| `crates/argus/src/config/output.rs` | `OutputConfig` enum (Stdout, File, UnixSocket, Http) |
| `crates/argus/src/pipeline/stages/redact.rs` | `RedactStage` — applies redaction to enriched events |
| `crates/argus/src/pipeline/output.rs` | `Output` trait definition |
| `crates/argus/src/pipeline/outputs/mod.rs` | Module declarations + `OutputList` fan-out struct |
| `crates/argus/src/pipeline/outputs/stdout.rs` | `StdoutOutput` — JSONL to stdout |
| `crates/argus/src/pipeline/outputs/file.rs` | `FileOutput` — rotated JSONL files |
| `crates/argus/src/pipeline/durability.rs` | `DurabilityLayer` — owns LocalCas, UploadPool, DigestCache |
| `deploy/demo/vector.yaml` | Example Vector config for production routing |

### Modified files

| File | Changes |
|-|-|
| `crates/argus/src/pipeline/captured.rs` | Add `data: Option<Vec<u8>>` to all content variants; add `FileTruncate` variant |
| `crates/argus/src/events/io.rs` | Add `text: Option<String>`, `encoding: Option<String>` to Stdio, PipeData, PtyData |
| `crates/argus/src/events/file.rs` | Add `data: Option<String>`, `encoding: Option<String>` to Read, Write, Unlink, Truncate |
| `crates/argus/src/events/network.rs` | Add `headers: Option<String>`, `body: Option<String>` to HttpRequest, HttpResponse |
| `crates/argus/src/pipeline/stages/capture.rs` | Retain raw bytes in CapturedContent (clone before hashing, respect max_inline_bytes) |
| `crates/argus/src/pipeline/stages/stamp.rs` | Populate inline fields from CapturedContent data; use EnrichConfig for category toggling |
| `crates/argus/src/pipeline/stages/mod.rs` | Add `pub(crate) mod redact;` |
| `crates/argus/src/pipeline/mod.rs` | Add `output`, `outputs`, `durability` modules; update re-exports |
| `crates/argus/src/config/mod.rs` | Add `enrich`, `redact`, `outputs` fields to SupervisorConfig; add module declarations |
| `crates/argus/src/runtime.rs` | Construct DurabilityLayer + OutputList; pass to CaptureStage and Runner |
| `crates/argus/src/pipeline/runner.rs` | Emit to OutputList instead of RecordBus for events; use DurabilityLayer reference |

---

## Chunk 1: Data Model (CapturedContent + Event Structs + Config)

These three tasks are independent and can be parallelized.

### Task 1: CapturedContent — carry raw bytes

**Files:**
- Modify: `crates/argus/src/pipeline/captured.rs`
- Modify: `crates/argus/src/pipeline/stages/stamp.rs` (update pattern matches)

- [ ] **Step 1: Add `data` field to CapturedContent variants and add FileTruncate**

In `crates/argus/src/pipeline/captured.rs`, update the enum:

```rust
pub enum CapturedContent {
    None,
    FileWrite {
        before_hash: Option<ContentHash>,
        after_hash: Option<ContentHash>,
        data: Option<Vec<u8>>,
        size: usize,
    },
    FileRead {
        content_hash: Option<ContentHash>,
        data: Option<Vec<u8>>,
        size: usize,
    },
    StreamData {
        content_hash: Option<ContentHash>,
        data: Option<Vec<u8>>,
        size: usize,
    },
    FileDelete {
        content_hash: Option<ContentHash>,
        data: Option<Vec<u8>>,
    },
    FileTruncate {
        before_hash: Option<ContentHash>,
        after_hash: Option<ContentHash>,
        before_data: Option<Vec<u8>>,
        after_data: Option<Vec<u8>>,
    },
}
```

- [ ] **Step 2: Fix all pattern match sites that destructure CapturedContent**

Grep for all `CapturedContent::` references across the codebase:
```
grep -rn "CapturedContent::" crates/argus/src/
```

Known sites to update:
- `crates/argus/src/pipeline/stages/stamp.rs` — destructures variants in `to_payload` and `stream_content` helper. Add `data: _` or `data` bindings to every match arm.
- `crates/argus/src/pipeline/stages/capture.rs` — constructs variants. Add `data: None` to each construction site (bytes will be populated in Task 4).
- `crates/argus/src/pipeline/runner.rs` — may pattern-match on content. Update match arms.
- Any test files that construct or match `CapturedContent`. Update with `data: None` or `data: _`.

- [ ] **Step 3: Build and verify no compilation errors**

Run: `docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p argus`

- [ ] **Step 4: Run existing tests to verify no regressions**

Run: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus`

- [ ] **Step 5: Commit**

```
git add crates/argus/src/pipeline/captured.rs crates/argus/src/pipeline/stages/stamp.rs crates/argus/src/pipeline/stages/capture.rs
git commit -m "add data field to CapturedContent variants for byte retention"
```

---

### Task 2: Event struct inline content fields

**Files:**
- Modify: `crates/argus/src/events/io.rs`
- Modify: `crates/argus/src/events/file.rs`
- Modify: `crates/argus/src/events/network.rs`

- [ ] **Step 1: Add fields to io.rs structs**

Add to `Stdio`, `PipeData`, `PtyData`:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub text: Option<String>,
#[serde(skip_serializing_if = "Option::is_none")]
pub encoding: Option<String>,
```

- [ ] **Step 2: Add fields to file.rs structs**

Add to `Read` and `Write`:
```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub data: Option<String>,
#[serde(skip_serializing_if = "Option::is_none")]
pub encoding: Option<String>,
```

Add to `Unlink`:
```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub data: Option<String>,
#[serde(skip_serializing_if = "Option::is_none")]
pub encoding: Option<String>,
```

Add to `Truncate`:
```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub before_data: Option<String>,
#[serde(skip_serializing_if = "Option::is_none")]
pub after_data: Option<String>,
#[serde(skip_serializing_if = "Option::is_none")]
pub encoding: Option<String>,
```

- [ ] **Step 3: Add fields to network.rs structs**

Add to `HttpRequest` and `HttpResponse`:
```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub headers: Option<String>,
#[serde(skip_serializing_if = "Option::is_none")]
pub body: Option<String>,
```

- [ ] **Step 4: Fix all construction sites**

Every place that constructs these structs (stamp.rs, test helpers, route handlers) needs the new fields set to `None`. Search with:
```
grep -rn "Stdio {" crates/argus/src/
grep -rn "PipeData {" crates/argus/src/
grep -rn "PtyData {" crates/argus/src/
grep -rn "ef::Write {" crates/argus/src/
grep -rn "ef::Read {" crates/argus/src/
grep -rn "ef::Unlink {" crates/argus/src/
grep -rn "ef::Truncate {" crates/argus/src/
grep -rn "HttpRequest {" crates/argus/src/
grep -rn "HttpResponse {" crates/argus/src/
```

Add the corresponding new fields to each construction site:
- `Stdio`, `PipeData`, `PtyData` → `text: None, encoding: None`
- `ef::Write`, `ef::Read`, `ef::Unlink` → `data: None, encoding: None`
- `ef::Truncate` → `before_data: None, after_data: None, encoding: None`
- `HttpRequest`, `HttpResponse` → `headers: None, body: None`

- [ ] **Step 5: Update serde round-trip tests**

The existing round-trip tests in `io.rs`, `file.rs`, `network.rs` construct structs without the new fields. Update them to include the new fields (set to `None` for backward compatibility tests, then add one test per struct with `Some(...)` values).

- [ ] **Step 6: Build and test**

Run: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus`

- [ ] **Step 7: Commit**

```
git add crates/argus/src/events/io.rs crates/argus/src/events/file.rs crates/argus/src/events/network.rs crates/argus/src/pipeline/stages/stamp.rs
git commit -m "add inline content fields to event structs"
```

---

### Task 3: Config sections (enrich, redact, outputs)

**Files:**
- Create: `crates/argus/src/config/enrich.rs`
- Create: `crates/argus/src/config/redact.rs`
- Create: `crates/argus/src/config/output.rs`
- Modify: `crates/argus/src/config/mod.rs`

- [ ] **Step 1: Write tests for EnrichConfig**

Create `crates/argus/src/config/enrich.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
}

impl Default for CategoryConfig {
    fn default() -> Self {
        Self { enabled: true, max_bytes: None }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrichConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_max_inline_bytes")]
    pub max_inline_bytes: usize,
    #[serde(default)]
    pub stdio_text: CategoryConfig,
    #[serde(default)]
    pub pipe_data: CategoryConfig,
    #[serde(default)]
    pub pty_data: CategoryConfig,
    #[serde(default)]
    pub file_content: CategoryConfig,
    #[serde(default)]
    pub delete_content: CategoryConfig,
    #[serde(default)]
    pub truncate_content: CategoryConfig,
    #[serde(default)]
    pub http_headers: CategoryConfig,
    #[serde(default)]
    pub http_bodies: CategoryConfig,
    #[serde(default)]
    pub exec_envp: CategoryConfig,
}

impl Default for EnrichConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_inline_bytes: default_max_inline_bytes(),
            stdio_text: CategoryConfig::default(),
            pipe_data: CategoryConfig::default(),
            pty_data: CategoryConfig::default(),
            file_content: CategoryConfig::default(),
            delete_content: CategoryConfig::default(),
            truncate_content: CategoryConfig::default(),
            http_headers: CategoryConfig::default(),
            http_bodies: CategoryConfig::default(),
            exec_envp: CategoryConfig::default(),
        }
    }
}

impl EnrichConfig {
    /// Effective max bytes for a category, falling back to global.
    pub fn max_bytes_for(&self, category: &CategoryConfig) -> usize {
        category.max_bytes.unwrap_or(self.max_inline_bytes)
    }

    /// Whether a category should inline content.
    pub fn should_inline(&self, category: &CategoryConfig) -> bool {
        self.enabled && category.enabled
    }
}

fn default_true() -> bool { true }
fn default_max_inline_bytes() -> usize { 256 * 1024 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_all_enabled() {
        let c = EnrichConfig::default();
        assert!(c.enabled);
        assert!(c.stdio_text.enabled);
        assert!(c.http_bodies.enabled);
        assert_eq!(c.max_inline_bytes, 256 * 1024);
    }

    #[test]
    fn disabled_globally() {
        let c = EnrichConfig { enabled: false, ..EnrichConfig::default() };
        assert!(!c.should_inline(&c.stdio_text));
    }

    #[test]
    fn category_max_bytes_override() {
        let mut c = EnrichConfig::default();
        c.http_bodies.max_bytes = Some(4 * 1024 * 1024);
        assert_eq!(c.max_bytes_for(&c.http_bodies), 4 * 1024 * 1024);
        assert_eq!(c.max_bytes_for(&c.stdio_text), 256 * 1024);
    }

    #[test]
    fn parse_yaml_minimal() {
        let yaml = "enabled: false\n";
        let c: EnrichConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(!c.enabled);
        assert!(c.stdio_text.enabled); // categories still default true
    }

    #[test]
    fn parse_yaml_with_category() {
        let yaml = r#"
http_bodies:
  enabled: true
  max_bytes: 4194304
"#;
        let c: EnrichConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(c.http_bodies.enabled);
        assert_eq!(c.http_bodies.max_bytes, Some(4194304));
    }
}
```

- [ ] **Step 2: Write RedactConfig**

Create `crates/argus/src/config/redact.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuiltinRedactions {
    #[serde(default = "default_true")]
    pub api_keys: bool,
    #[serde(default = "default_true")]
    pub credentials: bool,
    #[serde(default = "default_true")]
    pub private_keys: bool,
}

impl Default for BuiltinRedactions {
    fn default() -> Self {
        Self { api_keys: true, credentials: true, private_keys: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RedactPattern {
    pub name: String,
    pub regex: String,
    #[serde(default = "default_replacement")]
    pub replacement: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RedactConfig {
    #[serde(default)]
    pub builtins: BuiltinRedactions,
    #[serde(default)]
    pub patterns: Vec<RedactPattern>,
    #[serde(default = "default_exclude_paths")]
    pub exclude_paths: Vec<String>,
    #[serde(default)]
    pub exclude_fields: Vec<String>,
}

impl Default for RedactConfig {
    fn default() -> Self {
        Self {
            builtins: BuiltinRedactions::default(),
            patterns: Vec::new(),
            exclude_paths: default_exclude_paths(),
            exclude_fields: Vec::new(),
        }
    }
}

fn default_true() -> bool { true }
fn default_replacement() -> String { "[REDACTED]".to_owned() }
fn default_exclude_paths() -> Vec<String> {
    vec![
        "**/*.env".into(),
        "**/*.pem".into(),
        "**/*.key".into(),
        "**/credentials.json".into(),
        "**/.ssh/**".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_have_builtins_enabled() {
        let c = RedactConfig::default();
        assert!(c.builtins.api_keys);
        assert!(c.builtins.credentials);
        assert!(c.builtins.private_keys);
        assert!(!c.exclude_paths.is_empty());
    }

    #[test]
    fn parse_yaml_with_custom_pattern() {
        let yaml = r#"
patterns:
  - name: github_token
    regex: "ghp_[A-Za-z0-9_]{36}"
    replacement: "[GH_REDACTED]"
"#;
        let c: RedactConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(c.patterns.len(), 1);
        assert_eq!(c.patterns[0].name, "github_token");
    }

    #[test]
    fn round_trip() {
        let c = RedactConfig::default();
        let yaml = serde_yaml::to_string(&c).unwrap();
        let parsed: RedactConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(c, parsed);
    }
}
```

- [ ] **Step 3: Write OutputConfig**

Create `crates/argus/src/config/output.rs`:

```rust
use std::path::PathBuf;
use std::time::Duration;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputConfig {
    Stdout,
    File {
        path: PathBuf,
        #[serde(default = "default_max_file_size")]
        max_size: bytesize::ByteSize,
        #[serde(default = "default_max_files")]
        max_files: u32,
    },
    UnixSocket {
        path: PathBuf,
    },
    Http {
        endpoint: String,
        #[serde(default = "default_http_timeout", with = "humantime_serde")]
        timeout: Duration,
        #[serde(default = "default_retry_max")]
        retry_max: u32,
    },
}

fn default_max_file_size() -> bytesize::ByteSize { bytesize::ByteSize::mib(64) }
fn default_max_files() -> u32 { 10 }
fn default_http_timeout() -> Duration { Duration::from_secs(5) }
fn default_retry_max() -> u32 { 3 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stdout() {
        let yaml = "type: stdout\n";
        let c: OutputConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(c, OutputConfig::Stdout));
    }

    #[test]
    fn parse_file() {
        let yaml = r#"
type: file
path: /data/events.jsonl
max_size: 64MB
max_files: 5
"#;
        let c: OutputConfig = serde_yaml::from_str(yaml).unwrap();
        match c {
            OutputConfig::File { path, max_files, .. } => {
                assert_eq!(path, PathBuf::from("/data/events.jsonl"));
                assert_eq!(max_files, 5);
            }
            _ => panic!("expected File"),
        }
    }

    #[test]
    fn parse_unix_socket() {
        let yaml = "type: unix_socket\npath: /var/run/argus.sock\n";
        let c: OutputConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(c, OutputConfig::UnixSocket { .. }));
    }

    #[test]
    fn parse_http() {
        let yaml = "type: http\nendpoint: http://vector:8080\n";
        let c: OutputConfig = serde_yaml::from_str(yaml).unwrap();
        match c {
            OutputConfig::Http { endpoint, timeout, retry_max } => {
                assert_eq!(endpoint, "http://vector:8080");
                assert_eq!(timeout, Duration::from_secs(5));
                assert_eq!(retry_max, 3);
            }
            _ => panic!("expected Http"),
        }
    }

    #[test]
    fn parse_output_list() {
        let yaml = r#"
- type: stdout
- type: file
  path: /data/events.jsonl
"#;
        let configs: Vec<OutputConfig> = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(configs.len(), 2);
    }
}
```

- [ ] **Step 4: Wire config modules into SupervisorConfig**

In `crates/argus/src/config/mod.rs`, add:

```rust
mod enrich;
mod output;
mod redact;

pub use enrich::{CategoryConfig, EnrichConfig};
pub use output::OutputConfig;
pub use redact::{BuiltinRedactions, RedactConfig, RedactPattern};
```

Add fields to `SupervisorConfig`:

```rust
#[serde(default)]
pub enrich: EnrichConfig,

#[serde(default)]
pub redact: RedactConfig,

#[serde(default = "default_outputs")]
pub outputs: Vec<OutputConfig>,
```

Add:
```rust
fn default_outputs() -> Vec<OutputConfig> {
    vec![OutputConfig::Stdout]
}
```

Update `Default for SupervisorConfig` to include `enrich: EnrichConfig::default(), redact: RedactConfig::default(), outputs: default_outputs()`.

- [ ] **Step 5: Build and test**

Run: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus`

All existing config tests should pass (new fields default to sensible values). New config module tests should pass.

- [ ] **Step 6: Commit**

```
git add crates/argus/src/config/enrich.rs crates/argus/src/config/redact.rs crates/argus/src/config/output.rs crates/argus/src/config/mod.rs
git commit -m "add enrich, redact, and outputs config sections"
```

---

## Chunk 2: Capture + Stamp Enrichment

These tasks are sequential: capture must retain bytes before stamp can use them.

### Task 4: Capture stage retains raw bytes

**Files:**
- Modify: `crates/argus/src/pipeline/stages/capture.rs`

The capture stage already reads tracee memory and gets `Vec<u8>` back. Currently it hashes the bytes and discards them. Change: clone bytes (up to `max_inline_bytes`) into the `data` field of CapturedContent before hashing.

- [ ] **Step 1: Add max_inline_bytes field to CaptureStage**

Add a `max_inline_bytes: usize` field to `CaptureStage`. Update `CaptureStage::new` to accept this parameter:

```rust
pub struct CaptureStage {
    // ... existing fields ...
    max_inline_bytes: usize,
}

impl CaptureStage {
    pub fn new(
        handle: PtraceHandle,
        bus: RecordBus,
        policy: CapturePolicy,
        max_inline_bytes: usize,
    ) -> Self {
        Self { handle, bus, policy, max_inline_bytes, /* ... */ }
    }
}
```

Update the construction site in `runtime.rs` to pass `config.enrich.max_inline_bytes`.

- [ ] **Step 2: Retain bytes in capture_write**

In `capture_write`, after `read_memory` succeeds:

```rust
let inline_data = if d.len() <= self.max_inline_bytes {
    Some(d.clone())
} else {
    None
};
// then emit_content(bus, d) as before
// ...
CapturedContent::FileWrite { before_hash, after_hash, data: inline_data, size: len }
```

- [ ] **Step 3: Retain bytes in capture_read**

Same pattern in `capture_read`:

```rust
let inline_data = if d.len() <= self.max_inline_bytes {
    Some(d.clone())
} else {
    None
};
```

- [ ] **Step 4: Retain bytes in capture_stream**

Same pattern in `capture_stream` (covers stdio, pipe, pty):

```rust
let (content_hash, inline_data) = self.handle.read_memory(pid, buf_addr, len).await.ok()
    .map(|d| {
        self.policy.record_bytes(pid.as_raw() as u32, d.len());
        let inline = if d.len() <= self.max_inline_bytes { Some(d.clone()) } else { None };
        (Some(emit_content(&self.bus, d)), inline)
    })
    .unwrap_or((None, None));
CapturedContent::StreamData { content_hash, data: inline_data, size: len }
```

- [ ] **Step 5: Retain bytes in capture_delete**

```rust
let (content_hash, inline_data) = self.handle.read_file(path.to_path_buf()).await.ok()
    .map(|d| {
        let inline = if d.len() <= self.max_inline_bytes { Some(d.clone()) } else { None };
        (Some(hash_and_emit(&self.bus, d)), inline)
    })
    .unwrap_or((None, None));
CapturedContent::FileDelete { content_hash, data: inline_data }
```

- [ ] **Step 6: Add FileTruncate capture**

The spec defines `FileTruncate` with `before_data`/`after_data`. Add a `capture_truncate` method (or extend existing truncate handling):

```rust
let before_data = self.handle.read_file(path.to_path_buf()).await.ok()
    .and_then(|d| if d.len() <= self.max_inline_bytes { Some(d) } else { None });
// ... after truncate syscall completes ...
let after_data = self.handle.read_file(path.to_path_buf()).await.ok()
    .and_then(|d| if d.len() <= self.max_inline_bytes { Some(d) } else { None });
CapturedContent::FileTruncate { before_hash, after_hash, before_data, after_data }
```

- [ ] **Step 7: Write unit tests for byte retention**

Add tests in `capture.rs` (or a dedicated test module):

```rust
#[test]
fn retains_bytes_under_cap() {
    // Construct CapturedContent::FileWrite with data shorter than max_inline_bytes
    // Verify data field is Some
}

#[test]
fn drops_bytes_over_cap() {
    // Construct CapturedContent::FileWrite with data longer than max_inline_bytes
    // Verify data field is None
}
```

These tests verify the core invariant: bytes ≤ cap → `Some`, bytes > cap → `None`.

- [ ] **Step 8: Build and test**

Run: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus`

- [ ] **Step 9: Commit**

```
git add crates/argus/src/pipeline/stages/capture.rs
git commit -m "capture stage retains raw bytes for enrichment"
```

---

### Task 5: Stamp stage populates inline fields

**Files:**
- Modify: `crates/argus/src/pipeline/stages/stamp.rs`

- [ ] **Step 1: Add EnrichConfig to StampStage**

```rust
pub struct StampStage {
    pub seq_gen: Arc<SequenceGenerator>,
    pub agent_id: String,
    pub enrich: EnrichConfig,
}
```

Update `new()` to accept `EnrichConfig`.

- [ ] **Step 2: Write helper to convert bytes to inline string**

```rust
/// Convert captured bytes to an inline string with encoding detection.
fn bytes_to_inline(data: &[u8]) -> (String, Option<String>) {
    match std::str::from_utf8(data) {
        Ok(s) => (s.to_owned(), None),
        Err(_) => {
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(data);
            (encoded, Some("base64".to_owned()))
        }
    }
}
```

Note: add `base64` to `Cargo.toml` dependencies if not present.

- [ ] **Step 3: Update to_payload for Stdio/PipeData/PtyData**

In the `Classification::Stdio` match arm, after getting `stream_content`:

```rust
let (text, encoding) = match &content {
    CapturedContent::StreamData { data: Some(d), .. } if enrich.should_inline(&enrich.stdio_text) => {
        let (t, e) = bytes_to_inline(d);
        (Some(t), e)
    }
    _ => (None, None),
};
// Set on the Stdio struct:
EventPayload::Stdio(Stdio {
    // ... existing fields ...
    text,
    encoding,
})
```

Same pattern for PipeData (using `enrich.pipe_data`, field `text`, `encoding`) and PtyData (using `enrich.pty_data`, field `text`, `encoding`).

- [ ] **Step 4: Update to_payload for FileWrite/FileRead**

For `Classification::FileWrite`:
```rust
let (inline_data, encoding) = match &content {
    CapturedContent::FileWrite { data: Some(d), .. } if enrich.should_inline(&enrich.file_content) => {
        let (t, e) = bytes_to_inline(d);
        (Some(t), e)
    }
    _ => (None, None),
};
// set data: inline_data, encoding on ef::Write
```

Same for FileRead.

- [ ] **Step 5: Update to_payload for Unlink and Truncate**

For Unlink (using `enrich.delete_content`):
```rust
let (inline_data, encoding) = match &content {
    CapturedContent::FileDelete { data: Some(d), .. } if enrich.should_inline(&enrich.delete_content) => {
        let (t, e) = bytes_to_inline(d);
        (Some(t), e)
    }
    _ => (None, None),
};
// Set data: inline_data, encoding on ef::Unlink
```

For Truncate (using `enrich.truncate_content`):
```rust
let (before_data_str, after_data_str, encoding) = match &content {
    CapturedContent::FileTruncate { before_data, after_data, .. }
        if enrich.should_inline(&enrich.truncate_content) =>
    {
        let (bd, be) = before_data.as_ref().map(|d| bytes_to_inline(d)).unzip();
        let (ad, ae) = after_data.as_ref().map(|d| bytes_to_inline(d)).unzip();
        // Use base64 encoding if either part is binary
        let enc = be.flatten().or(ae.flatten());
        (bd, ad, enc)
    }
    _ => (None, None, None),
};
// Set before_data: before_data_str, after_data: after_data_str, encoding on ef::Truncate
```

- [ ] **Step 6: Pass enrich config through to_payload**

Change `to_payload` signature:
```rust
fn to_payload(
    pid: u32,
    cls: Classification,
    content: CapturedContent,
    tree_hash: Option<String>,
    enrich: &EnrichConfig,
) -> Option<EventPayload>
```

Update the call site in `StampStage::stamp`.

- [ ] **Step 7: Write tests**

Add tests in stamp.rs:
- `stamps_stdio_with_inline_text`: verify `text` field is populated when `CapturedContent::StreamData` has data
- `stamps_write_with_inline_data`: verify `data` field populated for FileWrite
- `enrichment_disabled_yields_none_text`: verify `text` is None when enrich config has `enabled: false`
- `binary_data_base64_encoded`: pass non-UTF-8 bytes, verify `encoding` is `Some("base64")`

- [ ] **Step 8: Build and test**

Run: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus`

- [ ] **Step 9: Commit**

```
git add crates/argus/src/pipeline/stages/stamp.rs crates/argus/Cargo.toml
git commit -m "stamp stage populates inline content fields from captured bytes"
```

Note: `base64` crate added to `Cargo.toml` in Step 2.

---

### Task 6: Redaction stage

**Files:**
- Create: `crates/argus/src/pipeline/stages/redact.rs`
- Modify: `crates/argus/src/pipeline/stages/mod.rs`

- [ ] **Step 1: Write failing test for pattern redaction**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_api_key_in_string() {
        let stage = RedactStage::new(RedactConfig::default());
        let input = "Authorization: Bearer sk-ant-api03-abc123xyz";
        let output = stage.scrub_string(input);
        assert!(!output.contains("sk-ant-api03"));
        assert!(output.contains("[REDACTED]"));
    }
}
```

- [ ] **Step 2: Implement RedactStage**

```rust
use regex::Regex;
use crate::config::RedactConfig;
use crate::events::Event;

pub struct RedactStage {
    patterns: Vec<CompiledPattern>,
    exclude_paths: Vec<glob::Pattern>,
    exclude_fields: Vec<String>,
}

struct CompiledPattern {
    regex: Regex,
    replacement: String,
}
```

Key methods:
- `new(config: RedactConfig) -> Self` — compile all regexes (builtins + custom)
- `scrub_string(&self, input: &str) -> String` — apply all patterns
- `redact(&self, event: &mut Event)` — walk event payload fields, apply scrubbing
- `should_exclude_path(&self, path: &str) -> bool` — check path exclusion

Built-in patterns to compile:
- API keys: `sk-ant-[A-Za-z0-9_-]+`, `sk-[A-Za-z0-9_-]{20,}`, `Bearer\s+[A-Za-z0-9_.-]+`
- Credentials: `(?i)(password|secret|token|api_key)\s*[=:]\s*\S+`
- Private keys: `-----BEGIN\s+\S+\s+PRIVATE KEY-----[\s\S]*?-----END\s+\S+\s+PRIVATE KEY-----`
- AWS keys: `AKIA[A-Z0-9]{16}`

- [ ] **Step 3: Write more tests**

- `redacts_aws_key`: input with AKIAIOSFODNN7EXAMPLE → redacted
- `redacts_private_key_block`: PEM block → redacted
- `redacts_custom_pattern`: custom pattern from config
- `path_exclusion_strips_inline`: event with path matching `**/*.env` has inline data stripped
- `field_exclusion_nullifies`: `exclude_fields: ["http_request.headers"]` nullifies headers
- `leaves_hash_intact`: verify content_hash is never modified

- [ ] **Step 4: Implement redact method on Event**

The `redact` method matches on `EventPayload` variants and applies `scrub_string` to each inline content field:

```rust
pub fn redact(&self, event: &mut Event) {
    match &mut event.payload {
        EventPayload::Stdio(ref mut s) => {
            if let Some(path) = self.event_path(event) {
                if self.should_exclude_path(&path) { s.text = None; return; }
            }
            if let Some(ref mut t) = s.text { *t = self.scrub_string(t); }
        }
        EventPayload::PipeData(ref mut p) => {
            if let Some(ref mut t) = p.text { *t = self.scrub_string(t); }
        }
        EventPayload::PtyData(ref mut p) => {
            if let Some(ref mut t) = p.text { *t = self.scrub_string(t); }
        }
        EventPayload::Write(ref mut w) => {
            if self.should_exclude_path(&w.path) { w.data = None; return; }
            if let Some(ref mut d) = w.data { *d = self.scrub_string(d); }
        }
        EventPayload::Read(ref mut r) => {
            if self.should_exclude_path(&r.path) { r.data = None; return; }
            if let Some(ref mut d) = r.data { *d = self.scrub_string(d); }
        }
        EventPayload::Unlink(ref mut u) => {
            if self.should_exclude_path(&u.path) { u.data = None; return; }
            if let Some(ref mut d) = u.data { *d = self.scrub_string(d); }
        }
        EventPayload::Truncate(ref mut t) => {
            if self.should_exclude_path(&t.path) { t.before_data = None; t.after_data = None; return; }
            if let Some(ref mut d) = t.before_data { *d = self.scrub_string(d); }
            if let Some(ref mut d) = t.after_data { *d = self.scrub_string(d); }
        }
        EventPayload::HttpRequest(ref mut h) => {
            if self.is_field_excluded("http_request.headers") { h.headers = None; }
            if let Some(ref mut hd) = h.headers { *hd = self.scrub_string(hd); }
            if let Some(ref mut b) = h.body { *b = self.scrub_string(b); }
        }
        EventPayload::HttpResponse(ref mut h) => {
            if self.is_field_excluded("http_response.headers") { h.headers = None; }
            if let Some(ref mut hd) = h.headers { *hd = self.scrub_string(hd); }
            if let Some(ref mut b) = h.body { *b = self.scrub_string(b); }
        }
        _ => {} // variants without inline content (exec, fork, etc.)
    }
}

fn is_field_excluded(&self, field: &str) -> bool {
    self.exclude_fields.iter().any(|f| f == field)
}
```

Note: `regex` crate may need adding to `Cargo.toml` and `glob` crate for path matching.

- [ ] **Step 5: Build and test**

Run: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus`

- [ ] **Step 6: Commit**

```
git add crates/argus/src/pipeline/stages/redact.rs crates/argus/src/pipeline/stages/mod.rs
git commit -m "add redaction stage with builtin and custom pattern scrubbing"
```

---

## Chunk 3: Output System + DurabilityLayer

### Task 7: Output trait and StdoutOutput

**Files:**
- Create: `crates/argus/src/pipeline/output.rs`
- Create: `crates/argus/src/pipeline/outputs/mod.rs`
- Create: `crates/argus/src/pipeline/outputs/stdout.rs`
- Modify: `crates/argus/src/pipeline/mod.rs`

- [ ] **Step 1: Define Output trait**

In `crates/argus/src/pipeline/output.rs`:

```rust
use anyhow::Result;
use crate::events::Event;

pub trait Output: Send {
    fn emit(&mut self, event: &Event) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
    fn shutdown(&mut self) -> Result<()> { self.flush() }
    fn name(&self) -> &str;
}
```

- [ ] **Step 2: Implement StdoutOutput**

In `crates/argus/src/pipeline/outputs/stdout.rs`:

```rust
use std::io::{self, BufWriter, Write as IoWrite};
use anyhow::{Context, Result};
use crate::events::Event;
use crate::pipeline::output::Output;

pub struct StdoutOutput {
    out: BufWriter<io::Stdout>,
}

impl StdoutOutput {
    pub fn new() -> Self {
        Self { out: BufWriter::new(io::stdout()) }
    }
}

impl Output for StdoutOutput {
    fn emit(&mut self, event: &Event) -> Result<()> {
        let json = serde_json::to_string(event)
            .with_context(|| format!("serialize event seq={}", event.seq))?;
        writeln!(self.out, "{json}").context("write to stdout")?;
        self.out.flush().context("flush stdout")
    }
    fn flush(&mut self) -> Result<()> {
        self.out.flush().context("flush stdout output")
    }
    fn name(&self) -> &str { "stdout" }
}
```

- [ ] **Step 3: Implement OutputList**

In `crates/argus/src/pipeline/outputs/mod.rs`:

```rust
pub mod stdout;
// pub mod file; — added in Task 8 when file.rs is created

pub use stdout::StdoutOutput;

use tracing::{event, Level};
use crate::events::Event;
use crate::pipeline::output::Output;

pub struct OutputList {
    outputs: Vec<Box<dyn Output>>,
}

impl OutputList {
    pub fn new(outputs: Vec<Box<dyn Output>>) -> Self {
        Self { outputs }
    }

    pub fn emit(&mut self, event: &Event) {
        for output in &mut self.outputs {
            if let Err(e) = output.emit(event) {
                event!(
                    name: "output.emit_error",
                    Level::WARN,
                    output.name = output.name(),
                    error.message = %e,
                    "output emit failed",
                );
            }
        }
    }

    pub fn flush(&mut self) {
        for output in &mut self.outputs {
            if let Err(e) = output.flush() {
                event!(
                    name: "output.flush_error",
                    Level::WARN,
                    output.name = output.name(),
                    error.message = %e,
                    "output flush failed",
                );
            }
        }
    }

    pub fn shutdown(&mut self) {
        for output in &mut self.outputs {
            if let Err(e) = output.shutdown() {
                event!(
                    name: "output.shutdown_error",
                    Level::WARN,
                    output.name = output.name(),
                    error.message = %e,
                    "output shutdown failed",
                );
            }
        }
    }
}
```

- [ ] **Step 4: Write tests for OutputList**

Test that emit reaches all outputs, that one failed output doesn't block others.

- [ ] **Step 5: Add modules to pipeline/mod.rs**

```rust
pub(crate) mod output;
pub(crate) mod outputs;
```

- [ ] **Step 6: Build and test**

- [ ] **Step 7: Commit**

```
git commit -m "add Output trait, StdoutOutput, and OutputList fan-out"
```

---

### Task 8: FileOutput (rotated JSONL)

**Files:**
- Create: `crates/argus/src/pipeline/outputs/file.rs`

- [ ] **Step 1: Implement FileOutput**

In `crates/argus/src/pipeline/outputs/file.rs`:

```rust
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write as IoWrite};
use std::path::PathBuf;
use anyhow::{Context, Result};
use bytesize::ByteSize;
use crate::events::Event;
use crate::pipeline::output::Output;

pub struct FileOutput {
    path: PathBuf,
    max_size: ByteSize,
    max_files: u32,
    writer: BufWriter<File>,
    current_size: u64,
}

impl FileOutput {
    pub fn new(path: PathBuf, max_size: ByteSize, max_files: u32) -> Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let current_size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path,
            max_size,
            max_files,
            writer: BufWriter::new(file),
            current_size,
        })
    }

    fn rotate(&mut self) -> Result<()> {
        self.writer.flush()?;
        // Shift existing rotated files up: .N → .N+1, delete .max_files
        for i in (1..self.max_files).rev() {
            let from = self.path.with_extension(format!("jsonl.{i}"));
            let to = self.path.with_extension(format!("jsonl.{}", i + 1));
            if from.exists() { fs::rename(&from, &to)?; }
        }
        // Delete oldest if it exists
        let oldest = self.path.with_extension(format!("jsonl.{}", self.max_files));
        if oldest.exists() { fs::remove_file(&oldest)?; }
        // Current → .1
        let rotated = self.path.with_extension("jsonl.1");
        fs::rename(&self.path, &rotated)?;
        // Open new file
        let file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.writer = BufWriter::new(file);
        self.current_size = 0;
        Ok(())
    }
}

impl Output for FileOutput {
    fn emit(&mut self, event: &Event) -> Result<()> {
        if self.current_size >= self.max_size.as_u64() {
            self.rotate().context("rotate file output")?;
        }
        let json = serde_json::to_string(event)?;
        let line = format!("{json}\n");
        self.writer.write_all(line.as_bytes())?;
        self.current_size += line.len() as u64;
        Ok(())
    }
    fn flush(&mut self) -> Result<()> { self.writer.flush().context("flush file output") }
    fn name(&self) -> &str { "file" }
}
```

Also add `pub mod file;` and `pub use file::FileOutput;` to `outputs/mod.rs`.

- [ ] **Step 2: Write tests**

- `writes_jsonl_line`: emit event, read file, verify valid JSON
- `rotates_at_max_size`: set small max_size, emit multiple events, verify rotation
- `respects_max_files`: verify old rotated files are deleted

Use `tempfile::tempdir()` for test isolation.

- [ ] **Step 3: Build and test**

- [ ] **Step 4: Commit**

```
git commit -m "add FileOutput with rotation"
```

---

### Task 9: DurabilityLayer

**Files:**
- Create: `crates/argus/src/pipeline/durability.rs`
- Modify: `crates/argus/src/pipeline/mod.rs`

- [ ] **Step 1: Define DurabilityLayer**

```rust
use std::sync::Arc;
use anyhow::Result;
use crate::cas::{ContentHash, LocalCas};
use crate::storage::digest_cache::DigestCache;
use crate::storage::upload_job::UploadJob;
use crate::storage::upload_pool::UploadPool;

pub struct DurabilityLayer {
    local_cas: LocalCas,
    upload_pool: Option<Arc<UploadPool>>,
    digest_cache: Option<Arc<DigestCache>>,
}

impl DurabilityLayer {
    pub fn new(
        local_cas: LocalCas,
        upload_pool: Option<Arc<UploadPool>>,
        digest_cache: Option<Arc<DigestCache>>,
    ) -> Self {
        Self { local_cas, upload_pool, digest_cache }
    }

    /// Persist content to local CAS (blocking). Returns the hash.
    pub fn persist(&mut self, data: &[u8]) -> Result<ContentHash> {
        let hash = ContentHash::from_data(data);
        self.local_cas.put_with_hash(hash, data)?;
        Ok(hash)
    }

    /// Persist with a pre-computed hash.
    pub fn persist_with_hash(&mut self, hash: ContentHash, data: &[u8]) -> Result<()> {
        self.local_cas.put_with_hash(hash, data)?;
        Ok(())
    }

    /// Enqueue async upload if remote is configured and hash is not cached.
    pub fn upload_async(&self, hash: ContentHash, data: Vec<u8>) {
        if let (Some(pool), Some(cache)) = (&self.upload_pool, &self.digest_cache) {
            if cache.contains(&hash) { return; }
            let _ = pool.submit(UploadJob::CasObject { hash, data });
        }
    }
}
```

- [ ] **Step 2: Write tests**

Use `tempfile::tempdir()` for the local CAS root. Construct real `LocalCas` and `DigestCache` instances (no mocking needed — they are lightweight file-based structs):

```rust
#[test]
fn persist_stores_locally() {
    let dir = tempfile::tempdir().unwrap();
    let cas = LocalCas::new(dir.path().join("cas")).unwrap();
    let mut dl = DurabilityLayer::new(cas, None, None);
    let hash = dl.persist(b"hello").unwrap();
    assert!(dl.local_cas.exists(&hash));
}

#[test]
fn upload_async_skips_when_no_remote() {
    let dir = tempfile::tempdir().unwrap();
    let cas = LocalCas::new(dir.path().join("cas")).unwrap();
    let dl = DurabilityLayer::new(cas, None, None);
    let hash = ContentHash::from_data(b"hello");
    dl.upload_async(hash, b"hello".to_vec()); // should not panic
}
```

For `upload_async_skips_cached`: construct a real `DigestCache`, pre-insert the hash, pass it to `DurabilityLayer`. Verify `UploadPool` is not called (use a bounded channel and assert it's empty after `upload_async`).

- [ ] **Step 3: Build and test**

- [ ] **Step 4: Commit**

```
git commit -m "add DurabilityLayer encapsulating CAS and upload"
```

---

## Chunk 4: Rewire Runtime and Runner

### Task 10: Rewire runtime to use DurabilityLayer + OutputList

**Files:**
- Modify: `crates/argus/src/runtime.rs`
- Modify: `crates/argus/src/pipeline/runner.rs`
- Modify: `crates/argus/src/pipeline/stages/capture.rs`

This is the integration task. The runtime constructs DurabilityLayer and OutputList instead of RecordBus for events. The RecordBus is retained only for content records to CAS (or can be simplified).

- [ ] **Step 1: Update runtime::build_bus → build_outputs**

Replace the `build_bus` function. New flow:

1. Construct `DurabilityLayer` from LocalCas + UploadPool + DigestCache
2. Construct `OutputList` from config.outputs (map OutputConfig → Box<dyn Output>)
3. Keep a simplified `RecordBus` for CAS content records only (LocalCasSink + RemoteCasSink), or pass DurabilityLayer directly to CaptureStage

- [ ] **Step 2: Update CaptureStage to use DurabilityLayer**

Replace `bus: RecordBus` with `durability: Arc<Mutex<DurabilityLayer>>` (or pass as a reference). The capture stage calls `durability.persist()` instead of `emit_content()` to the bus.

Update `emit_content` to use DurabilityLayer:
```rust
fn emit_content(durability: &Mutex<DurabilityLayer>, data: Vec<u8>) -> ContentHash {
    let hash = ContentHash::from_data(&data);
    if let Ok(mut dl) = durability.lock() {
        let _ = dl.persist_with_hash(hash, &data);
        dl.upload_async(hash, data);
    }
    hash
}
```

- [ ] **Step 3: Update PipelineRunner**

Replace `bus: RecordBus` with:
- `outputs: OutputList` — for enriched events
- `redact: RedactStage` — runs before output

After stamp:
```rust
if let Some(mut evt) = self.stamp.stamp(captured, tree_hash) {
    self.redact.redact(&mut evt);
    self.outputs.emit(&evt);
}
```

On shutdown:
```rust
self.outputs.shutdown();
```

Remove `self.bus.emit(Record::Event(...))` calls — replaced by `self.outputs.emit()`.

- [ ] **Step 4: Update runtime::into_pipeline**

Pass `OutputList` and `RedactStage` to `PipelineRunner::new` instead of `RecordBus`.

- [ ] **Step 5: Update emit_agent_start and emit_initial_state**

These currently emit through `ctx.bus`. Update to emit through outputs instead. Pass `&mut OutputList` as a parameter to these functions (they run before the pipeline loop starts, so ownership is straightforward — the runtime constructs `OutputList`, calls these emit helpers, then moves `OutputList` into `PipelineRunner`).

- [ ] **Step 5a: Remove RecordBus**

After rewiring, `RecordBus` is no longer needed. Remove:
- `crates/argus/src/pipeline/bus.rs` — delete the file
- `crates/argus/src/pipeline/mod.rs` — remove `pub(crate) mod bus;` and `pub(crate) use bus::RecordBus;`
- `crates/argus/src/pipeline/sink.rs` — delete (Sink trait replaced by Output trait)
- `crates/argus/src/pipeline/mod.rs` — remove `pub(crate) mod sink;`
- Search for all remaining `use super::bus::RecordBus` or `use crate::pipeline::bus::RecordBus` and remove them.

Verify with:
```
grep -rn "RecordBus\|use.*bus::" crates/argus/src/
grep -rn "Sink\b" crates/argus/src/pipeline/
```

- [ ] **Step 6: Build**

Run: `docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor`

Fix any compilation errors.

- [ ] **Step 7: Run unit tests**

Run: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus`

- [ ] **Step 8: Run validation tests**

Run: `docker exec argus-arm64 ./tests/validate.sh`

All 14 tests must pass. The validation harness reads stdout JSONL — StdoutOutput replaces StdoutSink with identical behavior.

- [ ] **Step 9: Commit**

```
git commit -m "rewire runtime to use DurabilityLayer + OutputList

Replace RecordBus event fan-out with OutputList. CAS durability
is now internal via DurabilityLayer. Events flow through stamp →
redact → outputs."
```

---

## Chunk 5: Deploy Examples + Cleanup

### Task 11: Vector example and cleanup

**Files:**
- Create: `deploy/demo/vector.yaml`
- Modify: `deploy/demo/supervisor.yaml` (add outputs section)

- [ ] **Step 1: Create deploy/demo/vector.yaml**

```yaml
sources:
  argus:
    type: file
    include: ["/data/events.jsonl"]

transforms:
  parsed:
    inputs: ["argus"]
    type: remap
    source: '. = parse_json!(.message)'

sinks:
  s3_events:
    inputs: ["parsed"]
    type: aws_s3
    bucket: argus-demo
    key_prefix: "claude-agent/events/"
    region: us-west-2
    endpoint: http://host.docker.internal:9100
    encoding:
      codec: json
    compression: gzip
    batch:
      timeout_secs: 60
      max_bytes: 10000000
```

- [ ] **Step 2: Update deploy/demo/supervisor.yaml**

Add outputs section:
```yaml
outputs:
  - type: stdout
  - type: file
    path: /data/events.jsonl
```

- [ ] **Step 3: Remove dead sink code**

First verify what's still referenced:
```
grep -rn "EventLogSink\|BroadcastSink\|IndexSink\|StdoutSink" crates/argus/src/ --include="*.rs"
grep -rn "LocalCasSink\|RemoteCasSink" crates/argus/src/ --include="*.rs"
```

Expected: `EventLogSink`, `BroadcastSink`, `IndexSink`, `StdoutSink` should have zero references outside their own files and `sinks/mod.rs` (all replaced by OutputList + DurabilityLayer).

`LocalCasSink` and `RemoteCasSink` should also be unreferenced if DurabilityLayer absorbed their logic in Task 9/10. If still referenced, keep them.

Remove unreferenced files:
- `crates/argus/src/pipeline/sinks/event_log.rs`
- `crates/argus/src/pipeline/sinks/broadcast.rs`
- `crates/argus/src/pipeline/sinks/index.rs`
- `crates/argus/src/pipeline/sinks/stdout.rs`
- `crates/argus/src/pipeline/sinks/local_cas.rs` (if absorbed into DurabilityLayer)
- `crates/argus/src/pipeline/sinks/remote_cas.rs` (if absorbed into DurabilityLayer)

Update `crates/argus/src/pipeline/sinks/mod.rs` to remove dead module declarations and re-exports. If the entire `sinks/` directory is empty, remove it and the `pub(crate) mod sinks;` from `pipeline/mod.rs`.

**Note:** unix_socket and http output types are deferred — document in the task doc under "What's missing".

- [ ] **Step 4: Update task doc**

Create or update `docs/tasks/enriched-output-pipeline.md` with status, what was done, what works, what's missing, how to test.

- [ ] **Step 5: Final build + test**

Run: `docker exec argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor`
Run: `docker exec argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus -p supervisor`
Run: `docker exec argus-arm64 ./tests/validate.sh`

- [ ] **Step 6: Commit**

```
git commit -m "add Vector deploy example, clean up dead sink code"
```

---

## Parallelization Guide

Tasks that can run in parallel (no dependencies between them):

| Parallel group | Tasks |
|-|-|
| Group 1 (data model) | Task 1, Task 2, Task 3 |
| Group 2 (enrichment, sequential) | Task 4 → Task 5 → Task 6 |
| Group 3 (output system) | Task 7, Task 8, Task 9 |
| Group 4 (integration) | Task 10 (depends on all above) |
| Group 5 (cleanup) | Task 11 (depends on Task 10) |

Recommended execution: Group 1 in parallel, then Group 2 sequentially, then Group 3 in parallel, then Group 4, then Group 5.

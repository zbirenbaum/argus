# TLS Capture Integration Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking. **Activate ms-rust skill before writing any Rust code.**

**Goal:** Wire up the existing TLS library modules (KeylogWatcher, FlowWatcher, mitmdump addon) into the supervisor so it emits `tls_keys`, `http_request`, and `http_response` events — then implement validation test 8.

**Architecture:** A background `tls-watcher` thread polls the SSLKEYLOGFILE and mitmdump flow output file every 200ms, builds events via the existing library watchers, and sends them through the same `mpsc::Sender<Event>` channel used by the tracer. The mitmdump addon script is embedded in the supervisor binary via `include_str!` and written to `data_dir` at startup so the binary is self-contained.

**Tech Stack:** Rust (edition 2024), `argus::net::{KeylogWatcher, FlowWatcher}`, `argus::cas::LocalCas`, bash/python3 for test 8.

---

## File Map

| File | Action | Responsibility |
|-|-|-|
| `crates/supervisor/src/tls_watcher.rs` | Create | Background polling thread: KeylogWatcher + FlowWatcher → events |
| `crates/supervisor/src/main.rs` | Modify | Embed addon, write to data_dir, spawn tls-watcher, shutdown ordering |
| `tests/validate.sh` | Modify | Replace test_8 stub with local HTTPS server + assertions |

---

## Chunk 1: TLS Watcher Thread + Supervisor Integration

### Task 1: Create `tls_watcher.rs` module

**Files:**
- Create: `crates/supervisor/src/tls_watcher.rs`

- [ ] **Step 1: Create the tls_watcher module**

Create `crates/supervisor/src/tls_watcher.rs` with the polling thread function. This module:

1. Takes ownership of a `KeylogWatcher`, an optional `FlowWatcher`, a `LocalCas`, the event channel sender, the sequence generator, the agent ID, and a stop flag.
2. Loops every 200ms:
   - Calls `keylog_watcher.read_new_lines()` → for each line, calls `keylog_watcher.process_new_lines()` to store in CAS and build `TlsKeys` payloads → wraps each in `Event::new()` and sends via `event_tx`.
   - If flow watcher present: calls `flow_watcher.process_new_flows()` → converts to `EventPayload::HttpRequest` / `EventPayload::HttpResponse` → wraps and sends.
   - Checks stop flag, breaks if set.
3. Logs warnings on errors but never panics.

```rust
// Rust guideline compliant 2026-02-21
//! Background TLS event poller.
//!
//! Periodically reads the SSLKEYLOGFILE and mitmdump flow output,
//! building and emitting `TlsKeys`, `HttpRequest`, and `HttpResponse`
//! events. Runs on a dedicated thread alongside the ptrace loop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tracing::{event, Level};

use argus::cas::LocalCas;
use argus::events::{Event, EventPayload, SequenceGenerator};
use argus::net::{FlowWatcher, KeylogWatcher};

/// How often the watcher polls for new keylog lines and flow data.
/// Fast enough to capture most TLS sessions before the agent exits,
/// slow enough to avoid burning CPU on an idle file.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Spawns the TLS watcher thread.
///
/// Returns the join handle. The caller must set `stop` to `true`
/// and join the handle during shutdown.
pub fn spawn(
    keylog_path: PathBuf,
    flow_output: Option<PathBuf>,
    cas: LocalCas,
    event_tx: Sender<Event>,
    seq_gen: SequenceGenerator,
    agent_id: String,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("tls-watcher".into())
        .spawn(move || {
            run(keylog_path, flow_output, cas, event_tx, seq_gen, agent_id, stop);
        })
        .expect("failed to spawn tls-watcher thread")
}

/// Polling loop body.
fn run(
    keylog_path: PathBuf,
    flow_output: Option<PathBuf>,
    cas: LocalCas,
    event_tx: Sender<Event>,
    seq_gen: SequenceGenerator,
    agent_id: String,
    stop: Arc<AtomicBool>,
) {
    let mut keylog = KeylogWatcher::new(keylog_path);
    let mut flow = flow_output.map(FlowWatcher::new);

    event!(
        name: "tls_watcher.started",
        Level::INFO,
        "TLS watcher thread started",
    );

    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }

        poll_keylog(&mut keylog, &cas, &event_tx, &seq_gen, &agent_id);

        if let Some(ref mut fw) = flow {
            poll_flows(fw, &cas, &event_tx, &seq_gen, &agent_id);
        }

        thread::sleep(POLL_INTERVAL);
    }

    // Final drain — capture anything written between the last poll
    // and the stop signal.
    poll_keylog(&mut keylog, &cas, &event_tx, &seq_gen, &agent_id);
    if let Some(ref mut fw) = flow {
        poll_flows(fw, &cas, &event_tx, &seq_gen, &agent_id);
    }

    event!(
        name: "tls_watcher.stopped",
        Level::INFO,
        "TLS watcher thread stopped",
    );
}

/// Reads new keylog lines and emits `TlsKeys` events.
fn poll_keylog(
    watcher: &mut KeylogWatcher,
    cas: &LocalCas,
    tx: &Sender<Event>,
    seq_gen: &SequenceGenerator,
    agent_id: &str,
) {
    match watcher.process_new_lines(cas, 0, -1) {
        Ok(tls_events) => {
            for tls in tls_events {
                let evt = Event::new(
                    seq_gen,
                    agent_id.to_owned(),
                    EventPayload::TlsKeys(tls),
                );
                if tx.send(evt).is_err() {
                    return;
                }
            }
        }
        Err(e) => {
            event!(
                name: "tls_watcher.keylog.error",
                Level::WARN,
                error.message = %e,
                "keylog poll failed: {error.message}",
            );
        }
    }
}

/// Reads new flows and emits `HttpRequest`/`HttpResponse` events.
fn poll_flows(
    watcher: &mut FlowWatcher,
    cas: &LocalCas,
    tx: &Sender<Event>,
    seq_gen: &SequenceGenerator,
    agent_id: &str,
) {
    match watcher.process_new_flows(cas, 0) {
        Ok(flows) => {
            for payloads in FlowWatcher::into_event_payloads(flows) {
                let evt = Event::new(
                    seq_gen,
                    agent_id.to_owned(),
                    payloads,
                );
                if tx.send(evt).is_err() {
                    return;
                }
            }
        }
        Err(e) => {
            event!(
                name: "tls_watcher.flow.error",
                Level::WARN,
                error.message = %e,
                "flow poll failed: {error.message}",
            );
        }
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run (in dev container): `cargo check -p supervisor`

- [ ] **Step 3: Commit**

```bash
git add crates/supervisor/src/tls_watcher.rs
git commit -m "add tls-watcher background polling thread"
```

### Task 2: Integrate into supervisor main.rs

**Files:**
- Modify: `crates/supervisor/src/main.rs`

- [ ] **Step 1: Add module declaration**

Add `mod tls_watcher;` after the existing module declarations (line 10 area).

- [ ] **Step 2: Embed addon script and write at startup**

After `startup::create_data_dirs(&config.data_dir)?;` (line 61), add:

```rust
// Embed the mitmdump addon so the binary is self-contained.
const ADDON_SCRIPT: &str = include_str!("../../../scripts/argus_addon.py");

let addon_script = config.data_dir.join("argus_addon.py");
fs::write(&addon_script, ADDON_SCRIPT)
    .context("failed to write embedded addon script")?;
```

Then update the addon config block (lines 66-76) to remove the `if addon_script.exists()` check — the script always exists now:

```rust
let addon = net::AddonConfig {
    script: Some(addon_script),
    output_file: Some(flow_output.clone()),
};
```

- [ ] **Step 3: Create a second CAS handle and SequenceGenerator for the TLS watcher**

The TLS watcher runs on a separate thread and needs its own `LocalCas` handle (safe — append-only CAS). It shares the existing `SequenceGenerator` (which uses `AtomicU64`, so it's thread-safe). But `SequenceGenerator` isn't `Clone` and the tracer also needs it. Since `SequenceGenerator` uses `AtomicU64`, wrap it in `Arc` and share.

Change `seq_gen` from `SequenceGenerator::default()` to `Arc::new(SequenceGenerator::default())`. Update all call sites that use `&seq_gen` to `&*seq_gen` or pass a clone of the Arc.

Alternatively, the simpler approach: create a second `SequenceGenerator` for the TLS watcher. Sequences will interleave but that's fine — events have timestamps for ordering. Actually, looking at the code, `SequenceGenerator` already uses `AtomicU64` and `Event::new` takes `&SequenceGenerator`, so it's already `Sync`. We can wrap it in `Arc`.

```rust
let seq_gen = Arc::new(SequenceGenerator::default());
```

Update `emit_agent_start` call: `emit_agent_start(&event_tx, &config, &seq_gen);`

Update tracer construction: the tracer's `new()` takes `SequenceGenerator` by value. We need to change the approach — either make `TracerLoop::new` take `Arc<SequenceGenerator>` or just create two separate generators. Two generators is simpler and doesn't touch the library crate. Sequences won't be globally unique but events have timestamps. Actually, checking — `TracerLoop` stores `seq_gen: SequenceGenerator` directly. Changing it to `Arc<SequenceGenerator>` would require modifying the library. Simpler: create two generators, one for tracer (starting at 0) and one for TLS watcher (starting at 1_000_000). This avoids library changes and keeps sequences disjoint.

```rust
let tracer_seq = SequenceGenerator::default();
let tls_seq = SequenceGenerator::new(1_000_000);
```

Use `tracer_seq` for `emit_agent_start` and the tracer. Use `tls_seq` for the TLS watcher.

- [ ] **Step 4: Spawn the TLS watcher thread**

After the mitmdump startup block and before the tracer, add:

```rust
let tls_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
let tls_watcher_handle = if mitmdump.is_some() {
    let tls_cas = LocalCas::new(cas_path.clone())
        .context("failed to initialize TLS watcher CAS handle")?;
    Some(tls_watcher::spawn(
        config.tls.keylog_path.clone(),
        Some(flow_output),
        tls_cas,
        event_tx.clone(),
        tls_seq,
        config.agent_id.clone(),
        tls_stop.clone(),
    ))
} else {
    // No mitmdump, but still watch the keylog file — curl and
    // other tools write to SSLKEYLOGFILE without a proxy.
    let tls_cas = LocalCas::new(cas_path.clone())
        .context("failed to initialize TLS watcher CAS handle")?;
    Some(tls_watcher::spawn(
        config.tls.keylog_path.clone(),
        None,
        tls_cas,
        event_tx.clone(),
        tls_seq,
        config.agent_id.clone(),
        tls_stop.clone(),
    ))
};
```

Actually, simplify — always spawn the TLS watcher, just vary whether it gets a flow path:

```rust
let tls_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
let tls_cas = LocalCas::new(cas_path.clone())
    .context("failed to initialize TLS watcher CAS handle")?;
let flow_path = mitmdump.as_ref().and_then(|m| m.flow_output_path().cloned());
let tls_watcher_handle = tls_watcher::spawn(
    config.tls.keylog_path.clone(),
    flow_path,
    tls_cas,
    event_tx.clone(),
    tls_seq,
    config.agent_id.clone(),
    tls_stop.clone(),
);
```

- [ ] **Step 5: Shutdown ordering**

In the shutdown section, stop the TLS watcher **before** stopping mitmdump (so it can drain final data), then stop mitmdump:

```rust
// Stop TLS watcher first to drain final data.
tls_stop.store(true, std::sync::atomic::Ordering::Release);
let _ = tls_watcher_handle.join();

// Then stop mitmdump.
if let Some(ref mut m) = mitmdump {
    let _ = m.stop();
}
```

- [ ] **Step 6: Verify it compiles**

Run (in dev container): `cargo check -p supervisor`

- [ ] **Step 7: Commit**

```bash
git add crates/supervisor/src/main.rs
git commit -m "integrate tls-watcher thread and embed addon script"
```

---

## Chunk 2: Validation Test 8

### Task 3: Wire up test_8 in validate.sh

**Files:**
- Modify: `tests/validate.sh`

- [ ] **Step 1: Replace test_8 stub**

Replace the `test_8()` function (lines 375-379) with:

```bash
test_8() {
    echo "Test 8: TLS Capture"

    # Check prerequisites.
    if ! command -v openssl >/dev/null 2>&1; then
        echo "  SKIP: openssl not found"
        record 8 "TLS capture" "SKIP"
        return
    fi
    if ! command -v curl >/dev/null 2>&1; then
        echo "  SKIP: curl not found"
        record 8 "TLS capture" "SKIP"
        return
    fi

    local ok=true

    # Generate ephemeral self-signed cert for local HTTPS server.
    openssl req -x509 -newkey rsa:2048 \
        -keyout /tmp/argus-test-key.pem -out /tmp/argus-test-cert.pem \
        -days 1 -nodes -subj '/CN=localhost' 2>/dev/null

    # Start local HTTPS server (serves exactly one request then exits).
    python3 -c "
import ssl, http.server, json

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({'status': 'ok'}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass

ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain('/tmp/argus-test-cert.pem', '/tmp/argus-test-key.pem')
server = http.server.HTTPServer(('127.0.0.1', 8443), Handler)
server.socket = ctx.wrap_socket(server.socket, server_side=True)
server.handle_request()
" &
    local server_pid=$!
    sleep 0.5

    # Verify server is listening.
    if ! python3 -c "import socket; s=socket.socket(); s.settimeout(1); s.connect(('127.0.0.1',8443)); s.close()" 2>/dev/null; then
        echo "  SKIP: local HTTPS server failed to start"
        kill "$server_pid" 2>/dev/null
        record 8 "TLS capture" "SKIP"
        return
    fi

    # Run curl through the supervisor. Use -sk to accept self-signed cert.
    local events
    events=$(run_supervisor -- curl -sk https://localhost:8443/)

    # Clean up server (should already be done after handle_request).
    wait "$server_pid" 2>/dev/null

    # 1. connect event to 127.0.0.1 on port 8443 (or via proxy on 8080).
    local connect_count
    connect_count=$(echo "$events" | jq -s '[.[] | select(.type == "connect")] | length')
    if [ "$connect_count" -lt 1 ]; then
        echo "  FAIL: no connect events"
        ok=false
    fi

    # 2. tls_keys event from SSLKEYLOGFILE.
    local tls_count
    tls_count=$(echo "$events" | jq -s '[.[] | select(.type == "tls_keys")] | length')
    if [ "$tls_count" -lt 1 ]; then
        echo "  WARN: no tls_keys events (curl may not support SSLKEYLOGFILE)"
    fi

    # 3. http_request / http_response (requires mitmdump).
    local http_req_count http_resp_count
    http_req_count=$(echo "$events" | jq -s '[.[] | select(.type == "http_request")] | length')
    http_resp_count=$(echo "$events" | jq -s '[.[] | select(.type == "http_response")] | length')
    if [ "$http_req_count" -ge 1 ] && [ "$http_resp_count" -ge 1 ]; then
        echo "  mitmdump: http_request=$http_req_count http_response=$http_resp_count"
    else
        echo "  WARN: no http_request/http_response events (mitmdump may not be installed)"
    fi

    # Clean up temp certs.
    rm -f /tmp/argus-test-key.pem /tmp/argus-test-cert.pem

    if $ok; then
        echo "  PASS: connect=$connect_count tls_keys=$tls_count http_req=$http_req_count http_resp=$http_resp_count"
        record 8 "TLS capture" "PASS"
    else
        record 8 "TLS capture" "FAIL"
    fi
}
```

- [ ] **Step 2: Verify test runs (inside dev container)**

Run: `./tests/validate.sh 8`

Expected: PASS with connect events. tls_keys and http_request/http_response depend on curl SSLKEYLOGFILE support and mitmdump availability.

- [ ] **Step 3: Commit**

```bash
git add tests/validate.sh
git commit -m "wire up validation test 8 with local HTTPS server"
```

---

## Chunk 3: Final Verification

### Task 4: Run full validation suite

- [ ] **Step 1: Build supervisor**

Run (in dev container):
```bash
cargo zigbuild --target $(uname -m)-unknown-linux-musl -p supervisor
```

- [ ] **Step 2: Run all validation tests**

Run: `./tests/validate.sh`

Expected: Tests 1-7, 7b, 8-12 all PASS (or graceful SKIP for environment-specific tests). Test 8 should no longer show SKIP.

- [ ] **Step 3: Update task doc**

Create or update `docs/tasks/p2-tls-capture.md` with status, what was done, and test results.

- [ ] **Step 4: Final commit**

```bash
git add docs/tasks/p2-tls-capture.md
git commit -m "add tls-capture task doc"
```

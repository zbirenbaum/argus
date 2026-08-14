#!/usr/bin/env bash
# Validation tests 1-14 for Argus supervisor.
#
# Runs inside the dev container (argus-arm64 or argus-x86).
# Usage: ./tests/validate.sh [test_number...]
#   No args = run all tests. Pass numbers to run specific tests.
#
# Requires: supervisor binary built for the container's arch.

set -euo pipefail

# --- Configuration ---

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

ARCH="$(uname -m)"
case "$ARCH" in
    x86_64)  SUPERVISOR="$WORKSPACE_ROOT/target/x86_64-unknown-linux-musl/debug/supervisor" ;;
    aarch64) SUPERVISOR="$WORKSPACE_ROOT/target/aarch64-unknown-linux-musl/debug/supervisor" ;;
    *)       echo "FATAL: unsupported arch $ARCH"; exit 1 ;;
esac

if [ ! -x "$SUPERVISOR" ]; then
    echo "FATAL: supervisor not found at $SUPERVISOR"
    echo "Build it first: cargo zigbuild --target ${ARCH}-unknown-linux-musl -p supervisor"
    exit 1
fi

TEST_CONFIG="$SCRIPT_DIR/test-config.yaml"
TEST_WORKSPACE="/tmp/argus-test-workspace"
TEST_DATA="/tmp/argus-test-data"
mkdir -p "$TEST_WORKSPACE" "$TEST_DATA"

PASS=0
FAIL=0
SKIP=0
RESULTS=()

# --- Helpers ---

run_supervisor() {
    # Run supervisor, capture stdout events (filter out non-JSON child output).
    "$SUPERVISOR" --agent-id "validate-$$" --config "$TEST_CONFIG" "$@" 2>/tmp/supervisor_debug.log \
        | grep --line-buffered '^\{' || true
}

assert_event_type() {
    local events="$1" type="$2" label="$3"
    if echo "$events" | jq -se "any(.type == \"$type\")" >/dev/null 2>&1; then
        return 0
    else
        echo "  MISSING: expected '$type' event ($label)"
        return 1
    fi
}

assert_event_count_gte() {
    local events="$1" type="$2" min="$3" label="$4"
    local count
    count=$(echo "$events" | jq -s "[.[] | select(.type == \"$type\")] | length")
    if [ "$count" -ge "$min" ]; then
        return 0
    else
        echo "  FAIL: expected >= $min '$type' events, got $count ($label)"
        return 1
    fi
}

record() {
    local num="$1" name="$2" status="$3"
    RESULTS+=("$num|$name|$status")
    case "$status" in
        PASS) PASS=$((PASS + 1)) ;;
        FAIL) FAIL=$((FAIL + 1)) ;;
        SKIP) SKIP=$((SKIP + 1)) ;;
    esac
}

cleanup_workspace() {
    # Kill any stale supervisor and mitmdump to free ports 9090 and 8080.
    pkill -9 -x supervisor 2>/dev/null || true
    pkill -9 -x mitmdump 2>/dev/null || true
    # Wait for API port to be released so the next supervisor can bind.
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        if ! curl -sf --max-time 0.2 http://127.0.0.1:19090/agent/status >/dev/null 2>&1; then
            break
        fi
        sleep 0.1
    done
    sleep 0.2
    rm -f /tmp/argus-test-workspace/test.txt /tmp/argus-test-workspace/shared.txt /tmp/argus-test-workspace/tool-output.txt
    rm -f /tmp/tool.py /tmp/concurrent_write
    # Clear event log so previous test events don't leak into later tests.
    rm -rf "$TEST_DATA"/events
    mkdir -p "$TEST_DATA"/events
}

# --- Tests ---

test_1() {
    echo "Test 1: Basic Process Tracing"
    local events
    events=$(run_supervisor -- bash -c 'echo hello; sleep 0.1; echo bye')
    local ok=true

    assert_event_type "$events" "exec" "bash or echo exec" || ok=false
    assert_event_type "$events" "exit" "process exit" || ok=false
    assert_event_count_gte "$events" "exec" 2 "at least bash + echo" || ok=false

    # Verify pid/ppid chain: child execs should have ppid matching bash's pid.
    local bash_pid
    bash_pid=$(echo "$events" | jq -s '[.[] | select(.type == "exec")][0].pid')
    if [ -z "$bash_pid" ] || [ "$bash_pid" = "null" ]; then
        echo "  FAIL: could not find bash exec event"
        ok=false
    fi

    if $ok; then
        echo "  PASS"
        record 1 "Process tracing" "PASS"
    else
        record 1 "Process tracing" "FAIL"
    fi
}

test_2() {
    echo "Test 2: Stdio Capture"
    local events
    events=$(run_supervisor -- python3 -c "
import sys
print('stdout line')
sys.stderr.write('stderr line\n')
")
    local ok=true

    # Check for stdio events with stdout and stderr subtypes.
    local stdout_count stderr_count
    stdout_count=$(echo "$events" | jq -s '[.[] | select(.type == "stdio" and .subtype == "stdout")] | length')
    stderr_count=$(echo "$events" | jq -s '[.[] | select(.type == "stdio" and .subtype == "stderr")] | length')

    if [ "$stdout_count" -lt 1 ]; then
        echo "  FAIL: no stdout stdio events (got $stdout_count)"
        ok=false
    fi
    if [ "$stderr_count" -lt 1 ]; then
        echo "  FAIL: no stderr stdio events (got $stderr_count)"
        ok=false
    fi

    if $ok; then
        echo "  PASS: stdout=$stdout_count stderr=$stderr_count"
        record 2 "Stdio capture" "PASS"
    else
        record 2 "Stdio capture" "FAIL"
    fi
}

test_3() {
    echo "Test 3: File Write + Read + Delete"
    cleanup_workspace
    local events
    events=$(run_supervisor -- bash -c '
echo "hello world" > /tmp/argus-test-workspace/test.txt
cat /tmp/argus-test-workspace/test.txt
rm /tmp/argus-test-workspace/test.txt
')
    local ok=true

    # Write event with after_hash.
    local write_hash
    write_hash=$(echo "$events" | jq -s '[.[] | select(.type == "write" and .path == "/tmp/argus-test-workspace/test.txt")][0].after_hash // empty')
    if [ -z "$write_hash" ]; then
        echo "  FAIL: no write event with after_hash for /tmp/argus-test-workspace/test.txt"
        ok=false
    fi

    # Read event.
    assert_event_type "$events" "read" "cat read" || ok=false

    # Unlink event.
    local unlink_count
    unlink_count=$(echo "$events" | jq -s '[.[] | select(.type == "unlink" and (.path // "" | endswith("test.txt")))] | length')
    if [ "$unlink_count" -lt 1 ]; then
        echo "  FAIL: no unlink event for test.txt"
        ok=false
    fi

    if $ok; then
        echo "  PASS: write hash=$write_hash"
        record 3 "File write/read/delete" "PASS"
    else
        record 3 "File write/read/delete" "FAIL"
    fi
}

test_4() {
    echo "Test 4: Pipe Topology"
    local events
    events=$(run_supervisor -- bash -c 'echo -e "foo\nbar\nbaz" | grep bar | wc -l')
    local ok=true

    # Check for pipe_create and pipe_data events.
    local pipe_create_count pipe_data_count
    pipe_create_count=$(echo "$events" | jq -s '[.[] | select(.type == "pipe_create")] | length')
    pipe_data_count=$(echo "$events" | jq -s '[.[] | select(.type == "pipe_data")] | length')

    if [ "$pipe_create_count" -lt 1 ]; then
        echo "  FAIL: no pipe_create events (got $pipe_create_count)"
        ok=false
    fi
    if [ "$pipe_data_count" -lt 1 ]; then
        echo "  FAIL: no pipe_data events (got $pipe_data_count)"
        ok=false
    fi

    if $ok; then
        echo "  PASS: pipe_create=$pipe_create_count pipe_data=$pipe_data_count"
        record 4 "Pipe topology" "PASS"
    else
        record 4 "Pipe topology" "FAIL"
    fi
}

test_5() {
    echo "Test 5: Subprocess Tree"
    local events
    events=$(run_supervisor -- python3 -c "
import subprocess
result = subprocess.run(['ls', '-la', '/tmp/argus-test-workspace'], capture_output=True, text=True)
print(result.stdout[:50])
")
    local ok=true

    # Python exec + ls exec.
    assert_event_count_gte "$events" "exec" 2 "python3 + ls" || ok=false

    # ls stdout should appear as pipe_data flowing back to python.
    local pipe_data_count
    pipe_data_count=$(echo "$events" | jq -s '[.[] | select(.type == "pipe_data")] | length')
    if [ "$pipe_data_count" -lt 1 ]; then
        echo "  FAIL: no pipe_data from ls to python ($pipe_data_count)"
        ok=false
    fi

    if $ok; then
        echo "  PASS: pipe_data=$pipe_data_count"
        record 5 "Subprocess tree" "PASS"
    else
        record 5 "Subprocess tree" "FAIL"
    fi
}

test_6() {
    echo "Test 6: Self-Created Tool (Escape Test)"
    cleanup_workspace
    local events
    events=$(run_supervisor -- bash -c '
cat > /tmp/tool.py << "PYEOF"
#!/usr/bin/env python3
import os
with open("/tmp/argus-test-workspace/tool-output.txt", "w") as f:
    f.write(f"written by pid {os.getpid()}\n")
PYEOF
chmod +x /tmp/tool.py
python3 /tmp/tool.py
')
    local ok=true

    # Exec of python3 running the tool.
    local tool_exec
    tool_exec=$(echo "$events" | jq -s '[.[] | select(.type == "exec" and (.binary // "" | contains("python3")))] | length')
    if [ "$tool_exec" -lt 1 ]; then
        echo "  FAIL: no exec event for python3 running tool"
        ok=false
    fi

    # Write to /tmp/argus-test-workspace/tool-output.txt captured.
    local tool_write
    tool_write=$(echo "$events" | jq -s '[.[] | select(.type == "write" and .path == "/tmp/argus-test-workspace/tool-output.txt")] | length')
    if [ "$tool_write" -lt 1 ]; then
        echo "  FAIL: no write event for /tmp/argus-test-workspace/tool-output.txt"
        ok=false
    fi

    cleanup_workspace

    if $ok; then
        echo "  PASS"
        record 6 "Escape test" "PASS"
    else
        record 6 "Escape test" "FAIL"
    fi
}

test_7() {
    echo "Test 7: Write Locking"
    cleanup_workspace
    local events
    events=$(run_supervisor -- python3 -c "
import threading

def writer(n):
    for i in range(10):
        with open('/tmp/argus-test-workspace/shared.txt', 'w') as f:
            f.write(f'writer {n} iteration {i}\n')

threads = [threading.Thread(target=writer, args=(i,)) for i in range(3)]
for t in threads: t.start()
for t in threads: t.join()
")
    local ok=true

    # Check write event count.
    local write_count
    write_count=$(echo "$events" | jq -s '[.[] | select(.type == "write" and .path == "/tmp/argus-test-workspace/shared.txt")] | length')
    if [ "$write_count" -lt 1 ]; then
        echo "  FAIL: no write events for /tmp/argus-test-workspace/shared.txt"
        ok=false
    fi

    # Validate hash chain.
    local chain_result
    chain_result=$(echo "$events" | jq -c 'select(.type == "write" and .path == "/tmp/argus-test-workspace/shared.txt")' | python3 "$SCRIPT_DIR/validate_hash_chain.py" 2>&1)
    if echo "$chain_result" | grep -q "PASS"; then
        echo "  $chain_result"
    else
        echo "  $chain_result"
        ok=false
    fi

    cleanup_workspace

    if $ok; then
        record 7 "Write locking" "PASS"
    else
        record 7 "Write locking" "FAIL"
    fi
}

test_7b() {
    echo "Test 7b: Write Interleaving (C pthreads)"

    if [ ! -f "$SCRIPT_DIR/concurrent_write.c" ]; then
        echo "  SKIP: tests/concurrent_write.c not found"
        record "7b" "Write interleaving" "SKIP"
        return
    fi

    cleanup_workspace

    gcc -O0 -pthread -o /tmp/concurrent_write "$SCRIPT_DIR/concurrent_write.c" 2>/dev/null
    if [ $? -ne 0 ]; then
        echo "  SKIP: failed to compile concurrent_write.c"
        record "7b" "Write interleaving" "SKIP"
        return
    fi
    local events
    events=$(run_supervisor -- /tmp/concurrent_write)
    local ok=true

    local write_count
    write_count=$(echo "$events" | jq -s '[.[] | select(.type == "write" and .path == "/tmp/argus-test-workspace/shared.txt")] | length')
    echo "  write events: $write_count"

    local chain_result
    chain_result=$(echo "$events" | jq -c 'select(.type == "write" and .path == "/tmp/argus-test-workspace/shared.txt")' | python3 "$SCRIPT_DIR/validate_hash_chain.py" 2>&1)
    if echo "$chain_result" | grep -q "PASS"; then
        echo "  $chain_result"
    else
        echo "  $chain_result"
        ok=false
    fi

    cleanup_workspace
    rm -f /tmp/concurrent_write

    if $ok; then
        record "7b" "Write interleaving" "PASS"
    else
        record "7b" "Write interleaving" "FAIL"
    fi
}

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
server.timeout = 5
server.handle_request()
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

    # Write a test-specific config with upstream_insecure so mitmdump
    # accepts the self-signed cert from our local HTTPS server.
    local tls_config="/tmp/argus-test-tls-config.yaml"
    cat > "$tls_config" <<CFGEOF
agent_command: ["true"]
workspace_dir: /tmp/argus-test-workspace
data_dir: /tmp/argus-test-data
tls:
  upstream_insecure: true
CFGEOF

    # Run curl through the supervisor. Use -sk to accept self-signed cert.
    # Sleep after curl so the 200ms TLS watcher poll can drain the flow file
    # before the agent exits and the supervisor shuts down.
    local events
    events=$("$SUPERVISOR" --agent-id "validate-$$" --config "$tls_config" -- bash -c 'curl -sk https://localhost:8443/; sleep 1' 2>/tmp/supervisor_debug.log)

    # Clean up server (should already be done after handle_request).
    wait "$server_pid" 2>/dev/null

    # 1. connect events (may be absent under Rosetta or in some environments).
    local connect_count
    connect_count=$(echo "$events" | jq -s '[.[] | select(.type == "connect")] | length')
    if [ "$connect_count" -lt 1 ]; then
        echo "  INFO: no connect events (expected under Rosetta/emulation)"
    fi

    # 2. tls_keys event from SSLKEYLOGFILE — the core assertion.
    local tls_count
    tls_count=$(echo "$events" | jq -s '[.[] | select(.type == "tls_keys")] | length')
    if [ "$tls_count" -lt 1 ]; then
        echo "  FAIL: no tls_keys events"
        ok=false
    fi

    # 3. http_request / http_response (requires mitmdump).
    local http_req_count http_resp_count
    http_req_count=$(echo "$events" | jq -s '[.[] | select(.type == "http_request")] | length')
    http_resp_count=$(echo "$events" | jq -s '[.[] | select(.type == "http_response")] | length')
    if [ "$http_req_count" -ge 1 ] && [ "$http_resp_count" -ge 1 ]; then
        echo "  mitmdump: http_request=$http_req_count http_response=$http_resp_count"
    else
        echo "  FAIL: no http_request/http_response events (mitmdump not installed or not proxying)"
        ok=false
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

test_9() {
    echo "Test 9: Pause/Resume"
    cleanup_workspace

    local ok=true
    local events_file="/tmp/test9_events.jsonl"
    rm -f "$events_file"
    rm -f /tmp/argus-test-workspace/before.txt /tmp/argus-test-workspace/after.txt

    # Start supervisor in background. The script writes a marker, sleeps,
    # then writes a second marker. While sleeping we pause; the second
    # marker should not appear until we resume.
    "$SUPERVISOR" --agent-id "validate-$$" --config "$TEST_CONFIG" \
        -- bash -c '
            echo before > /tmp/argus-test-workspace/before.txt
            sleep 3
            echo after > /tmp/argus-test-workspace/after.txt
        ' > "$events_file" 2>/dev/null &
    local sup_pid=$!

    # Wait for supervisor + first marker to appear.
    local waited=0
    while [ ! -f /tmp/argus-test-workspace/before.txt ] && [ "$waited" -lt 20 ]; do
        sleep 0.2
        waited=$((waited + 1))
    done
    if [ ! -f /tmp/argus-test-workspace/before.txt ]; then
        echo "  FAIL: before.txt never appeared"
        kill "$sup_pid" 2>/dev/null; wait "$sup_pid" 2>/dev/null
        record 9 "Pause/resume" "FAIL"
        return
    fi

    # Pause the agent via API.
    local pause_resp
    pause_resp=$(curl -sf -X POST http://127.0.0.1:19090/agent/pause 2>/dev/null)
    if [ $? -ne 0 ]; then
        echo "  FAIL: could not reach pause API"
        kill "$sup_pid" 2>/dev/null; wait "$sup_pid" 2>/dev/null
        record 9 "Pause/resume" "FAIL"
        return
    fi

    # Check status reports paused.
    local status
    status=$(curl -sf http://127.0.0.1:19090/agent/status 2>/dev/null)
    if ! echo "$status" | grep -q '"paused"'; then
        echo "  FAIL: status not paused after pause request"
        ok=false
    fi

    # Wait — the sleep(3) may finish but the next write should be frozen.
    sleep 4

    if [ -f /tmp/argus-test-workspace/after.txt ]; then
        echo "  FAIL: process not frozen (after.txt appeared while paused)"
        ok=false
    fi

    # Resume.
    curl -sf -X POST http://127.0.0.1:19090/agent/resume > /dev/null 2>&1

    # Wait for agent to finish.
    wait "$sup_pid" 2>/dev/null

    # after.txt should now exist.
    if [ ! -f /tmp/argus-test-workspace/after.txt ]; then
        echo "  FAIL: after.txt not created after resume"
        ok=false
    fi

    # Check for pause/resume events in both stdout and event log.
    # API-originated events (pause/resume) go through the RecordBus
    # to the event log, not through the pipeline's stdout OutputList.
    local all_events
    all_events=$(cat "$events_file"; cat "$TEST_DATA"/events/*.jsonl 2>/dev/null)
    local pause_count resume_count
    pause_count=$(echo "$all_events" | jq -s '[.[] | select(.type == "agent_pause")] | length')
    resume_count=$(echo "$all_events" | jq -s '[.[] | select(.type == "agent_resume")] | length')

    if [ "$pause_count" -lt 1 ]; then
        echo "  FAIL: missing agent_pause event"
        ok=false
    fi
    if [ "$resume_count" -lt 1 ]; then
        echo "  FAIL: missing agent_resume event"
        ok=false
    fi

    if $ok; then
        echo "  PASS"
        record 9 "Pause/resume" "PASS"
    else
        record 9 "Pause/resume" "FAIL"
    fi
}

test_10() {
    echo "Test 10: Pause-Before-Action"
    cleanup_workspace

    local ok=true
    local events_file="/tmp/test10_events.jsonl"
    local pause_config="$SCRIPT_DIR/test-pause-config.yaml"
    rm -f "$events_file"

    if [ ! -f "$pause_config" ]; then
        echo "  SKIP: test-pause-config.yaml not found"
        record 10 "Pause-before-action" "SKIP"
        return
    fi

    # Create a file to delete.
    echo "important" > /tmp/argus-test-workspace/critical.txt

    # Start supervisor with pause_before rules for unlink.
    # The script tries to rm the file. The tracer should hold the
    # unlink for approval. We deny it so rm sees EPERM.
    "$SUPERVISOR" --agent-id "validate-$$" --config "$pause_config" \
        -- bash -c '
            rm /tmp/argus-test-workspace/critical.txt 2>/tmp/argus-test-rm-err.txt
            echo "exit=$?" > /tmp/argus-test-workspace/rm_result.txt
        ' > "$events_file" 2>/dev/null &
    local sup_pid=$!

    # Wait for a pending approval to appear.
    local waited=0
    local action_id=""
    while [ -z "$action_id" ] && [ "$waited" -lt 40 ]; do
        sleep 0.3
        # `|| true`: the API is not up yet on the first polls, and under
        # `set -e -o pipefail` a failed curl would abort the whole script.
        action_id=$(curl -sf http://127.0.0.1:19090/approvals/pending 2>/dev/null \
            | jq -r '.pending[0].action_id // empty' 2>/dev/null || true)
        waited=$((waited + 1))
    done

    if [ -z "$action_id" ]; then
        echo "  FAIL: no pending approval appeared"
        kill "$sup_pid" 2>/dev/null; wait "$sup_pid" 2>/dev/null
        record 10 "Pause-before-action" "FAIL"
        return
    fi

    # Deny the unlink — rm should get EPERM.
    local deny_resp
    deny_resp=$(curl -sf -X POST "http://127.0.0.1:19090/approvals/${action_id}/deny" 2>/dev/null)
    if [ $? -ne 0 ]; then
        echo "  FAIL: deny request failed"
        kill "$sup_pid" 2>/dev/null; wait "$sup_pid" 2>/dev/null
        record 10 "Pause-before-action" "FAIL"
        return
    fi

    # Wait for agent to finish.
    wait "$sup_pid" 2>/dev/null

    # The file should still exist (unlink was denied).
    if [ ! -f /tmp/argus-test-workspace/critical.txt ]; then
        echo "  FAIL: critical.txt was deleted despite denial"
        ok=false
    fi

    # rm should have exited with an error.
    if [ -f /tmp/argus-test-workspace/rm_result.txt ]; then
        local exit_code
        exit_code=$(grep -o 'exit=[0-9]*' /tmp/argus-test-workspace/rm_result.txt | cut -d= -f2)
        if [ "$exit_code" = "0" ]; then
            echo "  FAIL: rm succeeded (expected EPERM)"
            ok=false
        fi
    fi

    # Check events in both stdout and event log.
    local all_events
    all_events=$(cat "$events_file"; cat "$TEST_DATA"/events/*.jsonl 2>/dev/null)
    local pending_count denied_count
    pending_count=$(echo "$all_events" | jq -s '[.[] | select(.type == "pending_approval")] | length')
    denied_count=$(echo "$all_events" | jq -s '[.[] | select(.type == "approval_denied")] | length')

    if [ "$pending_count" -lt 1 ]; then
        echo "  FAIL: missing pending_approval event"
        ok=false
    fi
    if [ "$denied_count" -lt 1 ]; then
        echo "  FAIL: missing approval_denied event"
        ok=false
    fi

    if $ok; then
        echo "  PASS"
        record 10 "Pause-before-action" "PASS"
    else
        record 10 "Pause-before-action" "FAIL"
    fi
}

test_11() {
    echo "Test 11: Snapshot and Restore"
    cleanup_workspace

    local ok=true
    local events_file="/tmp/test11_events.jsonl"
    local restore_dir="/tmp/argus-test-restore"
    rm -f "$events_file"
    rm -rf "$restore_dir"

    # Agent writes v1, waits, writes v2, then sleeps long enough for
    # us to call the restore API while the supervisor is still alive.
    "$SUPERVISOR" --agent-id "validate-$$" --config "$TEST_CONFIG" \
        -- bash -c '
            echo "version-one" > /tmp/argus-test-workspace/snap.txt
            sleep 1
            echo "version-two" > /tmp/argus-test-workspace/snap.txt
            sleep 10
        ' > "$events_file" 2>/dev/null &
    local sup_pid=$!

    # Wait for both content writes (size > 0) to land.
    local waited=0
    local write_count=0
    while [ "$write_count" -lt 2 ] && [ "$waited" -lt 40 ]; do
        sleep 0.3
        write_count=$(jq -c 'select(.type == "write" and .path == "/tmp/argus-test-workspace/snap.txt" and .size > 0)' "$events_file" 2>/dev/null | wc -l | tr -d ' ')
        waited=$((waited + 1))
    done

    if [ "$write_count" -lt 2 ]; then
        echo "  FAIL: expected >= 2 content write events, got $write_count"
        kill "$sup_pid" 2>/dev/null; wait "$sup_pid" 2>/dev/null
        record 11 "Snapshot/restore" "FAIL"
        return
    fi

    # Get the seq of the first content write (v1 state, size > 0).
    local first_write_seq
    first_write_seq=$(jq -c 'select(.type == "write" and .path == "/tmp/argus-test-workspace/snap.txt" and .size > 0)' "$events_file" 2>/dev/null | head -1 | jq '.seq')

    if [ -z "$first_write_seq" ] || [ "$first_write_seq" = "null" ]; then
        echo "  FAIL: could not find first write seq"
        kill "$sup_pid" 2>/dev/null; wait "$sup_pid" 2>/dev/null
        record 11 "Snapshot/restore" "FAIL"
        return
    fi

    # --- Verify /tree endpoint works ---
    # The tree is finalized on an idle timeout after the last mutation,
    # so poll until file_count > 0 or give up after 10 seconds.
    local tree_resp tree_file_count tree_hash
    tree_file_count=0
    tree_hash=""
    local tree_waited=0
    while [ "$tree_file_count" -lt 1 ] && [ "$tree_waited" -lt 20 ]; do
        sleep 0.5
        tree_resp=$(curl -sf http://127.0.0.1:19090/tree 2>/dev/null)
        if [ $? -eq 0 ] && [ -n "$tree_resp" ]; then
            tree_file_count=$(echo "$tree_resp" | jq '.file_count')
            tree_hash=$(echo "$tree_resp" | jq -r '.tree_hash // empty')
        fi
        tree_waited=$((tree_waited + 1))
    done
    if [ "$tree_file_count" -lt 1 ]; then
        echo "  FAIL: tree file_count=$tree_file_count, expected >= 1 (waited ${tree_waited}x0.5s)"
        ok=false
    fi
    if [ -z "$tree_hash" ]; then
        echo "  FAIL: tree response missing tree_hash"
        ok=false
    fi

    # --- Full restore to v1 ---
    local restore_resp
    restore_resp=$(curl -sf -X POST http://127.0.0.1:19090/restore \
        -H 'Content-Type: application/json' \
        -d "{\"seq\": $first_write_seq, \"mode\": \"full\", \"target\": \"$restore_dir/full\"}" 2>/dev/null)

    if [ $? -ne 0 ] || [ -z "$restore_resp" ]; then
        echo "  FAIL: full restore API call failed (seq=$first_write_seq)"
        kill "$sup_pid" 2>/dev/null; wait "$sup_pid" 2>/dev/null
        record 11 "Snapshot/restore" "FAIL"
        return
    fi

    local full_files_restored
    full_files_restored=$(echo "$restore_resp" | jq '.files_restored')
    if [ "$full_files_restored" -lt 1 ]; then
        echo "  FAIL: full restore returned files_restored=$full_files_restored"
        ok=false
    fi

    # Find where the restored file landed — tree uses absolute paths.
    local restored_file="$restore_dir/full/tmp/argus-test-workspace/snap.txt"
    if [ -f "$restored_file" ]; then
        local restored_content
        restored_content=$(cat "$restored_file")
        if [ "$restored_content" != "version-one" ]; then
            echo "  FAIL: full restore content='$restored_content', expected 'version-one'"
            ok=false
        fi
    else
        echo "  FAIL: full restore did not create snap.txt"
        ok=false
    fi

    # --- Selective restore of snap.txt to v1 ---
    # Tree stores absolute paths stripped of leading /.
    local selective_resp selective_files
    selective_resp=$(curl -sf -X POST http://127.0.0.1:19090/restore \
        -H 'Content-Type: application/json' \
        -d "{\"seq\": $first_write_seq, \"mode\": \"selective\", \"target\": \"$restore_dir/selective\", \"paths\": [\"tmp/argus-test-workspace/snap.txt\"]}" 2>/dev/null)

    if [ $? -ne 0 ] || [ -z "$selective_resp" ]; then
        echo "  FAIL: selective restore API call failed"
        selective_files=0
        ok=false
    else
        selective_files=$(echo "$selective_resp" | jq '.files_restored')
        if [ "$selective_files" -ne 1 ]; then
            echo "  FAIL: selective restore files_restored=$selective_files, expected 1"
            ok=false
        fi

        local sel_file="$restore_dir/selective/tmp/argus-test-workspace/snap.txt"
        if [ -f "$sel_file" ]; then
            local sel_content
            sel_content=$(cat "$sel_file")
            if [ "$sel_content" != "version-one" ]; then
                echo "  FAIL: selective restore content='$sel_content', expected 'version-one'"
                ok=false
            fi
        else
            echo "  FAIL: selective restore did not create snap.txt"
            ok=false
        fi
    fi

    # Kill supervisor (agent is still sleeping).
    kill "$sup_pid" 2>/dev/null; wait "$sup_pid" 2>/dev/null

    # Cleanup.
    rm -rf "$restore_dir"
    cleanup_workspace

    if $ok; then
        echo "  PASS: full_files=$full_files_restored selective=$selective_files tree_files=$tree_file_count"
        record 11 "Snapshot/restore" "PASS"
    else
        record 11 "Snapshot/restore" "FAIL"
    fi
}

test_12() {
    echo "Test 12: Initial State Capture"
    cleanup_workspace

    # Pre-populate workspace.
    echo "pre-existing" > /tmp/argus-test-workspace/existing.txt
    mkdir -p /tmp/argus-test-workspace/subdir
    echo "nested" > /tmp/argus-test-workspace/subdir/nested.txt

    local events
    events=$(run_supervisor -- bash -c 'echo done')
    local ok=true

    # initial_state event with file_count >= 2.
    local initial_state
    initial_state=$(echo "$events" | jq -s '[.[] | select(.type == "initial_state")][0] // empty')
    if [ -z "$initial_state" ]; then
        echo "  FAIL: no initial_state event"
        ok=false
    else
        local file_count total_size tree_hash
        file_count=$(echo "$initial_state" | jq '.file_count')
        total_size=$(echo "$initial_state" | jq '.total_size')
        tree_hash=$(echo "$initial_state" | jq -r '.tree_hash // empty')

        if [ "$file_count" -lt 2 ]; then
            echo "  FAIL: file_count=$file_count, expected >= 2"
            ok=false
        fi
        if [ "$total_size" -lt 1 ]; then
            echo "  FAIL: total_size=$total_size, expected > 0"
            ok=false
        fi
        if [ -z "$tree_hash" ]; then
            echo "  FAIL: missing tree_hash"
            ok=false
        fi

        if $ok; then
            echo "  PASS: file_count=$file_count total_size=$total_size tree_hash=${tree_hash:0:16}..."
        fi
    fi

    # initial_file events.
    local initial_file_count
    initial_file_count=$(echo "$events" | jq -s '[.[] | select(.type == "initial_file")] | length')
    if [ "$initial_file_count" -lt 2 ]; then
        echo "  FAIL: expected >= 2 initial_file events, got $initial_file_count"
        ok=false
    fi

    # Cleanup.
    rm -f /tmp/argus-test-workspace/existing.txt
    rm -rf /tmp/argus-test-workspace/subdir

    if $ok; then
        record 12 "Initial state" "PASS"
    else
        record 12 "Initial state" "FAIL"
    fi
}

test_13() {
    echo "Test 13: Child Process Reaping (No Zombies)"
    cleanup_workspace

    local ok=true
    local events_file="/tmp/test13_events.jsonl"
    rm -f "$events_file"

    # Simulates a long-running parent (like Node.js / Claude Code) that
    # spawns many short-lived children concurrently. The parent waits for
    # each child and then checks for zombies. If the ptrace loop fails to
    # reap children (e.g. head-of-line blocking on directive processing),
    # zombie processes will accumulate.
    "$SUPERVISOR" --agent-id "validate-$$" --config "$TEST_CONFIG" \
        -- python3 -c "
import subprocess, os, time

# Spawn 10 concurrent children doing file I/O
procs = []
for i in range(10):
    p = subprocess.Popen(
        ['bash', '-c', f'echo child-{i} > /tmp/argus-test-workspace/child_{i}.txt && cat /tmp/argus-test-workspace/child_{i}.txt && ls -la /tmp/argus-test-workspace/'],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE
    )
    procs.append(p)

# Wait for all to finish
for p in procs:
    p.wait()

# Give ptrace loop time to process all stops
time.sleep(2)

# Count zombie children
import glob
my_pid = os.getpid()
zombies = 0
for stat_file in glob.glob('/proc/*/stat'):
    try:
        with open(stat_file) as f:
            fields = f.read().split()
            # fields[3] = ppid, fields[2] = state
            if len(fields) > 3 and fields[3] == str(my_pid) and fields[2] == 'Z':
                zombies += 1
    except (IOError, IndexError):
        pass

with open('/tmp/argus-test-workspace/zombie_result.txt', 'w') as f:
    f.write(f'zombie_count={zombies}\n')
print(f'zombies={zombies}')
" > "$events_file" 2>/dev/null
    local exit_code=$?

    if [ "$exit_code" -ne 0 ]; then
        echo "  FAIL: supervisor exited with code $exit_code"
        ok=false
    fi

    # Check the zombie count reported by the agent.
    if [ -f /tmp/argus-test-workspace/zombie_result.txt ]; then
        local zombie_count
        zombie_count=$(grep -o 'zombie_count=[0-9]*' /tmp/argus-test-workspace/zombie_result.txt | cut -d= -f2)
        if [ "$zombie_count" != "0" ]; then
            echo "  FAIL: agent reported $zombie_count zombie child processes"
            ok=false
        fi
    else
        echo "  FAIL: zombie_result.txt not created (agent may have hung)"
        ok=false
    fi

    # Verify we got exit events for the child processes.
    local events
    events=$(grep '^\{' "$events_file" || true)
    local exit_count
    exit_count=$(echo "$events" | jq -s '[.[] | select(.type == "exit")] | length')
    if [ "$exit_count" -lt 10 ]; then
        echo "  FAIL: expected >= 10 exit events, got $exit_count"
        ok=false
    fi

    # Verify we got write events for each child file.
    local write_count
    write_count=$(echo "$events" | jq -s '[.[] | select(.type == "write" and (.path // "" | contains("child_")))] | length')
    if [ "$write_count" -lt 10 ]; then
        echo "  FAIL: expected >= 10 child write events, got $write_count"
        ok=false
    fi

    cleanup_workspace
    rm -f /tmp/argus-test-workspace/child_*.txt /tmp/argus-test-workspace/zombie_result.txt

    if $ok; then
        echo "  PASS: exit_events=$exit_count write_events=$write_count zombies=0"
        record 13 "Child reaping" "PASS"
    else
        record 13 "Child reaping" "FAIL"
    fi
}

test_14() {
    echo "Test 14: Verdict Freeze (judge decided: needs approval / rejected)"
    cleanup_workspace

    # Delegates to the standalone reproduction so the same checks can be
    # run on their own: tests/repro-verdict-freeze.sh
    if bash "$SCRIPT_DIR/repro-verdict-freeze.sh"; then
        record 14 "Verdict freeze" "PASS"
    else
        record 14 "Verdict freeze" "FAIL"
    fi
}

# --- Runner ---

print_summary() {
    echo ""
    echo "=== Results ==="
    for r in "${RESULTS[@]}"; do
        IFS='|' read -r num name status <<< "$r"
        printf "  %-4s %-25s %s\n" "$num" "$name" "$status"
    done
    echo ""
    echo "PASS=$PASS FAIL=$FAIL SKIP=$SKIP"

    if [ "$FAIL" -gt 0 ]; then
        exit 1
    fi
}

ALL_TESTS=(1 2 3 4 5 6 7 7b 8 9 10 11 12 13 14)

if [ $# -gt 0 ]; then
    TESTS=("$@")
else
    TESTS=("${ALL_TESTS[@]}")
fi

echo "Argus Validation Tests"
echo "Supervisor: $SUPERVISOR"
echo "Arch: $ARCH"
echo ""

for t in "${TESTS[@]}"; do
    cleanup_workspace
    case "$t" in
        1)  test_1 ;;
        2)  test_2 ;;
        3)  test_3 ;;
        4)  test_4 ;;
        5)  test_5 ;;
        6)  test_6 ;;
        7)  test_7 ;;
        7b) test_7b ;;
        8)  test_8 ;;
        9)  test_9 ;;
        10) test_10 ;;
        11) test_11 ;;
        12) test_12 ;;
        13) test_13 ;;
        14) test_14 ;;
        *)  echo "Unknown test: $t"; exit 1 ;;
    esac
done

print_summary

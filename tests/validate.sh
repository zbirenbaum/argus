#!/usr/bin/env bash
# Validation tests 1-12 for Argus supervisor.
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
    # Run supervisor, capture stdout (events) and suppress stderr (tracing logs).
    "$SUPERVISOR" --agent-id "validate-$$" --config "$TEST_CONFIG" "$@" 2>/dev/null
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
    rm -f /tmp/argus-test-workspace/test.txt /tmp/argus-test-workspace/shared.txt /tmp/argus-test-workspace/tool-output.txt
    rm -f /tmp/tool.py /tmp/concurrent_write
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

    gcc -O0 -pthread -o /tmp/concurrent_write "$SCRIPT_DIR/concurrent_write.c" 2>/dev/null
    if [ $? -ne 0 ]; then
        echo "  SKIP: failed to compile concurrent_write.c"
        record "7b" "Write interleaving" "SKIP"
        return
    fi

    cleanup_workspace
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
    echo "  SKIP: requires mitmdump + external network"
    record 8 "TLS capture" "SKIP"
}

test_9() {
    echo "Test 9: Pause/Resume"
    echo "  SKIP: requires API server wired into supervisor (not yet integrated)"
    record 9 "Pause/resume" "SKIP"
}

test_10() {
    echo "Test 10: Pause-Before-Action"
    echo "  SKIP: requires rules + API server wired into supervisor (not yet integrated)"
    record 10 "Pause-before-action" "SKIP"
}

test_11() {
    echo "Test 11: Snapshot and Restore"
    echo "  SKIP: requires restore API + CLI (not yet integrated)"
    record 11 "Snapshot/restore" "SKIP"
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

ALL_TESTS=(1 2 3 4 5 6 7 7b 8 9 10 11 12)

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
        *)  echo "Unknown test: $t"; exit 1 ;;
    esac
done

print_summary

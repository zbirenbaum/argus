#!/usr/bin/env bash
# Reproduction: a verdict must stop the supervised agent.
#
# Assumes the judges have already decided — the two outcomes that reach
# the ptrace loop are "needs approval" (escalated to the human backstop)
# and "rejected" (deny). Per docs/spec/06-agent-controls.md the agent
# must be *stopped* in both cases, and a rejected syscall must come back
# as EPERM.
#
# Checks (each prints PASS/FAIL):
#   1. needs approval  → every traced process is stopped, zero CPU
#   2. GET  /agent/status  lists the traced processes
#   3. POST /agent/pause   returns only once all processes are stopped,
#                          and reports which ones
#   4. rejected        → the syscall returns EPERM and the file survives
#   5. approved        → the syscall proceeds
#   6. pause of a freely running agent stops it and resume releases it
#
# Before the freeze wiring landed this reported nine violations: siblings
# kept burning CPU while a verdict was outstanding, pause returned an
# empty list without stopping anything, status listed no processes, and a
# denied unlink came back as ENOTTY instead of EPERM.
#
# Usage: tests/repro-verdict-freeze.sh    (inside the argus-arm64 container)

set -uo pipefail

# Cleanup SIGKILLs backgrounded supervisors, and the shell narrates every
# one of those as a multi-line "Killed <command>" notice. Results go to
# stdout; park the shell's own chatter in a log instead.
set +m
exec 2>/tmp/repro-verdict-freeze.err

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ARCH="$(uname -m)"
SUPERVISOR="$WORKSPACE_ROOT/target/${ARCH}-unknown-linux-musl/debug/supervisor"
CONFIG="$SCRIPT_DIR/test-pause-config.yaml"
API="http://127.0.0.1:19090"
WS=/tmp/argus-test-workspace
DATA=/tmp/argus-test-data

if [ ! -x "$SUPERVISOR" ]; then
    echo "FATAL: supervisor not found at $SUPERVISOR"
    exit 1
fi

FAILURES=0

# The sibling below is deliberately CPU-bound: a process that never traps
# is the only way to tell a real freeze from "held at the next syscall".
# Bound it by iteration count rather than time so it does no syscalls, and
# so a leaked one dies on its own — killing the supervisor does not kill
# its tracees, they are only detached.
SPIN_ITERATIONS=20000000

# Marker in the agent's command line so cleanup can find its processes.
SPIN_MARKER="argus-repro-spinner"

# Tracees survive the supervisor — ptrace detaches, it does not kill —
# so cleanup has to target them directly or a spinner outlives the run.
cleanup_agents() {
    pkill -9 -f "$SPIN_MARKER" 2>/dev/null
    for p in $(pgrep -x supervisor 2>/dev/null); do kill -9 "$p" 2>/dev/null; done
    pkill -9 -x mitmdump 2>/dev/null
    return 0
}
trap cleanup_agents EXIT INT TERM

check() { # check <description> <condition-result:0|1> [detail]
    if [ "$2" -eq 0 ]; then
        echo "  PASS: $1"
    else
        echo "  FAIL: $1${3:+ — $3}"
        FAILURES=$((FAILURES + 1))
    fi
}

proc_state() { awk '{print $3}' "/proc/$1/stat" 2>/dev/null || echo "gone"; }
cpu_ticks()  { awk '{print $14+$15}' "/proc/$1/stat" 2>/dev/null || echo 0; }

reset_env() {
    cleanup_agents
    sleep 0.3
    rm -rf "$WS" "$DATA/events"
    mkdir -p "$WS" "$DATA/events"
    rm -f /tmp/repro-spin.pid /tmp/repro-rm-err.txt
}

# Start an agent that keeps a CPU-bound sibling running while the main
# shell trips the pause-before-action rule on unlink. The sibling only
# does arithmetic, so nothing traps it — the supervisor has to stop it
# deliberately.
start_agent() {
    "$SUPERVISOR" --agent-id repro-verdict --config "$CONFIG" -- bash -c "
        : $SPIN_MARKER
        ( for ((n = 0; n < $SPIN_ITERATIONS; n++)); do :; done ) &
        echo \$! > /tmp/repro-spin.pid
        sleep 0.5
        rm $WS/critical.txt 2>/tmp/repro-rm-err.txt
        echo \"exit=\$?\" > $WS/rm_result.txt
    " > /tmp/repro-events.jsonl 2>/tmp/repro-debug.log &
    AGENT_SUP=$!
}

# Poll until an approval is pending; echoes the action id (empty on timeout).
await_pending() {
    local id="" i
    for i in $(seq 1 60); do
        sleep 0.3
        id=$(curl -sf "$API/approvals/pending" 2>/dev/null \
            | jq -r '.pending[0].action_id // empty' 2>/dev/null || true)
        [ -n "$id" ] && break
    done
    echo "$id"
}

echo "Reproduction: verdict must stop the supervised agent"

# ── Phase 1: needs approval / rejected ───────────────────────────────
reset_env
echo important > "$WS/critical.txt"
start_agent

ACTION_ID=$(await_pending)
if [ -z "$ACTION_ID" ]; then
    echo "  FAIL: no approval was ever queued — the rule never fired"
    kill -9 "$AGENT_SUP" 2>/dev/null
    exit 1
fi
echo "  (verdict pending: action_id=$ACTION_ID)"

SPIN=$(cat /tmp/repro-spin.pid 2>/dev/null || echo 0)
SPIN_BEFORE=$(cpu_ticks "$SPIN")
sleep 1.5
SPIN_AFTER=$(cpu_ticks "$SPIN")
SPIN_STATE=$(proc_state "$SPIN")

[ "$SPIN_STATE" = "t" ] || [ "$SPIN_STATE" = "T" ]
check "sibling process is stopped while a verdict is pending" $? "state=$SPIN_STATE"

[ "$SPIN_BEFORE" = "$SPIN_AFTER" ]
check "stopped sibling consumes zero CPU" $? "ticks $SPIN_BEFORE -> $SPIN_AFTER"

STATUS=$(curl -sf "$API/agent/status" 2>/dev/null || echo '{}')
PROC_COUNT=$(echo "$STATUS" | jq '.processes | length' 2>/dev/null || echo 0)
[ "${PROC_COUNT:-0}" -ge 2 ]
check "GET /agent/status lists the traced processes" $? "processes=$PROC_COUNT"

RUNNING=$(echo "$STATUS" | jq '[.processes[] | select(.state == "running")] | length' 2>/dev/null || echo 1)
[ "${PROC_COUNT:-0}" -ge 2 ] && [ "${RUNNING:-1}" -eq 0 ]
check "no traced process is running while a verdict is pending" $? "running=$RUNNING of $PROC_COUNT"

PAUSE=$(curl -sf -X POST "$API/agent/pause" 2>/dev/null || echo '{}')
STOPPED_COUNT=$(echo "$PAUSE" | jq '.stopped_processes | length' 2>/dev/null || echo 0)
[ "${STOPPED_COUNT:-0}" -ge 2 ]
check "POST /agent/pause reports the processes it stopped" $? "stopped=$STOPPED_COUNT"

ALL_STOPPED=0
[ "${STOPPED_COUNT:-0}" -ge 2 ] || ALL_STOPPED=1
for pid in $(echo "$PAUSE" | jq -r '.stopped_processes[].pid' 2>/dev/null); do
    st=$(proc_state "$pid")
    [ "$st" = "t" ] || [ "$st" = "T" ] || ALL_STOPPED=1
done
check "every process reported by pause is really stopped" $ALL_STOPPED
curl -sf -X POST "$API/agent/resume" >/dev/null 2>&1 || true

curl -sf -X POST "$API/approvals/$ACTION_ID/deny" >/dev/null 2>&1
DENY_RC=$?
check "deny request accepted" $DENY_RC

for _ in $(seq 1 40); do
    [ -f "$WS/rm_result.txt" ] && break
    sleep 0.25
done

[ -f "$WS/critical.txt" ]
check "rejected unlink did not delete the file" $?

RM_ERR=$(cat /tmp/repro-rm-err.txt 2>/dev/null)
echo "$RM_ERR" | grep -q "Operation not permitted"
check "rejected syscall returns EPERM" $? "got: ${RM_ERR:-<empty>}"

EVENTS=$(cat /tmp/repro-events.jsonl "$DATA"/events/*.jsonl 2>/dev/null)
DENIED=$(echo "$EVENTS" | jq -s '[.[] | select(.type == "approval_denied")] | length' 2>/dev/null || echo 0)
[ "${DENIED:-0}" -ge 1 ]
check "approval_denied event recorded" $? "count=$DENIED"

cleanup_agents
wait "$AGENT_SUP" 2>/dev/null || true

# ── Phase 2: approved ────────────────────────────────────────────────
reset_env
echo important > "$WS/critical.txt"
start_agent

ACTION_ID=$(await_pending)
if [ -z "$ACTION_ID" ]; then
    echo "  FAIL: no approval was queued in the approve phase"
    FAILURES=$((FAILURES + 1))
else
    curl -sf -X POST "$API/approvals/$ACTION_ID/approve" >/dev/null 2>&1
    for _ in $(seq 1 40); do
        [ -f "$WS/rm_result.txt" ] && break
        sleep 0.25
    done
    [ ! -f "$WS/critical.txt" ]
    check "approved unlink proceeds" $?
fi

cleanup_agents
wait "$AGENT_SUP" 2>/dev/null || true

# ── Phase 3: pause with no verdict outstanding ───────────────────────
# Nothing is trapped at this moment, so the ptrace thread is parked in
# waitpid — the freeze has to interrupt it rather than wait for the next
# syscall.
reset_env
echo important > "$WS/critical.txt"
"$SUPERVISOR" --agent-id repro-pause --config "$CONFIG" -- bash -c "
    : $SPIN_MARKER
    ( for ((n = 0; n < $SPIN_ITERATIONS; n++)); do :; done ) &
    echo \$! > /tmp/repro-spin.pid
    sleep 30
" > /tmp/repro-pause-events.jsonl 2>/tmp/repro-pause-debug.log &
PAUSE_SUP=$!

# Wait for the agent to be up and its spinner to be running.
for _ in $(seq 1 100); do
    sleep 0.2
    [ -s /tmp/repro-spin.pid ] && break
done
SPIN=$(cat /tmp/repro-spin.pid 2>/dev/null || echo 0)
sleep 0.5

PAUSE=$(curl -sf -X POST "$API/agent/pause" 2>/dev/null || echo '{}')
SPIN_STATE=$(proc_state "$SPIN")
[ "$SPIN_STATE" = "t" ] || [ "$SPIN_STATE" = "T" ]
check "pause stops a running agent that is not trapped in a syscall" $? "state=$SPIN_STATE"

BEFORE=$(cpu_ticks "$SPIN")
sleep 1
AFTER=$(cpu_ticks "$SPIN")
[ "$BEFORE" = "$AFTER" ]
check "paused agent consumes zero CPU" $? "ticks $BEFORE -> $AFTER"

PAUSED_STATUS=$(curl -sf "$API/agent/status" 2>/dev/null || echo '{}')
[ "$(echo "$PAUSED_STATUS" | jq -r '.status' 2>/dev/null)" = "paused" ]
check "GET /agent/status reports paused" $? "status=$(echo "$PAUSED_STATUS" | jq -r '.status' 2>/dev/null)"

curl -sf -X POST "$API/agent/resume" >/dev/null 2>&1
sleep 1
RESUMED=$(cpu_ticks "$SPIN")
sleep 1
[ "$RESUMED" != "$(cpu_ticks "$SPIN")" ]
check "resume lets the agent run again" $?

cleanup_agents
wait "$PAUSE_SUP" 2>/dev/null || true

echo
if [ "$FAILURES" -eq 0 ]; then
    echo "REPRODUCTION PASSES: the agent stops on every verdict"
    exit 0
fi
echo "REPRODUCTION FAILS: $FAILURES spec violation(s)"
exit 1

# Testing & Validation

## Minimal Infrastructure

Docker with SYS_PTRACE. No K8s, no S3. Supervisor runs in `local-only` storage mode.

**devcontainer.json:**

```json
{
  "name": "argus-dev",
  "image": "ubuntu:24.04",
  "capAdd": ["SYS_PTRACE"],
  "securityOpt": ["seccomp=unconfined"],
  "mounts": [
    "source=${localWorkspaceFolder},target=/workspace,type=bind"
  ],
  "postCreateCommand": "apt-get update && apt-get install -y build-essential curl git python3 python3-pip"
}
```

**Supervisor config for testing:**

```yaml
agent_id: test
watch_paths: ["/workspace", "/tmp"]
storage:
  backend: local-only
  local_buffer:
    cas_dir: /data/cas
    event_dir: /data/events
    max_size: 2GB
durability:
  default: local
tls:
  sslkeylogfile: /data/tls/keylog.txt
  proxy:
    enabled: true
    listen: 127.0.0.1:8080
    ca_cert: /etc/ssl/certs/sandbox-ca.pem
    ca_key: /data/tls/ca-key.pem
api:
  listen: 127.0.0.1:9090
```

## Validation Tests

Run in order. Later tests depend on earlier mechanisms working. Each test targets a specific capture mechanism.

---

### Test 1: Basic Process Tracing

Confirms: ptrace attach, fork-following, exec capture, exit capture, pid/ppid chain.

```bash
./supervisor --agent-id test --storage.backend local-only -- bash -c 'echo hello; sleep 0.1; echo bye'
```

**Expect:**
- `agent_start` event with agent_id "test"
- `exec` event for bash with correct argv
- `exec` events for echo (x2) and sleep with correct ppid → bash's pid
- `exit` events for each process with exit_code 0
- Every event has seq, ts_monotonic, ts_wall, agent_id

**Verify:**
```bash
sandbox log --type exec | jq '.argv'
sandbox log --type exit | jq '{pid, exit_code}'
```

---

### Test 2: Stdio Capture

Confirms: fd table classification, stdout/stderr separation, content capture.

```bash
./supervisor --agent-id test --storage.backend local-only -- python3 -c "
import sys
print('stdout line')
sys.stderr.write('stderr line\n')
"
```

**Expect:**
- `stdio` event with subtype `stdout`, content "stdout line\n"
- `stdio` event with subtype `stderr`, content "stderr line\n"
- Both attributed to python3's pid

**Verify:**
```bash
sandbox stdio <pid>
# Should show: stdout: "stdout line\n", stderr: "stderr line\n"
```

---

### Test 3: File Write + Read + Delete with Content

Confirms: CAS storage, before_hash/after_hash, content retrieval, unlink capture.

```bash
./supervisor --agent-id test --storage.backend local-only -- bash -c '
echo "hello world" > /workspace/test.txt
cat /workspace/test.txt
rm /workspace/test.txt
'
```

**Expect:**
- `write` event with after_hash for "hello world\n"
- `read` event with content_hash matching the write's after_hash
- `unlink` event with content_hash (file content captured before deletion)
- All hashes resolve in local CAS

**Verify:**
```bash
sandbox log --path /workspace/test.txt
# Get after_hash from write event
sandbox cat <after_hash>
# Should output: hello world
```

---

### Test 4: Pipe Topology

Confirms: pipe registry, pipe_create events, data flow attribution between processes.

```bash
./supervisor --agent-id test --storage.backend local-only -- bash -c '
echo -e "foo\nbar\nbaz" | grep bar | wc -l
'
```

**Expect:**
- `pipe_create` events (2 pipes: echo→grep, grep→wc)
- `pipe_data` events showing echo writing "foo\nbar\nbaz\n", grep writing "bar\n", wc writing "1\n"
- Correct writer_pid/reader_pid on each pipe_data
- dest_pids linking pipes to readers

**Verify:**
```bash
sandbox pipeline <bash_pid>
# Should show 3 stages connected by 2 pipes with correct byte counts
```

---

### Test 5: Subprocess Tree

Confirms: process tree reconstruction, pipe-connected stdio between parent and child.

```bash
./supervisor --agent-id test --storage.backend local-only -- python3 -c "
import subprocess
result = subprocess.run(['ls', '-la', '/workspace'], capture_output=True, text=True)
print(result.stdout[:50])
"
```

**Expect:**
- Process tree: python3 → ls
- ls stdout captured as pipe_data flowing to python3
- python3 stdout shows truncated ls output

**Verify:**
```bash
sandbox process-tree --stdio
# Should show python3 as root with ls as child, stdout on both
```

---

### Test 6: Agent-Created Tool Execution

Confirms: self-written tools don't escape tracing. Every new process is auto-traced regardless of how it was created.

```bash
./supervisor --agent-id test --storage.backend local-only -- bash -c '
cat > /tmp/tool.py << "EOF"
#!/usr/bin/env python3
import os
with open("/workspace/tool-output.txt", "w") as f:
    f.write(f"written by pid {os.getpid()}\n")
EOF
chmod +x /tmp/tool.py
python3 /tmp/tool.py
'
```

**Expect:**
- `write` event for /tmp/tool.py (the tool being created)
- `chmod` event for /tmp/tool.py
- `exec` event for python3 running tool.py
- `write` event for /workspace/tool-output.txt attributed to tool.py's pid (not bash's pid)

**Verify:**
```bash
sandbox log --path /workspace/tool-output.txt --type write | jq '{pid, process}'
# pid should be the python3 process running tool.py, not the parent bash
sandbox cat <after_hash>
# Should show: "written by pid <N>"
```

---

### Test 7: Write Lock Correctness

Confirms: concurrent writes to the same file produce correct before/after hash chains with no stale state.

```bash
./supervisor --agent-id test --storage.backend local-only -- python3 -c "
import threading

def writer(n):
    for i in range(10):
        with open('/workspace/shared.txt', 'w') as f:
            f.write(f'writer {n} iteration {i}\n')

threads = [threading.Thread(target=writer, args=(i,)) for i in range(3)]
for t in threads: t.start()
for t in threads: t.join()
"
```

**Expect:**
- 30 write events for /workspace/shared.txt
- For consecutive write events on the same file: event[N+1].before_hash == event[N].after_hash (no gaps in hash chain)
- No event has before_hash that doesn't match the prior event's after_hash

**Verify:**
```bash
sandbox log --path /workspace/shared.txt --type write | jq '[.before_hash, .after_hash]'
# Walk the pairs — each before_hash should equal the previous after_hash
# Script this:
sandbox log --path /workspace/shared.txt --type write --format json | python3 -c "
import json, sys
events = [json.loads(l) for l in sys.stdin]
for i in range(1, len(events)):
    if events[i]['before_hash'] != events[i-1]['after_hash']:
        print(f'HASH CHAIN BROKEN at seq {events[i][\"seq\"]}')
        sys.exit(1)
print(f'Hash chain valid across {len(events)} writes')
"
```

---

### Test 8: TLS Capture

Confirms: SSLKEYLOGFILE populated, mitmdump captures HTTP, content stored in CAS.

```bash
./supervisor --agent-id test --storage.backend local-only -- curl -s https://httpbin.org/get
```

**Expect:**
- `connect` event to httpbin.org:443
- `tls_keys` event (keylog entry stored in CAS)
- `http_request` event: GET https://httpbin.org/get
- `http_response` event: status 200, body_hash resolves to JSON response

**Verify:**
```bash
sandbox connections
# Should show httpbin.org:443, tls: true
sandbox log --type http_response | jq '.body_hash'
sandbox cat <body_hash>
# Should show httpbin.org JSON response
```

---

### Test 9: Pause/Resume

Confirms: all traced processes freeze on pause, unfreeze on resume, API returns correct state.

**Terminal 1:**
```bash
./supervisor --agent-id test --storage.backend local-only -- bash
```

**Terminal 2:**
```bash
# Pause
curl -s -X POST http://127.0.0.1:9090/agent/pause | jq .
# Expect: {"status":"paused","stopped_processes":[{"pid":...,"binary":"/bin/bash",...}]}

# Verify bash is frozen — typing in terminal 1 produces nothing

# Check status
curl -s http://127.0.0.1:9090/agent/status | jq .
# Expect: status: "paused"

# Resume
curl -s -X POST http://127.0.0.1:9090/agent/resume | jq .
# Expect: {"status":"running","resumed_count":1}

# Verify bash is responsive again
```

---

### Test 10: Pause-Before-Action

Confirms: rule matching, approval API, EPERM injection on deny.

**supervisor.yaml addition:**
```yaml
pause_before:
  - type: unlink
    paths: ["/workspace/**"]
```

**Terminal 1:**
```bash
./supervisor --config test-config.yaml -- bash
# In bash:
rm /workspace/test-file.txt
# bash hangs — the rm is paused at syscall entry
```

**Terminal 2:**
```bash
# Check pending
curl -s http://127.0.0.1:9090/approvals/pending | jq .
# Expect: action_id, pid, syscall: "unlink", path: "/workspace/test-file.txt"

# Deny it
curl -s -X POST http://127.0.0.1:9090/approvals/<action_id>/deny | jq .

# Terminal 1: bash shows "rm: cannot remove '/workspace/test-file.txt': Operation not permitted"
# File is still there
```

---

### Test 11: Snapshot and Restore

Confirms: Merkle tree, checkpoint, point-in-time restore to new directory.

```bash
./supervisor --agent-id test --storage.backend local-only -- bash
```

In the bash session:
```bash
echo "version 1" > /workspace/file.txt
sleep 0.5
echo "version 2" > /workspace/file.txt
sleep 0.5
echo "version 3" > /workspace/file.txt
```

From another terminal:
```bash
# Find timestamps
sandbox log --path /workspace/file.txt --type write
# Note the ts_wall after "version 1" write and before "version 2" write

# Restore to after version 1
sandbox restore --timestamp <T_after_v1> --target /tmp/snapshot-v1/
cat /tmp/snapshot-v1/workspace/file.txt
# Should output: version 1

# Restore to after version 2
sandbox restore --timestamp <T_after_v2> --target /tmp/snapshot-v2/
cat /tmp/snapshot-v2/workspace/file.txt
# Should output: version 2

# Diff between v1 and v3
sandbox diff --from <seq_v1> --to <seq_v3>
# Should show file.txt modified, before_hash → after_hash
```

---

### Test 12: Initial State Capture

Confirms: commit zero captures pre-agent filesystem, restore to seq 0 gives original state.

```bash
# Create files before the agent starts
echo "pre-existing" > /workspace/original.txt
mkdir -p /workspace/data
echo "dataset" > /workspace/data/train.csv

# Start supervisor
./supervisor --agent-id test --storage.backend local-only -- bash

# In bash: modify things
echo "modified" > /workspace/original.txt
rm /workspace/data/train.csv
echo "new file" > /workspace/new.txt
```

From another terminal:
```bash
# Restore to seq 0 (initial state)
sandbox restore --seq 0 --target /tmp/initial/

# Verify
cat /tmp/initial/workspace/original.txt
# Should output: pre-existing

cat /tmp/initial/workspace/data/train.csv
# Should output: dataset

ls /tmp/initial/workspace/new.txt
# Should not exist
```

---

## Integration Test: Trace a Coding Agent

The real validation. Trace your coding agent building or modifying Argus itself.

```bash
./supervisor --agent-id claude-code \
  --storage.backend local-only \
  --watch /workspace \
  -- claude-code  # or your agent's entrypoint
```

Ask the agent to make changes to the codebase. While it works:

```bash
# Watch events in real-time
sandbox log --follow --type write --path-prefix /workspace/argus/

# See what the agent's subprocesses are printing
sandbox stdio <pid> --follow

# After it says "done":

# Full process tree with output
sandbox process-tree --stdio

# Every file it changed
sandbox diff --from 0 --to latest

# Restore to before the agent touched anything
sandbox restore --seq 0 --target /tmp/before-agent/
diff -r /tmp/before-agent/workspace /workspace
```

**Success criteria:**
- `sandbox process-tree --stdio` shows every command the agent ran with its output
- `sandbox diff --from 0 --to latest` shows every file changed
- `sandbox restore --seq 0` produces the exact pre-agent state
- `diff -r` between restored state and a known-good copy shows zero differences

---

## Bug Indicators

Symptoms that indicate specific bugs:

| Symptom | Likely Bug |
|---------|-----------|
| `before_hash: null` on files that existed | Write lock race — before_hash captured after another write landed |
| Missing `exec` events for commands the agent ran | Fork-following gap — PTRACE_O_TRACEFORK not set on a process |
| `pipe_data` with no `pipe_create` | Pipe registry lost track — pipe created before tracing started or fd inherited without update |
| Stdio reconstruction missing chunks | Fd table lost a dup2/redirect — classification reverted to Unknown |
| Restore produces files the agent never created | Merkle tree corruption — tree update applied to wrong path |
| Content hash doesn't resolve in CAS | Content captured to memory but not flushed to disk (durability mode: memory + supervisor crashed) |
| Hash chain broken (before_hash ≠ prior after_hash) | Write lock not held, or lock acquired on wrong path (symlink resolution bug) |
| `exec` event with empty argv | PTRACE_GET_SYSCALL_INFO returned before args were populated — need to read at syscall exit instead |
| Duplicate events for same syscall | Tracee stopped twice — signal delivery confused with syscall stop (need PTRACE_O_TRACESYSGOOD) |
| Agent hangs on database write | Write lock deadlocked with fcntl advisory lock — database file not detected for group locking |

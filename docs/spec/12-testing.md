# Validation Testing

## Approach

Trace the agent that built Argus. A coding agent is the perfect workload: it reads files, writes files, spawns subprocesses (compilers, test runners, linters), makes network calls (git, package registries, LLM APIs), uses pipes, and occasionally does surprising things like writing temp files or spawning background processes. If Argus captures all of that correctly, it works.

## Minimal Setup

Docker, no K8s, no S3. The supervisor supports `local-only` storage mode. Local CAS + local event log is enough for validation. No Helm chart or service accounts needed. Just a Docker container with SYS_PTRACE.

```json
{
  "name": "argus-dev",
  "image": "ubuntu:24.04",
  "capAdd": ["SYS_PTRACE"],
  "securityOpt": ["seccomp=unconfined"],
  "mounts": [
    "source=${localWorkspaceFolder},target=/workspace,type=bind"
  ],
  "postCreateCommand": "apt-get update && apt-get install -y build-essential curl git python3 python3-pip strace"
}
```

The `seccomp=unconfined` sidesteps default profile issues during development. In production, use the narrower SYS_PTRACE capability only.

## Validation Tests

Run in order. Later tests depend on earlier mechanisms working.

| Test | Mechanism | Key Verification |
|-|-|-|
| 1 | Process tracing | pid/ppid chain, fork-following |
| 2 | Stdio | stdout/stderr separation, content |
| 3 | File write/read/delete | CAS storage, hash resolution, unlink content |
| 4 | Pipe topology | pipe_create, data flow, writer-to-reader attribution |
| 5 | Subprocess tree | process_tree reconstruction, pipe-connected stdio |
| 6 | Agent-created tools | Self-written scripts don't escape tracing |
| 7 | Write locking | Concurrent writes produce unbroken hash chain |
| 8 | TLS capture | SSLKEYLOGFILE, mitmdump, HTTP body in CAS |
| 9 | Pause/resume | Freeze/unfreeze via API |
| 10 | Pause-before-action | Rule matching, approval, EPERM injection |
| 11 | Snapshot and restore | Point-in-time restore to new directory |
| 12 | Initial state | Commit zero captures pre-agent filesystem |

---

### Test 1: Basic Process Tracing

```bash
./supervisor --agent-id test --storage.backend local-only -- bash -c 'echo hello; sleep 0.1; echo bye'
```

**Expect:** `exec` event for bash, `exec` for echo (x2), `exit` events. Confirm pid/ppid chain is correct.

**Validates:** Phase 1 tracer loop, fork/clone/exec following, seccomp-bpf filter.

---

### Test 2: Stdio Capture

```bash
./supervisor --agent-id test --storage.backend local-only -- python3 -c "
import sys
print('stdout line')
sys.stderr.write('stderr line\n')
"
```

**Expect:** `stdio` events with subtype `stdout` and `stderr`, correct content hashes. Content resolves in CAS.

**Validates:** Phase 1 fd classification (fd 1 = stdout, fd 2 = stderr), write classification logic.

---

### Test 3: File Write + Read + Delete with Content

```bash
./supervisor --agent-id test --storage.backend local-only -- bash -c '
echo "hello world" > /workspace/test.txt
cat /workspace/test.txt
rm /workspace/test.txt
'
```

**Expect:**
- `write` event with `after_hash`
- `read` event with `content_hash` matching the `after_hash`
- `unlink` event with `content_hash`
- `argus cat <hash>` returns `hello world\n`

**Validates:** Phase 2 content capture, CAS storage, hash-based content retrieval.

---

### Test 4: Pipe Topology

```bash
./supervisor --agent-id test --storage.backend local-only -- bash -c '
echo -e "foo\nbar\nbaz" | grep bar | wc -l
'
```

**Expect:** `pipe_create` events for each pipe in the pipeline. `pipe_data` showing data flow from echo to grep to wc. Correct byte counts at each stage.

**Validates:** Phase 1 pipe registry, fd table updates on pipe/dup2, pipe_data attribution.

---

### Test 5: Subprocess Tree

```bash
./supervisor --agent-id test --storage.backend local-only -- python3 -c "
import subprocess
result = subprocess.run(['ls', '-la', '/workspace'], capture_output=True, text=True)
print(result.stdout[:50])
"
```

**Expect:** Process tree shows python3 as parent of ls. The `ls` stdout is captured as pipe_data flowing back to python3. Python3's stdout shows the first 50 chars of ls output.

**Validates:** Phase 1 fork-following, pipe-connected stdio between parent and child, process_tree reconstruction.

---

### Test 6: Self-Created Tool Execution (The Escape Test)

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
- Write to `/tmp/tool.py` is captured
- `exec` of python3 running it is captured
- Write to `/workspace/tool-output.txt` by the new process is captured with correct pid attribution

Confirms agent-created tools don't escape tracing. All descendants of the original traced process inherit ptrace regardless of how they were spawned.

**Validates:** Phase 1 auto-follow on exec, PTRACE_TRACEME inheritance.

---

### Test 7: Write Locking Correctness

```bash
./supervisor --agent-id test --storage.backend local-only -- python3 -c "
import threading, os

def writer(n):
    for i in range(10):
        with open('/workspace/shared.txt', 'w') as f:
            f.write(f'writer {n} iteration {i}\n')

threads = [threading.Thread(target=writer, args=(i,)) for i in range(3)]
for t in threads: t.start()
for t in threads: t.join()
"
```

**Expect:** Every write event has a valid `before_hash` and `after_hash`. No `before_hash` is stale: `before_hash` of event N+1 must equal `after_hash` of event N for the same file.

**Verify:** `argus log --path /workspace/shared.txt --type write` and check the hash chain is unbroken.

**Validates:** Phase 2 per-path write locking, lock acquire on entry, release after exit with after_hash.

---

### Test 7b: Write Interleaving (Hardened)

Uses a C program with real pthreads (no GIL) to stress-test write serialization under true concurrency. Four threads × 100 writes with `O_TRUNC`.

```bash
gcc -O0 -pthread -o /tmp/concurrent_write tests/concurrent_write.c

./supervisor --agent-id interleave-test --storage.backend local-only \
  -- /tmp/concurrent_write

argus log --path /workspace/shared.txt --type write --format json \
  | python3 tests/validate_hash_chain.py
```

**Expect:**
- 400 write events (4 threads × 100 iterations), clean linear hash chain
- Zero hash chain breaks
- Each captured after-state contains a complete `"writer N iteration M\n"` line (no truncated/mixed content)

**Validates:** Per-path write queue in the ptrace loop serializes concurrent writes at syscall entry, preventing kernel-level interleaving.

---

### Test 8: TLS Capture

```bash
./supervisor --agent-id test --storage.backend local-only -- curl -s https://httpbin.org/get
```

**Expect:**
- `connect` event to httpbin.org:443
- `tls_keys` event from the keylog
- If mitmdump is running: `http_request`/`http_response` events with body hashes
- `argus cat <body_hash>` shows the JSON response

**Validates:** Phase 2 TLS content capture, SSLKEYLOGFILE env setup, mitmdump integration.

---

### Test 9: Pause/Resume

Terminal 1:
```bash
./supervisor --agent-id test --storage.backend local-only -- bash
```

Terminal 2:
```bash
# Freeze
curl -X POST http://127.0.0.1:9090/agent/pause
# Verify: bash session is frozen, typing produces nothing

# Check status
curl http://127.0.0.1:9090/agent/status
# Verify: status is "paused", bash listed as stopped

# Unfreeze
curl -X POST http://127.0.0.1:9090/agent/resume
# Verify: bash unfreezes, pending input is processed
```

**Validates:** Phase 2 pause/resume API, PTRACE_CONT suppression, agent_pause/agent_resume events.

---

### Test 10: Pause-Before-Action

Config:
```yaml
pause_before:
  - type: unlink
    paths: ["/workspace/**"]
```

```bash
./supervisor --agent-id test --storage.backend local-only --config test-pause.yaml -- bash
```

In the bash session:
```bash
echo "important" > /workspace/critical.txt
rm /workspace/critical.txt   # This should block
```

From another terminal:
```bash
# Check pending approvals
curl http://127.0.0.1:9090/approvals/pending
# Should show the unlink action

# Deny it
curl -X POST http://127.0.0.1:9090/approvals/<action_id>/deny
# The rm command should fail with "Permission denied"
```

**Expect:** `pending_approval` event emitted, process frozen until decision, `approval_denied` event, EPERM injected so process sees permission denied.

**Validates:** Phase 2 pause-before-action rules, rule matching, approval API, EPERM injection.

---

### Test 11: Snapshot and Restore

Terminal 1:
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
# Find the timestamp between version 1 and version 2
argus log --path /workspace/file.txt --type write

# Restore to after version 1 was written
argus restore --timestamp <T> --target /tmp/snapshot/
cat /tmp/snapshot/workspace/file.txt
# Should output: "version 1"
```

**Validates:** Phase 3 Merkle tree, checkpoint, full restore to new directory, CAS content retrieval by hash.

---

### Test 12: Initial State Capture

```bash
# Pre-populate workspace
echo "pre-existing" > /workspace/existing.txt
mkdir -p /workspace/subdir
echo "nested" > /workspace/subdir/nested.txt

./supervisor --agent-id test --storage.backend local-only -- bash -c 'echo done'
```

**Expect:**
- `initial_state` event with `tree_hash`, `file_count >= 2`, `total_size > 0`
- `argus snapshot --seq 0` shows `existing.txt` and `subdir/nested.txt` with correct hashes
- `argus cat <hash>` for each file returns correct content

**Validates:** Phase 1/2 startup sequence step 7, initial filesystem walk, commit zero, Merkle tree baseline.

---

## Integration Test: Trace Your Coding Agent Building Argus

Once tests 1-12 pass, the real validation:

```bash
./supervisor --agent-id claude-code \
  --storage.backend local-only \
  --watch /workspace \
  -- claude-code
```

Ask the agent to make changes to the Argus codebase. While it works:

```bash
# Watch events in real-time
argus log --follow --type write --path-prefix /workspace/argus/

# See what the agent's subprocess printed
argus stdio <pid> --follow

# After it says "done", check the full process tree
argus process-tree --stdio

# Diff what the workspace looked like before vs. after
argus diff --from 0 --to latest

# Restore to before the agent touched anything
argus restore --seq 0 --target /tmp/before-agent/
diff -r /tmp/before-agent/workspace /workspace
```

**Success criteria:**
- `argus process-tree --stdio` shows every command the agent ran with its output
- `argus diff` shows every file it changed
- `argus restore --seq 0` gives back the exact pre-agent state

---

## Bug Indicators

Symptoms mapped to specific bugs. If you see these during validation, investigate the corresponding mechanism.

| Symptom | Likely Bug |
|-|-|
| Events with `before_hash: null` on files that existed | Write lock race: before_hash captured before lock acquired |
| Missing `exec` events for commands you saw the agent run | Fork-following gap: PTRACE_O_TRACEFORK not set or clone event missed |
| `pipe_data` events with no corresponding `pipe_create` | Pipe registry out of sync: pipe2() not intercepted or fd inherited without tracking |
| Stdio reconstruction missing chunks | Fd table lost track of a redirect: dup2 to fd 1/2 not updating fd table |
| Restore produces files the agent never created | Merkle tree corruption: tree_hash not updated on unlink/rename |
| Content hashes that don't resolve in CAS | Content captured to memory but not flushed to disk: durability mode mismatch |
| `after_hash` doesn't match actual file content | process_vm_readv read wrong buffer: wrong address or count from syscall args |
| Wrong pid attribution on write events | Fork copied parent's fd table incorrectly, or fd closed in parent but not tracked |
| TLS bodies missing but tls_keys present | mitmdump not started, or HTTPS_PROXY env not inherited by agent subprocess |
| Events out of seq order in event log | Sequence generator not atomic, or concurrent event emission without lock |

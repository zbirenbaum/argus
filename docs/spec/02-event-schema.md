# Event Schema

Every event the supervisor emits is consumed by the query API, stream reconstruction, restore engine, and cross-agent correlation. The envelope must be complete from day one — adding fields later means format migration.

## Envelope

```rust
struct Event {
    seq: u64,                    // monotonically increasing, unique within agent
    ts_monotonic: u64,           // CLOCK_MONOTONIC_RAW nanoseconds (local ordering)
    ts_wall: String,             // CLOCK_REALTIME RFC 3339 nanosecond (cross-agent)
    agent_id: String,            // from config or auto-generated
    #[serde(flatten)]
    payload: EventPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    vclock: Option<serde_json::Value>,  // reserved for vector clock
}
```

```json
{
  "seq": 42,
  "ts_monotonic": 1234567890123456789,
  "ts_wall": "2026-03-11T14:23:01.847392123Z",
  "agent_id": "researcher-abc",
  "type": "write",
  "pid": 42,
  "path": "/workspace/output.csv",
  "before_hash": "ab12...",
  "after_hash": "cd34...",
  "size": 4096,
  "tree_hash": "ef56..."
}
```

## Dual Timestamps

| Field | Source | Use | Cross-node |
|-------|--------|-----|------------|
| ts_monotonic | CLOCK_MONOTONIC_RAW | Local ordering (with seq) | No |
| ts_wall | CLOCK_REALTIME | Cross-agent correlation | Yes (~1ms via NTP) |

Both always present. Within one agent: order by seq. Across agents: order by ts_wall, break ties by agent_id.

## agent_id

- Set via config or `--agent-id` flag. Auto-generated as `{hostname}-{random}` if not set.
- Appears on every event. No event emitted without it.
- Namespaces S3 paths: `s3://bucket/events/{agent_id}/`

## Causal Ordering: vclock Field

### The Problem

NTP gives ~1ms accuracy across nodes. Most causal relationships (Agent A writes, Agent B reads) are separated by more than 1ms. But when agents interact rapidly with the same shared resource — both writing to the same S3 key, both querying the same database row — ts_wall can't distinguish which happened first.

### Two Approaches

**Lamport timestamp:** Single counter. Every event increments it. When agents communicate (one reads what another wrote), the reader sets its counter to max(own, writer's) + 1. Simple. Gives you total ordering but not "who knew about whose events."

**Vector clock:** Map of `{agent_id → counter}`. Each agent increments its own entry on every event. When Agent B reads something Agent A wrote, B merges A's counter into its own vector: `B.vclock[A] = max(B.vclock[A], A.vclock[A])`. This gives you true causal ordering: you can determine if two events are causally related or concurrent.

### Schema

The `vclock` field in the event envelope is an optional JSON object:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
vclock: Option<HashMap<String, u64>>,  // agent_id → counter
```

**When populated (not MVP, but schema is ready):**

```json
{
  "seq": 42,
  "agent_id": "researcher-abc",
  "type": "write",
  "path": "s3://bucket/shared-data.csv",
  "vclock": {
    "researcher-abc": 42,
    "coder-def": 17
  }
}
```

This says: "researcher has seen 42 of its own events and is aware of coder's state as of coder's event 17." If coder later reads this file and sees `researcher-abc: 42`, coder knows it's reading data that was written with full knowledge of coder's first 17 events.

### When to Populate

Only on events that touch shared resources — identified by:
- Network writes to shared storage (S3 PUT, database INSERT)
- Network reads from shared storage (S3 GET, database SELECT)
- File writes to shared mounted volumes (NFS, EFS)

Local filesystem events don't need vclock (seq is sufficient within one agent).

### Implementation Path

1. **Now:** Field exists in schema, serialized as null/omitted. Zero cost.
2. **Future:** Supervisor detects shared resource access (S3 URLs, database connections) and populates vclock. Requires a lightweight coordination protocol — agents exchange their latest counters via a shared S3 metadata file or a small coordination service.

## Event Types

### Process

| Type | Fields |
|------|--------|
| exec | pid, ppid, binary, argv, envp, cwd |
| fork | parent_pid, child_pid |
| exit | pid, exit_code, signal |

### File Content

| Type | Fields |
|------|--------|
| read | pid, path, fd, offset, size, content_hash |
| write | pid, path, fd, offset, size, before_hash, after_hash, tree_hash |

### File Metadata

| Type | Fields |
|------|--------|
| rename | pid, old_path, new_path, tree_hash |
| unlink | pid, path, content_hash, tree_hash |
| mkdir | pid, path, tree_hash |
| rmdir | pid, path, tree_hash |
| chmod | pid, path, old_mode, new_mode |
| truncate | pid, path, old_size, new_size, before_hash, after_hash, tree_hash |
| link | pid, target, link_path, tree_hash |
| symlink | pid, target, link_path, tree_hash |

### Stdio / Pipe / PTY

| Type | Fields |
|------|--------|
| stdio | pid, subtype (stdout/stderr/stdin), content_hash, size, pipe_inode, dest_pid/source_pid |
| pipe_create | pid, inode, read_fd, write_fd |
| pipe_data | pid, inode, direction, content_hash, size, dest_pids |
| pipe_close | pid, inode, direction |
| pty_create | pid, master_fd, slave_path |
| pty_data | pid, subtype (slave_write/master_read), content_hash, size, slave_path |
| fd_redirect | pid, fd, target (type, inode/path, direction) |

### Network

| Type | Fields |
|------|--------|
| socket | pid, domain, sock_type, fd |
| connect | pid, fd, remote_addr, remote_port |
| accept | pid, fd, peer_addr, peer_port |
| tls_keys | pid, fd, sni, keylog_line_hash |
| http_request | pid, method, url, headers_hash, body_hash, status |
| http_response | pid, status, headers_hash, body_hash |

### Control

| Type | Fields |
|------|--------|
| agent_start | agent_id, config summary, node, pod |
| agent_pause | reason, stopped_pids |
| agent_resume | resumed_pids |
| pending_approval | pid, syscall, path/binary, rule_name |
| approval_granted | pid, rule_name, approver |
| approval_denied | pid, rule_name, approver |

### Snapshot

| Type | Fields |
|-|-|
| initial_file | pid, path, content_hash, size, mode |
| initial_state | tree_hash, file_count, total_size |
| checkpoint | seq, tree_hash, checkpoint_s3_key |
| mmap_warning | pid, path, fd, prot, flags |

`initial_file` events are emitted once per pre-existing file during the startup filesystem walk, before the `initial_state` summary. They enable event-log-only replay without reading CAS tree objects.

## Ordering Rules

- **Within agent:** order by seq (monotonic, no ambiguity)
- **Across agents:** order by ts_wall, break ties by agent_id
- **Causal:** ts_wall sufficient given NTP accuracy — causally related events are always milliseconds apart

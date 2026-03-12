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

## vclock

Reserved for future vector clock / Lamport timestamp. Serialized as null / omitted now. When agents access shared resources, they can increment a per-resource logical clock for sub-millisecond causal ordering. Schema supports it; implementation deferred.

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
|------|--------|
| initial_state | tree_hash, file_count, total_size |
| checkpoint | seq, tree_hash, checkpoint_s3_key |
| mmap_warning | pid, path, fd, prot, flags |

## Ordering Rules

- **Within agent:** order by seq (monotonic, no ambiguity)
- **Across agents:** order by ts_wall, break ties by agent_id
- **Causal:** ts_wall sufficient given NTP accuracy — causally related events are always milliseconds apart

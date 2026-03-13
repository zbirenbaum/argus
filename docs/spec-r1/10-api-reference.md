# API Reference

Supervisor REST API on `http://127.0.0.1:9090`. JSON responses. Streaming: `application/x-ndjson`.

---

## Agent Control

```
POST /agent/pause                → Freeze all. Response: {status, stopped_processes: [{pid, binary, current_syscall}]}
POST /agent/resume               → Resume all. Response: {status, resumed_count}
GET  /agent/status               → {status, agent_id, uptime_seconds, event_count, processes: [{pid, ppid, binary, argv, state}]}
```

## Approvals

```
GET  /approvals/pending                    → {pending: [{action_id, pid, process, syscall, path, timestamp, rule_matched}]}
POST /approvals/{action_id}/approve        → {action_id, result: "approved", pid}
POST /approvals/{action_id}/deny           → {action_id, result: "denied", pid, injected_errno: "EPERM"}
```

## Events

```
GET /events
```

| Param | Type | Description |
|-------|------|-------------|
| path | string | Exact path |
| path_prefix | string | Directory prefix |
| pid | int | Process ID |
| type | string | Event type |
| subtype | string | Event subtype |
| since / until | ISO8601 | Time range |
| seq_from / seq_to | int | Sequence range |
| limit | int | Max events (default 1000) |

Response: streaming JSONL.

```
GET /file_history?path=/workspace/config.yaml
  → {path, events: [{seq, type, content_hash, before_hash, after_hash, pid, ts_wall}]}
```

## Process Tree

```
GET /process_tree?root=10&stdio=true&depth=5
  → {pid, binary, argv, children: [{pid, binary, argv, stdout, stderr, connected_via, children}]}
```

## Stdio

```
GET /stdio?pid=42
  → {pid, binary, argv, exit_code, stdout, stderr, stdin, stdout_dest, stderr_dest}

GET /stdio?pid=42&stream=stdout&follow=true
  → SSE: event:stdout data:{"seq":100,"content":"Epoch 1: loss=0.42\n","ts_wall":"..."}

GET /stdio?pid=42&format=events
  → [{seq, type, subtype, content_hash, size, ts_wall}]
```

## Pipeline

```
GET /pipeline?shell_pid=55
  → {shell_pid, stages: [{pid, binary, argv, input_pipe, output_pipe, output_size}],
     pipes: [{inode, writer_pid, reader_pid, bytes}]}
```

## Content

```
GET /content/{hash}        → Raw bytes (application/octet-stream). Local cache, S3 fallback.
GET /content/{hash}/text   → UTF-8 text. 415 if not valid UTF-8.

GET /diff?before_hash=ab12&after_hash=cd34&format=unified
  → Unified diff text

GET /diff?before_hash=ab12&after_hash=cd34&format=json
  → {before_hash, after_hash, hunks: [{old_start, old_count, new_start, new_count, lines: [{type, content}]}]}
```

## Filesystem Snapshots

```
GET /tree?seq=42&path_prefix=/workspace/
  → {tree_hash, seq, entries: [{name, type, hash, size, mode}]}

GET /tree/diff?from_seq=10&to_seq=42
  → {from_seq, to_seq, added: [{path, hash, size}], modified: [{path, before_hash, after_hash}], deleted: [{path, hash}]}
```

## Restore

```
POST /restore
  Body: {timestamp, mode: "new_directory", target: "/data/restore/snapshot-1/"}
  Body: {timestamp, mode: "in_place", force: true}
  Body: {seq, mode: "selective", path: "/workspace/config.yaml", in_place: true}
  → {restored_to_seq, restored_to_ts, tree_hash, files_restored, bytes_restored, pre_restore_snapshot_seq}

POST /restore/undo
  Body: {last: 5}  or  {last_by_pid: 42}
  → Same response as /restore
```

## Network

```
GET /connections?pid=42&active_only=true
  → {connections: [{pid, fd, type, dest_addr, dest_port, sni, connected_at, bytes_sent, bytes_received, tls, active}]}
```

## Storage

```
GET /storage/status
  → {local_buffer: {cas_size_bytes, cas_objects, events_segments_local, pending_uploads},
     remote: {backend, bucket, cas_objects_known, events_segments_uploaded},
     digest_cache: {entries, last_snapshot_uploaded, ttl}}

GET /health
  → {status: "ok", agent_id, event_count}
```

---

## WebSocket

```
ws://127.0.0.1:9090/ws/events?type=write&path_prefix=/workspace/
  Server→Client: {seq, type, pid, path, ...}

ws://127.0.0.1:9090/ws/stdio/{pid}
  Server→Client: {stream: "stdout", content: "...", seq, ts_wall}

ws://127.0.0.1:9090/ws/approvals
  Server→Client: {action_id, pid, syscall, path}
  Client→Server: {action_id, decision: "approve"|"deny"}
```

---

## Cross-Agent Query Layer

Separate service/CLI. Reads S3 directly. No running supervisor needed.

```
GET /agents
  → {agents: [{agent_id, started, node, pod, last_event}]}

GET /timeline?agents=researcher,coder&since=1h&type=write
  → Streaming JSONL, interleaved by ts_wall

GET /correlation?write_agent=researcher&read_agent=coder&resource=*
  → {correlations: [{resource, write: {agent_id, seq, ts_wall}, read: {agent_id, seq, ts_wall}, latency_ms}]}
```

---

## CLI

```
# Control
sandbox status                                          → GET /agent/status
sandbox pause                                           → POST /agent/pause
sandbox resume                                          → POST /agent/resume

# Events
sandbox log [--since 5m] [--path P] [--pid N] [--type T] → GET /events
sandbox history <path>                                    → GET /file_history

# Processes & stdio
sandbox stdio <pid> [--stream stdout] [--follow]         → GET /stdio
sandbox pipeline <shell_pid>                             → GET /pipeline
sandbox process-tree [--stdio]                           → GET /process_tree

# Content
sandbox cat <hash>                                       → GET /content/{hash}/text
sandbox diff <before> <after>                            → GET /diff
sandbox diff --from <seq> --to <seq>                     → GET /tree/diff

# Snapshots & restore
sandbox snapshot [--seq N] [--path P]                    → GET /tree
sandbox restore --timestamp T --target <dir>             → POST /restore
sandbox restore --timestamp T --in-place --force         → POST /restore
sandbox restore --timestamp T --path P [--in-place]      → POST /restore
sandbox undo --last N                                    → POST /restore/undo

# Network & storage
sandbox connections [--pid N] [--active]                 → GET /connections
sandbox storage-status                                   → GET /storage/status

# Approvals
sandbox approvals                                        → GET /approvals/pending
sandbox approve <action_id>                              → POST /approvals/{id}/approve
sandbox deny <action_id>                                 → POST /approvals/{id}/deny

# Debug
sandbox dump-checkpoint --seq N --format json

# Cross-agent (reads S3, no running supervisor)
sandbox agents --bucket <bucket>                         → GET /agents
sandbox timeline --agents a,b --since 1h                 → GET /timeline
sandbox correlate --write-agent a --read-agent b         → GET /correlation
```

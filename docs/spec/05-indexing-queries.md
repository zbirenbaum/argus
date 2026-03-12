# Event Log Indexing & Queries

## Primary Index

Event log is time-ordered. Time range queries = binary search over segments + linear scan within segment.

## Secondary Indexes

Append-only files alongside event segments. Updated synchronously on event write. Rebuilt from event log on restart. Local-only (not archived to S3).

**Path index:** `path → [(seq, type)]`
```
/data/indexes/path/{path_hash}.idx
```
For directory queries: prefix trie or path hierarchy indexing.

**Process index:** `pid → [(seq, type)]`
```
/data/indexes/pid/{pid}.idx
```
Also: process tree index `pid → {ppid, binary, argv, start_seq, end_seq}`

**Type index:** `event_type → [seq]`
```
/data/indexes/type/{type}.idx
```

## Query Engine

REST API (see `10-api-reference.md` for full spec):

```
GET /events?path=/workspace/config.yaml&since=2026-03-11T14:00:00Z
GET /events?pid=42&type=write
GET /events?path_prefix=/workspace/&type=unlink&limit=100
GET /events?seq_from=1000&seq_to=2000
GET /file_history?path=/workspace/config.yaml
GET /process_tree
```

Returns streaming JSONL for large result sets.

## Stdio Reconstruction

All stdio/pipe/PTY events feed into a unified stream reconstruction layer.

**Per-process:** Walk event log for stdio/pipe_data/pty_data events matching pid, filter by fd classification (stdout=fd1, stderr=fd2, stdin=fd0), concatenate content in seq order.

```
GET /stdio?pid=42
→ { pid, binary, argv, exit_code, stdout: "...", stderr: "...", stdin: "",
     stdout_dest: {type: "pipe", inode, reader_pid},
     stderr_dest: {type: "pipe", inode, reader_pid} }
```

**Process tree with stdio:**
```
GET /process_tree?root=10&stdio=true
→ Full tree with reconstructed stdout/stderr per node, pipe connections between nodes
```

**Real-time:**
```
GET /stdio?pid=42&stream=stdout&follow=true → SSE stream
```

## Pipeline Reconstruction

```
GET /pipeline?shell_pid=55
→ { stages: [{pid, binary, argv, input_pipe, output_pipe, output_size}],
     pipes: [{inode, writer_pid, reader_pid, bytes}] }
```

Built from pipe_create + pipe_data + exec events for the shell's children.

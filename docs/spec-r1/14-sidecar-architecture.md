# 14 — Sidecar Architecture

## Principle

The supervisor captures. The sidecar drains. Neither depends on the other to function — the supervisor works standalone, the sidecar is an accelerator.

## Transport: Unix Domain Socket, not WebSocket

WebSocket has HTTP upgrade overhead and TCP buffering. A Unix domain socket in `/data/argus.sock` is:
- Zero network stack (no TCP, no HTTP)
- Kernel-buffered with backpressure (send blocks when socket buffer full)
- Survives sidecar restarts (supervisor re-accepts)
- Not exposed outside the pod (security)

The protocol is length-prefixed JSONL: `[4-byte big-endian length][JSON bytes]\n`. Simple to parse, no framing ambiguity, works with any language.

## Connection Lifecycle

```
Supervisor                          Sidecar
    │                                   │
    ├── listen(/data/argus.sock)        │
    │                                   │
    │◀──────── connect ─────────────────┤
    │                                   │
    │──── handshake ───────────────────▶│
    │     { "version": 1,              │
    │       "last_confirmed_seq": 0 }  │
    │                                   │
    │◀─── resume_from ─────────────────┤
    │     { "seq": 0 }                 │
    │                                   │
    │──── replay from event log ──────▶│
    │     (missed events in seq order)  │
    │                                   │
    │──── live stream ────────────────▶│
    │     (events as they happen)       │
    │                                   │
    │◀─── ack { "seq": N } ───────────┤
    │     (periodic, batched)           │
    │                                   │
    │──── GC segments ≤ N              │
    │                                   │
```

### Handshake

Supervisor sends its version and the highest seq it has confirmed as GC'd. Sidecar responds with the seq it wants to resume from (its last confirmed seq, or 0 for fresh start).

### Replay

If `resume_from.seq < current_seq`, the supervisor reads from event log segments on disk and replays them over the socket. Then transitions to live streaming. The sidecar sees a seamless ordered event stream regardless of whether events came from replay or live.

### Reconnection

Sidecar crashes → socket closes → supervisor drops the connection, continues buffering to local event log. Sidecar restarts → connects → sends `resume_from` with its last confirmed seq → supervisor replays the gap → back to live.

No events are lost. The event log on disk is the buffer.

## Backpressure

Three tiers, matching the pipeline's existing design:

| Tier | Behavior |
|-|-|
| Socket buffer full | `send()` blocks → pipeline's `fold` stage blocks → `unfold` stops polling ptrace → tracee frozen at next syscall |
| Sidecar slow but connected | Same effect — kernel socket buffer fills, same chain |
| Sidecar disconnected | Events go to local event log only. No backpressure on the pipeline. Replay catches up on reconnect |

The key insight: when the sidecar is connected, it IS a required sink (backpressure freezes the tracee). When disconnected, it's absent — the pipeline runs at full speed with local-only persistence. The sidecar opts in to backpressure by connecting.

This is configurable. Some deployments want "never freeze the tracee for telemetry" — they set the socket to non-blocking best-effort mode. Others want "every event must reach the sidecar" — they use blocking mode.

## ACK and GC Protocol

The sidecar sends periodic ACKs:

```json
{ "ack": { "event_seq": 4500, "cas_hashes": ["blake3:abc...", "blake3:def..."] } }
```

- `event_seq`: all events up to this seq have been durably stored externally (OTLP backend, S3, wherever). Supervisor can delete event log segments whose max seq ≤ this value.
- `cas_hashes`: content objects the sidecar has confirmed are in S3. Supervisor marks them in the digest cache and can delete local CAS copies.

The supervisor responds:

```json
{ "gc_result": { "segments_deleted": 3, "cas_freed_bytes": 1048576 } }
```

ACKs are batched — the sidecar doesn't ACK every event. A reasonable default is every 5 seconds or every 1000 events, whichever comes first.

## Supervisor-Side Changes

### New: Socket Listener

A new module `pipeline/socket.rs` that:
1. Listens on `/data/argus.sock` (configurable path)
2. Accepts one connection at a time (the sidecar)
3. On connect: reads `resume_from`, replays from event log, switches to live
4. On disconnect: logs warning, continues without it
5. Receives ACKs, triggers GC

### New: GC Module

`storage/gc.rs`:
- `gc_event_segments(confirmed_seq)` — deletes segment files whose max seq ≤ confirmed_seq
- `gc_cas_objects(confirmed_hashes)` — deletes local CAS files for confirmed hashes
- Both are idempotent — safe to call with stale ACKs

### Modified: Event Log

The event log already writes segments. Add:
- `segments_before(seq)` — list segments whose max seq ≤ a threshold (for GC)
- `replay_from(seq)` — iterator over events starting from seq (for replay)

### Modified: Pipeline Output

The socket sink becomes an optional output stage in the `fold` pipeline. When connected:
- Events are written to the socket after redaction (same as stdout)
- If the socket blocks, the fold blocks, backpressure propagates

When disconnected:
- The stage is a no-op
- Events still go to event log and CAS via the bus

## Sidecar Architecture

The sidecar is a separate binary (`crates/sidecar/`) or a separate repo. It:

```
┌──────────────────────────────────┐
│ Sidecar                         │
│                                  │
│  Unix Socket ──▶ Event Buffer   │
│                     │            │
│              ┌──────┴──────┐     │
│              ▼             ▼     │
│         OTLP Export    Stdout    │
│              │                   │
│              ▼                   │
│      S3 Snapshot Confirm ──────▶ ACK back to supervisor
│                                  │
└──────────────────────────────────┘
```

1. Connects to supervisor socket, receives events
2. Buffers in memory (bounded ring buffer, drops oldest on overflow)
3. Exports to configured backends:
   - **stdout**: JSONL, same format as supervisor's current stdout
   - **OTLP**: converts events to OTel logs/spans, pushes to collector
   - **Webhook**: POST batches to an HTTP endpoint
   - **S3 confirm**: polls S3 to verify CAS uploads completed, sends ACKs
4. Periodically ACKs the supervisor with confirmed seq + CAS hashes

## What Stays in the Supervisor

- ptrace capture, classify, policy, CAS write, event log write
- Snapshot persistence to S3 (tree objects, content blobs)
- Control API (pause, resume, rules, restore)
- Socket listener for sidecar connection

## What Moves to the Sidecar

- stdout JSONL output (sidecar writes to its own stdout)
- OTLP/telemetry export
- S3 upload confirmation polling
- Local storage GC decisions

## Configuration

Supervisor config adds:
```yaml
sidecar:
  socket_path: /data/argus.sock
  # "blocking" = backpressure freezes tracee
  # "best_effort" = drop events if socket full
  mode: blocking
```

Sidecar config:
```yaml
supervisor_socket: /data/argus.sock
outputs:
  - type: stdout
  - type: otlp
    endpoint: http://otel-collector:4317
  - type: webhook
    url: https://my-service.com/events
    batch_size: 100
    flush_interval: 5s
s3_confirm:
  bucket: argus-events
  poll_interval: 30s
ack:
  interval: 5s
  batch_size: 1000
```

## Deployment

```yaml
# Kubernetes pod
spec:
  containers:
    - name: supervisor
      image: argus-supervisor
      command: ["supervisor", "--", "your-agent-command"]
      volumeMounts:
        - name: data
          mountPath: /data

    - name: sidecar
      image: argus-sidecar
      volumeMounts:
        - name: data
          mountPath: /data
      env:
        - name: OTEL_EXPORTER_OTLP_ENDPOINT
          value: http://otel-collector:4317

  volumes:
    - name: data
      emptyDir: {}
```

The shared `/data` volume gives the sidecar access to the Unix socket and the event log files (for replay on reconnect).

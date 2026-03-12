# Storage Architecture

Stream-first. Content and events captured locally as a hot buffer, streamed to S3/GCS. External storage is source of truth; local disk is a cache. No sidecar required — supervisor handles streaming directly.

## CAS (Content-Addressable Store)

All content (file bodies, stdio, network payloads) stored by SHA-256 hash. Identical content stored once.

**Local:** `/data/cas/{hash[0:2]}/{hash[2:]}`
**Remote:** `s3://bucket/cas/{hash[0:2]}/{hash[2:]}`

**Write path:**
1. Compute hash
2. Check digest cache — if hash exists remotely, skip upload
3. If new: write to local buffer, enqueue async S3 upload
4. On upload confirmation: add to digest cache, eligible for local eviction
5. Event references hash

**Large files:** 4MB content-defined chunks (Rabin fingerprint). Manifest lists chunk hashes.

## Event Log

Append-only, JSONL, 64MB segments.

**Local:** `/data/events/{segment_seq}.jsonl`
**Remote:** `s3://bucket/events/{agent_id}/{segment_seq}.jsonl`

Uploaded on segment completion or age threshold (whichever first). Once uploaded, local segment eligible for eviction. Ordering: strictly by seq within segment, segments uploaded in order, no reordering possible.

## Digest Cache

Tracks which hashes exist in S3. Without it, every capture needs an S3 round-trip.

```rust
struct DigestCache {
    known_remote: HashMap<SHA256, DigestEntry>,
    cache_file: PathBuf,  // /data/digest-cache.bin
}
struct DigestEntry { hash: SHA256, uploaded_at: Instant, size_bytes: u64, ttl: Duration }
```

**Lifecycle:**
1. **Start:** Load from local disk. If missing, download `s3://bucket/meta/{agent_id}/digest-cache-latest.bin`, then incremental LIST for objects newer than snapshot timestamp.
2. **Capture:** Check cache. Hit → skip upload. Miss → upload, add on confirmation.
3. **Shutdown:** Persist to disk. Upload snapshot to S3 with generation timestamp.
4. **Periodic:** Upload cache snapshot to S3 every ~10 minutes. Bounds incremental LIST on restart.
5. **TTL:** Default 7 days. After expiry, re-check S3 (HEAD) on next reference.

**Size:** 1M unique hashes ≈ 40MB in memory.

## Initial State Capture

Before exec'ing agent (step 7 of startup, see `01-supervisor.md`):
- Walk all watched paths, hash every file into CAS, build initial Merkle tree
- Commit zero — baseline for all diffs. Emit `initial_state` event.
- S3 uploads enqueued; agent starts once hashing completes.

## S3/GCS Integration

No sidecar. Supervisor uses tokio async upload pool (aws-sdk-s3 / rust-s3).

- Supervisor knows when content is finalized — no polling
- Digest cache is in-process for fast path
- Failures retried with exponential backoff; unuploaded content stays in local buffer
- Uploads don't block ptrace loop

**Credentials:** K8s service account (IRSA on EKS, Workload Identity on GKE).

## Local Buffer

Bounded LRU cache:
- Exceed max_size → evict oldest confirmed-uploaded content
- Never evict upload-pending content (pinned)
- Never evict most recent N event segments
- On eviction: delete local file, hash stays in digest cache
- On cache miss: pull from S3 into local buffer

## Durability Modes

Per-path configurable. Controls what's persisted before tracee resumes.

| Mode | Persisted before resume | Risk | Latency |
|------|------------------------|------|---------|
| memory | Hash in supervisor heap only | Supervisor crash | Fastest |
| local (default) | Local CAS file + event segment | Node failure | Sub-ms to ~500ms |
| remote | Confirmed in S3 | None | Seconds to minutes |

```yaml
durability:
  default: local
  overrides:
    - paths: ["/workspace/checkpoints/**", "/workspace/models/**"]
      mode: memory
    - paths: ["*.key", "*.pem", "*.credentials"]
      mode: remote
```

## Configuration

```yaml
storage:
  backend: s3
  bucket: my-agent-argus
  prefix: agents/{agent_id}/
  region: us-west-2
  upload:
    max_concurrent: 4
    retry_max: 5
    retry_backoff_base: 1s
  local_buffer:
    cas_dir: /data/cas
    event_dir: /data/events
    max_size: 2GB
    min_retention: 5m
  digest_cache:
    path: /data/digest-cache.bin
    ttl: 7d
    snapshot_interval: 10m
    rebuild_on_start: true
```

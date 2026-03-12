# P2: Digest Cache

**Status**: not started

**Spec reference**: `docs/spec/03-storage.md` (digest cache section)

## Dependencies
- **Blocked by**: P2-cas (needs ContentHash type)
- **Blocks**: P2-content-capture (uses cache for read dedup)

## Parallelizable with
- All P1 tasks, P2-s3-upload (can develop against trait/interface), P2-pause-resume-api

## What needs to be done
- `crates/sandbox/src/storage/digest_cache.rs`:
  - `DigestCache`: `HashMap<PathBuf, CachedDigest>` tracking known file hashes
  - `CachedDigest`: hash (ContentHash), size (u64), mtime (SystemTime), last_verified (Instant)
  - `lookup(path: &Path) -> Option<&ContentHash>`: check if file still matches (stat mtime+size)
  - `insert(path: &Path, hash: ContentHash, size: u64, mtime: SystemTime)`
  - `invalidate(path: &Path)`: remove entry on known mutation
  - `save_to_disk(path: &Path) -> Result<()>`: serialize to bincode/messagepack
  - `load_from_disk(path: &Path) -> Result<Self>`
  - `load_from_s3(client: &S3Client) -> Result<Self>`: download latest snapshot
  - `save_to_s3(client: &S3Client) -> Result<()>`: upload snapshot
  - TTL: entries expire after 7 days without verification
  - Periodic snapshot: save every N minutes (configurable)

## How to test
```bash
cargo test -p sandbox --lib storage::digest_cache
```
Unit tests: insert+lookup, invalidation, mtime mismatch returns None, TTL expiry, serialization round-trip.

## Branch
- **Branch**: `p2-digest-cache`
- **Target**: `main`

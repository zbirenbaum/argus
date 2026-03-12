# P2: Digest Cache

**Status**: done

**Spec reference**: `docs/spec/03-storage.md` (digest cache section)

## Dependencies
- **Blocked by**: P2-cas (needs ContentHash type)
- **Blocks**: P2-content-capture (uses cache for read dedup)

## What was done
- `crates/argus/src/storage/digest_cache.rs`: full implementation
  - `DigestCache` struct with `HashMap<ContentHash, DigestEntry>` tracking remote-known hashes
  - `DigestEntry` with `size_bytes`, `uploaded_at` (SystemTime), `ttl` (Duration)
  - `DigestCacheStats` aggregate type
  - Methods: `new`, `contains`, `insert`, `insert_with_ttl`, `remove`, `prune_expired`, `len`, `is_empty`, `stats`, `save_to_disk`, `load_from_disk`
  - Atomic writes (write temp + rename) for crash safety
  - bincode serialization for compact disk format
- `crates/argus/src/storage/mod.rs`: added module and re-exports
- `crates/argus/Cargo.toml`: added `bincode = "1"` dependency

## What works
- Insert/lookup with TTL expiry
- Prune expired entries
- Save/load round-trip via bincode
- Atomic disk writes
- Stats computation (total entries, bytes, expired count)
- 9 unit tests all passing

## What's missing
- S3 download/upload of cache snapshots (deferred to P2-s3-upload task)
- Periodic snapshot timer (will be wired in supervisor integration)
- Configurable snapshot interval (will come from config module)

## How to test
```bash
cargo test -p argus -- digest_cache
```

## Branch
- **Branch**: `p2-digest-cache`
- **Target**: `main`

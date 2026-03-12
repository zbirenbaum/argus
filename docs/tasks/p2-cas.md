# P2: Content-Addressable Storage

**Status**: done

**Spec reference**: `docs/spec/03-storage.md` (CAS section)

## Dependencies
- **Blocked by**: nothing — pure data structure + filesystem, no tracer dependency
- **Blocks**: P2-digest-cache, P2-content-capture, P2-write-locking, P2-tls-content, P2-event-segments

## Parallelizable with
- ALL P1 tasks — can start immediately

## What was done
- `crates/sandbox/src/cas/hash.rs` — `ContentHash` newtype (SHA-256, 64-char hex), Display, Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, `from_data`, `as_str`, `prefix`, `suffix`
- `crates/sandbox/src/cas/stats.rs` — `CasStats` (atomic counters), `CasStatsSnapshot` (serializable snapshot)
- `crates/sandbox/src/cas/store.rs` — `CasStore` with `new`, `store`, `exists`, `read`, `delete`, `object_path`, `stats`; atomic writes via temp-file + fsync + rename
- `crates/sandbox/src/cas/mod.rs` — module re-exports
- `crates/sandbox/Cargo.toml` — added `tempfile` dev-dependency

## What works
- SHA-256 hashing with deterministic output
- Content-addressed storage at `{root}/{hash[0:2]}/{hash[2:]}`
- Atomic writes (temp file, fsync, rename)
- Dedup: second store of same content skips write, stats not double-counted
- Read, exists, delete operations
- Concurrent store of same content is safe (no corruption)
- Stats tracking (total objects/bytes, cumulative adds)
- Full serde round-trip for `ContentHash`

## What's missing
- Nothing — all spec requirements implemented

## How to test
```bash
docker exec -w /workspaces/argus-run silly_snyder cargo test -p sandbox --lib cas
```

## Branch
- **Branch**: `p2-cas`
- **Target**: `main`

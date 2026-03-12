# P2: Content-Addressable Storage

**Status**: not started

**Spec reference**: `docs/spec/03-storage.md` (CAS section)

## Dependencies
- **Blocked by**: nothing — pure data structure + filesystem, no tracer dependency
- **Blocks**: P2-digest-cache, P2-content-capture, P2-write-locking, P2-tls-content, P2-event-segments

## Parallelizable with
- ALL P1 tasks — can start immediately

## What needs to be done
- `crates/sandbox/src/cas/mod.rs`:

### Hasher
- `hash_content(data: &[u8]) -> ContentHash`: SHA-256, return hex string
- `ContentHash` newtype: 64-char hex string with Display, Serialize, Deserialize

### Local Store
- `CasStore`: manages `/data/cas/{hash[0:2]}/{hash[2:]}` on disk
- `store(data: &[u8]) -> Result<ContentHash>`: hash, write if not exists (atomic rename), return hash
- `exists(hash: &ContentHash) -> bool`
- `read(hash: &ContentHash) -> Result<Vec<u8>>`
- `delete(hash: &ContentHash) -> Result<()>`: for LRU eviction
- Atomic writes: write to temp file, fsync, rename into place
- Dedup: if file already exists at hash path, skip write

### Stats
- Track: total objects, total bytes, objects added since start

## How to test
```bash
cargo test -p sandbox --lib cas
```
Unit tests: hash determinism, store + read round-trip, dedup (store same content twice = one file), atomic write (concurrent stores don't corrupt), stats tracking.

## Branch
- **Branch**: `p2-cas`
- **Target**: `main`

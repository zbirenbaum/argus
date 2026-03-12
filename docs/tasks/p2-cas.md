# P2: Content-Addressable Storage

**Status**: done

**Spec reference**: `docs/spec/03-storage.md` (CAS section)

## Dependencies
- **Blocked by**: nothing — pure data structure + filesystem, no tracer dependency
- **Blocks**: P2-digest-cache, P2-content-capture, P2-write-locking, P2-tls-content, P2-event-segments

## Parallelizable with
- ALL P1 tasks — can start immediately

## What was done
- `crates/argus/src/cas/hash.rs` — `ContentHash` newtype (SHA-256, 64-char hex), Display, Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, `from_data`, `as_str`, `prefix`, `suffix`
- `crates/argus/src/cas/stats.rs` — `CasStats` (atomic counters), `CasStatsSnapshot` (serializable snapshot)
- `crates/argus/src/cas/store.rs` — `LocalCas` with `new`, `delete`, `object_path`, `stats`; atomic writes via temp-file + fsync + rename; implements `Cas` trait (`put`, `get`, `exists`)
- `crates/argus/src/cas/traits.rs` — `Cas` trait (provider-agnostic CAS contract: `get`, `put`, `exists`), blanket impl for `Arc<T: Cas>`
- `crates/argus/src/cas/memory.rs` — `MemoryCas` (test-only, `RwLock<HashMap>`), implements `Cas`
- `crates/argus/src/cas/remote.rs` — `RemoteCas` (S3-backed), implements `Cas`
- `crates/argus/src/cas/tiered.rs` — `TieredCas` (local-first with remote read-through + backfill), implements `Cas`
- `crates/argus/src/cas/mod.rs` — module re-exports
- All consumers use `&impl Cas` instead of `&LocalCas` — provider-agnostic signatures

## What works
- SHA-256 hashing with deterministic output
- Content-addressed storage at `{root}/{hash[0:2]}/{hash[2:]}`
- Atomic writes (temp file, fsync, rename)
- Dedup: second store of same content skips write, stats not double-counted
- Read, exists, delete operations
- Concurrent store of same content is safe (no corruption)
- Stats tracking (total objects/bytes, cumulative adds)
- Full serde round-trip for `ContentHash`
- Tiered CAS: local-first reads with remote fallback + backfill
- All free functions accept `&impl Cas` — testable with `MemoryCas`

## What's missing
- Large file chunking (4MB Rabin fingerprint) tracked separately per spec

## Future: CachedCas composition

The current `TieredCas` is a stepping stone. The end-state design is a generic
`CachedCas<Front, Back>` that composes any two backends:

```rust
trait CasBackend: Cas {
    fn delete(&self, hash: &ContentHash) -> Result<()>;
    fn stats(&self) -> BackendStats;
}

struct CachedCas<Front: CasBackend, Back: CasBackend> {
    front: Arc<Front>,
    back: Arc<Back>,
}
```

Composition stacks naturally:
- **Production**: `CachedCas<MemoryCas, CachedCas<LocalCas, RemoteCas>>`
- **Dev**: `CachedCas<MemoryCas, LocalCas>`
- **Test**: `MemoryCas`

Each layer has the same interface. Eviction, flush policy, and digest tracking
live in `CachedCas`. The ptrace thread and upload workers each hold a clone
(two Arc bumps). No mutex needed — `MemoryCas` has internal `RwLock`, `LocalCas`
writes are atomic (tmp + rename), S3 puts are inherently concurrent.

**Don't build this now.** Come back to it when you need a third tier or want to
test `StoragePipeline` without disk.

## How to test
```bash
docker exec argus-x86 cargo test -p argus --lib cas
```

## Branch
- **Branch**: `main`

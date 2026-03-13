# Code Quality Analysis — argus-run

Generated via static analysis + manual audit.

## Audit Results (post-investigation)

|-|Count|Verdict|Details|
|-|-|-|-|
|**Arc<Mutex<>>**|2 files|🔴 Delete|write_capture.rs and write_locks.rs are **dead code** — active pipeline uses DashMap + tokio::sync::Mutex in capture.rs|
|**std::sync::Mutex in async**|6 files|✅ OK|Audit confirmed: all locks held in synchronous scopes only, no .await violations|
|**Production .unwrap()/.expect()**|27 calls|✅ OK|3 provably safe (hardcoded parse, range-bounded, guarded iter), 24 justified (mutex poison, unrecoverable syscall)|
|**.clone() hotspots**|165 total|⚠️ Fix 3 files|runner.rs:157 (tree clone before Arc), runner.rs:99-125 (string clones in approval), diff.rs:46-48 (iterator clones)|
|**Clippy warnings**|25 total|⚠️ Fix|16 collapsible ifs, 2 &PathBuf→&Path, 2 map_or, 1 redundant closure, 1 derivable impl, 1 useless conversion, 1 boolean simplification|
|**fn(String) params**|7 cases|✅ OK|Most are Into<String> or stored as owned — correct|
|**Box<dyn>**|1|✅ OK|BoxFuture alias — acceptable|
|**String fmt in pipeline**|60|✅ OK|All at serialization boundary (stamp.rs: PathBuf→String, ContentHash→String for events). Not optimizable without event schema change.|
|**ContentHash is a String**|pervasive|⚠️ Refactor|ContentHash stores `canonical: String` — every clone heap-allocates ~72 bytes. Should store `[u8; 32]` + `HashAlgorithm` and format on demand.|

## Action Items

### 1. Delete dead code: write_capture.rs + write_locks.rs
**Status:** Dead code. Active pipeline (capture.rs) uses `DashMap<PathBuf, tokio::sync::Mutex<()>>`.

The old modules assumed capture happened in the ptrace thread. It doesn't — capture is an async stage that sends `ReadMemory` directives back to the ptrace thread via `PtraceHandle`. The DashMap + tokio::sync::Mutex is correct for async and ready for future concurrent captures.

Delete:
- `crates/argus/src/state/write_capture.rs`
- `crates/argus/src/state/write_locks.rs`
- Remove from `crates/argus/src/state/mod.rs`
- Remove any imports/references

### 2. Clone reduction in runner.rs (hot path)
**Line 157:** Clones entire MerkleTree then wraps in Arc. Should wrap in Arc first, clone the Arc.

**Lines 99–125:** 5+ string clones per pause-before-action in approval path. Use Cow or extend lifetimes to reduce allocation churn.

### 3. Clone reduction in snapshot/diff.rs
**Lines 46, 48:** ContentHash clones inside iterator collect. Could use references or restructure to avoid copying.

### 4. Clone reduction in snapshot/tree.rs
**Line 248:** ContentHash clone in build_dir_tree loop (per-file).
**Lines 342, 346:** ContentHash clones in walk_tree_object fallback.

### 5. Clippy auto-fixes
Run `cargo clippy --fix --workspace` for mechanical fixes:
- 16 collapsible if statements
- 2 &PathBuf → &Path (capture.rs:127, :147)
- 2 map_or simplifications
- 1 redundant closure
- 1 derivable impl
- 1 useless anyhow::Error conversion
- 1 boolean simplification

### 6. ContentHash: store raw bytes, not String
`ContentHash` stores `canonical: String` (`blake3:abcd1234...`). Every `.clone()` heap-allocates ~72 bytes. Should store:
```rust
struct ContentHash {
    algorithm: HashAlgorithm,
    digest: [u8; 32],
}
```
Format the `algorithm:hex_digest` string on demand in `Display`/`Serialize`/`as_str()`. This eliminates heap allocation from every clone, construction, and comparison. Hashes are compared/stored far more often than serialized.

### NOT doing (with reasons)
- **std::sync::Mutex migration** — audit confirmed no .await violations in any of the 6 files
- **unwrap/expect cleanup** — all 27 production calls are justified or provably safe
- **api/state.rs clones** — API refactor happening in parallel
- **fn(String) params** — correct as-is
- **String formatting in pipeline** — all at serialization boundary (stamp.rs), unavoidable without event schema change

## Architecture Note: Multi-Pipeline Design

The current single-pipeline design will evolve into independent pipelines per event source:

```
PtraceSource → classify → rules → capture_content → stamp → emit
ProxySource  → parse_http → extract_bodies →          stamp → emit
KeylogSource → parse_keylog →                          stamp → emit
```

Shared state (all thread-safe today):
- `SequenceGenerator` (AtomicU64)
- `CAS` (trait, thread-safe by design)
- `EventBus` (broadcast channel — multiple producers)
- `RuleSet` via `ArcSwap` (read-only from pipeline)

Only ptrace pipeline writes to MerkleTree. Proxy/keylog pipelines are stateless transforms.

The DashMap + tokio::sync::Mutex write lock stays in ptrace pipeline only.

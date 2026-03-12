# P2: Per-Path Write Locking

**Status**: not started

**Spec reference**: `docs/spec/01-supervisor.md` (write locking section)

## Dependencies
- **Blocked by**: P1-tracer-loop (needs syscall entry/exit control), P2-cas (before/after hashing)
- **Blocks**: P3-merkle-tree (reliable before/after hashes needed for tree updates)

## Parallelizable with
- P2-s3-upload, P2-digest-cache, P2-pause-resume-api, P2-tls-content

## What needs to be done
- Replace stub `WriteLockManager` in `crates/sandbox/src/state/mod.rs`:
  - On syscall entry (write, rename, unlink, truncate, chmod, link, symlink):
    1. Acquire per-path mutex
    2. Hash file at current state → `before_hash`
    3. Store before_hash, resume tracee with PTRACE_CONT
  - On syscall exit:
    4. Hash file at new state → `after_hash`
    5. Emit event with both hashes
    6. Release mutex
  - For rename: lock both source and destination paths
  - For unlink: before_hash from file, after_hash is None
  - Deadlock prevention: always acquire locks in sorted path order

## How to test
```bash
cargo test -p sandbox --lib state -- --ignored
```
Integration tests:
1. Two concurrent writes to same file — verify serialized, both before/after hashes correct
2. Rename with content verification
3. Unlink captures final hash

## Branch
- **Branch**: `p2-write-locking`
- **Target**: `main`

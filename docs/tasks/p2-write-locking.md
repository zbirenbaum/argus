# P2: Per-Path Write Locking

**Status**: done

**Spec reference**: `docs/spec/01-supervisor.md` (write locking section)

## Dependencies
- **Blocked by**: P1-tracer-loop (needs syscall entry/exit control), P2-cas (before/after hashing)
- **Blocks**: P3-merkle-tree (reliable before/after hashes needed for tree updates)

## What was done

### Phase 1: Mutex-based locking (merged in P2)
- `WriteLocks` per-path mutex registry (`state/write_locks.rs`)
- `CaptureGuard` / `RenameCaptureGuard` with before/after hashing (`state/write_capture.rs`)
- `path_hashes` cache in `TracerLoop` for hash chain continuity

### Phase 2: Per-path write queue (interleaving fix)
- Added `active_writes: HashMap<String, u32>` to `TracerLoop` — tracks which pid is in-kernel writing to each path
- Added `write_wait_queue: HashMap<String, VecDeque<PendingCapture>>` — holds tracees at syscall entry until the active writer finishes
- `try_start_write_capture` / `try_start_open_trunc_capture` now check `active_writes` before resuming; if path is busy, tracee is queued (stays ptrace-stopped)
- `handle_syscall_exit` calls `resume_next_queued_writer` after completing a write — dequeues next tracee, sets its before_hash to the just-completed after_hash, and resumes it
- `cleanup_dead_writer` handles process exit while active or queued
- Different paths are completely unaffected — no contention

### Files changed
- `crates/sandbox/src/tracer/trace_loop.rs` — active_writes, write_wait_queue, resume_next_queued_writer, cleanup_dead_writer
- `crates/sandbox/src/tracer/handlers/mod.rs` — queue-aware try_start_write_capture, try_start_open_trunc_capture
- `tests/concurrent_write.c` — C test: 4 threads × 100 writes with O_TRUNC
- `tests/validate_hash_chain.py` — hash chain validator script
- `docs/spec/12-testing.md` — added Test 7b (hardened interleaving test)

## What works
- Concurrent writes to same path serialized at ptrace level
- Hash chain guaranteed correct: queued writer's before_hash = previous after_hash
- No kernel-level write interleaving possible
- Different files unblocked (independent paths don't contend)
- Dead process cleanup releases locks and resumes queued writers
- 383 unit tests pass (5 new tests for write serialization)

## How to test
```bash
# Unit tests
docker exec argus-arm64 bash -c "cd /workspace && cargo test -p sandbox"

# Validation test 7 (Python threads)
docker exec argus-arm64 bash -c "cd /workspace && ./target/aarch64-unknown-linux-musl/debug/supervisor --agent-id test --storage.backend local-only -- python3 -c '
import threading
def writer(n):
    for i in range(10):
        with open(\"/workspace/shared.txt\", \"w\") as f:
            f.write(f\"writer {n} iteration {i}\n\")
threads = [threading.Thread(target=writer, args=(i,)) for i in range(3)]
for t in threads: t.start()
for t in threads: t.join()
'"

# Validation test 7b (C threads, hardened)
docker exec argus-arm64 bash -c "cd /workspace && gcc -O0 -pthread -o /tmp/concurrent_write tests/concurrent_write.c && ./target/aarch64-unknown-linux-musl/debug/supervisor --agent-id interleave-test --storage.backend local-only -- /tmp/concurrent_write"
```

## Branch
- Merged to `main`

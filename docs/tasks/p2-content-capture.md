# P2: Content Capture (process_vm_readv)

**Status**: not started

**Spec reference**: `docs/spec/03-storage.md` (content capture), `docs/spec/01-supervisor.md` (write locking)

## Dependencies
- **Blocked by**: P1-tracer-loop (integrates into syscall handlers), P2-cas (stores captured content), P2-digest-cache (read dedup)
- **Blocks**: P3-merkle-tree (needs content hashes on events)

## NOT parallelizable with P1-tracer-loop — extends it directly.

## What needs to be done
- Extend `crates/argus/src/tracer/handlers.rs`:
  - On **write()** syscall: capture buffer via process_vm_readv, store in CAS, include hash in Write event
  - On **read()** syscall: capture buffer, check digest cache for dedup, store if new, include hash in Read event
  - On **open()** for writing: prepare for before/after hashing (integrate write lock flow)
  - Stdio/pipe/PTY data: capture buffer content, store in CAS, include hash in StdioData/PipeData/PtyData events
  - Initial state capture: on startup, walk watched paths, hash each file into CAS, emit InitialState events

- Extend `crates/argus/src/tracer/memory.rs`:
  - `read_write_buffer(pid, addr, len) -> Result<Vec<u8>>`: read the buffer being written by tracee

## How to test
```bash
cargo test -p argus --lib tracer -- --ignored
```
Integration tests (Linux + SYS_PTRACE):
1. Trace `echo hello > /tmp/test` — verify Write event has valid after_hash, content retrievable from CAS
2. Trace `cat /etc/hostname` — verify Read event has hash matching file content
3. Trace process writing to stdout — verify StdioData has content hash

## Branch
- **Branch**: `p2-content-capture`
- **Target**: `main`

# P2: TLS Content Capture

**Status**: not started

**Spec reference**: `docs/spec/07-tls-network.md` (keylog capture, mitmdump parsing)

## Dependencies
- **Blocked by**: P1-net-env (mitmdump + env setup), P2-cas (store captured content)
- **Blocks**: nothing directly — additive feature

## Parallelizable with
- P1-config, P1-events, P1-state, P1-seccomp, P1-tracer-loop, P2-s3-upload, P2-pause-resume-api

## What needs to be done
- Extend `crates/sandbox/src/net/mod.rs`:
  - `KeylogWatcher`: watch SSLKEYLOGFILE via inotify, on change read new lines, store in CAS, emit TlsKeys event
  - `MitmdumpParser`: read mitmdump JSON output (--set hardump=... or custom addon), parse into HttpRequest/HttpResponse events
    - Extract: method, URL, status code, request headers, response headers
    - Store request body + response body in CAS
    - Emit HttpRequest event with req_hash, resp_hash
  - Dedup network events: track (fd, timestamp, content_hash) to avoid duplicate capture from both ptrace write() and mitmdump

## How to test
```bash
cargo test -p sandbox --lib net
```
Unit tests: keylog line parsing, HTTP event construction from mitmdump JSON.
Integration test (ignored): mitmdump proxy captures HTTPS request, events emitted with correct hashes.

## Branch
- **Branch**: `p2-tls-content`
- **Target**: `main`

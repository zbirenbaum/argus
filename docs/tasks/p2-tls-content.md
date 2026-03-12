# P2: TLS Content Capture

**Status**: in progress

**Spec reference**: `docs/spec/07-tls-network.md` (keylog capture, mitmdump parsing)

## Dependencies
- **Blocked by**: P1-net-env (mitmdump + env setup), P2-cas (store captured content)
- **Blocks**: nothing directly — additive feature

## Parallelizable with
- P1-config, P1-events, P1-state, P1-seccomp, P1-tracer-loop, P2-s3-upload, P2-pause-resume-api

## What was done
- `crates/sandbox/src/net/keylog.rs` — `KeylogWatcher` reads SSLKEYLOGFILE incrementally, deduplicates by client_random, stores lines in CAS, emits `TlsKeys` events
- `crates/sandbox/src/net/flow_parser.rs` — parses mitmdump addon JSON output (newline-delimited), extracts method/URL/status/headers/bodies, stores headers and base64-decoded bodies in CAS, produces `HttpRequest`/`HttpResponse` events
- `crates/sandbox/src/net/dedup.rs` — `NetworkDedup` tracks `(fd, content_hash)` pairs with time-based expiry to suppress duplicates from ptrace + proxy
- `crates/sandbox/src/events/network.rs` — `TlsKeys`, `HttpRequest`, `HttpResponse` event payload types
- `crates/sandbox/src/net/mod.rs` — updated to re-export new modules
- `crates/sandbox/Cargo.toml` — added `base64` dependency

## What works
- NSS Key Log Format line parsing and validation
- Incremental keylog file reading with offset tracking
- Deduplication by client_random in keylog watcher
- CAS storage of keylog lines with hash references in TlsKeys events
- Mitmdump flow JSON parsing (request + optional response)
- Base64-decoded body storage in CAS
- Header serialization and CAS storage
- HttpRequest/HttpResponse event construction with content hashes
- Network event dedup with time-based expiry window
- 29 unit tests passing, 0 warnings

## What's missing
- inotify-based file watching (currently poll-driven, needs Linux inotify integration)
- Integration with actual mitmdump addon script (script not yet written)
- Integration test with live mitmdump proxy
- Wiring into the supervisor ptrace loop event emission path

## How to test
```bash
cargo test -p sandbox --lib net
```

## Branch
- **Branch**: `p2-tls-content`
- **Target**: `main`

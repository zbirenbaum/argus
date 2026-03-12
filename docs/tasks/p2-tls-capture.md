# TLS Capture Integration

**Status:** done

**Spec reference:** `docs/spec/07-tls-network.md`, `docs/superpowers/plans/2026-03-12-tls-capture-integration.md`

## What was done

- Created `crates/supervisor/src/tls_watcher.rs`: background polling thread that drives `KeylogWatcher` and `FlowWatcher` every 200ms, emitting `TlsKeys`, `HttpRequest`, and `HttpResponse` events through the shared event channel.
- Modified `crates/supervisor/src/main.rs`:
  - Embedded `scripts/argus_addon.py` via `include_str!` so the binary is self-contained.
  - Added second `SequenceGenerator` (starting at 1,000,000) for TLS watcher to avoid collisions with tracer sequences without requiring library changes.
  - Added third `LocalCas` handle for the TLS watcher thread.
  - Spawn tls-watcher thread after mitmdump startup.
  - Shutdown ordering: tls-watcher stops before mitmdump to drain final data.
- Added `UpstreamVerify` config to `TlsConfig` with three modes:
  - `SystemStore` (default): mitmdump verifies upstream certs via OS trust store.
  - `CustomCa(path)`: mitmdump verifies against a specific CA bundle (for private PKI).
  - `Insecure`: skip all upstream verification (dev/test escape hatch).
- Modified `crates/argus/src/net/mitmdump.rs`: plumbed `UpstreamVerify` through to mitmdump command-line args (`ssl_insecure=true` or `ssl_verify_upstream_trusted_ca=<path>`).
- Modified `tests/validate.sh`: replaced test_8 stub with local HTTPS server + assertions for `tls_keys`, `http_request`, and `http_response` events. Uses `upstream_insecure: true` in test config.

## What works

- TLS key material captured from SSLKEYLOGFILE and emitted as `tls_keys` events (35 per test run — multiple TLS sessions through proxy).
- HTTP flow capture via mitmdump addon: `http_request` and `http_response` events with body hashes in CAS.
- Addon script embedded in binary, no external file dependency.
- Three-mode upstream verification: system store, custom CA, insecure.
- Validation test 8 passes with full pipeline (tls_keys + http_request + http_response).
- All existing tests still pass (17/17 unit tests, test 1 validation).

## What's missing

- HTTP request/response capture only activates when mitmdump is installed (by design — graceful degradation).
- Connect events not captured under Rosetta emulation (pre-existing environment limitation, not a regression).

## How to test

```bash
# Unit tests
docker exec argus-x86 cargo test -p argus -p supervisor

# Validation test 8
docker exec argus-x86 ./tests/validate.sh 8
```

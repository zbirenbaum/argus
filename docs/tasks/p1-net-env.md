# P1: TLS/Network Environment Setup

**Status**: not started

**Spec reference**: `docs/spec/07-tls-network.md` (env setup, CA generation), `docs/spec/01-supervisor.md` (startup step 4-6)

## Dependencies
- **Blocked by**: P1-config (needs TlsConfig struct)
- **Blocks**: P1-supervisor-main, P2-tls-content

## Parallelizable with
- P1-events, P1-state, P1-seccomp, P2-cas, P2-s3-upload, P2-digest-cache

## What needs to be done
- `crates/sandbox/src/net/mod.rs`:
  - `generate_ca(ca_dir: &Path) -> Result<()>`: generate self-signed CA cert+key for mitmdump (shell out to openssl or use rcgen crate)
  - `start_mitmdump(ca_dir: &Path, port: u16) -> Result<Child>`: spawn mitmdump as a child process, wait for readiness (probe port)
  - `agent_env_vars(config: &TlsConfig) -> HashMap<String, String>`: build env vars for the agent process:
    - `HTTPS_PROXY=http://127.0.0.1:{port}`
    - `HTTP_PROXY=http://127.0.0.1:{port}`
    - `SSLKEYLOGFILE={keylog_path}`
    - `SSL_CERT_FILE={ca_cert_path}`
    - `REQUESTS_CA_BUNDLE={ca_cert_path}`
    - `NODE_EXTRA_CA_CERTS={ca_cert_path}`
  - `watch_keylog_file(path: &Path) -> impl Stream<Item = Vec<u8>>`: inotify/poll watcher for SSLKEYLOGFILE changes (stub for Phase 1 — just set the env var)

## How to test
```bash
cargo test -p sandbox --lib net
```
Unit tests: env var map contains all required keys, CA generation creates valid cert file.
Integration test (ignored): mitmdump starts and accepts connections.

## Branch
- **Branch**: `p1-net-env`
- **Target**: `main`

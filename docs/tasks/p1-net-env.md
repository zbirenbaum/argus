# P1: TLS/Network Environment Setup

**Status**: done

**Spec reference**: `docs/spec/07-tls-network.md` (env setup, CA generation), `docs/spec/01-supervisor.md` (startup steps 4-6)

## Dependencies
- **Blocked by**: P1-config (needs TlsConfig struct)
- **Blocks**: P1-supervisor-main, P2-tls-content

## Parallelizable with
- P1-events, P1-state, P1-seccomp, P2-cas, P2-s3-upload, P2-digest-cache

## What was done
- `crates/sandbox/src/net/mod.rs` — module re-exports for `ca`, `env`, `mitmdump`
- `crates/sandbox/src/net/ca.rs` — self-signed CA generation with `rcgen` (ECDSA P-256, 10-year validity, idempotent)
- `crates/sandbox/src/net/env.rs` — builds 6 env vars (proxy, keylog, cert paths)
- `crates/sandbox/src/net/mitmdump.rs` — spawns `mitmdump` in regular proxy mode, readiness probe, graceful SIGTERM→SIGKILL shutdown
- `crates/sandbox/Cargo.toml` — added `rcgen = "0.13"`, `time = "0.3"`

## What works
- CA generation creates valid PEM cert+key files
- CA generation is idempotent (reuses existing files)
- CA generation creates parent directories
- `agent_env_vars` returns all 6 required keys with correct values
- Proxy URLs use configured port
- Mitmdump spawns, readiness probed, stop/is_running work
- Missing mitmdump gives clear error message
- Drop impl ensures cleanup

## What's missing
- Nothing — all specified functionality is implemented

## How to test
```bash
# In dev container:
cargo test -p sandbox --lib net           # 9 unit tests
cargo test -p sandbox --lib net -- --ignored  # 1 integration test (needs mitmdump)
cargo clippy -p sandbox                   # clean
```

## Branch
- **Branch**: `p1-net-env`
- **Target**: `main`

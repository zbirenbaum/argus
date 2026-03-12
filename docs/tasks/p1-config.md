# P1: Config Module

**Status**: done

**Spec reference**: `docs/spec/01-supervisor.md` (startup sequence, config parsing), `docs/spec/03-storage.md` (durability modes, storage config), `docs/spec/06-agent-controls.md` (pause-before-action rules)

## Dependencies
- **Blocked by**: nothing
- **Blocks**: P1-net-env, P1-supervisor-main, P2-pause-resume-api

## Parallelizable with
- P1-events, P1-state, P1-seccomp, P2-cas, P2-s3-upload, P2-digest-cache, P2-event-segments, P3-indexes

## What was done
- `crates/argus/src/config/mod.rs` — `SupervisorConfig` (top-level), YAML load, validation, defaults
- `crates/argus/src/config/storage.rs` — `StorageConfig`, `S3Config`, `UploadConfig`, `LocalBufferConfig`, `DigestCacheConfig`
- `crates/argus/src/config/durability.rs` — `DurabilityConfig`, `DurabilityMode` enum, `DurabilityOverride`, glob-based path matching
- `crates/argus/src/config/pause_rules.rs` — `PauseRule`, `PauseMatchKind` enum, `PauseAction` enum, glob/binary/destination matching
- `crates/argus/src/config/tls.rs` — `TlsConfig` (ca_dir, keylog_path, mitm_proxy_port)
- `crates/argus/Cargo.toml` — added `serde_yaml`, `glob`, `humantime-serde`, `bytesize` dependencies

## What works
- All structs derive Serialize, Deserialize, Debug, Clone
- YAML parsing via `SupervisorConfig::load(reader)`
- Sensible defaults: data_dir=/data, workspace_dir=/workspace, listen=127.0.0.1:9090, durability=Local
- Validation: agent_id non-empty, command non-empty, data_dir/workspace_dir non-empty, S3 bucket/region non-empty when configured, upload concurrency >= 1
- `DurabilityConfig::mode_for_path()` resolves per-path overrides with first-match-wins semantics
- `PauseRule::matches()` evaluates syscall category + path globs / binary names / destination patterns
- Human-readable durations (`5m`, `7d`) via humantime-serde
- Human-readable sizes (`2GB`) via bytesize
- 34 unit tests covering: defaults, YAML parse, validation errors, serde round-trips, durability mode resolution, pause rule matching

## What's missing
- CLI argument parsing (clap integration) — deferred to P1-supervisor-main where `main.rs` owns the CLI
- Filesystem writability check for data_dir (requires Linux runtime)

## How to test
```bash
cargo test -p argus --lib
```

## Branch
- **Branch**: `p1-config`
- **Target**: `main`

# P1: Config Module

**Status**: not started

**Spec reference**: `docs/spec/01-supervisor.md` (startup sequence, config parsing), `docs/spec/03-storage.md` (durability modes), `docs/spec/06-agent-controls.md` (pause-before-action rules)

## Dependencies
- **Blocked by**: nothing
- **Blocks**: P1-net-env, P1-supervisor-main, P2-pause-resume-api

## Parallelizable with
- P1-events, P1-state, P1-seccomp, P2-cas, P2-s3-upload, P2-digest-cache, P2-event-segments, P3-indexes

## What needs to be done
- `crates/sandbox/src/config/mod.rs` — all configuration structs:
  - `SupervisorConfig`: agent_id, agent_command (Vec<String>), data_dir, workspace_dir, watched_paths, s3 config, listen address, durability mode
  - `S3Config`: bucket, prefix, region, endpoint (optional, for minio)
  - `DurabilityMode`: enum { Memory, Local, Remote } with per-path overrides
  - `PauseRule`: match criteria (syscall type, path glob, binary name, destination) + action (pause/deny)
  - `TlsConfig`: ca_dir, keylog_path, mitm_proxy_port
- Parse from CLI args (clap) + optional TOML/JSON config file
- Validate: agent_id non-empty, command non-empty, data_dir exists or create, workspace_dir exists
- Defaults: data_dir=/data, workspace_dir=/workspace, listen=127.0.0.1:9090, durability=Local

## How to test
```bash
cargo test -p sandbox --lib config
```
Unit tests: parse from defaults, parse with overrides, validation errors for missing fields, durability mode serialization round-trip.

## Branch
- **Branch**: `p1-config`
- **Target**: `main`

# P0: Project Setup

**Status**: done

**Spec reference**: CLAUDE.md (Organization section)

## What was done
- Converted single-crate project to Cargo workspace with three crates:
  - `crates/argus/` — library crate with all logic modules
  - `crates/supervisor/` — binary crate (PID 1 entrypoint)
  - `crates/cli/` — binary crate (HTTP client to supervisor API)
- Created module stubs for all argus submodules: config, events, state, cas, storage, tracer, snapshot, index, net, api
- Set up workspace dependencies with correct versions
- Removed old `src/main.rs`

## What works
- Workspace structure matches CLAUDE.md spec
- All module stubs compile (on Linux — macOS will fail due to nix/ptrace deps)

## What's missing
- Nothing — this is scaffolding only

## How to test
```bash
# Inside dev container (Linux only):
cargo check --workspace
```

## Branch
- **Branch**: `main` (committed directly)
- **Target**: n/a

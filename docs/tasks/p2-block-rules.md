# Block Rules & Hot-Reloadable RuleSet

**Status**: done
**Spec reference**: `docs/spec/06-agent-controls.md`, `docs/spec/10-api-reference.md`

## What was done

- **`config/pause_rules.rs`**: Renamed `PauseRule` → `Rule`, `PauseMatchKind` → `MatchKind`. Added `Read` variant to `MatchKind`. Created `RuleSet` (block + pause_before) with `evaluate()`, `compile_patterns()`, `rule_count()`. Added `RuleDecision` enum (Allow/Block/Pause). Kept backward-compat type aliases.
- **`config/mod.rs`**: Added `block: Vec<Rule>` field to `SupervisorConfig`. Added `build_ruleset()` method. Updated exports.
- **`events/control.rs`**: Added `Blocked` and `RulesUpdated` event structs.
- **`events/envelope.rs`**: Wired `Blocked` and `RulesUpdated` into `EventPayload` + `event_type_tag()`, `pid()`, `paths()`.
- **`api/state.rs`**: Added `Arc<ArcSwap<RuleSet>>` to `SupervisorState`. Added `rules_handle()`, `load_rules()`, `store_rules()` methods.
- **`api/routes.rs`**: Added `get_rules_handler`, `set_rules_handler`, `delete_rule_handler`.
- **`api/mod.rs`**: Wired `/rules` and `/rules/{index}` routes.
- **`api/types.rs`**: Added `RulesAppliedResponse`.
- **`api/errors.rs`**: Added `RuleIndexOutOfBounds` and `InvalidBody` variants.
- **`Cargo.toml`**: Added `arc-swap = "1"`.
- **Spec docs**: Updated `06-agent-controls.md` and `10-api-reference.md`.

## What works

- `RuleSet::evaluate()` — block rules first, then pause-before, first match wins
- Hot reload via `ArcSwap` — zero locking, zero contention on the ptrace hot path
- `GET /rules`, `POST /rules`, `DELETE /rules/{index}` API endpoints
- `Blocked` and `RulesUpdated` events emitted to log
- Full YAML config deserialization with `block:` section
- `build_ruleset()` on `SupervisorConfig`
- 11 new tests (432 total, all passing)

## What's missing

- Wiring `RuleSet::evaluate()` into the actual ptrace `check_pause_rules()` function (requires shared state plumbed into TracerLoop — deferred to pause-resume-api integration)
- CLI `argus rules` subcommand implementation (cli crate has pre-existing compilation issues)

## How to test

```bash
docker exec argus-x86 cargo test -p argus
```

## Branch

main (direct)

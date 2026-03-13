# Enriched Output Pipeline

**Status**: done
**Branch**: worktree-enriched-output-pipeline

## Spec reference
- `docs/superpowers/specs/2026-03-13-enriched-output-pipeline-design.md`

## What was done
- CapturedContent carries raw bytes through pipeline (max_inline_bytes cap)
- Event envelope has `redactions: Vec<Redaction>` audit trail
- File events have `sensitive: bool` flag
- EnrichConfig, RedactConfig, OutputConfig added to SupervisorConfig
- CaptureStage retains bytes; StampStage populates inline fields
- Three-tier RedactStage (path exclusion → field drop → regex scrub)
- Output trait + OutputList fan-out (StdoutOutput, FileOutput)
- DurabilityLayer wraps LocalCas + UploadPool + DigestCache
- Runtime rewired: CaptureStage uses DurabilityLayer, Runner flows events through stamp → redact → outputs + bus
- StdoutSink removed (replaced by StdoutOutput)
- Vector sidecar config example in deploy/demo/

## What works
- All enrichment stages functional
- Redaction with audit trail
- Configurable outputs (stdout, file)
- DurabilityLayer for CAS persistence
- 620 unit tests passing
- Validation tests 1-7b passing

## What's missing
- UnixSocket output (not yet implemented, skipped with warning)
- Http output (not yet implemented, skipped with warning)
- Full RecordBus removal (EventLogSink, IndexSink, BroadcastSink still on bus)
- DigestCache DashMap migration (tracked as separate task)
- Validation tests 8-10 are pre-existing failures unrelated to this work

## How to test
```bash
docker exec -w /build/.claude/worktrees/enriched-output-pipeline argus-arm64 cargo test --target aarch64-unknown-linux-musl -p argus
docker exec -w /build/.claude/worktrees/enriched-output-pipeline argus-arm64 cargo build --target aarch64-unknown-linux-musl -p supervisor
docker exec -w /build/.claude/worktrees/enriched-output-pipeline argus-arm64 ./tests/validate.sh
```

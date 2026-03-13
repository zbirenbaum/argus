---
name: pipeline_sinks_architecture
description: Pipeline sink implementations and key type quirks discovered during implementation
type: project
---

Pipeline sinks live in `crates/argus/src/pipeline/sinks/`. Another agent creates `pipeline/mod.rs`, `pipeline/record.rs` (defines `Record` enum), and `pipeline/sink.rs` (defines `Sink` trait + `SinkPriority`). Sinks import from `crate::pipeline::record::Record` and `crate::pipeline::sink::{Sink, SinkPriority}`.

**Key type constraint**: `UploadPool` contains `mpsc::Receiver<UploadConfirmation>` making it `!Sync`. Therefore `RemoteCasSink` wraps it in `Arc<Mutex<UploadPool>>` rather than `Arc<UploadPool>`.

**EventLog::append signature**: takes `&mut self, event: &Event, upload_pool: Option<&UploadPool>`. The sink passes `None` to decouple segment uploads from the sink write path.

**Index methods**: PathIndex/PidIndex use `.insert(path/pid, seq, event_type)`. TypeIndex uses `.insert(event_type, seq)` — note reversed argument order.

**Why:** Pipeline sinks are parallel work from another agent; they need to be combined before compilation.
**How to apply:** When combining worktree branches, ensure `pipeline/mod.rs` is present and declares `pub mod record; pub mod sink; pub mod sinks;`.

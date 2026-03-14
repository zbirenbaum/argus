// Rust guideline compliant 2026-02-21
//! Test harness: wires a `PipelineRunner` from a `MockPtraceThread`.
//!
//! All stages are real; only the ptrace source is mocked. The `MemorySink`
//! attached to the `RecordBus` captures every emitted event for assertion.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, atomic::AtomicBool};

use arc_swap::ArcSwap;
use dashmap::DashMap;
use nix::unistd::Pid;
use tempfile::TempDir;

use crate::api::state::{SharedState, new_shared_state};
use crate::cas::{Cas, LocalCas, MemoryCas};
use crate::config::{EnrichConfig, RedactConfig, RuleSet, TreeConfig};
use crate::events::SequenceGenerator;
use crate::pipeline::bus::RecordBus;
use crate::pipeline::capture_policy::CapturePolicy;
use crate::pipeline::durability::DurabilityLayer;
use crate::pipeline::mock_ptrace::MockPtraceThread;
use crate::pipeline::outputs::OutputList;
use crate::pipeline::raw_stop::RawSyscallStop;
use crate::pipeline::runner::PipelineRunner;
use crate::pipeline::sink::{Sink, SinkPriority};
use crate::pipeline::sinks::memory::MemorySink;
use crate::pipeline::stages::{CaptureStage, ClassifyStage, PolicyGate, StampStage, TreeStage};
use crate::pipeline::stages::redact::RedactStage;
use crate::state::{FdTable, PipeRegistry, PtyRegistry};

/// Address used as a dummy proxy endpoint — loopback:8080.
const DUMMY_PROXY: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);

/// All resources owned by the harness so TempDir lives for the test duration.
pub struct TestHarness {
    /// Captures every event emitted by the runner.
    pub sink: Arc<MemorySink>,
    /// Shared state for pause/approval assertions.
    pub shared: SharedState,
    /// Ready-to-run pipeline.
    pub runner: PipelineRunner,
    /// Keeps the CAS temp directory alive for the test duration.
    _tmp: TempDir,
}

/// Build a `PipelineRunner` from canned stops with an optional ruleset.
///
/// The `mock` parameter allows callers to seed canned memory reads
/// (e.g. path strings) before calling `into_stream`. All stages are
/// real; only ptrace I/O is mocked.
pub fn build_harness(mock: MockPtraceThread, stops: Vec<RawSyscallStop>, rules: RuleSet) -> TestHarness {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cas_path = tmp.path().join("cas");

    let local_cas = LocalCas::new(cas_path.clone()).expect("LocalCas::new");
    let durability = DurabilityLayer::new(local_cas, None, None);

    let sink = Arc::new(MemorySink::new(SinkPriority::Blocking));
    let sinks: Vec<Arc<dyn Sink>> = vec![Arc::clone(&sink) as Arc<dyn Sink>];
    let bus = RecordBus::new(sinks);

    let api_cas: Arc<dyn Cas> = Arc::new(MemoryCas::new());
    let shared = new_shared_state("test".into(), api_cas, bus.clone());
    shared.store_rules(rules);

    let (ptrace_stream, handle) = mock.into_stream(stops);

    let fd_tables: Arc<DashMap<Pid, FdTable>> = Arc::new(DashMap::new());
    let pipe_registry = Arc::new(parking_lot::Mutex::new(PipeRegistry::new()));
    let pty_registry = Arc::new(parking_lot::Mutex::new(PtyRegistry::new()));
    let file_state = Arc::new(DashMap::new());

    let classify = ClassifyStage::new(
        handle.clone(),
        fd_tables,
        pipe_registry,
        pty_registry,
        false, // transparent_mode: disabled in tests
        DUMMY_PROXY,
        file_state.clone(),
    );

    let rules_swap = shared.rules_handle();
    let policy_gate = PolicyGate::new(handle.clone(), rules_swap, shared.clone());

    let tree_cas = LocalCas::new(cas_path).expect("tree CAS");
    let capture = CaptureStage::new(
        handle,
        durability,
        CapturePolicy::default_full(),
        file_state,
        64 * 1024, // max_inline_bytes
    );
    let tree = TreeStage::new(tree_cas);
    let seq = Arc::new(SequenceGenerator::default());
    let stamp = StampStage::new(seq, "test".into(), EnrichConfig::default());
    let redact = RedactStage::new(&RedactConfig::default());
    let paused = Arc::new(AtomicBool::new(false));

    let runner = PipelineRunner::new(
        ptrace_stream,
        classify,
        policy_gate,
        capture,
        tree,
        stamp,
        bus,
        OutputList::new(),
        redact,
        None,
        paused,
        shared.clone(),
        1, // persist_batch_size=1 so every write persists to CAS
    );

    TestHarness { sink, shared, runner, _tmp: tmp }
}

/// aarch64 syscall numbers — used in stop construction.
///
/// These match the values checked by `handle_entry_aarch64` in `syscall_handlers.rs`.
pub mod nr {
    pub const OPENAT: u64 = libc::SYS_openat as u64;
    pub const WRITE: u64 = libc::SYS_write as u64;
    pub const READ: u64 = libc::SYS_read as u64;
    pub const CLOSE: u64 = libc::SYS_close as u64;
    pub const UNLINKAT: u64 = libc::SYS_unlinkat as u64;
    pub const PIPE2: u64 = libc::SYS_pipe2 as u64;
}

/// Build a `SyscallEntry` stop.
pub fn entry(pid: i32, nr: u64, args: [u64; 6]) -> RawSyscallStop {
    use crate::pipeline::raw_stop::{StopType, SyscallArgs};
    RawSyscallStop {
        pid: Pid::from_raw(pid),
        stop_type: StopType::SyscallEntry {
            syscall_nr: nr,
            args: SyscallArgs::from_array(args),
        },
    }
}

/// Build a `SyscallExit` stop with the given return value.
pub fn exit_stop(pid: i32, nr: u64, retval: i64) -> RawSyscallStop {
    use crate::pipeline::raw_stop::{StopType};
    RawSyscallStop {
        pid: Pid::from_raw(pid),
        stop_type: StopType::SyscallExit { syscall_nr: nr, return_value: retval },
    }
}

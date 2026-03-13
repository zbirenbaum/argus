// Rust guideline compliant 2026-02-21
//! Classified ptrace stop types produced by the classify stage.
//!
//! A [`RawStop`] is the unprocessed event from the ptrace loop. A
//! [`ClassifiedStop`] carries the semantic operation determined by
//! [`ClassifyStage`], along with the PID and [`Classification`] tag.

use nix::unistd::Pid;

/// An unprocessed ptrace stop from the loop thread.
///
/// Placeholder — the real definition comes from the pipeline-ptrace agent.
// TODO: replace with real `RawStop` once pipeline-ptrace agent merges.
#[derive(Debug)]
pub struct RawStop {
    /// Process that produced this stop.
    pub pid: Pid,
}

/// The semantic operation inferred from a raw ptrace stop.
///
/// Placeholder — the real variants will reflect file, process, network, and
/// other operation categories once the classify stage agent lands.
// TODO: replace with real `Classification` variants once classify agent merges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    /// Stop requires no further processing; tracee should be resumed immediately.
    Passthrough,
    /// Stop represents a file mutation or read that needs capture and recording.
    FileOp,
    /// Stop represents a process lifecycle event (fork, exec, exit).
    ProcessEvent,
}

/// A raw stop annotated with its classification and resolved metadata.
///
/// Placeholder — the real definition will carry fd table state, resolved
/// paths, and syscall arguments once the classify stage agent lands.
// TODO: replace with real `ClassifiedStop` once classify agent merges.
#[derive(Debug)]
pub struct ClassifiedStop {
    /// Process that produced this stop.
    pub pid: Pid,
    /// The classification tag.
    pub classification: Classification,
}

/// A [`ClassifiedStop`] after content capture has been performed.
///
/// Placeholder — the real definition will carry content hash, file path,
/// and other captured metadata.
// TODO: replace with real `CapturedStop` once capture stage agent merges.
#[derive(Debug)]
pub struct CapturedStop {
    /// Process that produced this stop.
    pub pid: Pid,
}

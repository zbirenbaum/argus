// Rust guideline compliant 2026-02-21
//! Clap subcommand definitions and argument-building helpers.

use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum Command {
    /// Show agent status.
    Status,
    /// Check supervisor health (K8s readiness probe).
    Health,
    /// Pause all traced processes.
    Pause,
    /// Resume all traced processes.
    Resume,

    /// Query the event log.
    Log {
        /// Time filter: ISO8601 timestamp or duration (5m, 1h, 2d).
        #[arg(long)]
        since: Option<String>,
        /// Filter by exact path.
        #[arg(long)]
        path: Option<String>,
        /// Filter by PID.
        #[arg(long)]
        pid: Option<u32>,
        /// Filter by event type (e.g. write, read, exec).
        #[arg(long, name = "type")]
        event_type: Option<String>,
        /// Maximum events (default 1000).
        #[arg(long, default_value = "1000")]
        limit: u64,
        /// Output raw JSONL instead of human-readable format.
        #[arg(long)]
        json: bool,
    },

    /// Show file modification history.
    History {
        /// Filesystem path to query.
        path: String,
    },

    /// Reconstruct process stdio.
    Stdio {
        /// Process ID.
        pid: u32,
        /// Stream: stdout, stderr, stdin, or all.
        #[arg(long)]
        stream: Option<String>,
        /// Follow output in real time (SSE).
        #[arg(long)]
        follow: bool,
    },

    /// Show shell pipeline stages.
    Pipeline {
        /// Shell process ID.
        shell_pid: u32,
    },

    /// Show the process tree.
    ProcessTree {
        /// Root PID (default: init process).
        #[arg(long)]
        root: Option<u32>,
        /// Include stdio snippets.
        #[arg(long)]
        stdio: bool,
        /// Maximum tree depth.
        #[arg(long)]
        depth: Option<u32>,
    },

    /// Print CAS object content.
    Cat {
        /// Content hash (hex SHA-256).
        hash: String,
        /// Output raw bytes instead of UTF-8 text.
        #[arg(long)]
        raw: bool,
    },

    /// Show diff between content versions or tree snapshots.
    Diff {
        /// Before content hash (for content diff).
        before: Option<String>,
        /// After content hash (for content diff).
        after: Option<String>,
        /// From sequence number (for tree diff).
        #[arg(long)]
        from: Option<u64>,
        /// To sequence number (for tree diff).
        #[arg(long)]
        to: Option<u64>,
    },

    /// Show filesystem snapshot at a point in time.
    Snapshot {
        /// Sequence number.
        #[arg(long)]
        seq: Option<u64>,
        /// Path prefix filter.
        #[arg(long)]
        path: Option<String>,
    },

    /// Restore filesystem to a point in time.
    Restore {
        /// Restore to this ISO8601 timestamp.
        #[arg(long)]
        timestamp: Option<String>,
        /// Restore to this sequence number.
        #[arg(long)]
        seq: Option<u64>,
        /// Target directory (new_directory mode).
        #[arg(long)]
        target: Option<String>,
        /// Restore in place.
        #[arg(long)]
        in_place: bool,
        /// Force in-place restore.
        #[arg(long)]
        force: bool,
        /// Selective restore of a single path.
        #[arg(long)]
        path: Option<String>,
    },

    /// Undo recent writes.
    Undo {
        /// Undo last N writes.
        #[arg(long)]
        last: Option<u64>,
        /// Undo last writes by this PID.
        #[arg(long)]
        last_by_pid: Option<u32>,
    },

    /// List network connections.
    Connections {
        /// Filter by PID.
        #[arg(long)]
        pid: Option<u32>,
        /// Show only active connections.
        #[arg(long)]
        active: bool,
    },

    /// Show storage status.
    StorageStatus,

    /// Manage pause-before-action and block rules.
    Rules {
        #[command(subcommand)]
        action: Option<RulesCommand>,
    },

    /// List pending approval requests.
    Approvals,

    /// Approve a pending action.
    Approve {
        /// Action ID to approve.
        action_id: String,
    },

    /// Deny a pending action.
    Deny {
        /// Action ID to deny.
        action_id: String,
    },

    /// Dump checkpoint data.
    DumpCheckpoint {
        /// Sequence number.
        #[arg(long)]
        seq: u64,
        /// Output format (json).
        #[arg(long, default_value = "json")]
        format: String,
    },

    /// List agents (cross-agent query, reads S3).
    Agents {
        /// S3 bucket name.
        #[arg(long)]
        bucket: Option<String>,
    },

    /// Cross-agent timeline (streaming JSONL).
    Timeline {
        /// Comma-separated agent IDs.
        #[arg(long)]
        agents: String,
        /// Time filter: ISO8601 or duration (5m, 1h, 2d).
        #[arg(long)]
        since: Option<String>,
        /// Filter by event type.
        #[arg(long, name = "type")]
        event_type: Option<String>,
    },

    /// Cross-agent write/read correlation.
    Correlate {
        /// Agent that performed writes.
        #[arg(long)]
        write_agent: String,
        /// Agent that performed reads.
        #[arg(long)]
        read_agent: String,
        /// Resource glob filter (default: *).
        #[arg(long)]
        resource: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RulesCommand {
    /// Replace the entire ruleset from a JSON file.
    Set {
        /// Path to the rules JSON file.
        #[arg(long)]
        file: PathBuf,
    },
    /// Remove a rule by index.
    Remove {
        /// Rule index to remove.
        index: u64,
    },
}


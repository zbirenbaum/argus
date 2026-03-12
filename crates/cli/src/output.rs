// Rust guideline compliant 2026-02-21
//! Human-readable output formatting for CLI responses.
//!
//! Each function writes to stdout. JSON mode is handled at the call
//! site by printing the raw response instead of calling these.

use std::io::{self, Write};

use argus::api::types::{
    ApproveResponse, DenyResponse, PauseResponse, PendingApprovalsResponse,
    ResumeResponse, StatusResponse,
};

use crate::types::{
    AgentsResponse, ConnectionsResponse, CorrelationResponse,
    FileHistoryResponse, PipelineResponse, ProcessTreeNode, RestoreResponse,
    RulesAppliedResponse, RulesResponse, StorageStatusResponse,
    StdioResponse, TreeDiffResponse, TreeResponse,
};

pub fn print_status(r: &StatusResponse) {
    println!("Status:  {}", r.status);
    println!("Agent:   {}", r.agent_id);
    println!("Uptime:  {:.1}s", r.uptime_seconds);
    println!("Events:  {}", r.event_count);
    if !r.processes.is_empty() {
        println!("Processes:");
        for p in &r.processes {
            println!("  PID {:<6} {:20} {}", p.pid, p.binary, p.state);
        }
    }
}

pub fn print_pause(r: &PauseResponse) {
    println!("Agent {}", r.status);
    for p in &r.stopped_processes {
        println!("  stopped PID {} {}", p.pid, p.binary);
    }
}

pub fn print_resume(r: &ResumeResponse) {
    println!("Agent {} ({} processes)", r.status, r.resumed_count);
}

pub fn print_events_human(jsonl: &str) {
    let out = io::stdout();
    let mut w = out.lock();
    for line in jsonl.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            let seq = v.get("seq").and_then(|s| s.as_u64()).unwrap_or(0);
            let ts = v.get("ts_wall").and_then(|s| s.as_str()).unwrap_or("?");
            let ts_short = &ts[..ts.len().min(19)];
            let etype = v.get("type").and_then(|s| s.as_str()).unwrap_or("?");
            let pid = v.get("pid").and_then(|p| p.as_u64());
            let path = v.get("path").and_then(|p| p.as_str());
            let _ = write!(w, "[{seq}] {ts_short} {etype}");
            if let Some(p) = pid {
                let _ = write!(w, " pid={p}");
            }
            if let Some(p) = path {
                let _ = write!(w, " {p}");
            }
            let _ = writeln!(w);
        }
    }
}

pub fn print_file_history(r: &FileHistoryResponse) {
    println!("{}", r.path);
    for e in &r.events {
        let hash_info = match (&e.before_hash, &e.after_hash) {
            (Some(b), Some(a)) => format!("{}..{}", &b[..8.min(b.len())], &a[..8.min(a.len())]),
            (None, Some(a)) => format!("→{}", &a[..8.min(a.len())]),
            _ => String::new(),
        };
        println!(
            "  [{seq}] {ty} {hash} pid={pid} {ts}",
            seq = e.seq,
            ty = e.event_type,
            hash = hash_info,
            pid = e.pid,
            ts = &e.ts_wall[..e.ts_wall.len().min(19)],
        );
    }
}

pub fn print_stdio(r: &StdioResponse) {
    println!("PID {} {} {:?}", r.pid, r.binary, r.argv);
    if let Some(code) = r.exit_code {
        println!("Exit code: {code}");
    }
    if let Some(ref s) = r.stdout {
        print!("{s}");
    }
    if let Some(ref s) = r.stderr {
        eprint!("{s}");
    }
}

pub fn print_pipeline(r: &PipelineResponse) {
    println!("Shell PID {}", r.shell_pid);
    for (i, stage) in r.stages.iter().enumerate() {
        let sep = if i + 1 < r.stages.len() { " |" } else { "" };
        println!(
            "  [{pid}] {bin} {argv}{sep}",
            pid = stage.pid,
            bin = stage.binary,
            argv = stage.argv.join(" "),
        );
    }
    if !r.pipes.is_empty() {
        println!("Pipes:");
        for p in &r.pipes {
            println!(
                "  inode={} {}→{} ({} bytes)",
                p.inode, p.writer_pid, p.reader_pid, p.bytes
            );
        }
    }
}

pub fn print_process_tree(node: &ProcessTreeNode, depth: usize) {
    let indent = "  ".repeat(depth);
    println!(
        "{indent}[{pid}] {bin} {argv}",
        pid = node.pid,
        bin = node.binary,
        argv = node.argv.join(" "),
    );
    if let Some(ref out) = node.stdout {
        if !out.is_empty() {
            for line in out.lines().take(5) {
                println!("{indent}  stdout: {line}");
            }
        }
    }
    for child in &node.children {
        print_process_tree(child, depth + 1);
    }
}

pub fn print_tree(r: &TreeResponse) {
    println!("Tree {} seq={}", &r.tree_hash[..12.min(r.tree_hash.len())], r.seq);
    for e in &r.entries {
        println!(
            "  {mode:>6o} {ty:<4} {hash} {size:>8} {name}",
            mode = e.mode,
            ty = e.entry_type,
            hash = &e.hash[..12.min(e.hash.len())],
            size = e.size,
            name = e.name,
        );
    }
}

pub fn print_tree_diff(r: &TreeDiffResponse) {
    println!("Diff seq {}..{}", r.from_seq, r.to_seq);
    for e in &r.added {
        println!("  + {} ({} bytes)", e.path, e.size);
    }
    for e in &r.modified {
        println!(
            "  M {} {}..{}",
            e.path,
            &e.before_hash[..8.min(e.before_hash.len())],
            &e.after_hash[..8.min(e.after_hash.len())]
        );
    }
    for e in &r.deleted {
        println!("  - {} ({} bytes)", e.path, e.size);
    }
}

pub fn print_restore(r: &RestoreResponse) {
    println!("Restored to seq={} ts={}", r.restored_to_seq, r.restored_to_ts);
    println!(
        "  {} files, {} bytes",
        r.files_restored, r.bytes_restored
    );
    println!("  pre-restore snapshot: seq={}", r.pre_restore_snapshot_seq);
}

pub fn print_connections(r: &ConnectionsResponse) {
    if r.connections.is_empty() {
        println!("No connections");
        return;
    }
    for c in &r.connections {
        let tls = if c.tls { "tls" } else { "plain" };
        let active = if c.active { "active" } else { "closed" };
        let sni = c.sni.as_deref().unwrap_or("-");
        println!(
            "  PID {pid} fd={fd} {ty} {addr}:{port} sni={sni} \
             {sent}/{recv} {tls} {active}",
            pid = c.pid,
            fd = c.fd,
            ty = c.conn_type,
            addr = c.dest_addr,
            port = c.dest_port,
            sent = c.bytes_sent,
            recv = c.bytes_received,
        );
    }
}

pub fn print_storage_status(r: &StorageStatusResponse) {
    println!("Local buffer:");
    println!("  CAS: {} objects ({} bytes)", r.local_buffer.cas_objects, r.local_buffer.cas_size_bytes);
    println!("  Events: {} segments, {} pending", r.local_buffer.events_segments_local, r.local_buffer.pending_uploads);
    println!("Remote:");
    println!("  Backend: {} bucket={}", r.remote.backend, r.remote.bucket);
    println!("  CAS: {} objects, events: {} segments", r.remote.cas_objects_known, r.remote.events_segments_uploaded);
    println!("Digest cache:");
    println!("  {} entries, ttl={}s", r.digest_cache.entries, r.digest_cache.ttl);
    if let Some(ref ts) = r.digest_cache.last_snapshot_uploaded {
        println!("  last snapshot: {ts}");
    }
}

pub fn print_rules(r: &RulesResponse) {
    if !r.block.is_empty() {
        println!("Block rules:");
        for (i, rule) in r.block.iter().enumerate() {
            println!("  [{i}] {rule}");
        }
    }
    if !r.pause_before.is_empty() {
        println!("Pause-before rules:");
        for (i, rule) in r.pause_before.iter().enumerate() {
            let idx = r.block.len() + i;
            println!("  [{idx}] {rule}");
        }
    }
    if r.block.is_empty() && r.pause_before.is_empty() {
        println!("No rules configured");
    }
}

pub fn print_rules_applied(r: &RulesAppliedResponse) {
    println!("Applied: {} ({} rules)", r.applied, r.rule_count);
}

pub fn print_approvals(r: &PendingApprovalsResponse) {
    if r.pending.is_empty() {
        println!("No pending approvals");
        return;
    }
    for a in &r.pending {
        let path = a.path.as_deref().unwrap_or("-");
        println!(
            "  [{id}] pid={pid} {proc} {syscall} {path} ({rule}) {ts}",
            id = a.action_id,
            pid = a.pid,
            proc = a.process,
            syscall = a.syscall,
            rule = a.rule_matched,
            ts = a.timestamp,
        );
    }
}

pub fn print_approve(r: &ApproveResponse) {
    println!("Approved {} (pid {})", r.action_id, r.pid);
}

pub fn print_deny(r: &DenyResponse) {
    println!("Denied {} (pid {}, injected {})", r.action_id, r.pid, r.injected_errno);
}

pub fn print_agents(r: &AgentsResponse) {
    for a in &r.agents {
        let last = a.last_event.as_deref().unwrap_or("-");
        println!(
            "  {id} node={node} pod={pod} started={started} last={last}",
            id = a.agent_id,
            node = a.node,
            pod = a.pod,
            started = a.started,
        );
    }
}

pub fn print_correlations(r: &CorrelationResponse) {
    for c in &r.correlations {
        println!(
            "  {resource}: {w_agent}@{w_seq} → {r_agent}@{r_seq} ({latency}ms)",
            resource = c.resource,
            w_agent = c.write.agent_id,
            w_seq = c.write.seq,
            r_agent = c.read.agent_id,
            r_seq = c.read.seq,
            latency = c.latency_ms,
        );
    }
}

// Rust guideline compliant 2026-02-21
//! Stamp stage: converts captured events into structured `Event` records.
//!
//! Maps each `Classification` variant to the corresponding `EventPayload`
//! variant, fills in sequence numbers, timestamps, and the optional
//! Merkle-tree root hash.

use std::sync::Arc;

use tracing::event;
use tracing::Level;

use crate::cas::ContentHash;
use crate::events::envelope::{Event, EventPayload, SequenceGenerator, timestamp_pair};
use crate::events::{file as ef, io as eio, network as en, process as ep};
use crate::pipeline::captured::{CapturedContent, CapturedEvent};
use crate::pipeline::classified::{Classification, PipeDirection, PtyDataType, StdioType};

/// Stage that stamps captured events with sequence numbers and timestamps.
pub struct StampStage {
    pub seq_gen: Arc<SequenceGenerator>,
    pub agent_id: String,
}

impl StampStage {
    /// Create a new stamp stage.
    pub fn new(seq_gen: Arc<SequenceGenerator>, agent_id: String) -> Self {
        Self { seq_gen, agent_id }
    }

    /// Convert a `CapturedEvent` into a full structured `Event`.
    pub fn stamp(&self, captured: CapturedEvent, tree_hash: Option<ContentHash>) -> Option<Event> {
        let pid = captured.pid.as_raw() as u32;
        let tree_str = tree_hash.map(|h| h.to_string());
        let payload = to_payload(pid, captured.classification, captured.content, tree_str)?;
        let evt = self.make_event(payload);
        event!(
            name: "pipeline.stamp",
            Level::DEBUG,
            event.seq = evt.seq,
            event.type_ = evt.payload.event_type_tag(),
            pid,
            "stamped event",
        );
        Some(evt)
    }

    /// Build a `Blocked` event for a rule-blocked syscall.
    pub fn stamp_blocked(
        &self,
        pid: u32,
        syscall: String,
        path: Option<String>,
        description: String,
    ) -> Event {
        use crate::events::control;
        let payload = EventPayload::Blocked(control::Blocked {
            pid,
            syscall,
            path,
            rule: description,
        });
        self.make_event(payload)
    }

    fn make_event(&self, payload: EventPayload) -> Event {
        let (ts_monotonic, ts_wall) = timestamp_pair();
        Event {
            seq: self.seq_gen.next_seq(),
            ts_monotonic,
            ts_wall,
            agent_id: self.agent_id.clone(),
            vclock: None,
            payload,
        }
    }
}

/// Map a `Classification` and `CapturedContent` to an `EventPayload`.
///
/// Returns `None` for `Passthrough` and unknown variants.
fn to_payload(
    pid: u32,
    cls: Classification,
    content: CapturedContent,
    tree_hash: Option<String>,
) -> Option<EventPayload> {
    match cls {
        Classification::FileWrite { path, fd, .. } => {
            let (before_hash, after_hash, size) = match content {
                CapturedContent::FileWrite { before_hash, after_hash, size, .. } => {
                    (before_hash.map(|h| h.to_string()), after_hash.map(|h| h.to_string()), size)
                }
                _ => (None, None, 0),
            };
            Some(EventPayload::Write(ef::Write {
                pid,
                path: path.to_string_lossy().into(),
                fd,
                offset: 0,
                size: size as u64,
                before_hash,
                after_hash,
                tree_hash,
                data: None,
                encoding: None,
            }))
        }
        Classification::FileRead { path, fd, len, .. } => {
            let (content_hash, size) = match content {
                CapturedContent::FileRead { content_hash, size, .. } => {
                    (content_hash.map(|h| h.to_string()), size)
                }
                _ => (None, len),
            };
            Some(EventPayload::Read(ef::Read {
                pid,
                path: path.to_string_lossy().into(),
                fd,
                offset: 0,
                size: size as u64,
                content_hash,
                data: None,
                encoding: None,
            }))
        }
        Classification::FileRename { old_path, new_path } => {
            Some(EventPayload::Rename(ef::Rename {
                pid,
                old_path: old_path.to_string_lossy().into(),
                new_path: new_path.to_string_lossy().into(),
                tree_hash,
            }))
        }
        Classification::FileUnlink { path } => {
            let content_hash = match content {
                CapturedContent::FileDelete { content_hash, .. } => content_hash.map(|h| h.to_string()),
                _ => None,
            };
            Some(EventPayload::Unlink(ef::Unlink {
                pid,
                path: path.to_string_lossy().into(),
                content_hash,
                tree_hash,
                data: None,
                encoding: None,
            }))
        }
        Classification::FileMkdir { path } => {
            Some(EventPayload::Mkdir(ef::Mkdir {
                pid,
                path: path.to_string_lossy().into(),
                tree_hash,
            }))
        }
        Classification::FileRmdir { path } => {
            Some(EventPayload::Rmdir(ef::Rmdir {
                pid,
                path: path.to_string_lossy().into(),
                tree_hash,
            }))
        }
        Classification::FileChmod { path, mode } => {
            Some(EventPayload::Chmod(ef::Chmod {
                pid,
                path: path.to_string_lossy().into(),
                old_mode: 0,
                new_mode: mode,
            }))
        }
        Classification::FileTruncate { path, len } => {
            Some(EventPayload::Truncate(ef::Truncate {
                pid,
                path: path.to_string_lossy().into(),
                old_size: 0,
                new_size: len,
                before_hash: None,
                after_hash: None,
                tree_hash,
                before_data: None,
                after_data: None,
                encoding: None,
            }))
        }
        Classification::FileLink { target, link_path } => {
            Some(EventPayload::Link(ef::Link {
                pid,
                target: target.to_string_lossy().into(),
                link_path: link_path.to_string_lossy().into(),
                tree_hash,
            }))
        }
        Classification::FileSymlink { target, link_path } => {
            Some(EventPayload::Symlink(ef::Symlink {
                pid,
                target: target.to_string_lossy().into(),
                link_path: link_path.to_string_lossy().into(),
                tree_hash,
            }))
        }
        Classification::Stdio { subtype, pipe_inode, len, .. } => {
            let (content_hash, size) = stream_content(&content, len);
            Some(EventPayload::Stdio(eio::Stdio {
                pid,
                subtype: map_stdio_type(subtype),
                content_hash,
                size: size as u64,
                pipe_inode,
                dest_pid: None,
                source_pid: None,
                text: None,
                encoding: None,
            }))
        }
        Classification::PipeCreate { read_fd, write_fd, inode } => {
            Some(EventPayload::PipeCreate(eio::PipeCreate { pid, inode, read_fd, write_fd }))
        }
        Classification::PipeData { inode, direction, len, .. } => {
            let (content_hash, size) = stream_content(&content, len);
            Some(EventPayload::PipeData(eio::PipeData {
                pid,
                inode,
                direction: map_pipe_dir(direction),
                content_hash,
                size: size as u64,
                dest_pids: Vec::new(),
                text: None,
                encoding: None,
            }))
        }
        Classification::PtyCreate { master_fd, slave_path } => {
            Some(EventPayload::PtyCreate(eio::PtyCreate {
                pid,
                master_fd,
                slave_path: slave_path.to_string_lossy().into(),
            }))
        }
        Classification::PtyData { subtype, len, .. } => {
            let (content_hash, size) = stream_content(&content, len);
            Some(EventPayload::PtyData(eio::PtyData {
                pid,
                subtype: map_pty_type(subtype),
                content_hash,
                size: size as u64,
                slave_path: String::new(),
                text: None,
                encoding: None,
            }))
        }
        Classification::FdDup { old_fd: _, new_fd: fd } => {
            Some(EventPayload::FdRedirect(eio::FdRedirect {
                pid,
                fd,
                target: eio::FdTarget {
                    target_type: "dup".into(),
                    inode: None,
                    path: None,
                    direction: None,
                },
            }))
        }
        Classification::ProcessExec { binary, argv, envp } => {
            Some(EventPayload::Exec(ep::Exec {
                pid,
                ppid: 0,
                binary: binary.to_string_lossy().into(),
                argv,
                envp,
                cwd: String::new(),
            }))
        }
        Classification::ProcessFork { parent, child } => {
            Some(EventPayload::Fork(ep::Fork {
                parent_pid: parent.as_raw() as u32,
                child_pid: child.as_raw() as u32,
            }))
        }
        Classification::ProcessExit { exit_code } => {
            Some(EventPayload::Exit(ep::Exit { pid, exit_code, signal: None }))
        }
        Classification::NetSocket { domain, sock_type, fd } => {
            Some(EventPayload::Socket(en::Socket {
                pid,
                domain: domain.to_string(),
                sock_type: sock_type.to_string(),
                fd,
            }))
        }
        Classification::NetConnect { fd, addr } => {
            Some(EventPayload::Connect(en::Connect {
                pid,
                fd,
                remote_addr: addr.ip().to_string(),
                remote_port: addr.port(),
            }))
        }
        Classification::NetAccept { fd, peer } => {
            Some(EventPayload::Accept(en::Accept {
                pid,
                fd,
                peer_addr: peer.ip().to_string(),
                peer_port: peer.port(),
            }))
        }
        Classification::FileOpen { .. }
        | Classification::FileClose { .. }
        | Classification::Passthrough => None,
    }
}

fn stream_content(content: &CapturedContent, fallback_len: usize) -> (Option<String>, usize) {
    match content {
        CapturedContent::StreamData { content_hash, size, .. } => {
            (content_hash.as_ref().map(|h| h.to_string()), *size)
        }
        CapturedContent::FileRead { content_hash, size, .. } => {
            (content_hash.as_ref().map(|h| h.to_string()), *size)
        }
        _ => (None, fallback_len),
    }
}

fn map_stdio_type(t: StdioType) -> eio::StdioSubtype {
    match t {
        StdioType::Stdin => eio::StdioSubtype::Stdin,
        StdioType::Stdout => eio::StdioSubtype::Stdout,
        StdioType::Stderr => eio::StdioSubtype::Stderr,
    }
}

fn map_pipe_dir(d: PipeDirection) -> eio::PipeDirection {
    match d {
        PipeDirection::Read => eio::PipeDirection::Read,
        PipeDirection::Write => eio::PipeDirection::Write,
    }
}

fn map_pty_type(t: PtyDataType) -> eio::PtySubtype {
    match t {
        PtyDataType::Master => eio::PtySubtype::MasterRead,
        PtyDataType::Slave => eio::PtySubtype::SlaveWrite,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use nix::unistd::Pid;
    use crate::pipeline::captured::CapturedContent;

    fn stage() -> StampStage {
        StampStage::new(Arc::new(SequenceGenerator::new(0)), "test-agent".into())
    }

    fn captured(cls: Classification) -> CapturedEvent {
        CapturedEvent { pid: Pid::from_raw(1), classification: cls, content: CapturedContent::None }
    }

    #[test]
    fn stamps_write() {
        let s = stage();
        let ev = s.stamp(captured(Classification::FileWrite {
            path: PathBuf::from("/workspace/a.txt"),
            fd: 3,
            buf_addr: 0,
            len: 10,
        }), None);
        assert!(ev.is_some());
        assert!(matches!(ev.unwrap().payload, EventPayload::Write(_)));
    }

    #[test]
    fn passthrough_returns_none() {
        let s = stage();
        let ev = s.stamp(captured(Classification::Passthrough), None);
        assert!(ev.is_none());
    }

    #[test]
    fn stamp_blocked_emits_blocked_event() {
        let s = stage();
        let ev = s.stamp_blocked(42, "write".into(), Some("/workspace/x".into()), "test rule".into());
        assert!(matches!(ev.payload, EventPayload::Blocked(_)));
    }

    #[test]
    fn seq_increments() {
        let s = stage();
        let ev1 = s.stamp_blocked(1, "write".into(), None, String::new());
        let ev2 = s.stamp_blocked(2, "write".into(), None, String::new());
        assert_eq!(ev2.seq, ev1.seq + 1);
    }
}

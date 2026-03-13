// Rust guideline compliant 2026-02-21
//! Stamp stage: converts captured events into structured `Event` records.
//!
//! Maps each `Classification` variant to the corresponding `EventPayload`
//! variant, fills in sequence numbers, timestamps, and the optional
//! Merkle-tree root hash. When enrichment is enabled, raw bytes captured
//! by the capture stage are embedded as inline `text` or `data` fields.

use std::sync::Arc;

use tracing::event;
use tracing::Level;

use crate::cas::ContentHash;
use crate::config::{Category, EnrichConfig};
use crate::events::envelope::{Event, EventPayload, SequenceGenerator, timestamp_pair};
use crate::events::{file as ef, io as eio, network as en, process as ep};
use crate::pipeline::captured::{CapturedContent, CapturedEvent};
use crate::pipeline::classified::{Classification, PipeDirection, PtyDataType, StdioType};

/// Stage that stamps captured events with sequence numbers and timestamps.
pub struct StampStage {
    pub seq_gen: Arc<SequenceGenerator>,
    pub agent_id: String,
    pub enrich: EnrichConfig,
}

impl StampStage {
    /// Create a new stamp stage.
    pub fn new(seq_gen: Arc<SequenceGenerator>, agent_id: String, enrich: EnrichConfig) -> Self {
        Self { seq_gen, agent_id, enrich }
    }

    /// Convert a `CapturedEvent` into a full structured `Event`.
    pub fn stamp(&self, captured: CapturedEvent, tree_hash: Option<ContentHash>) -> Option<Event> {
        let pid = captured.pid.as_raw() as u32;
        let tree_str = tree_hash.map(|h| h.to_string());
        let payload = to_payload(pid, captured.classification, captured.content, tree_str, &self.enrich)?;
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
            redactions: Vec::new(),
            payload,
        }
    }
}

/// Encode bytes as UTF-8 text or base64-encoded binary.
///
/// Returns `(data_string, encoding)` where `encoding` is `None` for valid
/// UTF-8 and `Some("base64")` for binary data.
fn bytes_to_inline(data: &[u8]) -> (String, Option<String>) {
    match std::str::from_utf8(data) {
        Ok(s) => (s.to_owned(), None),
        Err(_) => {
            use base64::Engine as _;
            (base64::engine::general_purpose::STANDARD.encode(data), Some("base64".to_owned()))
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
    enrich: &EnrichConfig,
) -> Option<EventPayload> {
    match cls {
        Classification::FileWrite { path, fd, .. } => {
            let (before_hash, after_hash, size, data, encoding) = match &content {
                CapturedContent::FileWrite { before_hash, after_hash, size, data } => {
                    let (d, enc) = inline_file_bytes(data.as_deref(), enrich);
                    (
                        before_hash.as_ref().map(|h| h.to_string()),
                        after_hash.as_ref().map(|h| h.to_string()),
                        *size,
                        d,
                        enc,
                    )
                }
                _ => (None, None, 0, None, None),
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
                data,
                encoding,
                sensitive: false,
            }))
        }
        Classification::FileRead { path, fd, len, .. } => {
            let (content_hash, size, data, encoding) = match &content {
                CapturedContent::FileRead { content_hash, size, data } => {
                    let (d, enc) = inline_file_bytes(data.as_deref(), enrich);
                    (content_hash.as_ref().map(|h| h.to_string()), *size, d, enc)
                }
                _ => (None, len, None, None),
            };
            Some(EventPayload::Read(ef::Read {
                pid,
                path: path.to_string_lossy().into(),
                fd,
                offset: 0,
                size: size as u64,
                content_hash,
                data,
                encoding,
                sensitive: false,
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
            let (content_hash, data, encoding) = match &content {
                CapturedContent::FileDelete { content_hash, data } => {
                    let (d, enc) = if enrich.should_inline(Category::DeleteContent) {
                        data.as_deref().map(bytes_to_inline).map(|(d, e)| (Some(d), e)).unwrap_or((None, None))
                    } else {
                        (None, None)
                    };
                    (content_hash.as_ref().map(|h| h.to_string()), d, enc)
                }
                _ => (None, None, None),
            };
            Some(EventPayload::Unlink(ef::Unlink {
                pid,
                path: path.to_string_lossy().into(),
                content_hash,
                tree_hash,
                data,
                encoding,
                sensitive: false,
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
            let (before_hash, after_hash, before_data, after_data, encoding) = match &content {
                CapturedContent::FileTruncate { before_hash, after_hash, before_data, after_data } => {
                    if enrich.should_inline(Category::TruncateContent) {
                        let (bd, enc_b) = before_data.as_deref().map(bytes_to_inline).map(|(d, e)| (Some(d), e)).unwrap_or((None, None));
                        let (ad, enc_a) = after_data.as_deref().map(bytes_to_inline).map(|(d, e)| (Some(d), e)).unwrap_or((None, None));
                        // Prefer the before encoding; after-data encoding matches since same file.
                        let enc = enc_b.or(enc_a);
                        (
                            before_hash.as_ref().map(|h| h.to_string()),
                            after_hash.as_ref().map(|h| h.to_string()),
                            bd,
                            ad,
                            enc,
                        )
                    } else {
                        (
                            before_hash.as_ref().map(|h| h.to_string()),
                            after_hash.as_ref().map(|h| h.to_string()),
                            None,
                            None,
                            None,
                        )
                    }
                }
                _ => (None, None, None, None, None),
            };
            Some(EventPayload::Truncate(ef::Truncate {
                pid,
                path: path.to_string_lossy().into(),
                old_size: 0,
                new_size: len,
                before_hash,
                after_hash,
                tree_hash,
                before_data,
                after_data,
                encoding,
                sensitive: false,
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
            let (content_hash, size, text, encoding) = match &content {
                CapturedContent::StreamData { content_hash, size, data } => {
                    let (t, enc) = inline_stream_bytes(data.as_deref(), enrich, Category::StdioText);
                    (content_hash.as_ref().map(|h| h.to_string()), *size, t, enc)
                }
                _ => (None, len, None, None),
            };
            Some(EventPayload::Stdio(eio::Stdio {
                pid,
                subtype: map_stdio_type(subtype),
                content_hash,
                size: size as u64,
                pipe_inode,
                dest_pid: None,
                source_pid: None,
                text,
                encoding,
            }))
        }
        Classification::PipeCreate { read_fd, write_fd, inode } => {
            Some(EventPayload::PipeCreate(eio::PipeCreate { pid, inode, read_fd, write_fd }))
        }
        Classification::PipeData { inode, direction, len, .. } => {
            let (content_hash, size, text, encoding) = match &content {
                CapturedContent::StreamData { content_hash, size, data } => {
                    let (t, enc) = inline_stream_bytes(data.as_deref(), enrich, Category::PipeData);
                    (content_hash.as_ref().map(|h| h.to_string()), *size, t, enc)
                }
                _ => (None, len, None, None),
            };
            Some(EventPayload::PipeData(eio::PipeData {
                pid,
                inode,
                direction: map_pipe_dir(direction),
                content_hash,
                size: size as u64,
                dest_pids: Vec::new(),
                text,
                encoding,
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
            let (content_hash, size, text, encoding) = match &content {
                CapturedContent::StreamData { content_hash, size, data } => {
                    let (t, enc) = inline_stream_bytes(data.as_deref(), enrich, Category::PtyData);
                    (content_hash.as_ref().map(|h| h.to_string()), *size, t, enc)
                }
                _ => (None, len, None, None),
            };
            Some(EventPayload::PtyData(eio::PtyData {
                pid,
                subtype: map_pty_type(subtype),
                content_hash,
                size: size as u64,
                slave_path: String::new(),
                text,
                encoding,
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

/// Inline file bytes if `FileContent` enrichment is enabled.
fn inline_file_bytes(data: Option<&[u8]>, enrich: &EnrichConfig) -> (Option<String>, Option<String>) {
    if !enrich.should_inline(Category::FileContent) {
        return (None, None);
    }
    data.map(bytes_to_inline).map(|(d, e)| (Some(d), e)).unwrap_or((None, None))
}

/// Inline stream bytes for a given category.
fn inline_stream_bytes(
    data: Option<&[u8]>,
    enrich: &EnrichConfig,
    category: Category,
) -> (Option<String>, Option<String>) {
    if !enrich.should_inline(category) {
        return (None, None);
    }
    data.map(bytes_to_inline).map(|(d, e)| (Some(d), e)).unwrap_or((None, None))
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
        StampStage::new(
            Arc::new(SequenceGenerator::new(0)),
            "test-agent".into(),
            EnrichConfig::default(),
        )
    }

    fn stage_disabled() -> StampStage {
        StampStage::new(
            Arc::new(SequenceGenerator::new(0)),
            "test-agent".into(),
            EnrichConfig { enabled: false, ..EnrichConfig::default() },
        )
    }

    fn captured(cls: Classification) -> CapturedEvent {
        CapturedEvent { pid: Pid::from_raw(1), classification: cls, content: CapturedContent::None }
    }

    fn captured_with(cls: Classification, content: CapturedContent) -> CapturedEvent {
        CapturedEvent { pid: Pid::from_raw(1), classification: cls, content }
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

    #[test]
    fn stamps_stdio_with_inline_text() {
        let s = stage();
        let content = CapturedContent::StreamData {
            content_hash: None,
            data: Some(b"hello world".to_vec()),
            size: 11,
        };
        let ev = s.stamp(
            captured_with(
                Classification::Stdio {
                    subtype: StdioType::Stdout,
                    pipe_inode: None,
                    len: 11,
                    buf_addr: 0,
                },
                content,
            ),
            None,
        );
        let ev = ev.unwrap();
        match ev.payload {
            EventPayload::Stdio(ref s) => {
                assert_eq!(s.text.as_deref(), Some("hello world"));
                assert!(s.encoding.is_none(), "plain UTF-8 must not set encoding");
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn stamps_write_with_inline_data() {
        let s = stage();
        let content = CapturedContent::FileWrite {
            before_hash: None,
            after_hash: None,
            data: Some(b"contents".to_vec()),
            size: 8,
        };
        let ev = s.stamp(
            captured_with(
                Classification::FileWrite {
                    path: PathBuf::from("/workspace/f.txt"),
                    fd: 3,
                    buf_addr: 0,
                    len: 8,
                },
                content,
            ),
            None,
        );
        match ev.unwrap().payload {
            EventPayload::Write(ref w) => {
                assert_eq!(w.data.as_deref(), Some("contents"));
                assert!(w.encoding.is_none());
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn enrichment_disabled_yields_none() {
        let s = stage_disabled();
        let content = CapturedContent::StreamData {
            content_hash: None,
            data: Some(b"hello".to_vec()),
            size: 5,
        };
        let ev = s.stamp(
            captured_with(
                Classification::Stdio {
                    subtype: StdioType::Stdout,
                    pipe_inode: None,
                    len: 5,
                    buf_addr: 0,
                },
                content,
            ),
            None,
        );
        match ev.unwrap().payload {
            EventPayload::Stdio(ref s) => {
                assert!(s.text.is_none(), "enrichment disabled must not inline text");
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn binary_data_base64_encoded() {
        let s = stage();
        let binary = vec![0xFF, 0xFE, 0x00, 0x01];
        let content = CapturedContent::FileWrite {
            before_hash: None,
            after_hash: None,
            data: Some(binary.clone()),
            size: binary.len(),
        };
        let ev = s.stamp(
            captured_with(
                Classification::FileWrite {
                    path: PathBuf::from("/workspace/bin"),
                    fd: 5,
                    buf_addr: 0,
                    len: binary.len(),
                },
                content,
            ),
            None,
        );
        match ev.unwrap().payload {
            EventPayload::Write(ref w) => {
                assert!(w.data.is_some(), "binary data must be inlined as base64");
                assert_eq!(w.encoding.as_deref(), Some("base64"), "encoding must be 'base64'");
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }
}

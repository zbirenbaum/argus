use std::collections::HashMap;

use super::*;

#[test]
fn seq_generator_increments() {
    let seq_gen = SequenceGenerator::new(0);
    assert_eq!(seq_gen.next_seq(), 0);
    assert_eq!(seq_gen.next_seq(), 1);
    assert_eq!(seq_gen.next_seq(), 2);
}

#[test]
fn seq_generator_custom_start() {
    let seq_gen = SequenceGenerator::new(100);
    assert_eq!(seq_gen.next_seq(), 100);
    assert_eq!(seq_gen.next_seq(), 101);
}

#[test]
fn event_auto_fills_fields() {
    let seq_gen = SequenceGenerator::default();
    let payload = EventPayload::Fork(process::Fork {
        parent_pid: 1,
        child_pid: 2,
    });
    let event = Event::new(&seq_gen, "test-agent".into(), payload);

    assert_eq!(event.seq, 0);
    assert_eq!(event.agent_id, "test-agent");
    assert!(event.ts_wall.contains('T'));
    assert!(event.vclock.is_none());
}

#[test]
fn event_sequential_seq() {
    let seq_gen = SequenceGenerator::default();
    let mk = || {
        EventPayload::Exit(process::Exit {
            pid: 1,
            exit_code: 0,
            signal: None,
        })
    };
    let e1 = Event::new(&seq_gen, "a".into(), mk());
    let e2 = Event::new(&seq_gen, "a".into(), mk());
    let e3 = Event::new(&seq_gen, "a".into(), mk());
    assert_eq!(e1.seq, 0);
    assert_eq!(e2.seq, 1);
    assert_eq!(e3.seq, 2);
}

#[test]
fn timestamp_monotonicity() {
    let (t1, _) = timestamp_pair();
    let (t2, _) = timestamp_pair();
    assert!(t2 >= t1);
}

#[test]
fn timestamp_wall_is_rfc3339() {
    let (_, wall) = timestamp_pair();
    chrono::DateTime::parse_from_rfc3339(&wall)
        .expect("ts_wall must be valid RFC 3339");
}

#[test]
fn event_json_has_type_field() {
    let seq_gen = SequenceGenerator::default();
    let event = Event::new(
        &seq_gen,
        "test".into(),
        EventPayload::Fork(process::Fork {
            parent_pid: 1,
            child_pid: 2,
        }),
    );
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"type\":\"fork\""));
    assert!(json.contains("\"parent_pid\":1"));
    assert!(json.contains("\"child_pid\":2"));
    assert!(json.contains("\"seq\":0"));
    assert!(json.contains("\"agent_id\":\"test\""));
}

#[test]
fn event_round_trip_write() {
    let seq_gen = SequenceGenerator::default();
    let event = Event::new(
        &seq_gen,
        "agent-1".into(),
        EventPayload::Write(file::Write {
            pid: 42,
            path: "/workspace/output.csv".into(),
            fd: 3,
            offset: 0,
            size: 4096,
            before_hash: Some("ab12".into()),
            after_hash: Some("cd34".into()),
            tree_hash: Some("ef56".into()),
        }),
    );
    let json = serde_json::to_string(&event).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(event.seq, back.seq);
    assert_eq!(event.agent_id, back.agent_id);
    assert_eq!(event.payload, back.payload);
}

#[test]
fn event_vclock_serialization() {
    let seq_gen = SequenceGenerator::default();
    let mut event = Event::new(
        &seq_gen,
        "a".into(),
        EventPayload::Fork(process::Fork {
            parent_pid: 1,
            child_pid: 2,
        }),
    );

    let json = serde_json::to_string(&event).unwrap();
    assert!(!json.contains("vclock"));

    let mut vc = HashMap::new();
    vc.insert("a".into(), 5);
    vc.insert("b".into(), 3);
    event.vclock = Some(vc);
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("vclock"));
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(event.vclock, back.vclock);
}

#[test]
fn agent_start_avoids_field_collision() {
    let seq_gen = SequenceGenerator::default();
    let event = Event::new(
        &seq_gen,
        "envelope-agent".into(),
        EventPayload::AgentStart(control::AgentStart {
            agent_id: "payload-agent".into(),
            config_summary: "s".into(),
            node: None,
            pod: None,
        }),
    );
    let json = serde_json::to_string(&event).unwrap();
    // Envelope agent_id and payload start_agent_id coexist without collision
    assert!(json.contains("\"agent_id\":\"envelope-agent\""));
    assert!(json.contains("\"start_agent_id\":\"payload-agent\""));
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(back.agent_id, "envelope-agent");
    if let EventPayload::AgentStart(ref start) = back.payload {
        assert_eq!(start.agent_id, "payload-agent");
    } else {
        panic!("wrong variant");
    }
}

#[test]
fn checkpoint_avoids_seq_collision() {
    let seq_gen = SequenceGenerator::default();
    let event = Event::new(
        &seq_gen,
        "a".into(),
        EventPayload::Checkpoint(snapshot::Checkpoint {
            seq: 50,
            tree_hash: None,
            checkpoint_s3_key: "ck/a/50.bin".into(),
        }),
    );
    let json = serde_json::to_string(&event).unwrap();
    // Envelope seq and payload checkpoint_seq coexist without collision
    assert!(json.contains("\"checkpoint_seq\":50"));
    assert!(json.contains("\"seq\":0"));
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(back.seq, 0);
    if let EventPayload::Checkpoint(ref cp) = back.payload {
        assert_eq!(cp.seq, 50);
    } else {
        panic!("wrong variant");
    }
}

#[test]
fn all_variants_round_trip() {
    let seq_gen = SequenceGenerator::default();
    let agent = "rt".to_string();

    let payloads: Vec<EventPayload> = vec![
        EventPayload::Exec(process::Exec {
            pid: 1, ppid: 0, binary: "/bin/sh".into(),
            argv: vec![], envp: vec![], cwd: "/".into(),
        }),
        EventPayload::Fork(process::Fork { parent_pid: 1, child_pid: 2 }),
        EventPayload::Exit(process::Exit { pid: 1, exit_code: 0, signal: None }),
        EventPayload::Read(file::Read {
            pid: 1, path: "/f".into(), fd: 3, offset: 0, size: 10,
            content_hash: None,
        }),
        EventPayload::Write(file::Write {
            pid: 1, path: "/f".into(), fd: 3, offset: 0, size: 10,
            before_hash: None, after_hash: None, tree_hash: None,
        }),
        EventPayload::Rename(file::Rename {
            pid: 1, old_path: "/a".into(), new_path: "/b".into(), tree_hash: None,
        }),
        EventPayload::Unlink(file::Unlink {
            pid: 1, path: "/f".into(), content_hash: None, tree_hash: None,
        }),
        EventPayload::Mkdir(file::Mkdir { pid: 1, path: "/d".into(), tree_hash: None }),
        EventPayload::Rmdir(file::Rmdir { pid: 1, path: "/d".into(), tree_hash: None }),
        EventPayload::Chmod(file::Chmod {
            pid: 1, path: "/f".into(), old_mode: 0o644, new_mode: 0o755,
        }),
        EventPayload::Truncate(file::Truncate {
            pid: 1, path: "/f".into(), old_size: 100, new_size: 0,
            before_hash: None, after_hash: None, tree_hash: None,
        }),
        EventPayload::Link(file::Link {
            pid: 1, target: "/a".into(), link_path: "/b".into(), tree_hash: None,
        }),
        EventPayload::Symlink(file::Symlink {
            pid: 1, target: "/a".into(), link_path: "/s".into(), tree_hash: None,
        }),
        EventPayload::Stdio(io::Stdio {
            pid: 1, subtype: io::StdioSubtype::Stdout,
            content_hash: None, size: 5, pipe_inode: None,
            dest_pid: None, source_pid: None,
        }),
        EventPayload::PipeCreate(io::PipeCreate {
            pid: 1, inode: 99, read_fd: 3, write_fd: 4,
        }),
        EventPayload::PipeData(io::PipeData {
            pid: 1, inode: 99, direction: io::PipeDirection::Write,
            content_hash: None, size: 10, dest_pids: vec![],
        }),
        EventPayload::PipeClose(io::PipeClose {
            pid: 1, inode: 99, direction: io::PipeDirection::Read,
        }),
        EventPayload::PtyCreate(io::PtyCreate {
            pid: 1, master_fd: 5, slave_path: "/dev/pts/0".into(),
        }),
        EventPayload::PtyData(io::PtyData {
            pid: 1, subtype: io::PtySubtype::MasterRead,
            content_hash: None, size: 8, slave_path: "/dev/pts/0".into(),
        }),
        EventPayload::FdRedirect(io::FdRedirect {
            pid: 1, fd: 1, target: io::FdTarget {
                target_type: "file".into(), inode: None,
                path: Some("/tmp/out".into()), direction: None,
            },
        }),
        EventPayload::Socket(network::Socket {
            pid: 1, domain: "AF_INET".into(), sock_type: "SOCK_STREAM".into(), fd: 5,
        }),
        EventPayload::Connect(network::Connect {
            pid: 1, fd: 5, remote_addr: "1.2.3.4".into(), remote_port: 80,
        }),
        EventPayload::Accept(network::Accept {
            pid: 1, fd: 6, peer_addr: "5.6.7.8".into(), peer_port: 9999,
        }),
        EventPayload::TlsKeys(network::TlsKeys {
            pid: 1, fd: 5, sni: None, keylog_line_hash: None,
        }),
        EventPayload::HttpRequest(network::HttpRequest {
            pid: 1, method: "GET".into(), url: "/".into(),
            headers_hash: None, body_hash: None, status: None,
        }),
        EventPayload::HttpResponse(network::HttpResponse {
            pid: 1, status: 200, headers_hash: None, body_hash: None,
        }),
        EventPayload::AgentStart(control::AgentStart {
            agent_id: "a".into(), config_summary: "s".into(),
            node: None, pod: None,
        }),
        EventPayload::AgentPause(control::AgentPause {
            reason: "r".into(), stopped_pids: vec![1],
        }),
        EventPayload::AgentResume(control::AgentResume { resumed_pids: vec![1] }),
        EventPayload::PendingApproval(control::PendingApproval {
            pid: 1, syscall: "execve".into(), path: None,
            binary: Some("/bin/rm".into()), rule_name: "no_rm".into(),
        }),
        EventPayload::ApprovalGranted(control::ApprovalGranted {
            pid: 1, rule_name: "no_rm".into(), approver: "admin".into(),
        }),
        EventPayload::ApprovalDenied(control::ApprovalDenied {
            pid: 1, rule_name: "no_rm".into(), approver: "admin".into(),
        }),
        EventPayload::InitialState(snapshot::InitialState {
            tree_hash: None, file_count: 10, total_size: 1024,
        }),
        EventPayload::Checkpoint(snapshot::Checkpoint {
            seq: 50, tree_hash: None,
            checkpoint_s3_key: "ck/a/50.bin".into(),
        }),
        EventPayload::MmapWarning(snapshot::MmapWarning {
            pid: 1, path: "/f".into(), fd: 3, prot: 3, flags: 1,
        }),
    ];

    for payload in payloads {
        let event = Event::new(&seq_gen, agent.clone(), payload);
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event.seq, back.seq);
        assert_eq!(event.agent_id, back.agent_id);
        assert_eq!(event.payload, back.payload);
    }
}

//! Tests for the event log segment writer.

use tempfile::TempDir;

use crate::config::DurabilityMode;
use crate::events::process;
use crate::events::{Event, EventPayload, SequenceGenerator};
use crate::storage::event_log::EventLog;

fn make_event(seq_gen: &SequenceGenerator) -> Event {
    Event::new(
        seq_gen,
        "test-agent".into(),
        EventPayload::Exit(process::Exit {
            pid: 1,
            exit_code: 0,
            signal: None,
        }),
    )
}

fn setup(max_bytes: u64) -> (TempDir, EventLog) {
    let dir = TempDir::new().expect("create temp dir");
    let event_dir = dir.path().join("events");
    let log = EventLog::with_max_segment_bytes(
        "test-agent".into(),
        event_dir,
        DurabilityMode::Local,
        max_bytes,
    )
    .expect("create event log");
    (dir, log)
}

#[test]
fn append_creates_jsonl_file() {
    let (_dir, mut log) = setup(1024 * 1024);
    let seq_gen = SequenceGenerator::default();
    let event = make_event(&seq_gen);

    log.append(&event, None).expect("append");

    assert!(log.current_segment_size() > 0);
    assert_eq!(log.current_segment_seq(), 0);

    let path = log.current_path.clone();
    log.flush().expect("flush");
    let content = std::fs::read_to_string(&path).expect("read file");
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 1);

    let parsed: serde_json::Value =
        serde_json::from_str(lines[0]).expect("parse JSON");
    assert_eq!(parsed["agent_id"], "test-agent");
}

#[test]
fn multiple_appends_produce_multiple_lines() {
    let (_dir, mut log) = setup(1024 * 1024);
    let seq_gen = SequenceGenerator::default();

    for _ in 0..5 {
        let event = make_event(&seq_gen);
        log.append(&event, None).expect("append");
    }

    log.flush().expect("flush");
    let content =
        std::fs::read_to_string(&log.current_path).expect("read file");
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 5);
}

#[test]
fn segment_rotates_at_threshold() {
    // Use a tiny threshold to force rotation after one event.
    let (_dir, mut log) = setup(10);
    let seq_gen = SequenceGenerator::default();

    let event = make_event(&seq_gen);
    log.append(&event, None).expect("first append");

    // The first append exceeds 10 bytes, so rotation should occur.
    assert_eq!(
        log.current_segment_seq(),
        1,
        "segment should have rotated"
    );
    assert_eq!(log.current_segment_size(), 0);
}

#[test]
fn multiple_rotations() {
    let (_dir, mut log) = setup(10);
    let seq_gen = SequenceGenerator::default();

    for _ in 0..5 {
        let event = make_event(&seq_gen);
        log.append(&event, None).expect("append");
    }

    assert_eq!(log.current_segment_seq(), 5);
}

#[test]
fn segment_files_named_sequentially() {
    let (dir, mut log) = setup(10);
    let event_dir = dir.path().join("events");
    let seq_gen = SequenceGenerator::default();

    for _ in 0..3 {
        let event = make_event(&seq_gen);
        log.append(&event, None).expect("append");
    }

    for seq in 0..3 {
        let path = event_dir.join(format!("{seq}.jsonl"));
        assert!(path.exists(), "segment {seq}.jsonl should exist");
    }
}

#[test]
fn finalize_writes_and_closes() {
    let (_dir, mut log) = setup(1024 * 1024);
    let seq_gen = SequenceGenerator::default();

    let event = make_event(&seq_gen);
    log.append(&event, None).expect("append");

    let path = log.current_path.clone();
    log.finalize(None).expect("finalize");

    let content = std::fs::read_to_string(&path).expect("read");
    assert!(!content.is_empty());
    assert!(log.writer.is_none());
}

#[test]
fn memory_durability_does_not_fsync_per_append() {
    let dir = TempDir::new().expect("create temp dir");
    let event_dir = dir.path().join("events");
    let mut log = EventLog::with_max_segment_bytes(
        "test-agent".into(),
        event_dir,
        DurabilityMode::Memory,
        1024 * 1024,
    )
    .expect("create event log");

    let seq_gen = SequenceGenerator::default();
    let event = make_event(&seq_gen);
    // Memory mode should not error (no fsync per append).
    log.append(&event, None).expect("append in memory mode");
    assert!(log.current_segment_size() > 0);
}

#[test]
fn creates_event_dir_if_missing() {
    let dir = TempDir::new().expect("create temp dir");
    let event_dir = dir.path().join("nested").join("deep").join("events");
    assert!(!event_dir.exists());

    let log = EventLog::new(
        "test-agent".into(),
        event_dir.clone(),
        DurabilityMode::Local,
    );
    assert!(log.is_ok());
    assert!(event_dir.exists());
}

#[test]
fn each_line_is_valid_json() {
    let (_dir, mut log) = setup(1024 * 1024);
    let seq_gen = SequenceGenerator::default();

    for _ in 0..10 {
        let event = make_event(&seq_gen);
        log.append(&event, None).expect("append");
    }

    log.flush().expect("flush");
    let content =
        std::fs::read_to_string(&log.current_path).expect("read");

    for (i, line) in content.lines().enumerate() {
        let parsed: Result<serde_json::Value, _> =
            serde_json::from_str(line);
        assert!(
            parsed.is_ok(),
            "line {i} is not valid JSON: {line}"
        );
    }
}

#[test]
fn current_segment_size_tracks_bytes() {
    let (_dir, mut log) = setup(1024 * 1024);
    let seq_gen = SequenceGenerator::default();

    assert_eq!(log.current_segment_size(), 0);

    let event = make_event(&seq_gen);
    log.append(&event, None).expect("append");
    let size_after_one = log.current_segment_size();
    assert!(size_after_one > 0);

    let event = make_event(&seq_gen);
    log.append(&event, None).expect("append");
    let size_after_two = log.current_segment_size();
    assert!(size_after_two > size_after_one);
}

// Rust guideline compliant 2026-02-21

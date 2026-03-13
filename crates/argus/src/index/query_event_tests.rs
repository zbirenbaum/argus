//! Tests for `query_events` with time-range filtering.

use crate::events::file;
use crate::events::process;
use crate::events::{Event, EventPayload, SequenceGenerator};

use super::*;

fn make_events() -> Vec<Event> {
    let seq_gen = SequenceGenerator::new(1);
    let mut events = Vec::new();

    let mut e = Event::new(
        &seq_gen,
        "test".into(),
        EventPayload::Write(file::Write {
            pid: 10,
            path: "/workspace/a.txt".into(),
            fd: 3,
            offset: 0,
            size: 100,
            before_hash: None,
            after_hash: None,
            tree_hash: None,
            data: None,
            encoding: None,
            sensitive: false,
        }),
    );
    e.ts_wall = 1_773_237_600_000_000;
    events.push(e);

    let mut e = Event::new(
        &seq_gen,
        "test".into(),
        EventPayload::Read(file::Read {
            pid: 10,
            path: "/workspace/a.txt".into(),
            fd: 3,
            offset: 0,
            size: 100,
            content_hash: None,
            data: None,
            encoding: None,
            sensitive: false,
        }),
    );
    e.ts_wall = 1_773_241_200_000_000;
    events.push(e);

    let mut e = Event::new(
        &seq_gen,
        "test".into(),
        EventPayload::Exit(process::Exit {
            pid: 10,
            exit_code: 0,
            signal: None,
        }),
    );
    e.ts_wall = 1_773_244_800_000_000;
    events.push(e);

    events
}

fn index_events(
    events: &[Event],
) -> (PathIndex, PidIndex, TypeIndex) {
    let mut path_idx = PathIndex::new();
    let mut pid_idx = PidIndex::new();
    let mut type_idx = TypeIndex::new();
    for e in events {
        let tag = e.payload.event_type_tag();
        type_idx.insert(tag, e.seq).unwrap();
        if let Some(pid) = e.payload.pid() {
            pid_idx.insert(pid, e.seq, tag).unwrap();
        }
        for p in e.payload.paths() {
            path_idx.insert(p, e.seq, tag).unwrap();
        }
    }
    (path_idx, pid_idx, type_idx)
}

#[test]
fn query_events_with_time_range() {
    let events = make_events();
    let (path_idx, pid_idx, type_idx) = index_events(&events);
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query_events(
        &QueryFilter {
            since: Some("2026-03-11T14:30:00.000000000Z".into()),
            ..Default::default()
        },
        &events,
    );
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].seq, 2);
    assert_eq!(results[1].seq, 3);
}

#[test]
fn query_events_with_until() {
    let events = make_events();
    let (path_idx, pid_idx, type_idx) = index_events(&events);
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query_events(
        &QueryFilter {
            until: Some("2026-03-11T15:30:00.000000000Z".into()),
            ..Default::default()
        },
        &events,
    );
    assert_eq!(results.len(), 2);
}

#[test]
fn query_events_no_filters_returns_all() {
    let events = make_events();
    let (path_idx, pid_idx, type_idx) = index_events(&events);
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query_events(
        &QueryFilter::default(),
        &events,
    );
    assert_eq!(results.len(), 3);
}

#[test]
fn query_events_time_range_with_path_filter() {
    let events = make_events();
    let (path_idx, pid_idx, type_idx) = index_events(&events);
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    // Only seq 1 and 2 touch /workspace/a.txt, and only seq 2 is after 14:30
    let results = engine.query_events(
        &QueryFilter {
            path: Some("/workspace/a.txt".into()),
            since: Some("2026-03-11T14:30:00.000000000Z".into()),
            ..Default::default()
        },
        &events,
    );
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].seq, 2);
    assert_eq!(results[0].event_type, "read");
}

#[test]
fn query_events_time_range_with_pid_filter() {
    let events = make_events();
    let (path_idx, pid_idx, type_idx) = index_events(&events);
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    // All events are pid 10; filter to before 15:30
    let results = engine.query_events(
        &QueryFilter {
            pid: Some(10),
            until: Some("2026-03-11T15:30:00.000000000Z".into()),
            ..Default::default()
        },
        &events,
    );
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].seq, 1);
    assert_eq!(results[1].seq, 2);
}

// Rust guideline compliant 2026-02-21

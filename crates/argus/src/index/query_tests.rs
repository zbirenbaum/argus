//! Tests for the query engine (index-only, no time-range filtering).

use super::*;

fn setup_indexes() -> (PathIndex, PidIndex, TypeIndex) {
    let mut path_idx = PathIndex::new();
    let mut pid_idx = PidIndex::new();
    let mut type_idx = TypeIndex::new();

    // Event seq=1: pid=10 writes /workspace/a.txt
    path_idx.insert("/workspace/a.txt", 1, "write").unwrap();
    pid_idx.insert(10, 1, "write").unwrap();
    type_idx.insert("write", 1).unwrap();

    // Event seq=2: pid=10 reads /workspace/a.txt
    path_idx.insert("/workspace/a.txt", 2, "read").unwrap();
    pid_idx.insert(10, 2, "read").unwrap();
    type_idx.insert("read", 2).unwrap();

    // Event seq=3: pid=20 writes /workspace/b.txt
    path_idx.insert("/workspace/b.txt", 3, "write").unwrap();
    pid_idx.insert(20, 3, "write").unwrap();
    type_idx.insert("write", 3).unwrap();

    // Event seq=4: pid=10 execs
    pid_idx.insert(10, 4, "exec").unwrap();
    type_idx.insert("exec", 4).unwrap();

    // Event seq=5: pid=20 unlinks /workspace/b.txt
    path_idx.insert("/workspace/b.txt", 5, "unlink").unwrap();
    pid_idx.insert(20, 5, "unlink").unwrap();
    type_idx.insert("unlink", 5).unwrap();

    (path_idx, pid_idx, type_idx)
}

#[test]
fn query_by_path() {
    let (path_idx, pid_idx, type_idx) = setup_indexes();
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query(&QueryFilter {
        path: Some("/workspace/a.txt".into()),
        ..Default::default()
    });
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].seq, 1);
    assert_eq!(results[1].seq, 2);
}

#[test]
fn query_by_pid() {
    let (path_idx, pid_idx, type_idx) = setup_indexes();
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query(&QueryFilter {
        pid: Some(20),
        ..Default::default()
    });
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].seq, 3);
    assert_eq!(results[1].seq, 5);
}

#[test]
fn query_by_type() {
    let (path_idx, pid_idx, type_idx) = setup_indexes();
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query(&QueryFilter {
        event_type: Some("write".into()),
        ..Default::default()
    });
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].seq, 1);
    assert_eq!(results[1].seq, 3);
}

#[test]
fn query_by_path_and_type() {
    let (path_idx, pid_idx, type_idx) = setup_indexes();
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query(&QueryFilter {
        path: Some("/workspace/a.txt".into()),
        event_type: Some("write".into()),
        ..Default::default()
    });
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].seq, 1);
}

#[test]
fn query_by_pid_and_type() {
    let (path_idx, pid_idx, type_idx) = setup_indexes();
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query(&QueryFilter {
        pid: Some(10),
        event_type: Some("write".into()),
        ..Default::default()
    });
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].seq, 1);
}

#[test]
fn query_with_seq_range() {
    let (path_idx, pid_idx, type_idx) = setup_indexes();
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query(&QueryFilter {
        seq_from: Some(2),
        seq_to: Some(4),
        event_type: Some("write".into()),
        ..Default::default()
    });
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].seq, 3);
}

#[test]
fn query_with_limit() {
    let (path_idx, pid_idx, type_idx) = setup_indexes();
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query(&QueryFilter {
        pid: Some(10),
        limit: Some(1),
        ..Default::default()
    });
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].seq, 1);
}

#[test]
fn query_by_path_prefix() {
    let (path_idx, pid_idx, type_idx) = setup_indexes();
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query(&QueryFilter {
        path_prefix: Some("/workspace/".into()),
        ..Default::default()
    });
    assert_eq!(results.len(), 4);
}

#[test]
fn query_no_filters_returns_all_from_type_index() {
    let (path_idx, pid_idx, type_idx) = setup_indexes();
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query(&QueryFilter::default());
    assert_eq!(results.len(), 5);
}

#[test]
fn query_no_match_returns_empty() {
    let (path_idx, pid_idx, type_idx) = setup_indexes();
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query(&QueryFilter {
        path: Some("/nonexistent".into()),
        ..Default::default()
    });
    assert!(results.is_empty());
}

#[test]
fn query_intersect_disjoint_returns_empty() {
    let (path_idx, pid_idx, type_idx) = setup_indexes();
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    // pid=10 never touched /workspace/b.txt
    let results = engine.query(&QueryFilter {
        pid: Some(10),
        path: Some("/workspace/b.txt".into()),
        ..Default::default()
    });
    assert!(results.is_empty());
}

#[test]
fn query_filter_default_is_empty() {
    let f = QueryFilter::default();
    assert!(f.path.is_none());
    assert!(f.pid.is_none());
    assert!(f.event_type.is_none());
    assert!(f.limit.is_none());
}

#[test]
fn query_results_sorted_by_seq() {
    let (path_idx, pid_idx, type_idx) = setup_indexes();
    let engine = QueryEngine::new(&path_idx, &pid_idx, &type_idx);

    let results = engine.query(&QueryFilter::default());
    for window in results.windows(2) {
        assert!(window[0].seq <= window[1].seq);
    }
}

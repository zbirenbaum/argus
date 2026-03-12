//! Tests for the PID index.

use tempfile::TempDir;

use super::*;

#[test]
fn insert_and_lookup() {
    let mut idx = PidIndex::new();
    idx.insert(42, 1, "exec").unwrap();
    idx.insert(42, 5, "write").unwrap();
    idx.insert(43, 3, "fork").unwrap();

    let entries = idx.lookup(42);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].seq, 1);
    assert_eq!(entries[1].seq, 5);

    let entries = idx.lookup(43);
    assert_eq!(entries.len(), 1);
}

#[test]
fn lookup_missing_pid_returns_empty() {
    let idx = PidIndex::new();
    assert!(idx.lookup(999).is_empty());
}

#[test]
fn upsert_and_query_process_info() {
    let mut idx = PidIndex::new();
    let info = ProcessInfo {
        ppid: 1,
        binary: "/bin/sh".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), "echo hi".into()],
        start_seq: 10,
        end_seq: None,
    };
    idx.upsert_process(42, info.clone()).unwrap();

    let got = idx.process_info(42).unwrap();
    assert_eq!(got, &info);
    assert!(idx.process_info(999).is_none());
}

#[test]
fn mark_exit_sets_end_seq() {
    let mut idx = PidIndex::new();
    idx.upsert_process(
        42,
        ProcessInfo {
            ppid: 1,
            binary: "/bin/sh".into(),
            argv: vec![],
            start_seq: 10,
            end_seq: None,
        },
    )
    .unwrap();

    idx.mark_exit(42, 50).unwrap();
    assert_eq!(idx.process_info(42).unwrap().end_seq, Some(50));
}

#[test]
fn mark_exit_nonexistent_pid_is_noop() {
    let mut idx = PidIndex::new();
    idx.mark_exit(999, 50).unwrap();
}

#[test]
fn process_tree_returns_all() {
    let mut idx = PidIndex::new();
    for pid in [1, 2, 3] {
        idx.upsert_process(
            pid,
            ProcessInfo {
                ppid: 0,
                binary: "/bin/test".into(),
                argv: vec![],
                start_seq: u64::from(pid),
                end_seq: None,
            },
        )
        .unwrap();
    }
    assert_eq!(idx.process_tree().len(), 3);
}

#[test]
fn pid_count_and_entry_count() {
    let mut idx = PidIndex::new();
    assert_eq!(idx.pid_count(), 0);
    assert_eq!(idx.entry_count(), 0);

    idx.insert(1, 10, "exec").unwrap();
    idx.insert(1, 11, "write").unwrap();
    idx.insert(2, 12, "fork").unwrap();

    assert_eq!(idx.pid_count(), 2);
    assert_eq!(idx.entry_count(), 3);
}

#[test]
fn disk_backed_rebuild() {
    let dir = TempDir::new().unwrap();
    let idx_dir = dir.path().join("pid");

    let mut idx = PidIndex::with_dir(idx_dir.clone()).unwrap();
    idx.insert(10, 1, "exec").unwrap();
    idx.insert(10, 2, "write").unwrap();
    idx.insert(20, 3, "fork").unwrap();
    idx.upsert_process(
        10,
        ProcessInfo {
            ppid: 1,
            binary: "/usr/bin/cat".into(),
            argv: vec!["cat".into(), "file.txt".into()],
            start_seq: 1,
            end_seq: None,
        },
    )
    .unwrap();

    let mut idx2 = PidIndex::with_dir(idx_dir).unwrap();
    idx2.rebuild_from_disk().unwrap();

    assert_eq!(idx2.pid_count(), 2);
    assert_eq!(idx2.entry_count(), 3);
    let info = idx2.process_info(10).unwrap();
    assert_eq!(info.binary, "/usr/bin/cat");
    assert_eq!(info.argv, vec!["cat", "file.txt"]);
}

#[test]
fn rebuild_no_dir_is_noop() {
    let mut idx = PidIndex::new();
    idx.rebuild_from_disk().unwrap();
}

#[test]
fn default_creates_empty() {
    let idx = PidIndex::default();
    assert_eq!(idx.pid_count(), 0);
}

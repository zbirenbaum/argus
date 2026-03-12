//! Tests for the local buffer LRU eviction.

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use crate::storage::local_buffer::LocalBuffer;

fn create_file(dir: &TempDir, name: &str, size: usize) -> PathBuf {
    let path = dir.path().join(name);
    let data = vec![0u8; size];
    fs::write(&path, &data).expect("write test file");
    path
}

#[test]
fn track_updates_total_bytes() {
    let mut buf = LocalBuffer::new(1000);
    assert_eq!(buf.total_bytes(), 0);
    assert_eq!(buf.entry_count(), 0);

    buf.track(PathBuf::from("/tmp/a"), 100);
    assert_eq!(buf.total_bytes(), 100);
    assert_eq!(buf.entry_count(), 1);

    buf.track(PathBuf::from("/tmp/b"), 200);
    assert_eq!(buf.total_bytes(), 300);
    assert_eq!(buf.entry_count(), 2);
}

#[test]
fn prune_does_nothing_under_limit() {
    let mut buf = LocalBuffer::new(1000);
    buf.track(PathBuf::from("/tmp/a"), 100);
    let evicted = buf.prune().expect("prune");
    assert_eq!(evicted, 0);
}

#[test]
fn prune_skips_unconfirmed_entries() {
    let dir = TempDir::new().expect("temp dir");
    let path = create_file(&dir, "a.bin", 600);

    let mut buf = LocalBuffer::new(500);
    buf.track(path.clone(), 600);

    // Over limit but not confirmed, so nothing should be evicted.
    let evicted = buf.prune().expect("prune");
    assert_eq!(evicted, 0);
    assert!(path.exists());
    assert_eq!(buf.total_bytes(), 600);
}

#[test]
fn prune_evicts_confirmed_oldest_first() {
    let dir = TempDir::new().expect("temp dir");
    let p1 = create_file(&dir, "a.bin", 300);
    let p2 = create_file(&dir, "b.bin", 300);
    let p3 = create_file(&dir, "c.bin", 300);

    let mut buf = LocalBuffer::new(500);
    buf.track(p1.clone(), 300);
    buf.track(p2.clone(), 300);
    buf.track(p3.clone(), 300);

    // Confirm all three.
    buf.confirm_upload("a.bin");
    buf.confirm_upload("b.bin");
    buf.confirm_upload("c.bin");

    let evicted = buf.prune().expect("prune");
    // Need to evict at least 2 to get from 900 to <= 500.
    assert!(evicted >= 2);
    assert!(!p1.exists(), "oldest should be deleted");
    assert!(!p2.exists(), "second oldest should be deleted");
    assert!(buf.total_bytes() <= 500);
}

#[test]
fn prune_tolerates_already_deleted_files() {
    let dir = TempDir::new().expect("temp dir");
    let path = create_file(&dir, "gone.bin", 600);

    let mut buf = LocalBuffer::new(100);
    buf.track(path.clone(), 600);
    buf.confirm_upload("gone.bin");

    // Delete the file before pruning.
    fs::remove_file(&path).expect("pre-delete");

    let evicted = buf.prune().expect("prune");
    assert_eq!(evicted, 1);
    assert_eq!(buf.total_bytes(), 0);
}

#[test]
fn confirm_upload_marks_matching_entries() {
    let mut buf = LocalBuffer::new(1000);
    buf.track(PathBuf::from("/data/events/0.jsonl"), 100);
    buf.track(PathBuf::from("/data/events/1.jsonl"), 200);

    buf.confirm_upload("0.jsonl");

    // Only the first entry should be confirmed.
    let entries: Vec<_> = buf.entries.iter().collect();
    assert!(entries[0].upload_confirmed);
    assert!(!entries[1].upload_confirmed);
}

#[test]
fn new_sets_max_bytes() {
    let buf = LocalBuffer::new(42);
    assert_eq!(buf.max_bytes(), 42);
    assert_eq!(buf.total_bytes(), 0);
    assert_eq!(buf.entry_count(), 0);
}

// Rust guideline compliant 2026-02-21

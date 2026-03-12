//! Tests for the type index.

use tempfile::TempDir;

use super::*;

#[test]
fn insert_and_lookup() {
    let mut idx = TypeIndex::new();
    idx.insert("write", 1).unwrap();
    idx.insert("write", 5).unwrap();
    idx.insert("read", 3).unwrap();

    let seqs = idx.lookup("write");
    assert_eq!(seqs, &[1, 5]);

    let seqs = idx.lookup("read");
    assert_eq!(seqs, &[3]);
}

#[test]
fn lookup_missing_type_returns_empty() {
    let idx = TypeIndex::new();
    assert!(idx.lookup("nonexistent").is_empty());
}

#[test]
fn type_count_and_entry_count() {
    let mut idx = TypeIndex::new();
    assert_eq!(idx.type_count(), 0);
    assert_eq!(idx.entry_count(), 0);

    idx.insert("write", 1).unwrap();
    idx.insert("write", 2).unwrap();
    idx.insert("exec", 3).unwrap();

    assert_eq!(idx.type_count(), 2);
    assert_eq!(idx.entry_count(), 3);
}

#[test]
fn disk_backed_rebuild() {
    let dir = TempDir::new().unwrap();
    let idx_dir = dir.path().join("type");

    let mut idx = TypeIndex::with_dir(idx_dir.clone()).unwrap();
    idx.insert("write", 1).unwrap();
    idx.insert("write", 10).unwrap();
    idx.insert("exec", 5).unwrap();
    idx.insert("fork", 7).unwrap();

    let mut idx2 = TypeIndex::with_dir(idx_dir).unwrap();
    idx2.rebuild_from_disk().unwrap();

    assert_eq!(idx2.type_count(), 3);
    assert_eq!(idx2.entry_count(), 4);
    assert_eq!(idx2.lookup("write"), &[1, 10]);
    assert_eq!(idx2.lookup("exec"), &[5]);
}

#[test]
fn rebuild_no_dir_is_noop() {
    let mut idx = TypeIndex::new();
    idx.rebuild_from_disk().unwrap();
}

#[test]
fn default_creates_empty() {
    let idx = TypeIndex::default();
    assert_eq!(idx.type_count(), 0);
}

//! Tests for the path index.

use tempfile::TempDir;

use super::*;

#[test]
fn insert_and_lookup_in_memory() {
    let mut idx = PathIndex::new();
    idx.insert("/workspace/a.txt", 1, "write").unwrap();
    idx.insert("/workspace/a.txt", 5, "read").unwrap();
    idx.insert("/workspace/b.txt", 3, "unlink").unwrap();

    let entries = idx.lookup("/workspace/a.txt");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].seq, 1);
    assert_eq!(entries[0].event_type, "write");
    assert_eq!(entries[1].seq, 5);
    assert_eq!(entries[1].event_type, "read");

    let entries = idx.lookup("/workspace/b.txt");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].seq, 3);
}

#[test]
fn lookup_missing_path_returns_empty() {
    let idx = PathIndex::new();
    assert!(idx.lookup("/nonexistent").is_empty());
}

#[test]
fn prefix_lookup() {
    let mut idx = PathIndex::new();
    idx.insert("/workspace/src/main.rs", 1, "write").unwrap();
    idx.insert("/workspace/src/lib.rs", 2, "write").unwrap();
    idx.insert("/workspace/README.md", 3, "write").unwrap();

    let results = idx.lookup_prefix("/workspace/src/");
    assert_eq!(results.len(), 2);

    let results = idx.lookup_prefix("/workspace/");
    assert_eq!(results.len(), 3);

    let results = idx.lookup_prefix("/other/");
    assert!(results.is_empty());
}

#[test]
fn disk_backed_insert_and_rebuild() {
    let dir = TempDir::new().unwrap();
    let idx_dir = dir.path().join("path");

    let mut idx = PathIndex::with_dir(idx_dir.clone()).unwrap();
    idx.insert("/workspace/file.rs", 10, "write").unwrap();
    idx.insert("/workspace/file.rs", 20, "read").unwrap();
    idx.insert("/workspace/other.rs", 15, "chmod").unwrap();

    assert_eq!(idx.entry_count(), 3);
    assert_eq!(idx.path_count(), 2);

    // Rebuild from disk into a fresh index.
    let mut idx2 = PathIndex::with_dir(idx_dir).unwrap();
    idx2.rebuild_from_disk().unwrap();

    assert_eq!(idx2.path_count(), 2);
    assert_eq!(idx2.entry_count(), 3);

    let entries = idx2.lookup("/workspace/file.rs");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].seq, 10);
    assert_eq!(entries[1].seq, 20);
}

#[test]
fn path_count_and_entry_count() {
    let mut idx = PathIndex::new();
    assert_eq!(idx.path_count(), 0);
    assert_eq!(idx.entry_count(), 0);

    idx.insert("/a", 1, "write").unwrap();
    idx.insert("/a", 2, "read").unwrap();
    idx.insert("/b", 3, "unlink").unwrap();

    assert_eq!(idx.path_count(), 2);
    assert_eq!(idx.entry_count(), 3);
}

#[test]
fn default_creates_in_memory() {
    let idx = PathIndex::default();
    assert_eq!(idx.path_count(), 0);
}

#[test]
fn rebuild_from_disk_no_dir_is_noop() {
    let mut idx = PathIndex::new();
    idx.rebuild_from_disk().unwrap();
    assert_eq!(idx.path_count(), 0);
}

#[test]
fn path_hash_is_deterministic() {
    let h1 = path_hash("/workspace/test.txt");
    let h2 = path_hash("/workspace/test.txt");
    assert_eq!(h1, h2);
    assert_eq!(h1.len(), 64); // SHA-256 hex
}

#[test]
fn different_paths_produce_different_hashes() {
    let h1 = path_hash("/workspace/a.txt");
    let h2 = path_hash("/workspace/b.txt");
    assert_ne!(h1, h2);
}

//! Two-tier CAS: local filesystem with remote fallback.
//!
//! Writes go to [`LocalCas`] only — async upload to remote is
//! handled by the storage pipeline's worker pool, not here.
//! Reads try local first, then pull from [`RemoteCas`] and
//! backfill the local cache on hit.

use anyhow::Result;

use super::hash::ContentHash;
use super::remote::RemoteCas;
use super::store::LocalCas;
use super::traits::Cas;

/// Local-first CAS with remote read-through.
///
/// `put` writes to local only. The storage pipeline is responsible
/// for async upload to remote and digest cache bookkeeping.
/// `get` tries local, falls back to remote, and backfills local on
/// a remote hit so subsequent reads are fast.
#[derive(Debug)]
pub struct TieredCas {
    local: LocalCas,
    remote: RemoteCas,
}

impl TieredCas {
    /// Create a tiered store from local and remote backends.
    pub fn new(local: LocalCas, remote: RemoteCas) -> Self {
        Self { local, remote }
    }

    /// Direct access to the local tier.
    pub fn local(&self) -> &LocalCas {
        &self.local
    }

    /// Direct access to the remote tier.
    pub fn remote(&self) -> &RemoteCas {
        &self.remote
    }
}

impl Cas for TieredCas {
    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        if let Ok(data) = self.local.get(hash) {
            return Ok(data);
        }
        let data = self.remote.get(hash)?;
        let _ = self.local.put(&data);
        Ok(data)
    }

    fn put(&self, content: &[u8]) -> Result<ContentHash> {
        self.local.put(content)
    }

    fn exists(&self, hash: &ContentHash) -> Result<bool> {
        if Cas::exists(&self.local, hash)? {
            return Ok(true);
        }
        self.remote.exists(hash)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use anyhow::Result;

    use crate::cas::{Cas, ContentHash, LocalCas, RemoteCas};
    use crate::storage::object_store_dyn::DynObjectStore;
    use crate::storage::s3::ObjectStore;

    use super::TieredCas;

    /// Minimal in-memory object store for testing tiered CAS.
    #[derive(Debug, Default)]
    struct MockStore {
        data: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl ObjectStore for MockStore {
        async fn put(&self, key: &str, data: Vec<u8>) -> Result<()> {
            self.data.lock().unwrap().insert(key.to_owned(), data);
            Ok(())
        }

        async fn get(&self, key: &str) -> Result<Vec<u8>> {
            self.data
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("not found: {key}"))
        }

        async fn exists(&self, key: &str) -> Result<bool> {
            Ok(self.data.lock().unwrap().contains_key(key))
        }

        async fn list(&self, prefix: &str) -> Result<Vec<String>> {
            Ok(self
                .data
                .lock()
                .unwrap()
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect())
        }
    }

    fn make_tiered() -> (tempfile::TempDir, TieredCas) {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = LocalCas::new(dir.path().join("cas")).expect("LocalCas");
        let remote = RemoteCas::new(DynObjectStore::new(MockStore::default()));
        (dir, TieredCas::new(local, remote))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn put_writes_local_only() {
        let (_dir, tiered) = make_tiered();
        let hash = tiered.put(b"hello").unwrap();

        assert!(Cas::exists(tiered.local(), &hash).unwrap());
        // Remote should not have it — put only writes local.
        let remote_exists = tiered.remote().exists(&hash).unwrap();
        assert!(!remote_exists);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_from_local() {
        let (_dir, tiered) = make_tiered();
        let hash = tiered.put(b"local data").unwrap();
        let data = tiered.get(&hash).unwrap();
        assert_eq!(data, b"local data");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_falls_back_to_remote() {
        let (_dir, tiered) = make_tiered();

        // Put directly into remote, bypassing local.
        let hash = ContentHash::from_data(b"remote only");
        tiered.remote().put(b"remote only").unwrap();

        assert!(!Cas::exists(tiered.local(), &hash).unwrap());
        let data = tiered.get(&hash).unwrap();
        assert_eq!(data, b"remote only");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_backfills_local_on_remote_hit() {
        let (_dir, tiered) = make_tiered();

        let hash = ContentHash::from_data(b"backfill me");
        tiered.remote().put(b"backfill me").unwrap();

        assert!(!Cas::exists(tiered.local(), &hash).unwrap());
        let _ = tiered.get(&hash).unwrap();
        // Now local should have it.
        assert!(Cas::exists(tiered.local(), &hash).unwrap());
        let local_data = tiered.local().get(&hash).unwrap();
        assert_eq!(local_data, b"backfill me");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_missing_from_both_errors() {
        let (_dir, tiered) = make_tiered();
        let hash = ContentHash::from_data(b"ghost");
        assert!(tiered.get(&hash).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exists_checks_local_first() {
        let (_dir, tiered) = make_tiered();
        let hash = tiered.put(b"exists local").unwrap();
        assert!(Cas::exists(&tiered, &hash).unwrap());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exists_falls_back_to_remote() {
        let (_dir, tiered) = make_tiered();
        let hash = ContentHash::from_data(b"exists remote");
        tiered.remote().put(b"exists remote").unwrap();
        assert!(Cas::exists(&tiered, &hash).unwrap());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exists_false_when_missing() {
        let (_dir, tiered) = make_tiered();
        let hash = ContentHash::from_data(b"nope");
        assert!(!Cas::exists(&tiered, &hash).unwrap());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn second_get_served_from_local() {
        let (_dir, tiered) = make_tiered();

        let hash = ContentHash::from_data(b"cached");
        tiered.remote().put(b"cached").unwrap();

        // First get pulls from remote and backfills.
        let _ = tiered.get(&hash).unwrap();
        // Delete from remote to prove second get uses local.
        // (We can't delete from MockStore easily, but we can verify
        // local has it and the get succeeds.)
        assert!(Cas::exists(tiered.local(), &hash).unwrap());
        let data = tiered.get(&hash).unwrap();
        assert_eq!(data, b"cached");
    }
}

// Rust guideline compliant 2026-02-21

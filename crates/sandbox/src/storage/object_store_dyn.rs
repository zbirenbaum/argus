//! Dynamic dispatch wrapper for [`ObjectStore`].
//!
//! Since [`ObjectStore`] uses return-position `impl Trait` (RPITIT),
//! it cannot be used as `dyn ObjectStore`. This module provides
//! [`DynObjectStore`] as a type-erased wrapper that the upload pool
//! and other components can hold behind an `Arc`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;

use super::s3::ObjectStore;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Internal trait with boxed futures for dynamic dispatch.
trait ObjectStoreBoxed: Send + Sync + 'static {
    fn put_boxed<'a>(&'a self, key: &'a str, data: Vec<u8>) -> BoxFuture<'a, Result<()>>;
    fn get_boxed<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Vec<u8>>>;
    fn exists_boxed<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<bool>>;
    fn list_boxed<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<String>>>;
}

impl<T: ObjectStore> ObjectStoreBoxed for T {
    fn put_boxed<'a>(&'a self, key: &'a str, data: Vec<u8>) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.put(key, data))
    }

    fn get_boxed<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(self.get(key))
    }

    fn exists_boxed<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.exists(key))
    }

    fn list_boxed<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(self.list(prefix))
    }
}

/// Type-erased object store for dynamic dispatch.
///
/// Wraps any [`ObjectStore`] implementor and boxes the futures so it
/// can be shared via `Arc<DynObjectStore>`.
#[derive(Clone)]
pub struct DynObjectStore {
    inner: Arc<dyn ObjectStoreBoxed>,
}

impl std::fmt::Debug for DynObjectStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynObjectStore").finish_non_exhaustive()
    }
}

impl DynObjectStore {
    /// Wrap a concrete [`ObjectStore`] for dynamic dispatch.
    pub fn new<T: ObjectStore>(store: T) -> Self {
        Self {
            inner: Arc::new(store),
        }
    }

    /// Upload bytes to the given key.
    ///
    /// # Errors
    ///
    /// Returns an error if the upload fails.
    pub async fn put(&self, key: &str, data: Vec<u8>) -> Result<()> {
        self.inner.put_boxed(key, data).await
    }

    /// Download bytes from the given key.
    ///
    /// # Errors
    ///
    /// Returns an error if the download fails.
    pub async fn get(&self, key: &str) -> Result<Vec<u8>> {
        self.inner.get_boxed(key).await
    }

    /// Check whether a key exists.
    ///
    /// # Errors
    ///
    /// Returns an error on transient failures.
    pub async fn exists(&self, key: &str) -> Result<bool> {
        self.inner.exists_boxed(key).await
    }

    /// List all keys under a prefix.
    ///
    /// # Errors
    ///
    /// Returns an error on transient failures.
    pub async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        self.inner.list_boxed(prefix).await
    }
}

//! Storage layer: digest cache, event log, S3 integration, upload pool.
//!
//! The [`s3`] module provides the [`ObjectStore`](s3::ObjectStore)
//! trait and [`S3Client`](s3::S3Client) implementation. The
//! [`upload_pool`] module manages async upload workers with retry.
//! [`object_store_dyn`] bridges the RPITIT trait to dynamic dispatch.

pub(crate) mod digest_cache;
pub(crate) mod event_log;
pub(crate) mod local_buffer;
pub(crate) mod object_store_dyn;
pub(crate) mod s3;
pub(crate) mod upload_job;
pub(crate) mod upload_pool;

pub(crate) use digest_cache::{DigestCache, DigestCacheStats, DigestEntry};
pub(crate) use event_log::EventLog;
pub(crate) use local_buffer::LocalBuffer;
pub(crate) use object_store_dyn::DynObjectStore;
pub(crate) use s3::{ObjectStore, S3Client};
pub(crate) use upload_job::UploadJob;
pub(crate) use upload_pool::{
    UploadConfirmation, UploadPool, UploadStats, UploadStatsSnapshot,
};

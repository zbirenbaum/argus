//! Storage layer: digest cache, event log, S3 integration, upload pool.
//!
//! The [`s3`] module provides the [`ObjectStore`](s3::ObjectStore)
//! trait and [`S3Client`](s3::S3Client) implementation. The
//! [`upload_pool`] module manages async upload workers with retry.
//! [`object_store_dyn`] bridges the RPITIT trait to dynamic dispatch.

pub mod digest_cache;
pub mod event_log;
pub mod local_buffer;
pub mod object_store_dyn;
pub mod s3;
pub mod upload_job;
pub mod upload_pool;

pub use digest_cache::{DigestCache, DigestCacheStats, DigestEntry};
#[doc(inline)]
pub use event_log::EventLog;
#[doc(inline)]
pub use local_buffer::LocalBuffer;
#[doc(inline)]
pub use object_store_dyn::DynObjectStore;
#[doc(inline)]
pub use s3::{ObjectStore, S3Client};
#[doc(inline)]
pub use upload_job::UploadJob;
#[doc(inline)]
pub use upload_pool::{
    UploadConfirmation, UploadPool, UploadStats, UploadStatsSnapshot,
};

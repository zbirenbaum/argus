//! Remote storage, upload pool, and type-erased object store.
//!
//! The [`s3`] module provides the [`ObjectStore`](s3::ObjectStore)
//! trait and [`S3Client`](s3::S3Client) implementation. The
//! [`upload_pool`] module manages async upload workers with retry.
//! [`object_store_dyn`] bridges the RPITIT trait to dynamic dispatch.

pub mod object_store_dyn;
pub mod s3;
pub mod upload_job;
pub mod upload_pool;

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

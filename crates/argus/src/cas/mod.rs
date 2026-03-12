//! Content-addressable storage using SHA-256.
//!
//! All content (file bodies, stdio, network payloads) is addressed by
//! its SHA-256 digest. Identical content is stored exactly once.
//!
//! The [`Cas`] trait defines the provider-agnostic read/write contract.
//! [`CasBackend`] extends it with eviction and stats. Backends compose
//! via [`TieredCas`]: each layer is a fast cache in front of a slow
//! store.
//!
//! - [`MemoryCas`] — in-memory hot cache (RwLock + HashMap)
//! - [`LocalCas`] — filesystem-backed (atomic writes, dedup)
//! - [`RemoteCas`] — S3/GCS/Azure via [`DynObjectStore`](crate::storage::object_store_dyn::DynObjectStore)
//! - [`TieredCas`] — generic two-tier composition

pub mod tiered;
mod hash;
mod memory;
mod remote;
mod stats;
mod store;
mod traits;

#[doc(inline)]
pub use tiered::TieredCas;
#[doc(inline)]
pub use hash::{ContentHash, InvalidHashError};
#[doc(inline)]
pub use memory::MemoryCas;
#[doc(inline)]
pub use remote::RemoteCas;
#[doc(inline)]
pub use stats::{BackendStats, CasStats, CasStatsSnapshot};
#[doc(inline)]
pub use store::LocalCas;
#[doc(inline)]
pub use traits::{Cas, CasBackend};

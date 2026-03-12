//! Content-addressable storage using SHA-256.
//!
//! All content (file bodies, stdio, network payloads) is addressed by
//! its SHA-256 digest. Identical content is stored exactly once.
//!
//! The [`Cas`] trait defines the provider-agnostic contract.
//! [`LocalCas`] is the filesystem-backed implementation.
//! [`RemoteCas`] wraps any object store (S3, GCS, Azure) with CAS
//! semantics. [`TieredCas`] composes both: local-first writes with
//! remote read-through fallback.

mod hash;
mod remote;
mod stats;
mod store;
mod tiered;
mod traits;

#[cfg(test)]
mod memory;

#[doc(inline)]
pub use hash::{ContentHash, InvalidHashError};
#[cfg(test)]
#[doc(inline)]
pub use memory::MemoryCas;
#[doc(inline)]
pub use remote::RemoteCas;
#[doc(inline)]
pub use stats::{CasStats, CasStatsSnapshot};
#[doc(inline)]
pub use store::LocalCas;
#[doc(inline)]
pub use tiered::TieredCas;
#[doc(inline)]
pub use traits::Cas;

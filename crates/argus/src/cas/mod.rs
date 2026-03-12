//! Content-addressable storage using SHA-256.
//!
//! All content (file bodies, stdio, network payloads) is addressed by
//! its SHA-256 digest. Identical content is stored exactly once.
//! Objects live on disk at `{root}/{hash[0:2]}/{hash[2:]}` and are
//! written atomically via temp-file-fsync-rename.

mod hash;
mod stats;
mod store;

#[doc(inline)]
pub use hash::{ContentHash, InvalidHashError};
#[doc(inline)]
pub use stats::{CasStats, CasStatsSnapshot};
#[doc(inline)]
pub use store::CasStore;

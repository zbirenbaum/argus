//! Upload job types submitted to the [`UploadPool`](super::upload_pool::UploadPool).

use crate::cas::ContentHash;

use super::s3::S3Client;

/// A unit of work submitted to the upload pool.
#[derive(Debug, Clone)]
pub enum UploadJob {
    /// Content-addressable blob.
    CasObject {
        /// Content hash used as the key.
        hash: ContentHash,
        /// Raw content bytes.
        data: Vec<u8>,
    },
    /// Event log segment.
    EventSegment {
        /// Agent that produced this segment.
        agent_id: String,
        /// Monotonic segment sequence number.
        seq: u64,
        /// JSONL-encoded event data.
        data: Vec<u8>,
    },
    /// Merkle tree checkpoint.
    Checkpoint {
        /// Agent that owns this checkpoint.
        agent_id: String,
        /// Checkpoint sequence number.
        seq: u64,
        /// Serialized checkpoint data.
        data: Vec<u8>,
    },
    /// Digest cache snapshot for fast recovery.
    DigestCacheSnapshot {
        /// Agent that owns this cache.
        agent_id: String,
        /// Serialized cache data.
        data: Vec<u8>,
    },
}

impl UploadJob {
    pub(super) fn s3_key(&self) -> String {
        match self {
            Self::CasObject { hash, .. } => S3Client::cas_key(hash),
            Self::EventSegment { agent_id, seq, .. } => {
                S3Client::event_segment_key(agent_id, *seq)
            }
            Self::Checkpoint { agent_id, seq, .. } => {
                S3Client::checkpoint_key(agent_id, *seq)
            }
            Self::DigestCacheSnapshot { agent_id, .. } => {
                S3Client::digest_cache_key(agent_id)
            }
        }
    }

    pub(super) fn data(&self) -> &[u8] {
        match self {
            Self::CasObject { data, .. }
            | Self::EventSegment { data, .. }
            | Self::Checkpoint { data, .. }
            | Self::DigestCacheSnapshot { data, .. } => data,
        }
    }

    pub(super) fn into_data(self) -> Vec<u8> {
        match self {
            Self::CasObject { data, .. }
            | Self::EventSegment { data, .. }
            | Self::Checkpoint { data, .. }
            | Self::DigestCacheSnapshot { data, .. } => data,
        }
    }
}

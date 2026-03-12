//! Custom serde helpers for fd_table types.
//!
//! Extracted from `fd_table` to keep that module under the 300-line limit.

use std::net::SocketAddr;

use serde::Deserialize;

/// Serializes an optional `SocketAddr` as its string representation.
pub(crate) fn serialize_socket_addr<S: serde::Serializer>(
    addr: &Option<SocketAddr>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match addr {
        Some(a) => s.serialize_some(&a.to_string()),
        None => s.serialize_none(),
    }
}

/// Deserializes an optional `SocketAddr` from its string representation.
pub(crate) fn deserialize_socket_addr<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<SocketAddr>, D::Error> {
    let opt: Option<String> = Option::deserialize(d)?;
    match opt {
        Some(s) => s
            .parse()
            .map(Some)
            .map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

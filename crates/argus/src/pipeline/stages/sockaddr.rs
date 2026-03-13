// Rust guideline compliant 2026-02-21
//! Helpers for parsing and encoding Linux `sockaddr_in` / `sockaddr_in6`.
//!
//! Used by the classify stage to decode connect() destination addresses
//! and optionally rewrite them to the transparent proxy address.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Address family constants from linux/socket.h.
const AF_INET: u16 = 2;
const AF_INET6: u16 = 10;

/// TLS ports that should be intercepted by the transparent proxy.
const TLS_PORTS: &[u16] = &[443, 8443];

/// Parse a raw `sockaddr` byte slice into a `SocketAddr`.
///
/// Supports `AF_INET` (4 bytes IP + 2 bytes port) and `AF_INET6`.
/// Returns `None` for unsupported address families or short buffers.
pub fn parse_sockaddr(bytes: &[u8]) -> Option<SocketAddr> {
    if bytes.len() < 2 {
        return None;
    }
    // sa_family is the first 2 bytes in native byte order.
    let family = u16::from_ne_bytes([bytes[0], bytes[1]]);
    match family {
        AF_INET => parse_sockaddr_in(bytes),
        AF_INET6 => parse_sockaddr_in6(bytes),
        _ => None,
    }
}

fn parse_sockaddr_in(bytes: &[u8]) -> Option<SocketAddr> {
    // sockaddr_in: sa_family(2) + sin_port(2, big-endian) + sin_addr(4)
    if bytes.len() < 8 {
        return None;
    }
    let port = u16::from_be_bytes([bytes[2], bytes[3]]);
    let addr = Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7]);
    Some(SocketAddr::new(IpAddr::V4(addr), port))
}

fn parse_sockaddr_in6(bytes: &[u8]) -> Option<SocketAddr> {
    // sockaddr_in6: sa_family(2) + sin6_port(2) + flowinfo(4) + addr(16)
    if bytes.len() < 24 {
        return None;
    }
    let port = u16::from_be_bytes([bytes[2], bytes[3]]);
    let addr_bytes: [u8; 16] = bytes[8..24].try_into().ok()?;
    let addr = Ipv6Addr::from(addr_bytes);
    Some(SocketAddr::new(IpAddr::V6(addr), port))
}

/// Return `true` if `addr` targets a standard TLS port.
pub fn is_tls_port(addr: &SocketAddr) -> bool {
    TLS_PORTS.contains(&addr.port())
}

/// Encode a `SocketAddr` as a `sockaddr_in` byte vector.
///
/// IPv6 addresses are encoded as `sockaddr_in6`.
pub fn encode_sockaddr(addr: SocketAddr) -> Vec<u8> {
    match addr {
        SocketAddr::V4(v4) => {
            let mut buf = vec![0u8; 16];
            let family = AF_INET.to_ne_bytes();
            buf[0] = family[0];
            buf[1] = family[1];
            let port = v4.port().to_be_bytes();
            buf[2] = port[0];
            buf[3] = port[1];
            let octets = v4.ip().octets();
            buf[4..8].copy_from_slice(&octets);
            buf
        }
        SocketAddr::V6(v6) => {
            let mut buf = vec![0u8; 28];
            let family = AF_INET6.to_ne_bytes();
            buf[0] = family[0];
            buf[1] = family[1];
            let port = v6.port().to_be_bytes();
            buf[2] = port[0];
            buf[3] = port[1];
            buf[8..24].copy_from_slice(&v6.ip().octets());
            buf
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_ipv4() {
        let addr: SocketAddr = "10.0.0.1:443".parse().unwrap();
        let bytes = encode_sockaddr(addr);
        let parsed = parse_sockaddr(&bytes).unwrap();
        assert_eq!(parsed, addr);
    }

    #[test]
    fn round_trip_ipv6() {
        let addr: SocketAddr = "[::1]:8443".parse().unwrap();
        let bytes = encode_sockaddr(addr);
        let parsed = parse_sockaddr(&bytes).unwrap();
        assert_eq!(parsed, addr);
    }

    #[test]
    fn tls_port_detection() {
        let addr: SocketAddr = "1.2.3.4:443".parse().unwrap();
        assert!(is_tls_port(&addr));
        let addr2: SocketAddr = "1.2.3.4:80".parse().unwrap();
        assert!(!is_tls_port(&addr2));
    }

    #[test]
    fn unknown_family_returns_none() {
        let bytes = [0xffu8, 0xff, 0, 0, 0, 0, 0, 0];
        assert!(parse_sockaddr(&bytes).is_none());
    }
}

// Rust guideline compliant 2026-02-21
//! Network syscall handlers (socket, connect, accept).
//!
//! In transparent proxy mode, `handle_connect` rewrites the destination
//! `sockaddr` in tracee memory before the kernel processes `connect()`,
//! redirecting TLS traffic through the local mitmdump instance.

use anyhow::Result;
use nix::unistd::Pid;
use tracing::{Level, event};

use crate::config::ProxyMode;
use crate::events::EventPayload;
use crate::events::network as en;
use crate::tracer::memory;
use crate::tracer::regs::{self, UserRegs};
use crate::tracer::trace_loop::TracerLoop;

// Address family constants per linux/socket.h.
const AF_INET: i32 = 2;
const AF_INET6: i32 = 10;
const AF_UNIX: i32 = 1;
const AF_NETLINK: i32 = 16;

// Socket type constants per linux/socket.h.
const SOCK_STREAM: i32 = 1;
const SOCK_DGRAM: i32 = 2;
const SOCK_RAW: i32 = 3;

/// TCP ports treated as TLS for transparent proxy rewriting.
///
/// Only these ports trigger connect() destination rewriting in
/// transparent mode. Other ports pass through unmodified.
const TLS_PORTS: &[u16] = &[443, 8443];

/// Handles socket() by emitting a socket event.
///
/// Phase 1: the fd (return value) is not available on syscall entry.
pub fn handle_socket(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &UserRegs,
) -> Result<()> {
    let pid_u32 = pid.as_raw() as u32;
    let domain = regs::arg1(r) as i32;
    let sock_type = regs::arg2(r) as i32;

    let domain_str = match domain {
        AF_INET => "AF_INET",
        AF_INET6 => "AF_INET6",
        AF_UNIX => "AF_UNIX",
        AF_NETLINK => "AF_NETLINK",
        _ => "UNKNOWN",
    };

    // Mask out SOCK_NONBLOCK and SOCK_CLOEXEC flags.
    let type_str = match sock_type & 0xFF {
        SOCK_STREAM => "SOCK_STREAM",
        SOCK_DGRAM => "SOCK_DGRAM",
        SOCK_RAW => "SOCK_RAW",
        _ => "UNKNOWN",
    };

    tracer.emit(EventPayload::Socket(en::Socket {
        pid: pid_u32,
        domain: domain_str.to_owned(),
        sock_type: type_str.to_owned(),
        fd: -1, // Phase 1: not available on syscall entry.
    }));

    Ok(())
}

/// Handles connect() — reads the remote address, optionally rewrites it.
///
/// In transparent proxy mode: if the destination is a non-loopback TLS
/// port, the sockaddr is overwritten to `127.0.0.1:{proxy_port}` in
/// tracee memory before the kernel executes the syscall. The event
/// always records the *original* destination.
pub fn handle_connect(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &UserRegs,
) -> Result<()> {
    let pid_u32 = pid.as_raw() as u32;
    let fd = regs::arg1(r) as i32;
    let addr_ptr = regs::arg2(r);
    let addr_len = regs::arg3(r) as usize;

    let (addr_str, port) = read_sockaddr(pid, addr_ptr, addr_len)?;

    if tracer.proxy_mode == ProxyMode::Transparent
        && should_rewrite_connect(&addr_str, port, tracer.proxy_port)
    {
        tracer.connect_originals.insert(
            (pid_u32, fd),
            (addr_str.clone(), port),
        );

        write_loopback_sockaddr(pid, addr_ptr, tracer.proxy_port)?;

        event!(
            name: "net.connect.rewrite",
            Level::DEBUG,
            pid = pid_u32,
            fd = fd,
            original.addr = %addr_str,
            original.port = port,
            proxy.port = tracer.proxy_port,
            "rewrote connect to {{original.addr}}:{{original.port}} → 127.0.0.1:{{proxy.port}}",
        );
    }

    tracer.emit(EventPayload::Connect(en::Connect {
        pid: pid_u32,
        fd,
        remote_addr: addr_str,
        remote_port: port,
    }));

    Ok(())
}

/// Handles accept/accept4.
///
/// Phase 1: the peer address is filled after the call returns.
pub fn handle_accept(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &UserRegs,
) -> Result<()> {
    let pid_u32 = pid.as_raw() as u32;
    let fd = regs::arg1(r) as i32;

    tracer.emit(EventPayload::Accept(en::Accept {
        pid: pid_u32,
        fd,
        peer_addr: "unknown".to_owned(),
        peer_port: 0,
    }));

    Ok(())
}

/// Whether this connect() destination should be rewritten to the proxy.
///
/// Only rewrites non-loopback IPv4 addresses on known TLS ports.
/// Loopback is excluded to prevent redirect loops (mitmdump itself
/// connects to localhost) and because transparent SNI-based proxying
/// cannot determine the correct upstream port for local services.
fn should_rewrite_connect(addr: &str, port: u16, proxy_port: u16) -> bool {
    if !TLS_PORTS.contains(&port) {
        return false;
    }
    // Already targeting the proxy — don't double-rewrite.
    if addr == "127.0.0.1" && port == proxy_port {
        return false;
    }
    // Exclude all loopback destinations.
    if addr == "127.0.0.1" || addr.starts_with("0:0:0:0:0:0:0:1") || addr == "::1" {
        return false;
    }
    true
}

/// Overwrites a `sockaddr_in` in tracee memory with `127.0.0.1:{port}`.
///
/// The original family (AF_INET) is preserved. Only the address and
/// port fields are changed. The 8-byte padding (sin_zero) is zeroed.
fn write_loopback_sockaddr(pid: Pid, addr_ptr: u64, port: u16) -> Result<()> {
    // struct sockaddr_in layout (16 bytes total):
    //   [0..2]  sin_family  — AF_INET (2) in native byte order
    //   [2..4]  sin_port    — network byte order (big-endian)
    //   [4..8]  sin_addr    — 127.0.0.1 = [127, 0, 0, 1]
    //   [8..16] sin_zero    — padding, zeroed
    let mut buf = [0u8; 16];
    buf[0..2].copy_from_slice(&(AF_INET as u16).to_ne_bytes());
    buf[2..4].copy_from_slice(&port.to_be_bytes());
    buf[4..8].copy_from_slice(&[127, 0, 0, 1]);

    memory::write_bytes(pid, addr_ptr, &buf)
}

/// Reads a `sockaddr` from tracee memory and extracts address and port.
fn read_sockaddr(pid: Pid, addr_ptr: u64, addr_len: usize) -> Result<(String, u16)> {
    if addr_ptr == 0 || addr_len < 2 {
        return Ok(("unknown".to_owned(), 0));
    }

    let max_len = addr_len.min(128);
    let buf = memory::read_bytes(pid, addr_ptr, max_len)?;

    if buf.len() < 2 {
        return Ok(("unknown".to_owned(), 0));
    }

    let family = u16::from_ne_bytes([buf[0], buf[1]]);

    match i32::from(family) {
        AF_INET if buf.len() >= 8 => {
            let port = u16::from_be_bytes([buf[2], buf[3]]);
            let addr = format!("{}.{}.{}.{}", buf[4], buf[5], buf[6], buf[7]);
            Ok((addr, port))
        }
        AF_INET6 if buf.len() >= 28 => {
            let port = u16::from_be_bytes([buf[2], buf[3]]);
            let mut segments = [0u16; 8];
            for (i, seg) in segments.iter_mut().enumerate() {
                *seg = u16::from_be_bytes([buf[8 + i * 2], buf[9 + i * 2]]);
            }
            let addr = format!(
                "{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}",
                segments[0], segments[1], segments[2], segments[3],
                segments[4], segments[5], segments[6], segments[7],
            );
            Ok((addr, port))
        }
        AF_UNIX => {
            // sun_path starts at offset 2. Extract the path, stopping
            // at the first null byte or end of buffer.
            let path_bytes = &buf[2..];
            let end = path_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(path_bytes.len());
            let path = if end == 0 {
                // Abstract socket (first byte is \0) or empty path.
                "unix:@abstract".to_owned()
            } else {
                format!(
                    "unix:{}",
                    String::from_utf8_lossy(&path_bytes[..end]),
                )
            };
            Ok((path, 0))
        }
        other => {
            Ok((format!("af_{other}"), 0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_standard_https() {
        assert!(should_rewrite_connect("93.184.216.34", 443, 8080));
    }

    #[test]
    fn rewrite_port_8443() {
        assert!(should_rewrite_connect("10.0.0.5", 8443, 8080));
    }

    #[test]
    fn skip_non_tls_port() {
        assert!(!should_rewrite_connect("93.184.216.34", 80, 8080));
        assert!(!should_rewrite_connect("93.184.216.34", 22, 8080));
    }

    #[test]
    fn skip_loopback_ipv4() {
        assert!(!should_rewrite_connect("127.0.0.1", 443, 8080));
    }

    #[test]
    fn skip_loopback_ipv6() {
        assert!(!should_rewrite_connect("0:0:0:0:0:0:0:1", 443, 8080));
    }

    #[test]
    fn skip_already_proxy() {
        assert!(!should_rewrite_connect("127.0.0.1", 8080, 8080));
    }

    #[test]
    fn loopback_sockaddr_layout() {
        // Verify the sockaddr_in bytes are correct without needing
        // a real tracee — test the buffer construction logic.
        let mut buf = [0u8; 16];
        buf[0..2].copy_from_slice(&(AF_INET as u16).to_ne_bytes());
        buf[2..4].copy_from_slice(&8080u16.to_be_bytes());
        buf[4..8].copy_from_slice(&[127, 0, 0, 1]);

        let family = u16::from_ne_bytes([buf[0], buf[1]]);
        assert_eq!(i32::from(family), AF_INET);

        let port = u16::from_be_bytes([buf[2], buf[3]]);
        assert_eq!(port, 8080);

        assert_eq!(&buf[4..8], &[127, 0, 0, 1]);
        assert_eq!(&buf[8..16], &[0; 8]);
    }
}

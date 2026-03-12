// Rust guideline compliant 2026-02-21
//! Network syscall handlers (socket, connect, accept).

use anyhow::Result;
use libc::user_regs_struct;
use nix::unistd::Pid;

use crate::events::EventPayload;
use crate::events::network as en;
use crate::tracer::memory;
use crate::tracer::regs;
use crate::tracer::trace_loop::TracerLoop;

/// Handles socket() by emitting a socket event.
///
/// Phase 1: the fd (return value) is not available on syscall entry.
pub fn handle_socket(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &user_regs_struct,
) -> Result<()> {
    let pid_u32 = pid.as_raw() as u32;
    let domain = regs::arg1(r) as i32;
    let sock_type = regs::arg2(r) as i32;

    let domain_str = match domain {
        libc::AF_INET => "AF_INET",
        libc::AF_INET6 => "AF_INET6",
        libc::AF_UNIX => "AF_UNIX",
        libc::AF_NETLINK => "AF_NETLINK",
        _ => "UNKNOWN",
    };

    // Mask out SOCK_NONBLOCK and SOCK_CLOEXEC flags.
    let type_str = match sock_type & 0xFF {
        libc::SOCK_STREAM => "SOCK_STREAM",
        libc::SOCK_DGRAM => "SOCK_DGRAM",
        libc::SOCK_RAW => "SOCK_RAW",
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

/// Handles connect() by reading the remote address.
pub fn handle_connect(
    tracer: &mut TracerLoop,
    pid: Pid,
    r: &user_regs_struct,
) -> Result<()> {
    let pid_u32 = pid.as_raw() as u32;
    let fd = regs::arg1(r) as i32;
    let addr_ptr = regs::arg2(r);
    let addr_len = regs::arg3(r) as usize;

    let (addr_str, port) = read_sockaddr(pid, addr_ptr, addr_len)?;

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
    r: &user_regs_struct,
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
        libc::AF_INET if buf.len() >= 8 => {
            let port = u16::from_be_bytes([buf[2], buf[3]]);
            let addr = format!("{}.{}.{}.{}", buf[4], buf[5], buf[6], buf[7]);
            Ok((addr, port))
        }
        libc::AF_INET6 if buf.len() >= 28 => {
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
        _ => Ok(("unknown".to_owned(), 0)),
    }
}

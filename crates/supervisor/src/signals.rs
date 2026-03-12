// Rust guideline compliant 2026-02-21
//! Signal handling for graceful supervisor shutdown.
//!
//! Installs handlers for `SIGTERM` and `SIGINT` that set an atomic flag.
//! The ptrace loop can check this flag to initiate graceful shutdown
//! rather than being killed mid-operation.

use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{Level, event};

/// Global flag set by signal handlers to request shutdown.
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Returns `true` if a shutdown signal has been received.
///
/// The tracer loop will poll this once graceful shutdown is implemented.
pub fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Relaxed)
}

/// Installs `SIGTERM` and `SIGINT` handlers that set the shutdown flag.
///
/// Uses `signal(2)` for async-signal-safe behavior. The handler only
/// performs an atomic store, which is safe in signal context.
pub fn install_handler() {
    // SAFETY: the handler function only performs an atomic store,
    // which is async-signal-safe. We register for SIGTERM and SIGINT.
    unsafe {
        let handler = signal_handler as *const () as libc::sighandler_t;

        if libc::signal(libc::SIGTERM, handler) == libc::SIG_ERR {
            event!(
                name: "supervisor.signals.sigterm_error",
                Level::WARN,
                "failed to install SIGTERM handler",
            );
        }
        if libc::signal(libc::SIGINT, handler) == libc::SIG_ERR {
            event!(
                name: "supervisor.signals.sigint_error",
                Level::WARN,
                "failed to install SIGINT handler",
            );
        }
    }

    event!(
        name: "supervisor.signals.installed",
        Level::DEBUG,
        "installed SIGTERM/SIGINT shutdown handlers",
    );
}

/// Async-signal-safe handler that sets the shutdown flag.
///
/// Only performs an atomic store; no allocations or locks.
extern "C" fn signal_handler(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_flag_initially_false() {
        // Reset for test isolation (other tests may have set it).
        SHUTDOWN_REQUESTED.store(false, Ordering::Relaxed);
        assert!(!shutdown_requested());
    }

    #[test]
    fn signal_handler_sets_flag() {
        SHUTDOWN_REQUESTED.store(false, Ordering::Relaxed);
        signal_handler(libc::SIGTERM);
        assert!(shutdown_requested());
        // Reset.
        SHUTDOWN_REQUESTED.store(false, Ordering::Relaxed);
    }

    #[test]
    fn install_handler_does_not_panic() {
        install_handler();
    }
}

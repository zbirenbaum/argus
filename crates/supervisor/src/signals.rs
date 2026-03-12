// Rust guideline compliant 2026-02-21
//! Signal handling for graceful supervisor shutdown.
//!
//! Installs handlers for `SIGTERM` and `SIGINT` that set an atomic flag.
//! The ptrace loop checks this flag to initiate graceful shutdown
//! rather than being killed mid-operation.

use std::sync::atomic::{AtomicBool, Ordering};

use nix::sys::signal::{SigHandler, Signal, signal};
use tracing::{Level, event};

/// Global flag set by signal handlers to request shutdown.
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Returns `true` if a shutdown signal has been received.
pub fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Relaxed)
}

/// Resets the shutdown flag. Only for use in tests.
#[cfg(test)]
fn reset_shutdown_flag() {
    SHUTDOWN_REQUESTED.store(false, Ordering::Relaxed);
}

/// Installs `SIGTERM` and `SIGINT` handlers that set the shutdown flag.
///
/// # Errors
///
/// Logs a warning if handler installation fails but does not return
/// an error — the supervisor can still function without graceful
/// shutdown.
pub fn install_handler() {
    let handler = SigHandler::Handler(signal_handler);

    // SAFETY: the handler only performs an atomic store, which is
    // async-signal-safe.
    if let Err(e) = unsafe { signal(Signal::SIGTERM, handler) } {
        event!(
            name: "supervisor.signals.sigterm_error",
            Level::WARN,
            error.message = %e,
            "failed to install SIGTERM handler: {{error.message}}",
        );
    }
    if let Err(e) = unsafe { signal(Signal::SIGINT, handler) } {
        event!(
            name: "supervisor.signals.sigint_error",
            Level::WARN,
            error.message = %e,
            "failed to install SIGINT handler: {{error.message}}",
        );
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
extern "C" fn signal_handler(_sig: std::ffi::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes tests that touch the global `SHUTDOWN_REQUESTED` flag.
    static SIGNAL_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn shutdown_flag_initially_false() {
        let _guard = SIGNAL_LOCK.lock().unwrap();
        reset_shutdown_flag();
        assert!(!shutdown_requested());
    }

    #[test]
    fn signal_handler_sets_flag() {
        let _guard = SIGNAL_LOCK.lock().unwrap();
        reset_shutdown_flag();
        signal_handler(Signal::SIGTERM as std::ffi::c_int);
        assert!(shutdown_requested());
        reset_shutdown_flag();
    }

    #[test]
    fn install_handler_does_not_panic() {
        let _guard = SIGNAL_LOCK.lock().unwrap();
        install_handler();
        reset_shutdown_flag();
    }
}

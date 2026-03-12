//! Mitmdump process lifecycle management.
//!
//! Spawns `mitmdump` in transparent mode, waits for it to become ready
//! by probing the listen port, and provides graceful shutdown via
//! `SIGTERM`. The handle tracks whether the child is still running so
//! the supervisor can react to unexpected exits.

// Rust guideline compliant 2026-02-21

use std::net::TcpStream;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{event, Level};

use crate::net::CaPaths;

/// Maximum time to wait for mitmdump to accept connections.
const READINESS_TIMEOUT: Duration = Duration::from_secs(10);

/// Interval between TCP connect probes during readiness check.
const PROBE_INTERVAL: Duration = Duration::from_millis(50);

/// Handle to a running `mitmdump` child process.
#[derive(Debug)]
pub struct MitmdumpHandle {
    child: Child,
    port: u16,
}

impl MitmdumpHandle {
    /// Send `SIGTERM` and wait for the process to exit.
    ///
    /// # Errors
    ///
    /// Returns an error if the kill signal cannot be sent or the
    /// process cannot be waited on.
    pub fn stop(&mut self) -> Result<()> {
        event!(
            name: "net.mitmdump.stopping",
            Level::INFO,
            mitmdump.port = self.port,
            "stopping mitmdump on port {{mitmdump.port}}",
        );
        self.child
            .kill()
            .context("failed to send kill signal to mitmdump")?;
        self.child
            .wait()
            .context("failed to wait for mitmdump to exit")?;
        Ok(())
    }

    /// Check whether the mitmdump process is still running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for MitmdumpHandle {
    fn drop(&mut self) {
        if self.is_running() {
            let _ = self.stop();
        }
    }
}

/// Spawn `mitmdump` in transparent proxy mode.
///
/// Blocks until the proxy accepts TCP connections on `port`, or until
/// the readiness timeout (10 s) expires.
///
/// # Errors
///
/// Returns an error if `mitmdump` is not installed, fails to start,
/// or does not become ready within the timeout.
pub fn start_mitmdump(ca: &CaPaths, port: u16) -> Result<MitmdumpHandle> {
    let ca_dir = ca
        .cert
        .parent()
        .context("CA cert path has no parent directory")?;

    let child = Command::new("mitmdump")
        .args([
            "--mode",
            "transparent",
            "--listen-host",
            "127.0.0.1",
            "--listen-port",
            &port.to_string(),
            "--set",
            &format!("confdir={}", ca_dir.display()),
            "--quiet",
        ])
        .spawn()
        .context(
            "failed to spawn mitmdump — is it installed? \
             Install with: pip install mitmproxy",
        )?;

    event!(
        name: "net.mitmdump.spawned",
        Level::INFO,
        mitmdump.port = port,
        mitmdump.pid = child.id(),
        "spawned mitmdump on port {{mitmdump.port}} (pid {{mitmdump.pid}})",
    );

    let mut handle = MitmdumpHandle { child, port };

    wait_for_ready(port).inspect_err(|_| {
        let _ = handle.stop();
    })?;

    Ok(handle)
}

/// Poll TCP connect until the port accepts connections.
fn wait_for_ready(port: u16) -> Result<()> {
    let deadline = Instant::now() + READINESS_TIMEOUT;
    let addr = format!("127.0.0.1:{port}");

    while Instant::now() < deadline {
        if TcpStream::connect(&addr).is_ok() {
            event!(
                name: "net.mitmdump.ready",
                Level::INFO,
                mitmdump.port = port,
                "mitmdump is ready on port {{mitmdump.port}}",
            );
            return Ok(());
        }
        std::thread::sleep(PROBE_INTERVAL);
    }

    bail!(
        "mitmdump did not become ready on port {port} within {} seconds",
        READINESS_TIMEOUT.as_secs()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    #[ignore = "requires mitmdump to be installed"]
    fn mitmdump_starts_and_is_running() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ca = crate::net::generate_ca(tmp.path()).unwrap();

        let mut handle = start_mitmdump(&ca, 18080).unwrap();
        assert!(handle.is_running(), "mitmdump should be running");

        handle.stop().unwrap();
        assert!(!handle.is_running(), "mitmdump should have stopped");
    }

    #[test]
    fn missing_mitmdump_gives_clear_error() {
        let ca = CaPaths {
            cert: PathBuf::from("/nonexistent/ca-cert.pem"),
            key: PathBuf::from("/nonexistent/ca-key.pem"),
        };

        let result = start_mitmdump(&ca, 18081);

        if result.is_err() {
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("mitmdump") || msg.contains("not found"),
                "error should mention mitmdump: {msg}"
            );
        }
    }
}

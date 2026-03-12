//! Mitmdump process lifecycle management.
//!
//! Spawns `mitmdump` as a regular HTTP/HTTPS proxy, waits for it to
//! become ready by probing the listen port, and provides graceful
//! shutdown via `SIGTERM`. The handle tracks whether the child is still
//! running so the supervisor can react to unexpected exits.

use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use tracing::{event, Level};

use crate::net::CaPaths;

/// Maximum time to wait for mitmdump to accept connections.
const READINESS_TIMEOUT: Duration = Duration::from_secs(10);

/// Interval between TCP connect probes during readiness check.
const PROBE_INTERVAL: Duration = Duration::from_millis(50);

/// Grace period after SIGTERM before escalating to SIGKILL.
const SIGTERM_GRACE: Duration = Duration::from_secs(3);

/// Handle to a running `mitmdump` child process.
#[derive(Debug)]
pub struct MitmdumpHandle {
    child: Child,
    port: u16,
    /// Path to the flow output NDJSON file, if an addon is configured.
    flow_output: Option<PathBuf>,
}

impl MitmdumpHandle {
    /// Path to the NDJSON file where flows are written.
    ///
    /// Returns `None` if no addon script was configured.
    pub fn flow_output_path(&self) -> Option<&PathBuf> {
        self.flow_output.as_ref()
    }
}

impl MitmdumpHandle {
    /// Send `SIGTERM` and wait for the process to exit.
    ///
    /// Falls back to `SIGKILL` if the process does not exit within
    /// [`SIGTERM_GRACE`] seconds.
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

        let pid = Pid::from_raw(self.child.id() as i32);
        signal::kill(pid, Signal::SIGTERM)
            .context("failed to send SIGTERM to mitmdump")?;

        let deadline = Instant::now() + SIGTERM_GRACE;
        loop {
            if !self.is_running() {
                self.child
                    .wait()
                    .context("failed to wait for mitmdump after SIGTERM")?;
                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        event!(
            name: "net.mitmdump.sigkill_fallback",
            Level::WARN,
            mitmdump.port = self.port,
            "mitmdump did not exit after SIGTERM, sending SIGKILL",
        );
        self.child
            .kill()
            .context("failed to send SIGKILL to mitmdump")?;
        self.child
            .wait()
            .context("failed to wait for mitmdump after SIGKILL")?;
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

/// Configuration for the mitmdump addon script.
#[derive(Debug, Clone, Default)]
pub struct AddonConfig {
    /// Path to the Python addon script.
    pub script: Option<PathBuf>,
    /// Path where addon stdout is redirected (NDJSON flow output).
    pub output_file: Option<PathBuf>,
}

/// Spawn `mitmdump` as a regular HTTP/HTTPS proxy.
///
/// Traffic is routed here via `HTTP_PROXY`/`HTTPS_PROXY` env vars on the
/// agent process — no iptables rules needed. Blocks until the proxy
/// accepts TCP connections on `port`, or until the readiness timeout
/// (10 s) expires.
///
/// # Errors
///
/// Returns an error if `mitmdump` is not installed, fails to start,
/// or does not become ready within the timeout.
pub fn start_mitmdump(ca: &CaPaths, port: u16) -> Result<MitmdumpHandle> {
    start_mitmdump_with_addon("mitmdump", ca, port, &AddonConfig::default())
}

/// Spawn `mitmdump` with an addon script for flow capture.
///
/// The addon script's stdout is redirected to `addon.output_file`.
/// Use [`MitmdumpHandle::flow_output_path`] to get the path for
/// a [`FlowWatcher`](super::FlowWatcher).
///
/// # Errors
///
/// Returns an error if `mitmdump` is not installed, fails to start,
/// or does not become ready within the timeout.
pub fn start_mitmdump_with_flow_capture(
    ca: &CaPaths,
    port: u16,
    addon: &AddonConfig,
) -> Result<MitmdumpHandle> {
    start_mitmdump_with_addon("mitmdump", ca, port, addon)
}

/// Spawn a mitmdump-compatible binary with optional addon.
fn start_mitmdump_with_addon(
    cmd: &str,
    ca: &CaPaths,
    port: u16,
    addon: &AddonConfig,
) -> Result<MitmdumpHandle> {
    let ca_dir = ca
        .cert
        .parent()
        .context("CA cert path has no parent directory")?;

    let mut command = Command::new(cmd);
    command.args([
        "--listen-host",
        "127.0.0.1",
        "--listen-port",
        &port.to_string(),
        "--set",
        &format!("confdir={}", ca_dir.display()),
        "--quiet",
    ]);

    let flow_output = if let Some(script) = &addon.script {
        command.args(["-s", &script.to_string_lossy()]);
        command.arg("--set").arg("flow_detail=0");

        if let Some(output) = &addon.output_file {
            let file = std::fs::File::create(output)
                .with_context(|| format!("create flow output: {}", output.display()))?;
            command.stdout(Stdio::from(file));
        }

        addon.output_file.clone()
    } else {
        None
    };

    // Suppress mitmdump output so it doesn't pollute supervisor stdout.
    command.stderr(Stdio::null());
    if flow_output.is_none() {
        command.stdout(Stdio::null());
    }

    let child = command
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

    let mut handle = MitmdumpHandle {
        child,
        port,
        flow_output,
    };

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

        let result = start_mitmdump_with_addon(
            "/nonexistent/mitmdump",
            &ca,
            18081,
            &AddonConfig::default(),
        );

        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("mitmdump") || msg.contains("not found"),
            "error should mention mitmdump: {msg}"
        );
    }
}

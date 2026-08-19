//! Xvfb virtual display manager — the stealth foundation.
//!
//! Headless Chrome (`--headless=new`) is detectable: SwiftShader
//! WebGL, missing `window.chrome`, screen dimension mismatches.
//! Headful Chrome on a virtual X display is NOT — it has real
//! GPU compositing, real window objects, real screen geometry.
//!
//! This module starts one Xvfb at daemon init and keeps it warm.
//! Ghost launches headful Chrome on this display. The display
//! outlives individual browser processes (crash → relaunch uses
//! the same display, no Xvfb restart).
//!
//! Linux-only. macOS/Windows use headful off-screen mode
//! (`--window-position=-32000,-32000`) handled in ghost/mod.rs.

// ── Linux: real Xvfb implementation ──

#[cfg(linux_like)]
mod linux {
    use std::process::Stdio;
    use tokio::process::{Child, Command};

    use crate::error::FetchError;

    /// Display number for our Xvfb. High enough to avoid collision
    /// with real displays, low enough to be a valid X display.
    const DISPLAY_NUM: u8 = 99;

    pub struct Xvfb {
        /// None if we reused an existing Xvfb (borrowed — don't kill).
        child: Option<Child>,
    }

    impl Xvfb {
        /// Start Xvfb on :99, 1920x1080x24. Returns the DISPLAY env
        /// value (":99") for Chrome to use.
        ///
        /// If an Xvfb is already running on :99 (e.g. the MCP daemon
        /// started one), reuses it — does NOT kill or restart. This
        /// is critical for CLI+MCP coexistence: the CLI must not
        /// disrupt the daemon's warm Xvfb.
        pub async fn start() -> Result<Self, FetchError> {
            let display = format!(":{DISPLAY_NUM}");

            // Check if an X server is already alive on :99.
            // If so, reuse it — don't kill, don't restart.
            if display_alive(&display).await {
                if std::env::var_os("DONGHOST_DEBUG").is_some() {
                    eprintln!("[ghost] Xvfb already running on {display}, reusing");
                }
                return Ok(Self { child: None });
            }

            // Kill stale Xvfb on this display (crash recovery).
            let _ = tokio::process::Command::new("pkill")
                .args(["-f", &format!("Xvfb {display}")])
                .output()
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;

            // Remove stale socket + lock files. A dead Xvfb leaves
            // these behind — a new Xvfb can't bind to a stale socket,
            // and our readiness check would see the stale file and
            // think Xvfb is ready when it isn't.
            let sock_path = format!("/tmp/.X11-unix/X{DISPLAY_NUM}");
            let lock_path = format!("/tmp/.X{DISPLAY_NUM}-lock");
            let _ = std::fs::remove_file(&sock_path);
            let _ = std::fs::remove_file(&lock_path);

            // Ensure /tmp/.X11-unix/ exists. Under WSL and some minimal
            // container setups this directory is absent, and Xvfb can't
            // create the X11 socket without it.
            let _ = std::fs::create_dir_all("/tmp/.X11-unix");

            let mut cmd = Command::new("Xvfb");
            cmd.args([
                &display,
                "-screen",
                "0",
                "1920x1080x24",
                "-ac",
                "-nolisten",
                "tcp",
            ]);
            cmd.stdout(Stdio::null()).stderr(Stdio::null());

            let mut child = cmd.spawn().map_err(|e| {
                FetchError::ghost(format!(
                    "Xvfb spawn: {e} (install: apt install xvfb / pacman -S xorg-server-xvfb)"
                ))
            })?;

            // Wait for the display to be ready by polling the X11
            // socket file AND verifying we can connect to it.
            // Xvfb creates /tmp/.X11-unix/X99 when it's ready to
            // accept connections. We also try connecting to make
            // sure the socket is live, not just present.
            // 10s timeout: WSL and some containers are slower to start.
            let sock_path = format!("/tmp/.X11-unix/X{DISPLAY_NUM}");
            let ready = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    if std::fs::exists(&sock_path).unwrap_or(false)
                        && std::os::unix::net::UnixStream::connect(&sock_path).is_ok()
                    {
                        return;
                    }
                    // Check if Xvfb died early.
                    if child.try_wait().ok().flatten().is_some() {
                        return; // process exited — will fail below
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            })
            .await;

            if ready.is_err() || !std::fs::exists(&sock_path).unwrap_or(false) {
                return Err(FetchError::ghost(
                    "Xvfb failed to start (install: apt install xvfb / pacman -S xorg-server-xvfb)",
                ));
            }

            if std::env::var_os("DONGHOST_DEBUG").is_some() {
                eprintln!("[ghost] Xvfb started on {display}");
            }
            Ok(Self { child: Some(child) })
        }

        /// The DISPLAY environment value for Chrome.
        pub fn display_env(&self) -> String {
            format!(":{DISPLAY_NUM}")
        }

        /// Kill Xvfb (only if we own it).
        pub async fn kill(mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill().await;
            }
        }

        /// Check if Xvfb process is still alive.
        #[allow(dead_code)]
        pub fn is_alive(&mut self) -> bool {
            match &mut self.child {
                Some(c) => c.try_wait().map(|r| r.is_none()).unwrap_or(false),
                None => true, // borrowed — assume alive
            }
        }
    }

    /// Check if Xvfb binary is available on the system.
    pub fn is_available() -> bool {
        std::process::Command::new("which")
            .arg("Xvfb")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Check if an X display is alive by testing the X11
    /// socket file AND verifying someone is listening. A stale
    /// socket (from a killed Xvfb process) will still have the
    /// file but no server — connecting fails with ECONNREFUSED.
    async fn display_alive(_display: &str) -> bool {
        let sock = format!("/tmp/.X11-unix/X{DISPLAY_NUM}");
        if !std::fs::exists(&sock).unwrap_or(false) {
            return false;
        }
        // Socket exists — but is anyone listening? Try
        // connecting. If it fails, the socket is stale.
        std::os::unix::net::UnixStream::connect(&sock).is_ok()
    }
}

// ── Non-Linux: stub (macOS/Windows use off-screen headful mode) ──
// Android is linux_like so it uses the real Xvfb module (though
// Termux won't have Xvfb installed, the stub correctly reports
// not available and Ghost falls back to --headless=new).

#[cfg(not(linux_like))]
mod other {
    use crate::error::FetchError;

    pub struct Xvfb;

    impl Xvfb {
        pub async fn start() -> Result<Self, FetchError> {
            Err(FetchError::ghost("Xvfb not available on this platform"))
        }
        pub fn display_env(&self) -> String {
            String::new()
        }
        #[allow(dead_code)]
        pub async fn kill(self) {}
        #[allow(dead_code)]
        pub fn is_alive(&mut self) -> bool {
            false
        }
    }

    pub fn is_available() -> bool {
        false
    }
}

#[cfg(linux_like)]
pub use linux::*;
#[cfg(not(linux_like))]
pub use other::*;

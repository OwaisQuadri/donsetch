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

#[cfg(target_os = "linux")]
mod linux {
    use std::process::Stdio;
    use tokio::process::{Child, Command};

    use crate::error::FetchError;

    /// Display number for our Xvfb. High enough to avoid collision
    /// with real displays, low enough to be a valid X display.
    const DISPLAY_NUM: u8 = 99;

    pub struct Xvfb {
        child: Child,
    }

    impl Xvfb {
        /// Start Xvfb on :99, 1920x1080x24. Returns the DISPLAY env
        /// value (":99") for Chrome to use.
        pub async fn start() -> Result<Self, FetchError> {
            let display = format!(":{DISPLAY_NUM}");

            // Kill stale Xvfb on this display (crash recovery).
            let _ = tokio::process::Command::new("pkill")
                .args(["-f", &format!("Xvfb {display}")])
                .output()
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;

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
                    "Xvfb spawn: {e} (install: pacman -S xorg-server-xvfb)"
                ))
            })?;

            // Wait for the display to be ready.
            let ready = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    let r = tokio::process::Command::new("xdpyinfo")
                        .env("DISPLAY", &display)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .await;
                    if r.map(|s| s.success()).unwrap_or(false) {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            })
            .await;

            if ready.is_err() {
                // xdpyinfo might not be installed; check if the process
                // is still alive as a fallback.
                match child.try_wait() {
                    Ok(None) => {} // alive, probably fine
                    _ => {
                        return Err(FetchError::ghost(
                            "Xvfb failed to start (install: pacman -S xorg-server-xvfb)",
                        ));
                    }
                }
            }

            Ok(Self { child })
        }

        /// The DISPLAY environment value for Chrome.
        pub fn display_env(&self) -> String {
            format!(":{DISPLAY_NUM}")
        }

        /// Kill Xvfb.
        #[allow(dead_code)] // Xvfb lives for daemon lifetime (mem::forget)
        pub async fn kill(mut self) {
            let _ = self.child.kill().await;
        }

        /// Check if Xvfb process is still alive.
        #[allow(dead_code)]
        pub fn is_alive(&mut self) -> bool {
            self.child.try_wait().map(|r| r.is_none()).unwrap_or(false)
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
}

// ── Non-Linux: stub (macOS/Windows use off-screen headful mode) ──

#[cfg(not(target_os = "linux"))]
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

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(not(target_os = "linux"))]
pub use other::*;

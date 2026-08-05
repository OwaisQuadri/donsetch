//! DonGhost — the tier-2 ghost browser.
//!
//! A real Chromium, driven over raw CDP with zero
//! automation flags and zero script injection. Exists for
//! exactly two jobs DonShadow can't do:
//!   SOLVE  — pass a JS challenge, harvest clearance
//!            cookies, hand them to tier 1.
//!   RENDER — execute a JS-rendered page, hand the DOM
//!            HTML to DonSift.
//!
//! Lifecycle: lazy launch → freeze (SIGSTOP the process
//! group, 0 CPU, swappable RAM) between jobs → reap after
//! 10 min frozen. The persistent profile dir keeps cookie
//! warmth across restarts.

pub mod cache;
pub mod cdp;
pub mod manager;
pub mod ops;
pub mod proc;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Instant;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt as _;

use crate::error::FetchError;
use crate::profile::BrowserProfile;

/// Idle this long → SIGSTOP the process group.
/// (Daemon lifecycle — used by the MCP idle reaper.)
pub const FREEZE_AFTER: std::time::Duration =
    std::time::Duration::from_secs(20);
/// Frozen this long → reap entirely.
pub const REAP_AFTER: std::time::Duration =
    std::time::Duration::from_secs(600);

pub struct Ghost {
    child: Child,
    proc: proc::Proc,
    pub cdp: cdp::Cdp,
    /// Attached page session id.
    pub session: String,
    /// Our page target id.
    target: String,
    frozen: bool,
    pub last_used: Instant,
}

/// Persistent profile dir: aged state passes challenges
/// easier, and clearance cookies survive daemon restarts.
pub fn profile_dir() -> PathBuf {
    crate::paths::cache_dir().join("ghost-profile")
}

/// Locate a Chrome-family binary. Env override first, then
/// platform-specific known paths, then PATH search. No `which`
/// subprocess — works on Linux, macOS, and Windows.
fn chrome_binary() -> Result<String, FetchError> {
    if let Some(p) = std::env::var_os("DONGHOST_CHROME") {
        return Ok(p.to_string_lossy().into_owned());
    }
    // Known install locations (most reliable, no PATH needed).
    for path in known_chrome_paths() {
        if is_executable(&path) {
            return Ok(path.to_string_lossy().into_owned());
        }
    }
    // PATH search.
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in chrome_names() {
                let candidate = dir.join(name);
                if is_executable(&candidate) {
                    return Ok(candidate.to_string_lossy().into_owned());
                }
            }
        }
    }
    Err(FetchError::ghost(
        "no chromium/chrome binary found (set DONGHOST_CHROME)",
    ))
}

#[cfg(target_os = "linux")]
fn known_chrome_paths() -> Vec<PathBuf> {
    [
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/snap/bin/chromium",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(target_os = "macos")]
fn known_chrome_paths() -> Vec<PathBuf> {
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(windows)]
fn known_chrome_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        paths.push(PathBuf::from(&pf).join("Google\\Chrome\\Application\\chrome.exe"));
    }
    if let Some(pf) = std::env::var_os("ProgramFiles(x86)") {
        paths.push(PathBuf::from(&pf).join("Google\\Chrome\\Application\\chrome.exe"));
    }
    if let Some(la) = std::env::var_os("LOCALAPPDATA") {
        paths.push(PathBuf::from(&la).join("Google\\Chrome\\Application\\chrome.exe"));
    }
    paths
}

#[cfg(windows)]
fn chrome_names() -> &'static [&'static str] {
    // Edge is Chromium-based, pre-installed on Windows — often
    // the only available CDP-capable browser.
    &["chrome.exe", "msedge.exe", "chromium.exe"]
}

#[cfg(not(windows))]
fn chrome_names() -> &'static [&'static str] {
    &["chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "chrome"]
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

impl Ghost {
    /// Launch cold. Clean by construction: no automation
    /// flags, no anti-automation flags, UA pinned to the
    /// DonShadow profile so harvested cookies stay valid
    /// when tier 1 reuses them (cf_clearance binds IP+UA).
    pub async fn launch(
        profile: &BrowserProfile,
    ) -> Result<Self, FetchError> {
        let bin = chrome_binary()?;
        let dir = profile_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| FetchError::ghost(format!("profile dir: {e}")))?;
        // Stale singleton files from a SIGKILLed ghost
        // (e.g. outer timeout) block the next launch.
        for f in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
            let _ = std::fs::remove_file(dir.join(f));
        }
        let mut cmd = Command::new(bin);
        let mut chrome_args: Vec<String> = vec![
            "--headless=new".into(),
            "--remote-debugging-port=0".into(),
            format!("--user-data-dir={}", dir.display()),
            format!("--user-agent={}", profile.user_agent),
            "--window-size=1920,1080".into(),
            "--window-position=0,0".into(),
            "--lang=en-US".into(),
            "--no-first-run".into(),
            "--no-default-browser-check".into(),
            "--disable-background-networking".into(),
            "--disable-component-update".into(),
            "--disable-sync".into(),
            "--disable-translate".into(),
            "--mute-audio".into(),
        ];
        // ANGLE GPU backend: Vulkan on Linux (headless needs it);
        // Windows/macOS use their native defaults (D3D11/Metal).
        #[cfg(target_os = "linux")]
        chrome_args.push("--use-angle=vulkan".into());
        // Modern Chrome (136+) sets navigator.webdriver
        // under --headless/--remote-debugging-port even
        // raw. This blink switch restores the real-
        // browser default; not JS-enumerable.
        chrome_args.push("--disable-blink-features=AutomationControlled".into());
        chrome_args.push("about:blank".into());
        cmd.args(&chrome_args);
        // Own process group (Unix) / Job Object (Windows):
        // freeze/thaw/kill the whole browser tree.
        proc::Proc::prepare_cmd(&mut cmd);
        cmd.stdout(Stdio::null())
            .stderr(Stdio::piped());
        // No orphans even if donsetch dies hard. Linux-only:
        // macOS has no prctl; Windows uses the Job Object's
        // KILL_ON_JOB_CLOSE.
        #[cfg(target_os = "linux")]
        unsafe {
            cmd.as_std_mut().pre_exec(proc::pdeath_pre_exec);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| FetchError::ghost(format!("spawn: {e}")))?;
        let proc = proc::Proc::from_child(&child)?;

        // The ws endpoint arrives on stderr:
        // "DevTools listening on ws://127.0.0.1:PORT/..."
        let stderr = child.stderr.take().ok_or_else(|| {
            FetchError::ghost("no stderr pipe")
        })?;
        let mut lines = BufReader::new(stderr).lines();
        let ws_url = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            async {
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(i) = line.find("ws://") {
                        return Some(line[i..].trim().to_string());
                    }
                }
                None
            },
        )
        .await
        .map_err(|_| FetchError::ghost("devtools ws timeout"))?
        .ok_or_else(|| FetchError::ghost("no devtools ws line"))?;

        let cdp = cdp::Cdp::connect(&ws_url).await?;

        // One page target, attached flat.
        let target = cdp
            .call(
                None,
                "Target.createTarget",
                json!({ "url": "about:blank" }),
            )
            .await?
            .get("targetId")
            .and_then(Value::as_str)
            .ok_or_else(|| FetchError::ghost("no targetId"))?
            .to_string();
        let session = cdp
            .call(
                None,
                "Target.attachToTarget",
                json!({ "targetId": target, "flatten": true }),
            )
            .await?
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| FetchError::ghost("no sessionId"))?
            .to_string();
        cdp.call(Some(&session), "Page.enable", json!({})).await?;
        // Headless reports screen 800x600 + outer 0x0 while
        // the window is 1920x1080 — a glaring contradiction.
        // Emulation.setDeviceMetricsOverride makes the
        // geometry tell ONE story. Not Runtime; the standard
        // non-injectable way to align the viewport.
        cdp.call(
            Some(&session),
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": 1920,
                "height": 1080,
                "deviceScaleFactor": 1,
                "mobile": false,
                "screenWidth": 1920,
                "screenHeight": 1080
            }),
        )
        .await?;
        // outerWidth/Height stay 0 in headless — a classic
        // tell. Give the window real bounds so outer size
        // is window size + chrome frame.
        if let Ok(win) = cdp
            .call(
                Some(&session),
                "Browser.getWindowForTarget",
                json!({}),
            )
            .await
        {
            if let Some(id) = win.get("windowId").and_then(Value::as_i64)
            {
                let _ = cdp
                    .call(
                        None,
                        "Browser.setWindowBounds",
                        json!({
                            "windowId": id,
                            "bounds": {
                                "left": 0, "top": 0,
                                "width": 1920, "height": 1167
                            }
                        }),
                    )
                    .await;
            }
        }

        Ok(Self {
            child,
            proc,
            cdp,
            session,
            target,
            frozen: false,
            last_used: Instant::now(),
        })
    }

    #[allow(dead_code)] // useful accessor for debugging/agent surface
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Freeze the whole process tree. CPU → 0, RAM goes
    /// cold and swappable. Resume is ~ms.
    pub fn freeze(&mut self) {
        if self.frozen {
            return;
        }
        self.proc.freeze();
        self.frozen = true;
    }

    /// Resume the process tree. False if the browser died while
    /// frozen (caller relaunches).
    pub fn thaw(&mut self) -> bool {
        if !self.frozen {
            return true;
        }
        match self.child.try_wait() {
            Ok(None) => {
                self.proc.thaw();
                self.frozen = false;
                true
            }
            // Exited (or error) → caller relaunches.
            _ => false,
        }
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    /// Reap the browser entirely — the whole process tree,
    /// plus crashpad handlers on Unix (they daemonize into
    /// their own groups and escape the group kill; on Windows
    /// the Job Object already owns them).
    pub async fn kill(&mut self) {
        self.proc.kill_group();
        sweep_crashpad();
        let _ = self.child.wait().await;
    }

    pub fn touch(&mut self) {
        self.last_used = Instant::now();
    }

    /// Navigate the attached page.
    pub async fn navigate(&self, url: &str) -> Result<(), FetchError> {
        self.cdp
            .call(
                Some(&self.session),
                "Page.navigate",
                json!({ "url": url }),
            )
            .await?;
        Ok(())
    }

    /// Current document HTML. DOM domain only — no Runtime,
    /// no script execution.
    pub async fn outer_html(&self) -> Result<String, FetchError> {
        let root = self
            .cdp
            .call(Some(&self.session), "DOM.getDocument", json!({}))
            .await?
            .get("root")
            .and_then(|r| r.get("nodeId"))
            .and_then(Value::as_i64)
            .ok_or_else(|| FetchError::ghost("no root node"))?;
        Ok(self
            .cdp
            .call(
                Some(&self.session),
                "DOM.getOuterHTML",
                json!({ "nodeId": root }),
            )
            .await?
            .get("outerHTML")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Current page URL (targetInfo — no Runtime).
    pub async fn current_url(&self) -> Result<String, FetchError> {
        Ok(self
            .cdp
            .call(
                None,
                "Target.getTargetInfo",
                json!({ "targetId": self.target }),
            )
            .await?
            .get("targetInfo")
            .and_then(|t| t.get("url"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// All browser cookies (browser-level Storage domain).
    pub async fn cookies(&self) -> Result<Vec<(String, String, String)>, FetchError> {
        let res = self
            .cdp
            .call(None, "Storage.getCookies", json!({}))
            .await?;
        let mut out = Vec::new();
        if let Some(arr) = res.get("cookies").and_then(Value::as_array) {
            for c in arr {
                let name = c.get("name").and_then(Value::as_str).unwrap_or("");
                let value = c.get("value").and_then(Value::as_str).unwrap_or("");
                let domain = c.get("domain").and_then(Value::as_str).unwrap_or("");
                if !name.is_empty() {
                    out.push((name.to_string(), value.to_string(), domain.to_string()));
                }
            }
        }
        Ok(out)
    }

    /// PNG screenshot → path (D16 byproduct).
    pub async fn screenshot(&self, path: &str) -> Result<(), FetchError> {
        let data = self
            .cdp
            .call(
                Some(&self.session),
                "Page.captureScreenshot",
                json!({ "format": "png" }),
            )
            .await?
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| FetchError::ghost("no screenshot data"))?
            .to_string();
        // base64 decode (no new dep: manual).
        let bytes = b64decode(data.as_bytes());
        std::fs::write(path, bytes)
            .map_err(|e| FetchError::ghost(format!("screenshot: {e}")))
    }

    /// One trusted click with a human-ish pre-move path.
    /// CDP input events are isTrusted=true; detection is
    /// behavioral, so the path curves and overshoots.
    pub async fn click(&self, x: f64, y: f64) -> Result<(), FetchError> {
        // Pre-movement: bezier-ish arc from a random offset.
        let mut rng = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let mut rand = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % 1000) as f64 / 1000.0
        };
        let sx = x - 200.0 - rand() * 300.0;
        let sy = y - 100.0 - rand() * 200.0;
        for i in 1..=12 {
            let t = i as f64 / 12.0;
            // Ease-out cubic + slight wobble.
            let e = 1.0 - (1.0 - t).powi(3);
            let wob = (t * 9.0).sin() * 3.0 * (1.0 - t);
            let px = sx + (x - sx) * e + wob;
            let py = sy + (y - sy) * e + wob * 0.6;
            self.cdp
                .call(
                    Some(&self.session),
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseMoved", "x": px, "y": py }),
                )
                .await?;
            tokio::time::sleep(std::time::Duration::from_millis(
                8 + (rand() * 14.0) as u64,
            ))
            .await;
        }
        for ty in ["mousePressed", "mouseReleased"] {
            self.cdp
                .call(
                    Some(&self.session),
                    "Input.dispatchMouseEvent",
                    json!({
                        "type": ty, "x": x, "y": y,
                        "button": "left", "clickCount": 1
                    }),
                )
                .await?;
            tokio::time::sleep(std::time::Duration::from_millis(
                35 + (rand() * 60.0) as u64,
            ))
            .await;
        }
        Ok(())
    }
}

/// Kill chrome_crashpad processes belonging to our
/// ghost profile (they daemonize into their own
/// process groups and escape group kills). Linux-only:
/// uses /proc; macOS has no /proc and Windows's Job
/// Object already owns the crashpad handlers.
#[cfg(target_os = "linux")]
fn sweep_crashpad() {
    let marker = profile_dir().to_string_lossy().into_owned();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) =
            std::fs::read_to_string(e.path().join("cmdline"))
        else {
            continue;
        };
        if cmdline.contains("crashpad") && cmdline.contains(&marker) {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn sweep_crashpad() {}

/// Minimal base64 decode (avoids a dep for one call).
fn b64decode(s: &[u8]) -> Vec<u8> {
    fn val(b: u8) -> u8 {
        match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => 0,
        }
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let clean: Vec<u8> =
        s.iter().copied().filter(|b| !b"=\n\r ".contains(b)).collect();
    for chunk in clean.chunks(4) {
        if chunk.len() < 4 {
            break;
        }
        let n = ((val(chunk[0]) as u32) << 18)
            | ((val(chunk[1]) as u32) << 12)
            | ((val(chunk[2]) as u32) << 6)
            | (val(chunk[3]) as u32);
        out.push((n >> 16) as u8);
        out.push((n >> 8) as u8);
        out.push(n as u8);
    }
    out
}

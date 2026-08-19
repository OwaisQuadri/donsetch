//! Browser fingerprint profiles as data.
//!
//! Captured live from Chromium 150 via tls.peet.ws/api/all (2026-07-30).
//! New browser version = new table, not new code.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Windows,
    MacOs,
}

impl Platform {
    pub fn host() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }

    /// Sec-CH-UA-Platform value.
    pub fn ch_platform(self) -> &'static str {
        match self {
            Self::Linux => "Linux",
            Self::Windows => "Windows",
            Self::MacOs => "macOS",
        }
    }

    /// UA platform token.
    fn ua_token(self) -> &'static str {
        match self {
            Self::Linux => "X11; Linux x86_64",
            Self::Windows => "Windows NT 10.0; Win64; x64",
            Self::MacOs => "Macintosh; Intel Mac OS X 10_15_7",
        }
    }
}

#[derive(Clone, Debug)]
pub struct TlsProfile {
    /// TLS <= 1.2 cipher list (SSL_CTX_set_cipher_list), Chrome order.
    /// (TLS 1.3 suites need no config: BoringSSL's default order IS
    /// Chrome's 4865-4866-4867.)
    pub ciphers_12: &'static str,
    /// Supported groups / key shares, Chrome order.
    pub groups: &'static str,
    /// Signature algorithms.
    pub sigalgs: &'static str,
    /// ALPN wire format.
    pub alpn: &'static [u8],
}

#[derive(Clone, Copy, Debug)]
pub struct H2Profile {
    pub header_table_size: u32,
    pub enable_push: u32,
    pub initial_window_size: u32,
    pub max_header_list_size: u32,
    /// Connection-level WINDOW_UPDATE increment sent after preface.
    pub conn_window_update: u32,
}

#[derive(Clone, Debug)]
pub struct BrowserProfile {
    #[allow(dead_code)]
    pub name: &'static str,
    pub tls: TlsProfile,
    pub h2: H2Profile,
    pub user_agent: String,
    pub sec_ch_ua: String,
    pub platform: Platform,
}

impl BrowserProfile {
    /// Chrome 150 on the given platform. Ground truth: Chromium 150 capture, 2026-07-30.
    pub fn chrome_150(platform: Platform) -> Self {
        Self::chrome(150, platform)
    }

    /// Chrome `major` on the given platform. The TLS/H2 tables are
    /// the Chrome 150 capture (stable across adjacent versions);
    /// the UA and client hints carry the real version so the ghost
    /// browser and tier 1 advertise the SAME identity — clearance
    /// cookies are bound to it, and a ghost solving on Chromium 151
    /// while tier 1 claims 150 gets its replays rejected.
    pub fn chrome(major: u32, platform: Platform) -> Self {
        Self {
            name: "chrome-150",
            tls: TlsProfile {
                // 4865-4866-4867 then 49195-49199-49196-49200-52393-52392-49171-49172-156-157-47-53
                ciphers_12: "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:\
                             ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:\
                             ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:\
                             ECDHE-RSA-AES128-SHA:ECDHE-RSA-AES256-SHA:\
                             AES128-GCM-SHA256:AES256-GCM-SHA384:AES128-SHA:AES256-SHA",
                groups: "X25519MLKEM768:X25519:P-256:P-384",
                sigalgs: "ecdsa_secp256r1_sha256:rsa_pss_rsae_sha256:rsa_pkcs1_sha256:\
                          ecdsa_secp384r1_sha384:rsa_pss_rsae_sha384:rsa_pkcs1_sha384:\
                          rsa_pss_rsae_sha512:rsa_pkcs1_sha512",
                alpn: b"\x02h2\x08http/1.1",
            },
            h2: H2Profile {
                header_table_size: 65536,
                enable_push: 0,
                initial_window_size: 6291456,
                max_header_list_size: 262144,
                conn_window_update: 15663105,
            },
            user_agent: format!(
                "Mozilla/5. ({}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36",
                platform.ua_token()
            ),
            sec_ch_ua: format!(
                "\"Not;A=Brand\";v=\"8\", \"Chromium\";v=\"{major}\", \"Google Chrome\";v=\"{major}\""
            ),
            platform,
        }
    }

    /// Default: host-coherent identity (a Windows agent looks like Windows Chrome).
    /// The major version is probed from the INSTALLED browser when
    /// one exists (ghost + tier 1 must claim the same version).
    pub fn host_default() -> Self {
        match probe_installed_major() {
            Some(major) => Self::chrome(major, Platform::host()),
            None => Self::chrome_150(Platform::host()),
        }
    }

    /// Ordered header template for a document GET, Chrome order.
    /// (name, value-or-placeholder). Placeholders filled by caller.
    pub fn h1_headers(&self, host: &str) -> Vec<(String, String)> {
        vec![
            ("host".into(), host.into()),
            ("connection".into(), "keep-alive".into()),
            ("sec-ch-ua".into(), self.sec_ch_ua.clone()),
            ("sec-ch-ua-mobile".into(), "?0".into()),
            ("sec-ch-ua-platform".into(), format!("\"{}\"", self.platform.ch_platform())),
            ("upgrade-insecure-requests".into(), "1".into()),
            ("user-agent".into(), self.user_agent.clone()),
            ("accept".into(), "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7".into()),
            ("sec-fetch-site".into(), "none".into()),
            ("sec-fetch-mode".into(), "navigate".into()),
            ("sec-fetch-user".into(), "?1".into()),
            ("sec-fetch-dest".into(), "document".into()),
            ("accept-encoding".into(), "gzip, deflate, br, zstd".into()),
            ("accept-language".into(), "en-US,en;q=0.9".into()),
            ("priority".into(), "u=0, i".into()),
        ]
    }
}

/// Probe the installed browser's major version. Cached after first call.
///
/// On Windows, `chrome.exe --version` without `--headless` opens a
/// visible GUI window (and may pop the profile picker since no
/// `--user-data-dir` is passed). We pass `--headless=new` plus a temp
/// `--user-data-dir` so Chrome prints the version and exits without
/// any visible window. The result is cached in a `OnceLock` so the
/// probe runs at most once per process.
static PROBED_MAJOR: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();

pub fn probe_installed_major() -> Option<u32> {
    *PROBED_MAJOR.get_or_init(|| {
        let bin = crate::ghost::chrome_binary().ok()?;
        let mut cmd = std::process::Command::new(&bin);
        cmd.arg("--version");
        // On Windows and macOS, --version without --headless may open a
        // GUI window. Pass --headless=new + a temp --user-data-dir so Chrome
        // exits silently. Harmless on Linux (ignores --headless with --version).
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            let tmp = std::env::temp_dir().join("donsetch-chrome-probe");
            let _ = std::fs::create_dir_all(&tmp);
            cmd.arg("--headless=new");
            cmd.arg(format!("--user-data-dir={}", tmp.display()));
            cmd.arg("--no-first-run");
            cmd.arg("--no-default-browser-check");
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::null());
        let out = cmd.output().ok()?;
        let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // "Chromium 151.0.7922.108 Arch Linux" / "Google Chrome 150..."
        // / "Microsoft Edge 151..."
        let tokens: Vec<&str> = line.split_whitespace().collect();
        for t in tokens {
            if let Some(first) = t.split('.').next()
                && let Ok(major) = first.parse::<u32>()
                && (20..=400).contains(&major)
            {
                return Some(major);
            }
        }
        None
    })
}

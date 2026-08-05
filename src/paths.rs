//! Cross-platform path helpers for DonSeTch persistent state.
//!
//! All persistent state lives under one per-platform cache dir:
//!   Linux:   $XDG_CACHE_HOME/donsetch  or  ~/.cache/donsetch
//!   macOS:   ~/Library/Caches/donsetch
//!   Windows: %LOCALAPPDATA%\donsetch
//!
//! Backed by the `dirs` crate (zero transitive deps, the de
//! facto standard for this exact problem).

use std::path::PathBuf;

/// The DonSeTch cache root. Falls back to the system temp dir
/// if the platform reports no cache directory (shouldn't happen
/// on any real user account).
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("donsetch")
}

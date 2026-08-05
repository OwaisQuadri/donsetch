// Build script: vendored PDFium (static) acquisition + linking.
//
// PDFium is the one heavy vendored primitive (Chrome's own PDF engine,
// BSD-licensed). We statically link prebuilt static archives from
// kognitos/pdfium-static (a fork of bblanchon/pdfium-binaries producing
// .a/.lib instead of shared libs). The archives bundle Chromium's
// namespace-mangled libc++, so there is no host C++ runtime conflict.
//
// If vendor/pdfium/lib does not contain the static archive for the target,
// we download and unpack the pinned release with curl/tar. The SHA-256 of
// the downloaded tarball is verified against a pinned map when present.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Pinned pdfium-static release (PDFium 149.x, Chromium milestone 7809).
const PDFIUM_TAG: &str = "chromium/7809";

/// sha256 of the pinned tarball per platform "os-arch". Verified on download;
/// entries are filled as each platform's artifact is prepped for CI.
const KNOWN_HASHES: &[(&str, &str)] = &[(
    "linux-x64",
    "13908bb2d40a6e017c4c5a6a7baecc6efd7b1c30392c8a79e80072d2b48b18eb",
)];

fn main() {
    let os = env::var("CARGO_CFG_TARGET_OS").expect("no target os");
    let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("no target arch");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("no manifest dir"));
    let vendored = manifest.join("vendor").join("pdfium");
    let libdir = vendored.join("lib");

    let pdfium_name = if os == "windows" { "pdfium.lib" } else { "libpdfium.a" };
    if !libdir.join(pdfium_name).exists() {
        fetch_pdfium(&os, &arch, &vendored);
    }

    println!("cargo:rustc-link-search=native={}", libdir.display());
    println!("cargo:rustc-link-lib=static=pdfium");
    match os.as_str() {
        "linux" => {
            // Bundled namespace-mangled libc++ satisfies pdfium's internal
            // std::__Cr::* references without touching the host runtime.
            println!("cargo:rustc-link-lib=static=c++");
            println!("cargo:rustc-link-lib=static=c++abi");
            println!("cargo:rustc-link-lib=dylib=pthread");
            println!("cargo:rustc-link-lib=dylib=dl");
            println!("cargo:rustc-link-lib=dylib=m");
        }
        "macos" => {
            println!("cargo:rustc-link-lib=dylib=c++");
            for f in ["CoreGraphics", "CoreFoundation", "CoreText", "AppKit"] {
                println!("cargo:rustc-link-lib=framework={}", f);
            }
        }
        "windows" => {
            for l in ["gdi32", "user32", "advapi32", "comdlg32", "shell32"] {
                println!("cargo:rustc-link-lib=dylib={}", l);
            }
        }
        other => panic!("pdfium: unsupported target os {other}"),
    }

    println!("cargo:rerun-if-changed=build.rs");
}

fn target_pair(os: &str, arch: &str) -> &'static str {
    match (os, arch) {
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "x86_64") => "mac-x64",
        ("macos", "aarch64") => "mac-arm64",
        ("windows", "x86_64") => "win-x64",
        ("windows", "aarch64") => "win-arm64",
        (o, a) => panic!("pdfium: unsupported target pair {o}-{a}"),
    }
}

/// Download the pinned static-archive release into `vendored` when missing.
/// Fails the build loudly rather than silently proceeding.
fn fetch_pdfium(os: &str, arch: &str, vendored: &Path) {
    let pair = target_pair(os, arch);
    let url = format!(
        "https://github.com/kognitos/pdfium-static/releases/download/{PDFIUM_TAG}/pdfium-{pair}-static.tgz"
    );
    let tgz = vendored.join(format!("pdfium-{pair}-static.tgz"));
    let _ = fs::create_dir_all(vendored);

    eprintln!("donsetch build: fetching pdfium static library {pair} from {url}");
    let status = Command::new("curl")
        .args(["-fSL", "--retry", "3", "-o"])
        .arg(&tgz)
        .arg(&url)
        .status()
        .expect("pdfium: failed to spawn curl");
    if !status.success() {
        panic!("pdfium: curl download failed for {url}");
    }

    if let Some(hash) = KNOWN_HASHES
        .iter()
        .find(|(p, _)| *p == pair)
        .map(|(_, h)| *h)
    {
        let out = Command::new("sha256sum")
            .arg(&tgz)
            .output()
            .expect("pdfium: failed to spawn sha256sum");
        let got = String::from_utf8_lossy(&out.stdout);
        let got = got.split_whitespace().next().unwrap_or("");
        assert_eq!(
            got, hash,
            "pdfium: sha256 mismatch for {pair} (expected {hash}, got {got})"
        );
    } else {
        eprintln!(
            "donsetch build: no pinned sha256 for pdfium {pair} yet (unverified download)"
        );
    }

    let status = Command::new("tar")
        .arg("xzf")
        .arg(&tgz)
        .arg("-C")
        .arg(vendored)
        .status()
        .expect("pdfium: failed to spawn tar");
    if !status.success() {
        panic!("pdfium: tar extraction failed for {tgz:?}");
    }
    let _ = fs::remove_file(&tgz);
}

// Build script: PDFium acquisition + linking.
//
// PDFium is the one heavy vendored primitive (Chrome's own PDF engine,
// BSD-licensed).
//
// **Linux/macOS**: statically link prebuilt static archives from
// kognitos/pdfium-static (a fork of bblanchon/pdfium-binaries producing
// .a instead of shared libs). The archives bundle Chromium's
// namespace-mangled libc++, so there is no host C++ runtime conflict.
//
// **Windows**: use the shared library (pdfium.dll) from
// bblanchon/pdfium-binaries. PDFium's static .lib is built with /MT
// (static CRT), but Rust's MSVC target uses /MD (dynamic CRT). Mixing
// /MT and /MD in one binary is undefined behavior on MSVC — the linker
// emits LNK2005 multiply-defined symbols for C++ CRT internals
// (std::_Raise_handler, std::ctype<char>::id, etc.) that exist in
// libcpmt.lib but not msvcprt.lib. The DLL sidesteps this entirely:
// pdfium.dll has its own CRT baked in, no conflict with the host.
//
// If vendor/pdfium/lib does not contain the library for the target,
// we download and unpack the pinned release with curl/tar. The SHA-256
// of the downloaded tarball is verified against a pinned map when present.

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Pinned PDFium release for static archives (Linux/macOS).
/// Source: kognitos/pdfium-static (fork of bblanchon/pdfium-binaries).
const PDFIUM_STATIC_TAG: &str = "chromium/7809";

/// Pinned PDFium release for the Windows shared library (DLL).
/// Source: bblanchon/pdfium-binaries. This is the closest release to
/// 7809 that bblanchon provides. The FFI surface is identical — both
/// are Chromium 149-era PDFium builds.
const PDFIUM_SHARED_TAG: &str = "chromium/7802";

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

    let is_windows = os == "windows";

    // Windows: pdfium.lib is an import library for pdfium.dll.
    // Linux/macOS: libpdfium.a is a full static archive.
    let pdfium_name = if is_windows {
        "pdfium.lib"
    } else {
        "libpdfium.a"
    };

    if !libdir.join(pdfium_name).exists() {
        fetch_pdfium(&os, &arch, &vendored);
    }

    println!("cargo:rustc-link-search=native={}", libdir.display());

    if is_windows {
        // Link against the import library; pdfium.dll is resolved at runtime.
        println!("cargo:rustc-link-lib=dylib=pdfium");
        for l in ["gdi32", "user32", "advapi32", "comdlg32", "shell32"] {
            println!("cargo:rustc-link-lib=dylib={}", l);
        }

        // Copy pdfium.dll to the output directory so tests and the binary
        // can find it at runtime without requiring it in PATH.
        let bindir = vendored.join("bin");
        let dll = bindir.join("pdfium.dll");
        if dll.exists() {
            let out_dir = env::var("OUT_DIR").expect("no OUT_DIR");
            // OUT_DIR is something like target/release/build/donsetch-xxx/out
            // or target/<target>/release/build/donsetch-xxx/out (with --target).
            // Walk up to find the profile dir (release or debug).
            let out_path = PathBuf::from(&out_dir);
            let profile_dir = out_path
                .ancestors()
                .nth(3)
                .expect("cannot find profile dir from OUT_DIR");
            // Copy to the profile dir (next to donsetch.exe) and to deps/
            // (next to test executables) so both find it at runtime.
            for dest in [
                profile_dir.join("pdfium.dll"),
                profile_dir.join("deps").join("pdfium.dll"),
            ] {
                if !dest.exists() {
                    let _ = fs::copy(&dll, &dest);
                }
            }
        }
    } else {
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
            other => panic!("pdfium: unsupported target os {other}"),
        }
    }

    // Force re-run when the vendor lib dir is missing or changes.
    // On a fresh CI checkout, vendor/pdfium/lib doesn't exist, so cargo
    // always re-runs this build script and triggers the download.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor/pdfium/lib");

    // ── Compile-time metadata for `donsetch -v` ────────────────
    // Captured here so the binary self-reports its build identity.

    // Git short hash (best-effort — may not be a git repo).
    if let Ok(out) = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
    {
        if let Ok(s) = String::from_utf8(out.stdout) {
            println!("cargo:rustc-env=DONSHEET_GIT_HASH={}", s.trim());
        } else {
            println!("cargo:rustc-env=DONSHEET_GIT_HASH=unknown");
        }
    } else {
        println!("cargo:rustc-env=DONSHEET_GIT_HASH=unknown");
    }

    // PDFium variant string.
    let pdfium_tag = if is_windows {
        PDFIUM_SHARED_TAG
    } else {
        PDFIUM_STATIC_TAG
    };
    let pdfium_kind = if is_windows { "dll" } else { "static" };
    println!("cargo:rustc-env=DONSHEET_PDFIUM={pdfium_kind}, {pdfium_tag}");

    // Target triple.
    let triple = match (os.as_str(), arch.as_str()) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => "unknown",
    };
    println!("cargo:rustc-env=DONSHEET_TARGET={triple}");

    // Enabled feature flags.
    let mut feats = Vec::new();
    if env::var_os("CARGO_FEATURE_OCR").is_some() {
        feats.push("ocr");
    }
    if env::var_os("CARGO_FEATURE_RERANK").is_some() {
        feats.push("rerank");
    }
    println!("cargo:rustc-env=DONSHEET_FEATURES={}", feats.join(", "));
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

/// Download the pinned PDFium release into `vendored` when missing.
///
/// Linux/macOS: static archive from kognitos/pdfium-static.
/// Windows: shared library (DLL + import lib) from bblanchon/pdfium-binaries.
///
/// Fails the build loudly rather than silently proceeding.
fn fetch_pdfium(os: &str, arch: &str, vendored: &Path) {
    let pair = target_pair(os, arch);
    let is_windows = os == "windows";

    let (url, tgz_name) = if is_windows {
        (
            format!(
                "https://github.com/bblanchon/pdfium-binaries/releases/download/{PDFIUM_SHARED_TAG}/pdfium-{pair}.tgz"
            ),
            format!("pdfium-{pair}.tgz"),
        )
    } else {
        (
            format!(
                "https://github.com/kognitos/pdfium-static/releases/download/{PDFIUM_STATIC_TAG}/pdfium-{pair}-static.tgz"
            ),
            format!("pdfium-{pair}-static.tgz"),
        )
    };

    let tgz = vendored.join(&tgz_name);
    let _ = fs::create_dir_all(vendored);

    eprintln!("donsetch build: fetching pdfium {pair} from {url}");
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
        let mut f = fs::File::open(&tgz).expect("pdfium: cannot open downloaded tarball");
        let mut buf = Vec::with_capacity(8 * 1024 * 1024);
        f.read_to_end(&mut buf)
            .expect("pdfium: cannot read tarball");
        let got = sha256_hex(&buf);
        assert_eq!(
            got, hash,
            "pdfium: sha256 mismatch for {pair} (expected {hash}, got {got})"
        );
    } else {
        eprintln!("donsetch build: no pinned sha256 for pdfium {pair} yet (unverified download)");
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

    // bblanchon names the import library pdfium.dll.lib, but the MSVC
    // linker (via cargo:rustc-link-lib=dylib=pdfium) looks for pdfium.lib.
    // Rename so the linker finds it.
    if is_windows {
        let old = vendored.join("lib").join("pdfium.dll.lib");
        let new = vendored.join("lib").join("pdfium.lib");
        if old.exists() && !new.exists() {
            let _ = fs::rename(&old, &new);
        }
    }
}

/// SHA-256 hex digest of a byte slice (build-time, no external tool).
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(data);
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

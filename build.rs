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
// of the downloaded tarball is verified against a pinned map and is
// required — builds refuse to download when no pinned hash exists.

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

/// sha256 of the pinned tarball per platform "os-arch".
/// Verified on download and required; builds fail before any network download if no entry exists.
/// Entries are release-asset digests for the exact pinned artifacts: static
/// Linux/macOS assets from kognitos/pdfium-static chromium/7809, and shared
/// Android/Windows assets from bblanchon/pdfium-binaries chromium/7802.
/// They were cross-checked against each release's attestation metadata; a
/// sidecar checksum file from the same release is not authorization.
const KNOWN_HASHES: &[(&str, &str)] = &[
    (
        "linux-x64",
        "13908bb2d40a6e017c4c5a6a7baecc6efd7b1c30392c8a79e80072d2b48b18eb",
    ),
    (
        "linux-arm64",
        "abe1c3d5b168ec2baaafc7a8fcddfda1a09417f39199c7993fd28d34d3a7f70e",
    ),
    (
        "mac-x64",
        "c097fd17a07826bb36617dda0cd02bd7829c0f2087f33e927124df21dc5cef06",
    ),
    (
        "mac-arm64",
        "08556377b5d33b2fef7c6bfec66e01b9b23007c10533ab0404fe54538cbb2837",
    ),
    (
        "win-x64",
        "487156c28d81bd162107ca0ba85849cbfbb0127be4210a7cfec6def66802082d",
    ),
    (
        "win-arm64",
        "15d679b0baf8bb470c9fae155c0abc4ab752017b38f4cc71940314af565c53e2",
    ),
    (
        "android-x64",
        "596cbe4fd6cbb118a9f0576fa96f2c0f4476f7a85779e7f32d64888a6e4f1ddd",
    ),
    (
        "android-arm64",
        "4e510dd0757af1439107c23577fafdc854fac9c403a5a5b20f78ebf87097672c",
    ),
];

fn main() {
    // Declare the custom cfg so rustc doesn't warn about it.
    println!("cargo::rustc-check-cfg=cfg(linux_like)");

    let os = env::var("CARGO_CFG_TARGET_OS").expect("no target os");
    let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("no target arch");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("no manifest dir"));
    let vendored = manifest.join("vendor").join("pdfium");
    let libdir = vendored.join("lib");

    let is_windows = os == "windows";
    let is_android = os == "android";
    // Windows and Android use shared libraries (DLL / .so) from
    // bblanchon/pdfium-binaries. Linux/macOS use static archives from
    // kognitos/pdfium-static. Android's bionic libc cannot link the
    // glibc-targeted static archives (issue #16).
    let is_shared = is_windows || is_android;

    // Windows: pdfium.lib is an import library for pdfium.dll.
    // Android: libpdfium.so is a shared library.
    // Linux/macOS: libpdfium.a is a full static archive.
    let pdfium_name = if is_shared {
        if is_windows {
            "pdfium.lib"
        } else {
            "libpdfium.so"
        }
    } else {
        "libpdfium.a"
    };

    if !libdir.join(pdfium_name).exists() {
        fetch_pdfium(&os, &arch, &vendored);
    }

    println!("cargo:rustc-link-search=native={}", libdir.display());

    if is_shared {
        // Link against the shared library. On Windows this links
        // against the import library; pdfium.dll is resolved at
        // runtime. On Android, libpdfium.so is linked directly.
        println!("cargo:rustc-link-lib=dylib=pdfium");
        if is_windows {
            for l in ["gdi32", "user32", "advapi32", "comdlg32", "shell32"] {
                println!("cargo:rustc-link-lib=dylib={}", l);
            }
        } else if is_android {
            // PDFium's Android .so links against libc++_shared.so
            // (NDK C++ runtime). Termux ships libc++_shared.so in
            // $PREFIX/lib; the linker finds it via the search path.
            println!("cargo:rustc-link-lib=dylib=c++_shared");
            println!("cargo:rustc-link-lib=dylib=log");
        }

        // Copy the shared library to the output directory so tests
        // and the binary can find it at runtime without requiring it
        // in PATH (Windows) or LD_LIBRARY_PATH (Android).
        let bindir = vendored.join("bin");
        let shared_lib = if is_windows {
            bindir.join("pdfium.dll")
        } else {
            bindir.join("libpdfium.so")
        };
        if shared_lib.exists() {
            let out_dir = env::var("OUT_DIR").expect("no OUT_DIR");
            let out_path = PathBuf::from(&out_dir);
            let profile_dir = out_path
                .ancestors()
                .nth(3)
                .expect("cannot find profile dir from OUT_DIR");
            let dest_name = if is_windows {
                "pdfium.dll"
            } else {
                "libpdfium.so"
            };
            for dest in [
                profile_dir.join(dest_name),
                profile_dir.join("deps").join(dest_name),
            ] {
                if !dest.exists() {
                    let _ = fs::copy(&shared_lib, &dest);
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

    // Android shares the Linux code paths (process groups, /proc,
    // prctl, known Chrome paths, headless fallback). Emit a cfg so
    // the source uses #[cfg(linux_like)] instead of repeating
    // #[cfg(any(target_os = "linux", target_os = "android"))] everywhere.
    if os == "linux" || os == "android" {
        println!("cargo:rustc-cfg=linux_like");
    }

    // Force re-run when the vendor lib dir is missing or changes.
    // On a fresh CI checkout, vendor/pdfium/lib doesn't exist, so cargo
    // always re-runs this build script and triggers the download.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor/pdfium/lib");

    // ── LLD auto-detection (Linux) ────────────────────────────
    //
    // The PDFium static archives are produced by LLVM/Clang. GNU ld
    // on some versions/architectures can't parse the ELF machine type
    // of LLVM-generated archive members — notably aarch64 with GNU
    // ld 2.42+ reports "architecture: UNKNOWN!" and skips them as
    // incompatible. Newer GNU binutils (2.44+) on x86_64 also reject
    // LLVM CREL relocations in the same archives.
    //
    // LLD handles both correctly. If ld.lld is available, we inject
    // -fuse-ld=lld into the link step so the compiler driver (cc/gcc)
    // uses LLD instead of GNU ld. This is transparent to the user —
    // no RUSTFLAGS or .cargo/config.toml needed.
    if os == "linux" || os == "android" {
        let lld_available = Command::new("ld.lld")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if lld_available {
            println!("cargo:rustc-link-arg=-fuse-ld=lld");
        } else if arch == "aarch64" {
            eprintln!(
                "warning: donsetch: LLD not found. GNU ld on aarch64 may fail \
                 to link LLVM-produced PDFium archives. \
                 Install lld (e.g., apt install lld / pkg install lld) and rebuild."
            );
        }
    }

    // ── Windows main-thread stack size ────────────────────────
    //
    // On Windows the main thread's stack size comes from the PE header,
    // which defaults to 1MB. Linux gives 8MB. donsetch runs its whole
    // future tree on the main thread via tokio's block_on, and the
    // fetch_tool state machine does not fit in 1MB unoptimized — a debug
    // build dies with 0xc00000fd (STATUS_STACK_OVERFLOW) inside __chkstk
    // before entering the function body, with no usable backtrace since
    // a stack overflow is not a panic. Release only fits because
    // optimization shrinks the frame, which is luck, not headroom.
    //
    // Ask the linker for Linux's 8MB so the ceiling stops being
    // profile-dependent. Transparent to the user, like the LLD flag above.
    if is_windows {
        let abi = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
        if abi == "gnu" {
            // MinGW: GNU ld spelling.
            println!("cargo:rustc-link-arg=-Wl,--stack,8388608");
        } else {
            // MSVC link.exe.
            println!("cargo:rustc-link-arg=/STACK:8388608");
        }
    }

    // ── aarch64 + ONNX note ──────────────────────────────────
    //
    // ONNX Runtime is now dynamically loaded (ort load-dynamic).
    // The C++ global constructor deadlock that affected aarch64
    // with static linking no longer occurs at process start.
    // ONNX is dlopen'd at runtime only when OCR/rerank is used.
    // The aarch64 ONNX prebuilt is not available from pyke, so
    // aarch64 builds simply lack OCR/rerank (reported at runtime).

    // ── ONNX Runtime shared library (dynamic loading) ────────
    //
    // ONNX Runtime's prebuilt static archive contains unguarded
    // AVX instructions in C++ global constructors. Statically linking
    // it causes SIGILL at process start on non-AVX CPUs (pre-2011
    // Intel, QEMU default, Docker VMs without AVX passthrough).
    //
    // With ort's `load-dynamic` feature, ONNX is NOT statically
    // linked. Instead, we build a shared library (.so/.dylib/.dll)
    // from the prebuilt static archive at build time, and dlopen it
    // at runtime after an AVX check. Non-AVX CPUs get a working binary
    // (minus OCR/rerank) instead of SIGILL.
    let has_onnx =
        env::var_os("CARGO_FEATURE_OCR").is_some() || env::var_os("CARGO_FEATURE_RERANK").is_some();
    if has_onnx {
        if let Some(info) = onnx_target_info(&os, &arch) {
            // Linux x86_64: build shared lib for dynamic loading.
            build_onnx_shared_lib(info, &manifest);
        } else {
            // macOS/Windows: ort crate uses download-binaries (static
            // linking). No shared library build needed.
            eprintln!("donsetch build: OCR/rerank enabled, ort static link for {os}-{arch}");
        }
    }

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
    let pdfium_tag = if is_shared {
        PDFIUM_SHARED_TAG
    } else {
        PDFIUM_STATIC_TAG
    };
    let pdfium_kind = if is_shared { "shared" } else { "static" };
    println!("cargo:rustc-env=DONSHEET_PDFIUM={pdfium_kind}, {pdfium_tag}");

    // Target triple.
    let triple = match (os.as_str(), arch.as_str()) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("android", "aarch64") => "aarch64-linux-android",
        ("android", "x86_64") => "x86_64-linux-android",
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
    let feats_display = if feats.is_empty() {
        "(none)".to_string()
    } else {
        feats.join(", ")
    };
    println!("cargo:rustc-env=DONSHEET_FEATURES={feats_display}");
}

fn target_pair(os: &str, arch: &str) -> &'static str {
    match (os, arch) {
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        // Android uses bblanchon's shared library (.so), NOT the
        // kognitos static archive (.a). The static archive is
        // glibc-targeted and cannot link on bionic (issue #16).
        ("android", "x86_64") => "android-x64",
        ("android", "aarch64") => "android-arm64",
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
    let is_android = os == "android";
    let is_shared = is_windows || is_android;

    // Shared library (Windows DLL, Android .so): bblanchon/pdfium-binaries.
    // Static archive (Linux/macOS): kognitos/pdfium-static.
    let (url, tgz_name) = if is_shared {
        let tag = PDFIUM_SHARED_TAG;
        (
            format!(
                "https://github.com/bblanchon/pdfium-binaries/releases/download/{tag}/pdfium-{pair}.tgz"
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

    let pinned_hash = KNOWN_HASHES
        .iter()
        .find(|(p, _)| *p == pair)
        .map(|(_, h)| *h)
        .unwrap_or_else(|| {
            panic!(
                "pdfium: no pinned sha256 for {pair} — refusing unverified download. \
                Build from source with a vendored pdfium, or add an audited hash to KNOWN_HASHES in build.rs"
            )
        });

    let tgz = vendored.join(&tgz_name);
    let _ = fs::create_dir_all(vendored);

    eprintln!("donsetch build: fetching pdfium {pair} from {url}");
    let status = Command::new("curl")
        .args([
            "-fSL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--retry",
            "3",
            "-o",
        ])
        .arg(&tgz)
        .arg(&url)
        .status()
        .unwrap_or_else(|e| {
            panic!("pdfium: failed to spawn curl ({e}) — install curl: apt install curl")
        });
    if !status.success() {
        panic!("pdfium: curl download failed for {url}");
    }

    let mut f = fs::File::open(&tgz).expect("pdfium: cannot open downloaded tarball");
    let mut buf = Vec::with_capacity(8 * 1024 * 1024);
    f.read_to_end(&mut buf)
        .expect("pdfium: cannot read tarball");
    let got = sha256_hex(&buf);
    assert_eq!(
        got, pinned_hash,
        "pdfium: sha256 mismatch for {pair} (expected {pinned_hash}, got {got})"
    );

    let status = Command::new("tar")
        .arg("xzf")
        .arg(&tgz)
        .arg("-C")
        .arg(vendored)
        .status()
        .unwrap_or_else(|e| {
            panic!("pdfium: failed to spawn tar ({e}) — install tar: apt install tar")
        });
    if !status.success() {
        panic!("pdfium: tar extraction failed for {tgz:?}");
    }
    let _ = fs::remove_file(&tgz);

    // bblanchon names the Windows import library pdfium.dll.lib, but
    // the MSVC linker (via cargo:rustc-link-lib=dylib=pdfium) looks for
    // pdfium.lib. Rename so the linker finds it.
    if is_windows {
        let old = vendored.join("lib").join("pdfium.dll.lib");
        let new = vendored.join("lib").join("pdfium.lib");
        if old.exists() && !new.exists() {
            let _ = fs::rename(&old, &new);
        }
    }
}

/// Pinned ONNX Runtime tarball info per platform.
/// Source: pyke CDN (cdn.pyke.io), ort-sys 2.0.0-rc.12.
/// Hashes verified from the ort-sys dist.txt.
struct OnnxTarget {
    url: &'static str,
    sha256: &'static str,
    /// Static archive filename inside the tarball.
    archive_name: &'static str,
    /// Shared library filename to produce.
    shared_name: &'static str,
}

/// Return ONNX target info if a prebuilt is available for this platform.
/// Only Linux x86_64 uses dynamic loading. macOS/Windows use static
/// linking (no shared library build needed).
fn onnx_target_info(os: &str, arch: &str) -> Option<OnnxTarget> {
    let (url, sha256, archive_name, shared_name) = match (os, arch) {
        ("linux", "x86_64") => (
            "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.24.2/x86_64-unknown-linux-gnu.tar.lzma2",
            "acc1cba79c337594ead1d88ca72516147aa60054c84217b53399a31caa5ba671",
            "libonnxruntime.a",
            "libonnxruntime.so",
        ),
        _ => return None,
    };
    Some(OnnxTarget {
        url,
        sha256,
        archive_name,
        shared_name,
    })
}

/// Download the ONNX tarball, decompress (custom LZMA2), extract the
/// static archive, build a shared library from it, and copy to the
/// output directory. This replaces ort-sys's static linking with a
/// runtime dlopen approach to avoid SIGILL on non-AVX CPUs.
fn build_onnx_shared_lib(info: OnnxTarget, manifest: &Path) {
    let vendored = manifest.join("vendor").join("onnx");
    let _ = fs::create_dir_all(&vendored);
    let archive_path = vendored.join(info.archive_name);
    let shared_path = vendored.join(info.shared_name);

    // If the shared library already exists, skip the download+build.
    // The vendored dir is cleaned on `cargo clean`.
    if shared_path.exists() {
        eprintln!(
            "donsetch build: ONNX shared lib already present at {}",
            shared_path.display()
        );
        copy_onnx_shared_lib(&shared_path);
        return;
    }

    // 1. Download the tarball.
    let tarball = vendored.join("onnx.tar.lzma2");
    if !archive_path.exists() {
        eprintln!("donsetch build: fetching ONNX Runtime from {}", info.url);
        let status = Command::new("curl")
            .args([
                "-fSL",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--retry",
                "3",
                "-o",
            ])
            .arg(&tarball)
            .arg(info.url)
            .status()
            .unwrap_or_else(|e| panic!("ONNX: failed to spawn curl ({e})"));
        if !status.success() {
            panic!("ONNX: curl download failed for {}", info.url);
        }

        // 2. Verify SHA-256.
        let mut f = fs::File::open(&tarball).expect("ONNX: cannot open tarball");
        let mut buf = Vec::with_capacity(8 * 1024 * 1024);
        f.read_to_end(&mut buf).expect("ONNX: cannot read tarball");
        let got = sha256_hex(&buf);
        assert_eq!(
            got, info.sha256,
            "ONNX: sha256 mismatch (expected {}, got {})",
            info.sha256, got
        );

        // 3. Decompress custom LZMA2 format (pyke uses lzma-rust2, not standard xz).
        let decompressed = lzma2_decompress(&buf);

        // 4. Parse tar to extract the static archive.
        let entry = extract_tar_entry(&decompressed, info.archive_name)
            .unwrap_or_else(|| panic!("ONNX: {} not found in tarball", info.archive_name));
        fs::write(&archive_path, &entry).expect("ONNX: cannot write archive");
        let _ = fs::remove_file(&tarball);
    }

    // 5. Build the shared library from the static archive.
    build_shared_from_archive(&archive_path, &shared_path, info.shared_name);

    // 6. Copy to output directories.
    copy_onnx_shared_lib(&shared_path);
}

/// Copy the ONNX shared library to all locations where the binary
/// and tests can find it at runtime.
fn copy_onnx_shared_lib(shared_path: &Path) {
    let out_dir = env::var("OUT_DIR").expect("no OUT_DIR");
    let out_path = PathBuf::from(&out_dir);
    let profile_dir = out_path
        .ancestors()
        .nth(3)
        .expect("cannot find profile dir from OUT_DIR");
    let dest_name = shared_path.file_name().unwrap();
    for dest in [
        profile_dir.join(dest_name),
        profile_dir.join("deps").join(dest_name),
        profile_dir.join("examples").join(dest_name),
        out_path.join(dest_name),
    ] {
        if !dest.exists() {
            let _ = fs::copy(shared_path, &dest);
        }
    }
}

/// Decompress pyke's custom LZMA2 format using lzma-rust2.
fn lzma2_decompress(data: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut reader = lzma_rust2::Lzma2Reader::new(data, 1 << 26, None);
    let mut out = Vec::with_capacity(90 * 1024 * 1024);
    reader
        .read_to_end(&mut out)
        .expect("ONNX: LZMA2 decompression failed");
    out
}

/// Simple tar parser: find and extract a single file by name.
/// Tar format: 512-byte header blocks, file data padded to 512-byte boundaries.
fn extract_tar_entry(data: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut pos = 0;
    while pos + 512 <= data.len() {
        let header = &data[pos..pos + 512];
        // End-of-archive: two zero blocks.
        if header.iter().all(|&b| b == 0) {
            break;
        }
        // File name: bytes 0-99, null-terminated.
        let entry_name = header.split(|&b| b == 0).next().unwrap_or(&[]);
        let entry_name = String::from_utf8_lossy(entry_name);
        // File size: bytes 124-135, octal string.
        let size_str = header[124..136]
            .split(|&b| b == 0 || b == b' ')
            .next()
            .unwrap_or(&[]);
        let size_str = std::str::from_utf8(size_str).unwrap_or("");
        let size = usize::from_str_radix(size_str.trim(), 8).unwrap_or(0);
        // Data starts at pos + 512.
        let data_start = pos + 512;
        let data_end = data_start + size;
        if data_end > data.len() {
            break;
        }
        if entry_name == name {
            return Some(data[data_start..data_end].to_vec());
        }
        // Next entry: data padded to 512-byte boundary.
        let padded = (size + 511) & !511;
        pos = data_start + padded;
    }
    None
}

/// Build a shared library from a static archive using the system linker.
/// Only called on Linux x86_64 (see onnx_target_info).
/// Uses --whole-archive + --allow-multiple-definition to handle
/// duplicate protobuf symbols in the ONNX archive, and -z noexecstack
/// to clear the executable stack flag (ONNX assembly objects lack
/// .note.GNU-stack, which defaults to execstack on Linux).
fn build_shared_from_archive(archive: &Path, output: &Path, _shared_name: &str) {
    eprintln!(
        "donsetch build: building ONNX shared lib from {}",
        archive.display()
    );
    let status = Command::new("cc")
        .args(["-shared", "-z", "noexecstack", "-o"])
        .arg(output)
        .arg("-Wl,--whole-archive,--allow-multiple-definition")
        .arg(archive)
        .arg("-Wl,--no-whole-archive")
        .args(["-lstdc++", "-lpthread", "-ldl", "-lm"])
        .status()
        .unwrap_or_else(|e| panic!("ONNX: failed to spawn linker ({e})"));
    if !status.success() {
        panic!("ONNX: shared library build failed");
    }
}

/// SHA-256 hex digest of a byte slice (build-time, no external tool).
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(data);
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

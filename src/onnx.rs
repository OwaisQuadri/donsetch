//! Runtime ONNX Runtime initialization.
//!
//! ONNX Runtime is loaded differently depending on platform:
//!
//! **Linux x86_64**: dynamically loaded via dlopen to avoid SIGILL
//! on non-AVX CPUs. The prebuilt ONNX static archive contains
//! unguarded AVX instructions in C++ global constructors that run
//! before `main()`. Statically linking it causes SIGILL at process
//! start on any CPU without AVX. With `ort`'s `load-dynamic` feature,
//! ONNX is NOT statically linked. A shared library (.so) is built
//! from the prebuilt archive at compile time and dlopen'd at runtime
//! after an AVX check. Non-AVX CPUs get a working binary (minus
//! OCR/rerank) instead of SIGILL.
//!
//! **macOS / Windows**: statically linked (ort `download-binaries`).
//! macOS ARM64 has no AVX concept (ARM NEON). Windows x64 AVX issues
//! are rare (most x64 CPUs since 2011 have AVX) and can't be fixed
//! with dynamic loading because the MSVC linker can't build a DLL
//! from the static archive with duplicate protobuf symbols.

#[cfg(all(target_os = "linux", any(feature = "ocr", feature = "rerank")))]
use std::path::PathBuf;
#[cfg(any(feature = "ocr", feature = "rerank"))]
use std::sync::OnceLock;

/// Error returned when the CPU lacks AVX support (Linux only).
pub const NO_AVX_MSG: &str = "ONNX Runtime requires AVX CPU support. \
    Your CPU does not support AVX (pre-2011 Intel or virtualized \
    without AVX passthrough). OCR and rerank are disabled. \
    All other features work normally.";

/// Ensure ONNX Runtime is loaded and initialized.
///
/// Returns `Ok(())` if ONNX is ready for use, or an `Err` with a
/// human-readable message explaining why OCR/rerank is unavailable.
///
/// Safe to call multiple times: the first call loads+inits, all
/// subsequent calls return immediately.
pub fn ensure_loaded() -> Result<(), String> {
    #[cfg(not(any(feature = "ocr", feature = "rerank")))]
    {
        Err("not compiled with OCR/rerank support".to_string())
    }
    #[cfg(any(feature = "ocr", feature = "rerank"))]
    {
        static STATE: OnceLock<Result<(), String>> = OnceLock::new();
        STATE.get_or_init(load_and_init).clone()
    }
}

// ── Linux: dynamic loading via dlopen ───────────────────────────

#[cfg(all(target_os = "linux", any(feature = "ocr", feature = "rerank")))]
fn load_and_init() -> Result<(), String> {
    // 1. AVX gate (disk-cached, permanent if true).
    if !crate::cpu::has_avx() {
        return Err(NO_AVX_MSG.to_string());
    }

    // 2. Find the shared library.
    let lib_path = find_shared_lib().ok_or_else(|| {
        "ONNX Runtime shared library not found. \
            OCR and rerank are disabled."
            .to_string()
    })?;

    // 3. dlopen and init.
    //    ort::init_from loads the .so via libloading.
    //    builder.commit() initializes the ONNX environment.
    let builder = ort::init_from(&lib_path).map_err(|e| {
        format!(
            "Failed to load ONNX Runtime from {}: {e}",
            lib_path.display()
        )
    })?;

    builder.commit();

    eprintln!("[onnx] Runtime loaded from {}", lib_path.display());
    Ok(())
}

/// Find the ONNX Runtime shared library (Linux only).
///
/// Searches:
/// 1. Next to the current executable (primary).
/// 2. `cache_dir()/onnx/` (fallback for relocatable installs).
#[cfg(all(target_os = "linux", any(feature = "ocr", feature = "rerank")))]
fn find_shared_lib() -> Option<PathBuf> {
    let lib_name = shared_lib_name();

    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let candidate = parent.join(lib_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    let cache = crate::paths::cache_dir().join("onnx").join(lib_name);
    if cache.exists() {
        return Some(cache);
    }

    None
}

/// Platform-specific shared library filename (Linux only).
#[cfg(all(target_os = "linux", any(feature = "ocr", feature = "rerank")))]
fn shared_lib_name() -> &'static str {
    "libonnxruntime.so"
}

// ── macOS / Windows: static linking ────────────────────────────

#[cfg(all(not(target_os = "linux"), any(feature = "ocr", feature = "rerank")))]
fn load_and_init() -> Result<(), String> {
    // macOS ARM64: no AVX concept (ARM NEON). Always works.
    // Windows x64: if no AVX, process already crashed at startup
    //   (static constructors ran before main). This code only
    //   runs on AVX-capable machines.
    // Just initialize the ONNX environment (static link).
    ort::init().commit();
    eprintln!("[onnx] Runtime initialized (static link)");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(all(target_os = "linux", any(feature = "ocr", feature = "rerank")))]
    #[test]
    fn shared_lib_name_is_so_on_linux() {
        assert_eq!(super::shared_lib_name(), "libonnxruntime.so");
    }
}

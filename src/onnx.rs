//! Runtime ONNX Runtime initialization via dlopen.
//!
//! ONNX Runtime is loaded dynamically at runtime to avoid SIGILL
//! on non-AVX CPUs. The prebuilt ONNX static archive contains
//! unguarded AVX instructions in its C++ global constructors.
//! Statically linking it causes SIGILL at process start, before
//! `main()` runs.
//!
//! With `ort`'s `load-dynamic` feature, ONNX is NOT statically
//! linked. Instead, we dlopen the shared library at runtime,
//! but only after confirming AVX support via the CPU module.
//!
//! ## Lifecycle
//!
//! 1. Process starts (no ONNX code loaded, no AVX constructors).
//! 2. User runs OCR or rerank.
//! 3. `ensure_loaded()` checks AVX support (disk-cached).
//! 4. If no AVX: return error, OCR/rerank disabled gracefully.
//! 5. If AVX: dlopen `libonnxruntime.so` (or `.dylib`/`.dll`),
//!    call `ort::init_from(path)`, then `ort::init()`.
//! 6. Subsequent OCR/rerank calls reuse the initialized runtime.
//!
//! ## Shared library location
//!
//! The shared library is built at compile time from the prebuilt
//! ONNX static archive and placed next to the donsetch binary.
//! At runtime, we search:
//!   a. Next to the current executable
//!   b. `cache_dir()/onnx/` (fallback for relocatable installs)

#[cfg(any(feature = "ocr", feature = "rerank"))]
use std::path::PathBuf;
#[cfg(any(feature = "ocr", feature = "rerank"))]
use std::sync::OnceLock;

/// Error returned when the CPU lacks AVX support.
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

#[cfg(any(feature = "ocr", feature = "rerank"))]
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
    //    ort::init_from loads the .so/.dylib/.dll via libloading.
    //    ort::init() initializes the ONNX environment using the
    //    dlopen'd library.
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

/// Find the ONNX Runtime shared library.
///
/// Searches:
/// 1. Next to the current executable (primary).
/// 2. `cache_dir()/onnx/` (fallback for relocatable installs).
#[cfg(any(feature = "ocr", feature = "rerank"))]
fn find_shared_lib() -> Option<PathBuf> {
    let lib_name = shared_lib_name();

    // 1. Next to executable.
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let candidate = parent.join(lib_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // 2. Cache dir fallback.
    let cache = crate::paths::cache_dir().join("onnx").join(lib_name);
    if cache.exists() {
        return Some(cache);
    }

    None
}

/// Platform-specific shared library filename.
#[cfg(any(feature = "ocr", feature = "rerank"))]
fn shared_lib_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "libonnxruntime.so"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else {
        "libonnxruntime.so" // fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_lib_name_matches_platform() {
        let name = shared_lib_name();
        if cfg!(target_os = "linux") {
            assert_eq!(name, "libonnxruntime.so");
        } else if cfg!(target_os = "macos") {
            assert_eq!(name, "libonnxruntime.dylib");
        } else if cfg!(target_os = "windows") {
            assert_eq!(name, "onnxruntime.dll");
        }
    }
}

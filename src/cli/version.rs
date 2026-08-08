//! `donsetch -v` / `donsetch --version` — build identity at a glance.

pub fn run() {
    crate::cli::init();
    crate::cli::print_title(&format!("DonSeTch {}", env!("CARGO_PKG_VERSION")));

    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    crate::cli::print_kv("binary", &exe);
    crate::cli::print_kv(
        "target",
        option_env!("DONSHEET_TARGET").unwrap_or("unknown"),
    );
    crate::cli::print_kv("profile", "chrome-150");
    crate::cli::print_kv(
        "features",
        option_env!("DONSHEET_FEATURES").unwrap_or("(none)"),
    );
    crate::cli::print_kv(
        "pdfium",
        option_env!("DONSHEET_PDFIUM").unwrap_or("unknown"),
    );
    crate::cli::print_kv(
        "git",
        option_env!("DONSHEET_GIT_HASH").unwrap_or("unknown"),
    );
}

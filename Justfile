# DonSeTch dev loop — one profile, everything fast.
#
# Everything below runs on the `ci` cargo profile (release opts,
# no fat LTO): test links take seconds, the binary behaves like
# release (`panic = "abort"` inherited), and all artifacts share
# one graph in target/ci. Fat LTO runs once, at ship time, via a
# plain `cargo build --release` (see status.md Workflow section).
#
#   just check    compile-check the full feature set (~10s warm)
#   just test     full test suite, full features, fail-fast
#   just lint     clippy -Dwarnings on the full feature set
#   just all      the pre-push gate: fmt + lint + test
#   just bin      build target/ci/donsetch (for live smokes)
#   just smoke    bin + doctor + fetch/search/bypass smoke
#   just fuzz extract    30s fuzz burst on one target

# Pre-push gate: everything CI will flag.
all: fmt-check lint test

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

# Clippy on the full feature set; --profile ci reuses the test
# artifact graph instead of compiling a dev one.
lint:
    cargo clippy --profile ci --all-targets --features ocr,rerank,http -- -Dwarnings

# Full suite, full feature set, fail-fast. The cargo profile is
# pinned via CLI (older nextest ignores the config-level
# cargo-profile key).
test:
    cargo nextest run --cargo-profile ci --features ocr,rerank,http

# The binary for live smoke runs (fast profile, real behavior).
bin:
    cargo build --profile ci --features ocr,rerank,http

# Compile-only full-feature check, fastest structural signal.
check:
    cargo check --profile ci --all-targets --features ocr,rerank,http

# Live smoke: payload, normal site, walled site, search.
smoke: bin
    target/ci/donsetch doctor 2>&1 | rg -i 'ONNX|Status' | head -4
    target/ci/donsetch fetch https://en.wikipedia.org/wiki/Markdown --json 2>/dev/null | head -c 120
    echo
    target/ci/donsetch search "linux kernel" --json 2>/dev/null | head -c 120
    echo

# 30-second fuzz burst on one target: just fuzz extract
fuzz target:
    cd fuzz && cargo fuzz run {{target}} -s none -- -max_total_time=30
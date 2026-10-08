//! Build identity for `degenbot --version` (ADR-051 D2).
//!
//! The console embeds the shared workspace build receipt that
//! `degenbot-python`'s `build.rs` maintains (`<repo>/.build-number`, holding
//! `<count> <fingerprint>`). Cargo gives no ordering guarantee between the two
//! packages' build scripts, and the receipt can be REWRITTEN by
//! `degenbot-python` inside the very invocation that reads it here (its scan
//! sees changed sources and re-stamps) — so a value read from the file can
//! silently lag the tree, which is exactly the order-flake the receipt test
//! used to hit on warm targets. The fingerprint is therefore recomputed with
//! the SAME shared scanner (`../degenbot-python/build_scan.rs`) over the same
//! tree: identical inputs, identical output, independent of script ordering.
//! The count stays a best-effort read — the counter state lives only in the
//! receipt, and the file may sit one advance ahead inside that in-flight
//! window.
//!
//! Python's `degenbot._ffi.build_fingerprint()` and `degenbot --version` still
//! report one identity: same scanner, same inputs, so the two surfaces can be
//! compared directly rather than trusted.
//!
//! This script deliberately never writes the receipt: advancing or restamping
//! it from here would defeat the Python stale-`.so` gate (a plain
//! `cargo build -p degenbot-cli` must not bless the installed extension as
//! fresh).

include!("../degenbot-python/build_scan.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=DEGENBOT_BUILD_NUMBER_FILE");

    let crate_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let receipt = std::env::var_os("DEGENBOT_BUILD_NUMBER_FILE")
        .map(PathBuf::from)
        .or_else(|| {
            // The ancestor holding `rust/Cargo.toml` IS the repository root -
            // only the repo root has a `rust/` directory - so the receipt sits
            // directly inside it. Climbing a further `.parent()` resolves one
            // directory above the repository and silently reads nothing.
            crate_dir
                .ancestors()
                .find(|ancestor| ancestor.join("rust/Cargo.toml").is_file())
                .map(|root| root.join(".build-number"))
        })
        .unwrap_or_else(|| PathBuf::from(".build-number"));

    // Watch the receipt whether or not it exists yet, and watch the scanned
    // sources exactly like the receipt's writer does, so the recomputed
    // fingerprint is refreshed whenever its inputs move.
    println!("cargo:rerun-if-changed={}", receipt.display());
    let scan = scan_workspace(&crate_dir);
    if let Some(scan) = &scan {
        for file in &scan.files {
            println!("cargo:rerun-if-changed={}", file.path.display());
        }
        for dir in &scan.dirs {
            println!("cargo:rerun-if-changed={}", dir.display());
        }
    }

    let text = fs::read_to_string(&receipt).unwrap_or_default();
    let mut parts = text.split_whitespace();
    let count = parts.next().unwrap_or("0");
    let receipt_fingerprint = parts.next();

    // Recomputed fingerprint first; fall back to the receipt's when the scan
    // cannot run (the same degradation the receipt writer accepts).
    let fingerprint = scan
        .map(|scan| format!("{:016x}", scan.fingerprint))
        .or_else(|| receipt_fingerprint.map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=DEGENBOT_CLI_BUILD_NUMBER={count}");
    println!("cargo:rustc-env=DEGENBOT_CLI_BUILD_FINGERPRINT={fingerprint}");
}

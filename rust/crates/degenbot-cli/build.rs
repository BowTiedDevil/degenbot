//! Build identity for `degenbot --version` (ADR-051 D2).
//!
//! The fingerprint is NOT recomputed here: this crate embeds the shared build
//! receipt that `degenbot-python`'s `build.rs` maintains
//! (`<repo>/.build-number`, holding `<count> <fingerprint>`). Python's
//! `degenbot._ffi.build_number()` / `build_fingerprint()` report the same two
//! values, so `degenbot --version` and `python -m degenbot.build_info` agree by
//! construction - the console is never a second, drifting build identity.
//!
//! This script deliberately only READS: it must never advance the counter or
//! rewrite the fingerprint, or the Python stale-`.so` gate would be defeated by
//! a plain `cargo build -p degenbot-cli`.

use std::fs;
use std::path::PathBuf;

fn main() {
    let crate_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let receipt = std::env::var_os("DEGENBOT_BUILD_NUMBER_FILE")
        .map(PathBuf::from)
        .or_else(|| {
            crate_dir
                .ancestors()
                .nth(3)
                .map(|root| root.join(".build-number"))
        })
        .unwrap_or_else(|| PathBuf::from(".build-number"));

    let text = fs::read_to_string(&receipt).unwrap_or_default();
    let mut parts = text.split_whitespace();
    let count = parts.next().unwrap_or("0");
    let fingerprint = parts.next().unwrap_or("unknown");

    println!("cargo:rustc-env=DEGENBOT_CLI_BUILD_NUMBER={count}");
    println!("cargo:rustc-env=DEGENBOT_CLI_BUILD_FINGERPRINT={fingerprint}");
    if receipt.exists() {
        // Re-embed when the receipt advances, so a fresh compile always
        // reports the receipt the Python extension was built against.
        println!("cargo:rerun-if-changed={}", receipt.display());
    }
}

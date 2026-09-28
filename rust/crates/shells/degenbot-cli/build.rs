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

    // Watch the receipt whether or not it exists yet: cargo re-runs this
    // script once a watched missing path appears. A COLD target can still
    // embed `0 unknown` on its first build, because cargo gives no ordering
    // guarantee between build scripts and `degenbot-python` may not have
    // written the receipt when this one runs (its `cargo:rustc-env` cannot
    // reach another package, so there is no ordering-independent channel).
    // The unconditional watch makes that zero NON-PERMANENT - the next build
    // re-embeds the receipt - but it cannot make it impossible.
    println!("cargo:rerun-if-changed={}", receipt.display());

    let text = fs::read_to_string(&receipt).unwrap_or_default();
    let mut parts = text.split_whitespace();
    let count = parts.next().unwrap_or("0");
    let fingerprint = parts.next().unwrap_or("unknown");

    println!("cargo:rustc-env=DEGENBOT_CLI_BUILD_NUMBER={count}");
    println!("cargo:rustc-env=DEGENBOT_CLI_BUILD_FINGERPRINT={fingerprint}");
}

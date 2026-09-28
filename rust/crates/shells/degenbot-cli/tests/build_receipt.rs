//! The console's embedded build receipt must match the repo receipt file.
//!
//! `degenbot --version` embeds the shared receipt `<repo>/.build-number`
//! through `build.rs`. This gate makes a drift between the two loud instead
//! of a silently stale `--version`. It is tolerant of cold builds: a fresh
//! checkout legitimately has no receipt until the first `degenbot-python`
//! build writes one, and a cold target can bake a zero into the console
//! until the next build re-embeds it (see `build.rs` for why that first
//! build cannot be fixed from inside this crate).

use std::path::PathBuf;

/// The same rule `build.rs` uses. The ancestor holding `rust/Cargo.toml` IS
/// the repository root - only the repo root has a `rust/` directory - so the
/// receipt sits directly inside it. Taking a further `.parent()` here would
/// resolve one directory above the repository and silently read nothing.
#[must_use]
fn repo_receipt() -> Option<PathBuf> {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .ancestors()
        .find(|ancestor| ancestor.join("rust/Cargo.toml").is_file())
        .map(|root| root.join(".build-number"))
}

#[test]
#[expect(
    clippy::print_stderr,
    reason = "a skipped gate must state its reason where the runner shows it"
)]
fn embedded_receipt_matches_the_repo_receipt_file() {
    let Some(receipt) = repo_receipt() else {
        eprintln!(
            "SKIP: no repository root found above {}",
            env!("CARGO_MANIFEST_DIR")
        );
        return;
    };
    let Ok(text) = std::fs::read_to_string(&receipt) else {
        eprintln!(
            "SKIP: {} does not exist - a fresh checkout has no receipt until \
             the first degenbot-python build writes one",
            receipt.display()
        );
        return;
    };
    let mut parts = text.split_whitespace();
    let (Some(count), Some(fingerprint)) = (parts.next(), parts.next()) else {
        eprintln!(
            "SKIP: {} is empty or malformed ({text:?}) - nothing to compare against",
            receipt.display()
        );
        return;
    };
    assert_eq!(
        env!("DEGENBOT_CLI_BUILD_NUMBER"),
        count,
        "the console's embedded build number must equal the repo receipt"
    );
    assert_eq!(
        env!("DEGENBOT_CLI_BUILD_FINGERPRINT"),
        fingerprint,
        "the console's embedded build fingerprint must equal the repo receipt"
    );
}

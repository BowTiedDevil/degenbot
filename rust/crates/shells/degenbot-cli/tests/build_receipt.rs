//! The console's embedded build receipt must match the repo receipt file.
//!
//! `degenbot --version` embeds the shared workspace build identity through
//! `build.rs`. This gate makes a drift between the two loud instead of a
//! silently stale `--version`, deterministically on warm AND cold targets:
//!
//! - the embedded fingerprint is asserted against a scan of the tree taken
//!   HERE, in the test — the same scanner, the same inputs `build.rs` used —
//!   so a build script that merely READS the racy receipt (which
//!   `degenbot-python` can rewrite mid-invocation) fails even when the file
//!   happens to look settled;
//! - when the receipt file is settled on the same tree, it agrees with the
//!   embed byte-for-byte; a receipt lagging the tree (a committed fix awaiting
//!   the next extension build) is the documented stale state, not a drift, and
//!   only relaxes the count check to never-ahead.

include!("../../degenbot-python/build_scan.rs");

/// The same rule `build.rs` uses. The ancestor holding `rust/Cargo.toml` IS
/// the repository root - only the repo root has a `rust/` directory - so the
/// receipt sits directly inside it. Taking a further `.parent()` here would
/// resolve one directory above the repository and silently read nothing.
#[must_use]
fn repo_receipt() -> Option<PathBuf> {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .ancestors()
        .find(|ancestor: &&Path| ancestor.join("rust/Cargo.toml").is_file())
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
    // The embedded fingerprint is the tree's identity, recomputed by the
    // shared scanner at build time — identical inputs to this scan, taken in
    // the same invocation, so the two must agree regardless of the order in
    // which cargo ran the two packages' build scripts.
    let computed = scan_workspace(&PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .map(|scan| format!("{:016x}", scan.fingerprint));
    if let Some(computed) = &computed {
        assert_eq!(
            env!("DEGENBOT_CLI_BUILD_FINGERPRINT"),
            computed,
            "the console's embedded fingerprint must be the current tree's              identity, not a racy read of the receipt file"
        );
    } else {
        eprintln!("SKIP: the workspace scan did not run - fingerprint unchecked");
    }

    let Ok(text) = std::fs::read_to_string(&receipt) else {
        eprintln!(
            "SKIP: {} does not exist - a fresh checkout has no receipt until              the first degenbot-python build writes one",
            receipt.display()
        );
        return;
    };
    let mut parts = text.split_whitespace();
    let (count, fingerprint) = (parts.next(), parts.next());
    // The count is monotonic state read best-effort: the console may sit one
    // in-flight advance behind the receipt, never ahead of it.
    if let (Ok(embedded), Some(Ok(file_count))) = (
        env!("DEGENBOT_CLI_BUILD_NUMBER").parse::<u64>(),
        count.map(str::parse::<u64>),
    ) {
        assert!(
            embedded <= file_count,
            "the console's embedded build number must never be ahead of the              repo receipt (embedded {embedded}, receipt {file_count})"
        );
    }
    // When the receipt is settled on the same tree this console was built
    // from, it must agree with the embed byte-for-byte.
    if let (Some(file_fp), Some(computed)) = (fingerprint, computed.as_deref()) {
        if file_fp == computed {
            assert_eq!(
                env!("DEGENBOT_CLI_BUILD_FINGERPRINT"),
                file_fp,
                "the console's embedded fingerprint must equal the settled repo receipt"
            );
        }
    }
}

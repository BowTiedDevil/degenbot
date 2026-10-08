//! The console startup freshness gate: the loaded extension build stamp
//! versus the source tree.
//!
//! The `degenbot` console lives INSIDE the compiled extension — the venv
//! `degenbot._ffi` cdylib embeds the argv facade, and `degenbot._cli:main` is
//! a passthrough into it. maturin/uv have repeatedly served a stale cached
//! cdylib after Rust edits while reporting a successful rebuild (AGENTS.md
//! "Rebuilding the Rust `.so` after edits"), which silently ran old console
//! code through entire sessions. `degenbot.build_info` guards the Python test
//! harness against that; this module is the same guard on every CLI startup,
//! comparing against what the tree looks like NOW:
//!
//! 1. **receipt vs baked stamp** — the workspace receipt `.build-number`
//!    (the same one `build.rs` embeds) names the latest build; a fingerprint
//!    or count ahead of this build means the workspace was rebuilt after
//!    this console was;
//! 2. **sources vs receipt** — the newest source mtime under `rust/crates/`
//!    newer than the receipt means committed source that the last build (and
//!    so this console) predates.
//!
//! Both are local stat/read comparisons — zero network. The gate WARNS, it
//! does not refuse: running an old console deliberately (bisecting, pinned
//! deploys) must stay possible, and a run with no receipt reachable from the
//! working directory has no ground truth and is skipped.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The receipt file written by `degenbot-python` build.rs; keep the name and
/// the `<count> <fingerprint-hex>` format in sync with that script.
const RECEIPT_NAME: &str = ".build-number";

/// The fingerprint value build.rs bakes when it could not compute one.
const NO_FINGERPRINT: &str = "unknown";

/// The stamp this console was built with (baked by build.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BakedStamp {
    pub number: &'static str,
    pub fingerprint: &'static str,
}

/// The repo receipt: `<count> <fingerprint>` from `.build-number`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub count: u64,
    pub fingerprint: Option<String>,
}

/// Why the loaded console is older than the source tree, if it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleReason {
    /// The receipt and this console carry different build fingerprints: the
    /// workspace was rebuilt after this console was built, or this console
    /// was built from newer sources than the last receipt-writing build.
    /// Either way the two sides are out of identity.
    ReceiptAdvanced,
    /// Sources were committed after the last receipt-writing build, so this
    /// console — built from that tree — predates them.
    SourceNewerThanBuild,
}

impl StaleReason {
    /// The one-line startup warning: names the loaded stamp, the source-tree
    /// evidence, and the fix.
    #[must_use]
    pub fn warning(&self, baked: &BakedStamp, receipt: &Receipt) -> String {
        let loaded = format!("loaded build {} ({})", baked.number, baked.fingerprint);
        let receipt_text = format!(
            "repo receipt {} ({})",
            receipt.count,
            receipt.fingerprint.as_deref().unwrap_or("-")
        );
        match self {
            Self::ReceiptAdvanced => format!(
                "WARNING: degenbot console may be stale: {loaded} vs {receipt_text} — the loaded build identity does not match the latest receipt; rebuild the extension with 'just dev'"
            ),
            Self::SourceNewerThanBuild => format!(
                "WARNING: degenbot console may be stale: {loaded} vs {receipt_text} — rust/crates has sources newer than the last build; rebuild the extension with 'just dev'"
            ),
        }
    }
}

/// Decide staleness from the parsed inputs (pure, so the branches are testable
/// without a repository).
///
/// A matching fingerprint on both sides is identity, even when the counts lag:
/// the counter only advances when the fingerprint changes, and an in-flight
/// receipt rewrite can sit one advance ahead of a concurrently built console.
#[must_use]
pub fn staleness(
    baked: &BakedStamp,
    receipt: &Receipt,
    newest_source: SystemTime,
    receipt_mtime: SystemTime,
) -> Option<StaleReason> {
    match (&receipt.fingerprint, baked.fingerprint) {
        (Some(receipt_fp), baked_fp) if baked_fp != NO_FINGERPRINT => {
            if baked_fp != receipt_fp.as_str() {
                return Some(StaleReason::ReceiptAdvanced);
            }
        }
        // Legacy receipt (pre-fingerprint) or a console whose tagging broke:
        // fall back to the monotonic count.
        _ => {
            if let Ok(baked_count) = baked.number.parse::<u64>() {
                if baked_count < receipt.count {
                    return Some(StaleReason::ReceiptAdvanced);
                }
            }
        }
    }
    (newest_source > receipt_mtime).then_some(StaleReason::SourceNewerThanBuild)
}

/// The repository root (the one ancestor holding `rust/Cargo.toml`), found
/// from the working directory the console was invoked in.
#[must_use]
fn repo_root_from_cwd() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    cwd.ancestors()
        .find(|ancestor| ancestor.join("rust/Cargo.toml").is_file())
        .map(Path::to_path_buf)
}

/// Parse `<count> <fingerprint>` receipt text.
#[must_use]
pub fn parse_receipt(text: &str) -> Option<Receipt> {
    let mut parts = text.split_whitespace();
    let count = parts.next()?.parse::<u64>().ok()?;
    Some(Receipt {
        count,
        fingerprint: parts.next().map(str::to_string),
    })
}

/// The newest mtime among the Rust sources and manifests under `rust/crates/`
/// (plus the workspace manifests), skipping build-output target dirs.
#[must_use]
fn newest_source_mtime(root: &Path) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;
    let mut stack = vec![
        root.join("rust/crates"),
        root.join("rust/Cargo.toml"),
        root.join("Cargo.lock"),
    ];
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    stack.push(entry.path());
                }
            }
        } else if path.file_name().is_some_and(|name| name == "Cargo.lock")
            || path
                .extension()
                .is_some_and(|ext| ext == "rs" || ext == "toml")
        {
            if let Ok(mtime) = path.metadata().and_then(|meta| meta.modified()) {
                if newest.is_none_or(|n| mtime > n) {
                    newest = Some(mtime);
                }
            }
        }
    }
    newest
}

/// Emit the startup warning to `out`; returns whether one was written.
fn warn_to(out: &mut dyn std::io::Write, baked: &BakedStamp) -> bool {
    let Some(root) = repo_root_from_cwd() else {
        return false;
    };
    let receipt_path = root.join(RECEIPT_NAME);
    let receipt_mtime = receipt_path.metadata().and_then(|meta| meta.modified());
    let text = std::fs::read_to_string(&receipt_path);
    let (Ok(text), Ok(receipt_mtime)) = (text, receipt_mtime) else {
        // Non-checkout run or a receipt never yet written: no ground truth.
        return false;
    };
    let Some(receipt) = parse_receipt(&text) else {
        return false;
    };
    let Some(newest_source) = newest_source_mtime(&root) else {
        return false;
    };
    if let Some(reason) = staleness(baked, &receipt, newest_source, receipt_mtime) {
        let _ = writeln!(out, "{}", reason.warning(baked, &receipt));
        return true;
    }
    false
}

/// The startup gate: print a loud one-line warning on stderr when the loaded
/// console build stamp is older than the source tree. Never refuses, never
/// touches the network.
pub fn warn_if_extension_stale() {
    let baked = BakedStamp {
        number: env!("DEGENBOT_CLI_BUILD_NUMBER"),
        fingerprint: env!("DEGENBOT_CLI_BUILD_FINGERPRINT"),
    };
    let mut stderr = std::io::stderr().lock();
    let _ = warn_to(&mut stderr, &baked);
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used)]

    use std::time::{Duration, SystemTime};

    use super::{
        newest_source_mtime, parse_receipt, repo_root_from_cwd, staleness, BakedStamp, Receipt,
        StaleReason,
    };

    fn baked(number: &str, fingerprint: &str) -> BakedStamp {
        BakedStamp {
            number: Box::leak(number.to_string().into_boxed_str()),
            fingerprint: Box::leak(fingerprint.to_string().into_boxed_str()),
        }
    }

    fn receipt(count: u64, fingerprint: Option<&str>) -> Receipt {
        Receipt {
            count,
            fingerprint: fingerprint.map(str::to_string),
        }
    }

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn matching_fingerprint_is_fresh_regardless_of_count_lag() {
        // An in-flight receipt rewrite can sit one advance ahead of a
        // concurrently built console; identity is the fingerprint.
        assert_eq!(
            staleness(
                &baked("5", "aaaa0000aaaa0000"),
                &receipt(6, Some("aaaa0000aaaa0000")),
                at(100),
                at(100),
            ),
            None
        );
    }

    #[test]
    fn receipt_ahead_of_the_console_is_stale() {
        assert_eq!(
            staleness(
                &baked("5", "aaaa0000aaaa0000"),
                &receipt(6, Some("bbbb0000bbbb0000")),
                at(100),
                at(100),
            ),
            Some(StaleReason::ReceiptAdvanced)
        );
    }

    #[test]
    fn legacy_count_compare_applies_without_fingerprints() {
        assert_eq!(
            staleness(&baked("5", "unknown"), &receipt(6, None), at(100), at(100)),
            Some(StaleReason::ReceiptAdvanced)
        );
        assert_eq!(
            staleness(&baked("6", "unknown"), &receipt(6, None), at(100), at(100)),
            None
        );
    }

    #[test]
    fn source_newer_than_the_receipt_is_stale() {
        assert_eq!(
            staleness(
                &baked("6", "aaaa0000aaaa0000"),
                &receipt(6, Some("aaaa0000aaaa0000")),
                at(200),
                at(100),
            ),
            Some(StaleReason::SourceNewerThanBuild)
        );
    }

    #[test]
    fn warning_names_both_stamps_and_the_fix() {
        let line = StaleReason::ReceiptAdvanced.warning(
            &baked("5", "aaaa0000aaaa0000"),
            &receipt(6, Some("bbbb0000bbbb0000")),
        );
        assert!(line.contains("loaded build 5 (aaaa0000aaaa0000)"), "{line}");
        assert!(line.contains("repo receipt 6 (bbbb0000bbbb0000)"), "{line}");
        assert!(line.contains("'just dev'"), "{line}");
        assert_eq!(line.lines().count(), 1, "the warning is one line: {line}");
    }

    #[test]
    fn receipt_parsing() {
        let parsed = parse_receipt("2119 a0ba4c0c3993de2f\n").unwrap();
        assert_eq!(parsed.count, 2119);
        assert_eq!(parsed.fingerprint.as_deref(), Some("a0ba4c0c3993de2f"));
        assert!(parse_receipt("not-a-count\n").is_none());
        let legacy = parse_receipt("42\n").unwrap();
        assert_eq!(legacy.count, 42);
        assert_eq!(legacy.fingerprint, None);
    }

    #[test]
    fn repo_root_is_found_from_the_crate_dir() {
        // Tests run inside the checkout, so the same ancestor walk build.rs
        // and the receipt test use must land on the repository root.
        let root = repo_root_from_cwd().expect("tests run inside a checkout");
        assert!(root.join("rust/Cargo.toml").is_file());
        assert!(newest_source_mtime(&root).is_some());
    }
}

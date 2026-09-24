//! Build identity: the monotonic build counter + source fingerprint baked in
//! by `build.rs`.
//!
//! Purpose (AGENTS.md "Rebuilding the Rust `.so` after edits"): maturin/uv
//! have repeatedly served a stale cached artifact for this cdylib after Rust
//! edits while reporting a successful rebuild. Every compile of `degenbot_rs`
//! runs `build.rs`, which stores `<count> <fingerprint>` in
//! `<repo>/.build-number` and embeds both values — the counter advances only
//! when the fingerprint (content hash of this crate's sources) changes, so a
//! no-change recompile never marks an installed wheel stale, while any wheel
//! actually built from different sources carries a different fingerprint.
//! (The counter lives outside `rust/target` so `cargo clean` and sweeps can
//! never roll it back.)
//!
//! Python surface: `degenbot._ffi.build_number()` (registered in `c_api`) and
//! the cross-check helper `degenbot.build_info` (staleness comparisons live
//! Python-side, where the receipt file and the installed library meet).

use pyo3::prelude::*;

/// The counter string baked into THIS compilation by `build.rs`.
/// `"0"` is the no-`build.rs` fallback (build.rs runs for every cargo build
/// of this crate, so 0 in the wild means the tagging itself broke).
pub const BUILD_NUMBER_STR: &str = match option_env!("DEGENBOT_BUILD_NUMBER") {
    Some(n) => n,
    None => "0",
};

/// The source fingerprint hex string baked into THIS compilation.
pub const BUILD_FINGERPRINT_STR: &str = match option_env!("DEGENBOT_BUILD_FINGERPRINT") {
    Some(fp) => fp,
    None => "",
};

/// The build counter as a number.
#[must_use]
pub fn build_number_value() -> u64 {
    BUILD_NUMBER_STR.parse().unwrap_or(0)
}

/// Pyfunction: the build counter baked into this compiled extension.
/// Registered on the root `degenbot._ffi` module (unconditional — the
/// staleness check must work in every feature configuration).
#[must_use]
#[pyfunction]
pub fn build_number() -> u64 {
    build_number_value()
}

/// Pyfunction: the source fingerprint hex string baked into this compiled
/// extension (empty string when build.rs did not run).
#[must_use]
#[pyfunction]
pub fn build_fingerprint() -> &'static str {
    BUILD_FINGERPRINT_STR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_number_is_baked_by_build_rs() {
        // build.rs runs on EVERY compile of this crate (including this test
        // build), so the number must be present and positive.
        assert!(
            build_number_value() >= 1,
            "DEGENBOT_BUILD_NUMBER missing - build.rs did not run (baked: {BUILD_NUMBER_STR:?})"
        );
        assert_eq!(build_number(), build_number_value());
    }

    #[test]
    fn fingerprint_is_baked_by_build_rs() {
        // The stored receipt always carries a fingerprint once the first
        // build has run; an empty value means the tagging broke.
        assert!(
            BUILD_FINGERPRINT_STR.len() == 16,
            "DEGENBOT_BUILD_FINGERPRINT missing/ill-formed (baked: {BUILD_FINGERPRINT_STR:?})"
        );
        assert!(
            u64::from_str_radix(BUILD_FINGERPRINT_STR, 16).is_ok(),
            "fingerprint is not valid hex: {BUILD_FINGERPRINT_STR:?}"
        );
    }

    #[test]
    fn counter_file_never_rolls_back() {
        // The stored count must be >= the value baked into this binary: it
        // advances only, and lives outside rust/target so `cargo clean` and
        // gc-target sweeps never roll it back.
        let path = env!("DEGENBOT_BUILD_NUMBER_FILE");
        if let Ok(text) = std::fs::read_to_string(path) {
            let count = text
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok());
            assert!(
                count.is_some_and(|c| c >= build_number_value()),
                "counter file first token missing/non-numeric or below the baked value: {text:?}"
            );
        }
    }
}

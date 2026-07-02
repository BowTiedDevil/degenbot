//! Revert-data taxonomy: classify raw simulation revert return-data into a
//! short stable label.
//!
//! Pure bytes→label transform (no I/O, never panics). Used by the `[sim]`
//! summary to break the `N failed` bucket down by root cause, and by the
//! executor + Simulation paths to tally reverts by canonical error name.
//!
//! This is a sibling decode concern to the event-log decoders
//! (`v2_sync_decoder`, …): both hand-slice EVM bytes into a structured
//! result. The taxonomy lives here (not under the executor or Simulation
//! epics) so *both* paths consume it without a cross-epic dependency.
//!
//! # §4.2 parity
//!
//! The oracle is the Python `classify_revert` in
//! `examples/eth_backrun_helpers.py` (L336–L377). Every revert category
//! produces the **identical label string** in Rust and Python — pinned by the
//! `revert::tests::parity_vs_python_oracle` fixture corpus covering every
//! category + the truncation / empty / undecodable edge cases. The Python
//! taxonomy is the source of truth; the Rust port reproduces its hex-string
//! slicing + UTF-8-lossy decode byte-for-byte.

use alloy::hex;
use alloy::primitives::U256;

/// The Solidity `Error(string)` selector — `keccak256("Error(string)")[:4]`.
pub const ERROR_STRING_SELECTOR: &str = "08c379a0";

/// The Solidity `Panic(uint256)` selector — `keccak256("Panic(uint256)")[:4]`.
pub const PANIC_SELECTOR: &str = "4e487b71";

/// Selector (8 lowercase hex chars) → full custom-error signature for the V4
/// `PoolManager` revert selectors. The label drops the params (everything
/// before the first `(`), so `InsufficientProfit(1,2)` and
/// `InsufficientProfit(3,4)` tally together. Ported verbatim from the Python
/// `_V4_REVERT_SELECTORS` (`examples/eth_backrun_helpers.py` L304–L314).
pub const V4_REVERT_SELECTORS: &[(&str, &str)] = &[
    ("5212cba1", "CurrencyNotSettled()"),
    ("486aa307", "PoolNotInitialized()"),
    ("1e048e1d", "InvalidHookResponse()"),
    ("a3603d66", "SwapQuantityCannotBeZero()"),
    ("38606b01", "PriceLimitAlreadyExceeded()"),
    ("30d6072a", "PriceLimitOutOfBounds()"),
    ("a40afa38", "LockFailure()"),
    ("5090d6c6", "AlreadyUnlocked()"),
    ("54e3ca0d", "ManagerLocked()"),
];

/// Selector → full custom-error signature for the `cmd_executor` revert
/// selectors (legacy bare-assert labels + Vyper 0.5.0a3+ custom errors). Ported
/// verbatim from the Python `_EXECUTOR_REVERT_SELECTORS`
/// (`examples/eth_backrun_helpers.py` L316–L327).
pub const EXECUTOR_REVERT_SELECTORS: &[(&str, &str)] = &[
    // Legacy (bare assert)
    ("4b9dfc58", "!OWNER"),
    ("49494100", "IIA(insufficient-input-amount)"),
    // Custom errors (Vyper 0.5.0a3+)
    ("8e4a23d6", "Unauthorized(caller)"),
    ("b028a63a", "InvalidCallback(caller)"),
    ("cf479181", "InsufficientBalance(amount,available)"),
    ("4e88422a", "InsufficientProfit(actual,expected)"),
    ("83276224", "InvalidCommand(opcode)"),
    ("60ef0bb0", "BipsTooHigh(bips)"),
    ("a61be9f0", "InvalidMsgValue(value)"),
    ("e5b6bf32", "NotPlainEthTransfer()"),
];

/// A classified revert category. The structured form (a standalone Rust
/// consumer matches on the variant); [`RevertClass::label`] renders the exact
/// Python label string (§4.2 parity).
///
/// Deliberately classifies *every* revert, even malformed ones — a taxonomy
/// must tally, so the summary always adds up. [`RevertClass::classify`] never
/// panics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevertClass {
    /// Empty return-data (`0x` or empty).
    Empty,
    /// 1–3 bytes of return-data — too short for a 4-byte selector.
    /// Carries the full lowercase hex of the trailing bytes.
    Short(String),
    /// `Panic(uint256 code)` — carries the 256-bit panic code.
    Panic(U256),
    /// `Error(string)` revert whose length field could not be read
    /// (return-data truncated before the length word).
    ErrorStringUndecodable,
    /// `Error(string)` revert with an empty decoded message.
    ErrorStringEmpty,
    /// `Error(string)` revert — carries the UTF-8-lossy-decoded message.
    ErrorString(String),
    /// A V4 `PoolManager` custom-error selector — carries the params-dropped
    /// base name (e.g. `CurrencyNotSettled`).
    V4Error(&'static str),
    /// A `cmd_executor` custom-error selector — carries the params-dropped
    /// base name (e.g. `InsufficientProfit`).
    ExecutorError(&'static str),
    /// Bare 32-byte numeric revert (Vyper): first 12 bytes all zero.
    NumericRevert,
    /// An unrecognised 4-byte selector — carries the raw selector bytes.
    Unknown([u8; 4]),
}

impl RevertClass {
    /// Classify raw simulation revert return-data into a stable category.
    ///
    /// Never panics: malformed input returns a best-effort variant
    /// (`Short` / `ErrorStringUndecodable` / `Unknown`).
    #[must_use]
    pub fn classify(revert_data: &[u8]) -> Self {
        // Python: hexed = revert_data.hex()  (lowercase, no 0x prefix)
        let hexed = hex::encode(revert_data);
        if hexed.is_empty() {
            return Self::Empty;
        }
        if hexed.len() < 8 {
            // 1–3 bytes: too short for a selector.
            return Self::Short(hexed);
        }
        let selector = &hexed[..8];
        if selector == PANIC_SELECTOR {
            // Panic(uint256 code) — code = bytes[4..36] if present, else 0.
            return Self::Panic(panic_code(&hexed));
        }
        if selector == ERROR_STRING_SELECTOR {
            return classify_error_string(&hexed);
        }
        if let Some(name) = lookup(V4_REVERT_SELECTORS, selector) {
            return Self::V4Error(name);
        }
        if let Some(name) = lookup(EXECUTOR_REVERT_SELECTORS, selector) {
            return Self::ExecutorError(name);
        }
        // Bare 32-byte numeric revert (Vyper): first 12 bytes all zero.
        if hexed.len() >= 64 && hexed.as_bytes()[..24].iter().all(|&c| c == b'0') {
            return Self::NumericRevert;
        }
        Self::Unknown([
            revert_data[0],
            revert_data[1],
            revert_data[2],
            revert_data[3],
        ])
    }

    /// Render the exact Python label for this category (§4.2 parity).
    ///
    /// Matches `examples/eth_backrun_helpers.py::classify_revert` byte-for-byte.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Empty => "empty".to_string(),
            Self::Short(h) => format!("short:{h}"),
            Self::Panic(code) => format!("Panic(0x{code:x})"),
            Self::ErrorStringUndecodable => "Error(string:undecodable)".to_string(),
            Self::ErrorStringEmpty => "Error(string:empty)".to_string(),
            Self::ErrorString(msg) => msg.clone(),
            Self::V4Error(name) | Self::ExecutorError(name) => (*name).to_string(),
            Self::NumericRevert => "numeric-revert".to_string(),
            Self::Unknown(sel) => format!("unknown:0x{}", hex::encode(sel)),
        }
    }
}

/// Read the `Panic(uint256)` code from `hexed[8..72]` (bytes 4..36), or 0 if
/// the return-data is shorter than 36 bytes. Python: `int(hexed[8:72], 16)`
/// when `len(hexed) >= 72` else 0.
fn panic_code(hexed: &str) -> U256 {
    if hexed.len() < 72 {
        return U256::ZERO;
    }
    // hexed[8..72] is exactly 64 hex chars (32 bytes) — a 256-bit big-endian.
    U256::from_str_radix(&hexed[8..72], 16).unwrap_or(U256::ZERO)
}

/// Classify an `Error(string)` revert. Python unpacks the standard layout
/// `[sel:4][offset:32][len:32][data:N]` (assumes offset = 0x20 — does NOT read
/// the offset field, matching the oracle exactly).
///
/// - `str_len = int(hexed[72:136], 16)` — the length word (bytes 36..68).
/// - `msg = bytes.fromhex(hexed[136..136 + str_len*2]).decode("utf-8",
///   errors="replace")`, truncated to available.
///
/// Undecodable ⟺ the length-word slice is empty (return-data length ≤ 36
/// bytes / `hexed.len() ≤ 72`), mirroring the Python `ValueError` on
/// `int("", 16)`. An empty decoded message → `Error(string:empty)`.
fn classify_error_string(hexed: &str) -> RevertClass {
    // Python: str_len = int(hexed[8+64 : 8+128], 16) = int(hexed[72:136], 16).
    // The slice clamps to available; empty (len ≤ 72) → ValueError → undecodable.
    let len_slice_end = hexed.len().min(136);
    if len_slice_end <= 72 {
        return RevertClass::ErrorStringUndecodable;
    }
    let len_slice = &hexed[72..len_slice_end];
    let Ok(str_len) = U256::from_str_radix(len_slice, 16) else {
        return RevertClass::ErrorStringUndecodable;
    };
    // Python: msg = bytes.fromhex(hexed[str_start : str_start + str_len*2])
    //   where str_start = 8 + 64 + 64 = 136.
    // str_len*2 as a byte count, clamped to the available bytes (Python slicing
    // clamps; we saturate the U256→usize to avoid overflow on huge lens).
    let str_start = 136_usize;
    if str_start >= hexed.len() {
        // No data bytes at all — empty message.
        return RevertClass::ErrorStringEmpty;
    }
    let take = str_len
        .saturating_mul(U256::from(2_usize))
        .to::<usize>()
        .min(hexed.len() - str_start);
    // The data slice is even-aligned (str_start=136 is even; the whole hexed is
    // even-length), so hex::decode succeeds — matching Python's
    // bytes.fromhex (which would raise ValueError on odd-length, unreachable
    // here). A malformed-but-truncated half is impossible.
    let Ok(msg_bytes) = hex::decode(&hexed[str_start..str_start + take]) else {
        return RevertClass::ErrorStringUndecodable;
    };
    // Python: .decode("utf-8", errors="replace") — U+FFFD per ill-formed byte.
    let msg = String::from_utf8_lossy(&msg_bytes).into_owned();
    if msg.is_empty() {
        RevertClass::ErrorStringEmpty
    } else {
        RevertClass::ErrorString(msg)
    }
}

/// Look up a selector (8 lowercase hex chars) in a `(selector, signature)`
/// table and return the params-dropped base name — the part before the first
/// `(`, or the whole signature if it has no params. Matches Python's
/// `sig.split("(", 1)[0]`.
fn lookup(table: &'static [(&'static str, &'static str)], selector: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(sel, _)| *sel == selector)
        .map(|(_, sig)| {
            // sig is a &'static str; splitting yields a &'static str substring.
            sig.split('(').next().unwrap_or(sig)
        })
}

/// Convenience: classify revert-data and render the Python label in one call.
///
/// Equivalent to [`RevertClass::classify`]`(...).`[`label`][RevertClass::label].
#[must_use]
pub fn classify_revert(revert_data: &[u8]) -> String {
    RevertClass::classify(revert_data).label()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    // ── §4.2 parity vs the Python `classify_revert` oracle ──────────────
    //
    // The oracle is `examples/eth_backrun_helpers.py::classify_revert`
    // (L336–L377 + the selector constants L304–L333). Every fixture's
    // expected label was captured from the Python oracle directly (the
    // corpus was validated against the live Python function — 0 mismatches).
    // The fixtures cover every category + the truncation / empty /
    // undecodable / odd-boundary edge cases.

    /// `(revert_data_hex, expected_python_label)` — the §4.2 parity corpus.
    /// Built at runtime (Rust `&str` has no repeat operator); the strings are
    /// the same bytes the Python oracle was run against.
    fn parity_fixtures() -> Vec<(String, &'static str)> {
        let z = |n: usize| "0".repeat(n * 2);
        vec![
            (String::new(), "empty"),
            ("de".to_string(), "short:de"),
            ("deadbe".to_string(), "short:deadbe"),
            ("12345678".to_string(), "unknown:0x12345678"),
            // Panic(uint256)
            (format!("4e487b71{}", z(32)), "Panic(0x0)"),
            (format!("4e487b71{}11", z(31)), "Panic(0x11)"),
            (format!("4e487b71{}1234", z(30)), "Panic(0x1234)"),
            ("4e487b71".to_string(), "Panic(0x0)"), // len < 72 → code=0
            (format!("4e487b71{}ff", z(10)), "Panic(0x0)"), // len 30 < 72
            // Error(string): [sel][offset:32][len:32][data:N]
            (
                format!(
                    "08c379a0{}000000000000000000000000000000000000000000000000000000000000000568656c6c6f{}",
                    z(32), z(21)
                ),
                "hello",
            ),
            (
                format!(
                    "08c379a0{}0000000000000000000000000000000000000000000000000000000000000000",
                    z(32)
                ),
                "Error(string:empty)",
            ),
            ("08c379a0".to_string(), "Error(string:undecodable)"), // len ≤ 72
            (format!("08c379a0{}", z(32)), "Error(string:undecodable)"), // len == 72
            // V4 selectors (params dropped)
            ("5212cba1".to_string(), "CurrencyNotSettled"),
            (format!("486aa307{}", z(32)), "PoolNotInitialized"),
            ("a40afa38deadbeef".to_string(), "LockFailure"),
            ("54e3ca0d".to_string(), "ManagerLocked"),
            // cmd_executor selectors
            (format!("49494100{}", z(32)), "IIA"),
            ("4b9dfc58".to_string(), "!OWNER"), // no "(" in signature
            (format!("8e4a23d6{}", z(32)), "Unauthorized"),
            (format!("4e88422a{}", z(64)), "InsufficientProfit"),
            (format!("83276224{}", "1".repeat(64)), "InvalidCommand"),
            (format!("60ef0bb0{}", z(32)), "BipsTooHigh"),
            ("e5b6bf32".to_string(), "NotPlainEthTransfer"),
            (format!("a61be9f0{}", "f".repeat(64)), "InvalidMsgValue"),
            (format!("cf479181{}", z(64)), "InsufficientBalance"),
            (format!("b028a63a{}", z(32)), "InvalidCallback"),
            // numeric-revert (Vyper bare 32-byte): first 12 bytes all zero
            (z(32), "numeric-revert"),
            (format!("{}1234", z(30)), "numeric-revert"),
            (format!("{}ff", z(31)), "numeric-revert"),
            (format!("00000000{}", z(28)), "numeric-revert"),
            // unknown selectors
            (format!("deadbeef{}", z(32)), "unknown:0xdeadbeef"),
            ("cafebabe".to_string(), "unknown:0xcafebabe"),
            // first 12 bytes NOT all zero → not numeric → unknown (sel 00000000)
            (format!("{}01{}", z(11), z(20)), "unknown:0x00000000"),
        ]
    }

    #[test]
    fn parity_vs_python_oracle() {
        for (hex_str, expected) in parity_fixtures() {
            let bytes = hex::decode(&hex_str).unwrap_or_default();
            let label = classify_revert(&bytes);
            assert_eq!(
                &label, expected,
                "revert_data=0x{hex_str}: Rust label {label:?} != Python oracle {expected:?}"
            );
        }
    }

    #[test]
    fn classify_structural_matches_label_every_category() {
        // The structured enum and the rendered label classify every category
        // consistently: classify(data).label() == classify_revert(data).
        for (hex_str, _) in parity_fixtures() {
            let bytes = hex::decode(&hex_str).unwrap_or_default();
            assert_eq!(
                RevertClass::classify(&bytes).label(),
                classify_revert(&bytes),
                "label() must agree with classify_revert for {hex_str}"
            );
        }
    }

    #[test]
    fn never_panics_on_malformed_input() {
        // A taxonomy must classify EVERY revert, even malformed ones.
        // Fuzz-ish: every prefix of a panic + a corrupted error-string must
        // yield *some* label, never panic.
        let mut rng_state = 0x1234_5678u64;
        for _ in 0..2048 {
            // poor man's LCG — no rand dep needed for a smoke property.
            rng_state = rng_state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let len = (rng_state % 80) as usize;
            let bytes: Vec<u8> = (0..len)
                .map(|i| ((rng_state >> (i % 8)) & 0xff) as u8)
                .collect();
            let _label = classify_revert(&bytes); // must not panic
        }
        // Explicit malformed cases.
        let _ = classify_revert(&[]);
        let _ = classify_revert(&[0x4e, 0x48, 0x7b, 0x71]); // Panic, no code
        let _ = classify_revert(&[0x08, 0xc3, 0x79, 0xa0]); // Error(string), no len
    }
}

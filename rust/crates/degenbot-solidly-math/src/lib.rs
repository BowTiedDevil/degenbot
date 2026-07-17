//! Solidly-stable invariant math — pure-Rust ports of the deployed-contract
//! `calc_d` / `calc_k` / `calc_f` / `calc_exact_in_stable` /
//! `calc_exact_in_volatile` / `get_y_solidly` (Solidly / Aerodrome) and the
//! Camelot-variant `f_camelot` / `k_camelot` / `get_y_camelot` pool-math
//! library functions.
//
// Lint allows: the variable names (`x0`, `y`, `a`, `b`, `k`, `xy`, `dy`)
// mirror the deployed Solidity + the Python oracle
// (`src/degenbot/calculations/{solidly_stable,camelot}.py`) notation
// (1:1 direct ports), so renaming obscures the direct-port correspondence.
#![allow(clippy::doc_markdown)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::missing_errors_doc)]
// Direct Solidity ports that mirror the deployed-contract control flow
// (index-based loops, single-function 1:1 mirrors).
#![allow(clippy::needless_range_loop)]
#![allow(clippy::too_many_lines)]
//!
//! ADR-005 slice 12f — the **pure-math port** sub-slice of slice 12 (the
//! Solidly-derived DEX port: Solidly/Aerodrome/Camelot). Mirrors
//! `degenbot-curve-math`'s shape (ADR-005 slice 11c) +
//! `degenbot-balancer-math`'s shape (slice 12e): a standalone, `pyo3`-free,
//! `tokio`-free crate depending only on `alloy::primitives`. Cross-checked
//! against the Python oracle (`src/degenbot/calculations/{solidly_stable,
//! camelot}.py`) by the frozen oracle snapshot in
//! `tests/oracle_crosscheck.rs`.
//!
//! # Module layout (mirrors the Python module split)
//!
//! - [`solidly`] — `calc_d` / `calc_k` / `calc_f` / `calc_exact_in_stable` /
//!   `calc_exact_in_volatile` / `get_y_solidly` (Solidly / Aerodrome).
//! - [`camelot`] — `f_camelot` / `k_camelot` / `get_y_camelot`.
//!
//! # Design: closure-free API
//!
//! The Python oracle's `calc_exact_in_stable` accepts injected `k_func` /
//! `get_y_func` closures so the same orchestration can route Aerodrome
//! (`calc_k` + `get_y_solidly`) or Camelot (`k_camelot` + `get_y_camelot`).
//! Rust's solver has no closure parameter: the two orchestration flavors
//! are exposed as two distinct top-level entrypoints
//! (`calc_exact_in_stable_solidly` / `calc_exact_in_stable_camelot`). This
//! avoids `dyn Fn` indirection in the hot path and keeps the API
//! `&impl Fn`-free. The two pre-baked entrypoints are the only call shapes
//! the Aerodrome and Camelot companions actually use; any future Solidly
//! variant with a new k/y pair ships as a third entrypoint.
//!
//! # Errors
//!
//! Math overflow (a value outside the `uint256` range), zero-decimals, and
//! Newton's-method non-convergence surface as [`SolidlyMathError`] variants.
//! The PyO3 binding layer translates them to the matching Python exceptions
//! (`EVMRevertError` for non-convergence + overflow reverts,
//! `DegenbotValueError` for invalid `token_in`).

/// 10**18 — the Solidly 18-decimal fixed-point unit.
pub const ONE: alloy::primitives::U256 =
    alloy::primitives::U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);

/// Maximum value of a Solidity `uint256`.
pub const MAX_UINT256: alloy::primitives::U256 = alloy::primitives::U256::MAX;

pub mod camelot;
pub mod solidly;

pub use camelot::{calc_exact_in_stable_camelot, f_camelot, get_y_camelot, k_camelot};
pub use solidly::{
    calc_d, calc_exact_in_stable_solidly, calc_exact_in_volatile, calc_exact_out_stable_solidly,
    calc_f, calc_k, get_y_solidly,
};

/// Errors raised by the Solidly / Camelot math ports.
///
/// All variants surface to Python as `EVMRevertError` (matching the Python
/// oracle's `InvalidUint256` / `EVMRevertError` for the Solidity-arithmetic
/// reverts) or as `DegenbotValueError` (for invalid `token_in`).
///
/// Mirrors `degenbot_balancer_math::BalancerMathError`: a plain `#[derive]`
/// enum with no `thiserror` dep — the PyO3 binding layer translates each
/// variant to the matching Python exception directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolidlyMathError {
    /// A computation produced a value outside the `uint256` range.
    ///
    /// Mirrors the Python oracle's `raise_if_invalid_uint256` (Solidity
    /// uses checked uint256 arithmetic — multiplication overflow reverts).
    Overflow,
    /// `token_in` was not 0 or 1.
    InvalidTokenIn,
    /// Newton's method did not converge within 255 iterations.
    ///
    /// Mirrors the Python oracle's
    /// `raise EVMRevertError(error="Failed to converge on a value for y")`.
    DidNotConverge,
}

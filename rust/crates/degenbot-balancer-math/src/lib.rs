//! Balancer V2 math — pure-Rust ports of the deployed-contract
//! `FixedPoint` / `LogExpMath` / `WeightedMath` / `StableMath` libraries.
//
// Lint allows: the variable names (`x`, `y`, `a`, `c`, `p_d`, `d_p`, `sum_`,
// `series_sum`, `z`, `num`) mirror the deployed Solidity + Python oracle
// notation (1:1 direct ports), so renaming obscures the direct-port
// correspondence.
#![allow(clippy::doc_markdown)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::missing_errors_doc)]
// Direct Solidity ports: index-based loops, conditional subtract-then-multiply
// patterns, single-function 1:1 mirrors of the deployed contract.
#![allow(clippy::needless_range_loop)]
#![allow(clippy::too_many_lines)]
//!
//! ADR-005 slice 12e — the **pure-math port** sub-slice of slice 12 (the
//! Balancer family port). Mirrors `degenbot-curve-math`'s shape
//! (ADR-005 slice 11c): a standalone, `pyo3`-free, `tokio`-free crate
//! depending only on `alloy::primitives`. Cross-checked against the Python
//! oracle (`src/degenbot/balancer/libraries/{fixed_point,log_exp_math,
//! weighted_math,stable_math}.py`) by the property tests + the frozen
//! oracle snapshot in `tests/oracle_crosscheck.rs`.
//!
//! # Module layout (mirrors the Python module split)
//!
//! - [`constants`] — `ONE`/`TWO`/`FOUR` 18-decimal units + `PowVersion`
//!   discriminant.
//! - [`fixed_point`] — `add`/`sub`/`mul_down`/`mul_up`/`div_down`/`div_up`/
//!   `pow_down`/`pow_up`/`complement`. Overflow-checked vs `MAX_UINT256`.
//! - [`log_exp_math`] — `pow`/`exp`/`ln`/`log`; the Taylor-series
//!   approximation engine driving `pow_down`/`pow_up`.
//! - [`weighted_math`] — weighted-product invariant + `out_given_in` /
//!   `in_given_out` swap math + fee helpers.
//! - [`stable_math`] — StableSwap invariant (V1 always-roundDown D_P
//!   accumulation + V2 `round_up`-param P_D accumulation — the
//!   systematic-1-wei-error guard) + `out_given_in` / `in_given_out` swap
//!   math + `_get_token_balance_given_invariant_and_all_other_balances`.
//!
//! # Variant axes (the systematic-1-wei guards)
//!
//! Two discriminants thread through this crate, both byte-for-byte
//! cross-checked vs the Python oracle:
//!
//! 1. [`PowVersion`] (`V1` general path / `V2` fast paths for
//!    `y == ONE/TWO/FOUR`) — controls `FixedPoint::pow_down`/`pow_up`
//!    fast paths. Detected from bytecode; carried on the pool.
//! 2. `round_up: bool` parameter on `StableMath::_calculate_invariant_
//!    deployed` — the V1-vs-V2 invariant accumulation. `INVARIANT_V1`
//!    (always-roundDown D_P) selects `_calculate_invariant` (the
//!    monorepo version); `INVARIANT_V2` (roundUp-parameter P_D) selects
//!    `_calculate_invariant_deployed` with `round_up = true` for swaps.
//!
//! All functions are pure: numeric inputs → numeric outputs (or
//! [`BalancerMathError`]).

pub mod constants;
pub mod fixed_point;
pub mod log_exp_math;
pub mod stable_math;
pub mod weighted_math;

pub use constants::{PowVersion, FOUR, MAX_POW_RELATIVE_ERROR, ONE, TWO};
pub use fixed_point::{add, complement, div_down, div_up, mul_down, mul_up, pow_down, pow_up, sub};
pub use log_exp_math::{exp, ln, log, pow as log_exp_pow};
pub use stable_math::{
    calc_in_given_out as stable_calc_in_given_out, calc_out_given_in as stable_calc_out_given_in,
    calculate_invariant as stable_calculate_invariant,
    calculate_invariant_deployed as stable_calculate_invariant_deployed,
    get_token_balance_given_invariant_and_all_other_balances,
};
pub use weighted_math::{
    add_swap_fee_amount, calc_in_given_out as weighted_calc_in_given_out,
    calc_out_given_in as weighted_calc_out_given_in,
    calculate_invariant as weighted_calculate_invariant, subtract_swap_fee_amount,
};

/// Errors raised by the Balancer math port — direct equivalent of the
/// Python `EVMRevertError`s the oracle raises (with `error="<TAG>"`).
///
/// The `tests/oracle_crosscheck.rs` snapshot cross-check treats every
/// `EVMRevertError` (regardless of tag) as `Expected::Err` — i.e. only
/// "the function reverts" vs "returns a value" is observable. This enum
/// preserves the tag for callers that want to distinguish (the deployed
/// contract's revert reason is observable on-chain).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalancerMathError {
    /// `ADD_OVERFLOW` — `add(a, b)` overflowed `MAX_UINT256`.
    AddOverflow,
    /// `SUB_OVERFLOW` — `sub(a, b)` with `b > a`.
    SubOverflow,
    /// `MUL_OVERFLOW` — `mul` overflowed `MAX_UINT256`.
    MulOverflow,
    /// `DIV_INTERNAL` — `div` internal scaling overflowed `MAX_UINT256`.
    DivInternal,
    /// `ZERO_DIVISION` — division by zero.
    ZeroDivision,
    /// `X_OUT_OF_BOUNDS` — `LogExpMath::pow` with `x >= 2**255` (signed
    /// range violation).
    XOutOfBounds,
    /// `Y_OUT_OF_BOUNDS` — `LogExpMath::pow` with `y >= MILD_EXPONENT_BOUND`.
    YOutOfBounds,
    /// `PRODUCT_OUT_OF_BOUNDS` — `LogExpMath::pow` with `y * ln(x)` outside
    /// `[MIN_NATURAL_EXPONENT, MAX_NATURAL_EXPONENT]`.
    ProductOutOfBounds,
    /// `Invalid exponent` / `OUT_OF_BOUNDS` — `exp` / `ln` argument out of
    /// the natural-exponent domain.
    OutOfBounds,
    /// `ZERO_INVARIANT` — `WeightedMath::calculate_invariant` produced 0.
    ZeroInvariant,
    /// `MAX_IN_RATIO` — `_calc_out_given_in` exceeded the 30% in-ratio cap.
    MaxInRatio,
    /// `MAX_OUT_RATIO` — `_calc_in_given_out` exceeded the 30% out-ratio cap.
    MaxOutRatio,
    /// `STABLE_INVARIANT_DIDNT_CONVERGE` — `_calculate_invariant` Newton
    /// iteration failed to converge in 255 steps.
    StableInvariantDidntConverge,
    /// `STABLE_GET_BALANCE_DIDNT_CONVERGE` —
    /// `_get_token_balance_given_invariant_and_all_other_balances` Newton
    /// iteration failed to converge in 255 steps.
    StableGetBalanceDidntConverge,
}

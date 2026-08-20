//! Curve StableSwap invariant math — pure-Rust ports of the Vyper contract
//! D/Y solvers and their variant forms.
//
// Lint allows: the single-char names (d, s, c, x, y) mirror the Vyper
// contract + Python oracle notation, so renaming obscures the direct-port
// correspondence; `StableSwap`/`D`/`Y` are domain terms that read better
// without backticks; the step functions return `Result` for overflow/
// non-convergence but their contracts are trivial (documented under the
// solvers that call them).
#![expect(clippy::doc_markdown)]
#![expect(clippy::many_single_char_names)]
#![expect(clippy::missing_errors_doc)]
// n_coins (U256 -> usize) truncation is intentional — coin counts are always
// <= MAX_COINS (8).
#![expect(clippy::cast_possible_truncation)]
// Direct Vyper ports: index-based loops that conditionally skip indices
// (the contract's `if _i == i` pattern) read clearer as range loops than
// iterator chains, and the solvers are intentionally single-function
// 1:1 mirrors of the contract's Newton iterations.
#![expect(clippy::needless_range_loop)]
#![expect(clippy::too_many_lines)]
//!
//! ADR-005 slice 11c (pure-math port). The counterpart of
//! [`degenbot_math::cl`] for the Curve StableSwap family: a standalone,
//! `pyo3`-free, `tokio`-free crate depending only on `alloy::primitives`.
//! Cross-checked against the Python oracle
//! (`src/degenbot/calculations/stableswap.py`) by the property tests in this
//! crate.
//!
//! # Two function levels (mirroring the Python module)
//!
//! 1. **Step functions** ([`calc_d`], [`calc_dp`], …) — a single Newton step.
//!    Variant selection is explicit via [`DVariant`], [`YVariant`],
//!    [`YDVariant`].
//! 2. **Iterative solvers** ([`stableswap_get_d`], [`stableswap_get_y`],
//!    [`stableswap_get_y_d`], [`stableswap_newton_y`]) — run the Newton
//!    iteration to convergence (Δ ≤ 1) or 255 steps (revert), selecting the
//!    step function by variant enum. Direct ports of the Vyper contract logic.
//!
//! All functions are pure: numeric inputs → numeric outputs.

/// Curve `StableSwap` xp precision: balances are stored as 18-decimal
/// fixed point and the xp form is `balance * rate_multiplier / X_PRECISION`
/// (Vyper contracts store the same value unadjusted, so the caller divides).
pub const X_PRECISION: u128 = 1_000_000_000_000_000_000;

/// Curve fee denominator: fees are fractions of 1/1e10 (4 = 0.04% = 4 bps),
/// matching the contract fee convention.
pub const FEE_DENOMINATOR: u128 = 10_000_000_000;

pub mod curve_dy_calculator;
pub mod stableswap;

pub use curve_dy_calculator::{
    calculate_dy, calculate_dy_underlying, resolve_amp, resolve_ramping_a, ARampingParams,
    CurveBasePoolPort, CurveSwapError, DyCalculationInputs, SwapStyle,
};
pub use stableswap::{
    calc_d, calc_d_variant_alpha, calc_dp, calc_dp_variant_alpha, calc_dp_variant_beta,
    calc_dp_variant_gamma, stableswap_get_d, stableswap_get_y, stableswap_get_y_d,
    stableswap_newton_y, stableswap_reduction_coefficient, CurveMathError, DVariant, YDVariant,
    YVariant,
};

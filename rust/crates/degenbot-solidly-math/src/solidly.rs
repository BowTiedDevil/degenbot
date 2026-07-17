//! Solidly / Aerodrome stable-pool invariant math — pure-Rust ports of
//! `src/degenbot/calculations/solidly_stable.py`.
//!
//! Direct 1:1 ports of the deployed Solidity `Pool.sol` math: 18-decimal
//! fixed-point arithmetic, Newton's method solver for `y`, and the
//! `calc_k` / `calc_f` / `calc_d` invariant-family helpers.
//!
//! # Inputs (`U256`-bounded)
//!
//! All inputs are bounded `uint256` (Solidity's native width). The Python
//! oracle's `raise_if_invalid_uint256` is reproduced via
//! [`crate::SolidlyMathError::Overflow`] on every multiplication that
//! Solidity would have checked.
//!
//! # `fee` representation
//!
//! The Python oracle accepts `fee: Fraction` (e.g. `Fraction(997, 1000)`
//! for a 0.3% Solidly fee — `gamma_numer=997`, `gamma_denom=1000` retained
//! post-fee fraction). Rust accepts `fee_numer` + `fee_denom` as two `U256`s
//! to avoid either pulling `num-rational` into the pure-math leaf or
//! surface-coupling the wrapper to the Python `Fraction` API. The pre-baked
//! wrapper normalizes the PyO3 `Fraction`-shaped call into two integers.

use alloy::primitives::U256;

use crate::{SolidlyMathError, ONE};

/// `D = 3*x0*y^2 + x0^3*y`, all scaled by 10^18.
///
/// Direct port of `solidly_stable.calc_d`. Used by `get_y_solidly`'s
/// Newton step.
#[must_use]
pub fn calc_d(x0: U256, y: U256) -> U256 {
    // Mirrors the Python form:
    //   (3 * x0 * (y * y // 1e18)) // 1e18
    //   + (((x0 * x0) // 1e18) * x0) // 1e18
    //
    // All inner products overflow-check on Solidity; the deployed contract
    // checks `raise_if_invalid_uint256` only on `calc_k`'s `a * b` (the
    // single widest multiply), NOT on `calc_d` / `calc_f`. The Python
    // oracle's intermediate products silently wrap modulo 2^256 here too:
    // no overflow check, mirroring Solidity unchecked arithmetic on the
    // `calc_d`/`calc_f` paths (the deployed Solidly pool wraps them under
    // `unchecked { }`).
    let three = U256::from(3u64);
    let yy = y.wrapping_mul(y) / ONE;
    let term1 = three.wrapping_mul(x0).wrapping_mul(yy) / ONE;
    let x0x0 = x0.wrapping_mul(x0) / ONE;
    let term2 = x0x0.wrapping_mul(x0) / ONE;
    term1.wrapping_add(term2)
}

/// `k = a*b` for unnormalized reserves, where `a = x*y`, `b = x^2 + y^2`
/// (reserves scaled by their decimals). Direct port of `solidly_stable.calc_k`.
///
/// # Errors
///
/// Returns [`SolidlyMathError::Overflow`] if `a * b` exceeds `MAX_UINT256`
/// (mirrors the Python `raise_if_invalid_uint256(a * b)` revert).
pub fn calc_k(
    balance_0: U256,
    balance_1: U256,
    decimals_0: U256,
    decimals_1: U256,
) -> Result<U256, SolidlyMathError> {
    if decimals_0.is_zero() || decimals_1.is_zero() {
        // Solidity would revert on division by zero — surface as overflow
        // (the deployment's revert reason is `Division or modulo by 0`).
        return Err(SolidlyMathError::Overflow);
    }
    let x = balance_0.wrapping_mul(ONE) / decimals_0;
    let y = balance_1.wrapping_mul(ONE) / decimals_1;
    let a = x.wrapping_mul(y) / ONE;
    let b = (x.wrapping_mul(x) / ONE).wrapping_add(y.wrapping_mul(y) / ONE);
    let ab = a.checked_mul(b).ok_or(SolidlyMathError::Overflow)?;
    Ok(ab / ONE)
}

/// `f(x0, y) = x0*y^3 + x0^3*y` (Solidly/Aerodrome invariant form).
///
/// Direct port of `solidly_stable.calc_f`. Used by `get_y_solidly`'s loop.
#[must_use]
pub fn calc_f(x0: U256, y: U256) -> U256 {
    let a = x0.wrapping_mul(y) / ONE;
    let b = (x0.wrapping_mul(x0) / ONE).wrapping_add(y.wrapping_mul(y) / ONE);
    a.wrapping_mul(b) / ONE
}

/// Solve for `y` in the Solidly/Aerodrome invariant `f(x0, y) >= xy`.
///
/// Newton's method on `f` against `D` (the analytic `calc_d`), bounded by
/// 255 iterations. Direct port of `solidly_stable.get_y_solidly`.
///
/// # Errors
///
/// - [`SolidlyMathError::Overflow`] if `y + 1` overflows (vanishingly
///   rare — only when y == `MAX_UINT256` is the loop's only candidate).
/// - [`SolidlyMathError::DidNotConverge`] after 255 iterations.
pub fn get_y_solidly(
    x0: U256,
    xy: U256,
    y_seed: U256,
    // Decimals are retained on the signature for API stability + the
    // Solidly `_get_y`'s deployed-contract callers that pass them through;
    // they were previously fed to a `calc_k` edge-case probe that
    // mis-scaled (the 1e18-scaled `x0`/`y_plus_one` were re-normalized by
    // `calc_k`, producing a wrong `k_info` for non-18-decimal tokens and
    // premature convergence on the up-walking branch). The Solidity-faithful
    // probe is `calc_f(x0, y+1)` — a direct invariant check that needs no
    // decimal normalization — so the decimals are now unused here.
    decimals_0: U256,
    decimals_1: U256,
) -> Result<U256, SolidlyMathError> {
    let _ = (decimals_0, decimals_1);
    let mut y = y_seed;
    for _ in 0..255 {
        let k = calc_f(x0, y);
        if k < xy {
            let dy = (xy.wrapping_sub(k))
                .wrapping_mul(ONE)
                .wrapping_div(calc_d(x0, y));
            if dy.is_zero() {
                if k == xy {
                    return Ok(y);
                }
                let y_plus_one = y
                    .checked_add(U256::from(1u64))
                    .ok_or(SolidlyMathError::Overflow)?;
                // Solidity-faithful probe: `_f(x0, y+1) > xy` (NOT a
                // decimal-normalized `calc_k`, which mis-scales when `x0`/
                // `y_plus_one` are already 1e18-scaled — the deployed-contract
                // `_get_y` uses `_f` here, not `_k`).
                if calc_f(x0, y_plus_one) > xy {
                    return Ok(y_plus_one);
                }
                y = y_plus_one;
            } else {
                y = y.wrapping_add(dy);
            }
        } else {
            let dy = (k.wrapping_sub(xy))
                .wrapping_mul(ONE)
                .wrapping_div(calc_d(x0, y));
            if dy.is_zero() {
                if k == xy {
                    return Ok(y);
                }
                let y_minus_one = y
                    .checked_sub(U256::from(1u64))
                    .ok_or(SolidlyMathError::Overflow)?;
                if calc_f(x0, y_minus_one) < xy {
                    return Ok(y);
                }
                y = y_minus_one;
            } else {
                y = y.wrapping_sub(dy);
            }
        }
    }
    Err(SolidlyMathError::DidNotConverge)
}

/// `amountOut` for an exact-input swap to a Solidly volatile pool (`x*y >= k`).
///
/// Direct port of `solidly_stable.calc_exact_in_volatile`.
///
/// # Errors
///
/// Returns [`SolidlyMathError::InvalidTokenIn`] if `token_in` is not 0 or 1.
pub fn calc_exact_in_volatile(
    amount_in: U256,
    token_in: u8,
    reserves_0: U256,
    reserves_1: U256,
    fee_numer: U256,
    fee_denom: U256,
) -> Result<U256, SolidlyMathError> {
    if token_in != 0 && token_in != 1 {
        return Err(SolidlyMathError::InvalidTokenIn);
    }
    // Mirrors the Python form:
    //   amount_in_after_fee = amount_in - amount_in * fee_numer // fee_denom
    //   reserves_a, reserves_b = (reserves_0, reserves_1) if token_in == 0 else (reserves_1, reserves_0)
    //   return (amount_in_after_fee * reserves_b) // (reserves_a + amount_in_after_fee)
    let amount_in_after_fee = amount_in.wrapping_sub(amount_in.wrapping_mul(fee_numer) / fee_denom);
    let (reserves_a, reserves_b) = if token_in == 0 {
        (reserves_0, reserves_1)
    } else {
        (reserves_1, reserves_0)
    };
    let denom = reserves_a.wrapping_add(amount_in_after_fee);
    if denom.is_zero() {
        // Solidity revert (Division or modulo by 0) — matches EVMRevertError
        // raised when `ZeroDivisionError` fires in the Python `calc_exact_in_stable`
        // path; the volatile path doesn't catch it in Python, but surfacing
        // Overflow keeps the wrapper's revert-class uniform.
        return Err(SolidlyMathError::Overflow);
    }
    Ok(amount_in_after_fee.wrapping_mul(reserves_b) / denom)
}

/// `amountOut` for an exact-input swap to a Solidly/Aerodrome stable pool
/// (`x^3*y + y^3*x >= k`). Pre-baked Solidly/Aerodrome variant: uses
/// [`calc_k`] + [`get_y_solidly`].
///
/// Direct port of `solidly_stable.calc_exact_in_stable` with `k_func=calc_k`
/// and `get_y_func=get_y_solidly`. The Camelot flavor
/// ([`crate::calc_exact_in_stable_camelot`]) uses `k_camelot` /
/// `get_y_camelot`.
///
/// # Errors
///
/// - [`SolidlyMathError::InvalidTokenIn`] if `token_in` is not 0 or 1.
/// - [`SolidlyMathError::Overflow`] on divide-by-zero (matches the Python
///   oracle's `EVMRevertError("Division by zero")` raise).
/// - Propagates [`get_y_solidly`]'s revert on non-convergence.
#[allow(clippy::too_many_arguments)]
pub fn calc_exact_in_stable_solidly(
    amount_in: U256,
    token_in: u8,
    reserves_0: U256,
    reserves_1: U256,
    decimals_0: U256,
    decimals_1: U256,
    fee_numer: U256,
    fee_denom: U256,
) -> Result<U256, SolidlyMathError> {
    if token_in != 0 && token_in != 1 {
        return Err(SolidlyMathError::InvalidTokenIn);
    }
    if decimals_0.is_zero() || decimals_1.is_zero() || fee_denom.is_zero() {
        return Err(SolidlyMathError::Overflow);
    }
    let amount_in_after_fee_pre =
        amount_in.wrapping_sub(amount_in.wrapping_mul(fee_numer) / fee_denom);

    let xy = calc_k(reserves_0, reserves_1, decimals_0, decimals_1)?;

    let scaled_reserves_0 = reserves_0.wrapping_mul(ONE) / decimals_0;
    let scaled_reserves_1 = reserves_1.wrapping_mul(ONE) / decimals_1;

    let (reserves_a, reserves_b, amount_in_after_fee, out_decimals) = if token_in == 0 {
        (
            scaled_reserves_0,
            scaled_reserves_1,
            amount_in_after_fee_pre.wrapping_mul(ONE) / decimals_0,
            decimals_1,
        )
    } else {
        (
            scaled_reserves_1,
            scaled_reserves_0,
            amount_in_after_fee_pre.wrapping_mul(ONE) / decimals_1,
            decimals_0,
        )
    };

    let y_sol = get_y_solidly(
        amount_in_after_fee.wrapping_add(reserves_a),
        xy,
        reserves_b,
        decimals_0,
        decimals_1,
    )?;
    let y = reserves_b.wrapping_sub(y_sol);
    Ok(y.wrapping_mul(out_decimals) / ONE)
}

/// `amountIn` for an exact-output swap to a Solidly / Aerodrome stable pool
/// — the inverse of [`calc_exact_in_stable_solidly`]. Given a target
/// `amount_out`, returns the `amount_in` required to produce it.
///
/// # Derivation (inversion via the invariant's symmetry)
///
/// The exact-in path fixes `x0 = reserves_a + amount_in_after_fee` and solves
/// `f(x0, y_new) = xy` for `y_new` via [`get_y_solidly`]; the output is
/// `reserves_b − y_new`. The exact-out path instead fixes the post-swap
/// `y_target = reserves_b − amount_out` and solves `f(x0, y_target) = xy` for
/// `x0`; the post-fee input is `x0 − reserves_a`.
///
/// Because `f(x0, y) = x0·y·(x0² + y²)` is **symmetric** in its two
/// arguments, solving `f(x0, y_target) = xy` for `x0` is exactly the same
/// Newton solve as [`get_y_solidly`]`(y_target, xy, seed = reserves_a)` —
/// the solver returns the second argument, which by symmetry *is* `x0`.
/// The fee is then inverted with ceiling division (the Uniswap
/// `getAmountIn` convention: `amount_in = ⌈amount_in_after_fee ·
/// fee_denom / (fee_denom − fee_numer)⌉` so the realized output is at least
/// the requested target).
///
/// # Errors
///
/// - [`SolidlyMathError::InvalidTokenIn`] if `token_in` is not 0 or 1.
/// - [`SolidlyMathError::Overflow`] on divide-by-zero (zero decimals /
///   zero fee-keep / `amount_out` not smaller than the output reserve).
/// - Propagates [`get_y_solidly`]'s revert on non-convergence.
#[allow(clippy::too_many_arguments)]
pub fn calc_exact_out_stable_solidly(
    amount_out: U256,
    token_in: u8,
    reserves_0: U256,
    reserves_1: U256,
    decimals_0: U256,
    decimals_1: U256,
    fee_numer: U256,
    fee_denom: U256,
) -> Result<U256, SolidlyMathError> {
    if token_in != 0 && token_in != 1 {
        return Err(SolidlyMathError::InvalidTokenIn);
    }
    if decimals_0.is_zero() || decimals_1.is_zero() || fee_denom.is_zero() {
        return Err(SolidlyMathError::Overflow);
    }
    // Retained post-fee fraction = fee_denom − fee_numer (e.g. 1000 − 3 =
    // 997 for a 0.3% Solidly fee). Zero (a 100% fee) is degenerate.
    let fee_keep = fee_denom.wrapping_sub(fee_numer);
    if fee_keep.is_zero() {
        return Err(SolidlyMathError::Overflow);
    }

    let xy = calc_k(reserves_0, reserves_1, decimals_0, decimals_1)?;

    let scaled_reserves_0 = reserves_0.wrapping_mul(ONE) / decimals_0;
    let scaled_reserves_1 = reserves_1.wrapping_mul(ONE) / decimals_1;

    // reserves_a = in-side scaled reserve; reserves_b = out-side scaled
    // reserve; in_decimals / out_decimals = the corresponding token decimals.
    let (reserves_a, reserves_b, in_decimals, out_decimals) = if token_in == 0 {
        (scaled_reserves_0, scaled_reserves_1, decimals_0, decimals_1)
    } else {
        (scaled_reserves_1, scaled_reserves_0, decimals_1, decimals_0)
    };

    // amount_out → 1e18-scaled → the target post-swap out-side reserve.
    let amount_out_scaled = amount_out.wrapping_mul(ONE) / out_decimals;
    if amount_out_scaled >= reserves_b {
        // Output exceeds (or equals) the available reserve — Solidity revert
        // (the Solidly `getAmountIn` reverts when `amountOut >= reserveOut`).
        return Err(SolidlyMathError::Overflow);
    }
    let y_target = reserves_b.wrapping_sub(amount_out_scaled);

    // Solve `f(x0, y_target) = xy` for x0. By the symmetry of f, this is the
    // same Newton solve as get_y_solidly(y_target, xy, seed = reserves_a)
    // — the solver returns the second-argument solution, which by symmetry
    // equals the desired x0.
    let x0_sol = get_y_solidly(y_target, xy, reserves_a, decimals_0, decimals_1)?;

    // x0 = reserves_a + amount_in_after_fee → recover the post-fee input
    // (1e18-scaled). The in-side reserve strictly increases (x0_sol >
    // reserves_a for any non-zero output), so the subtraction is well-defined
    // under wrapping.
    let amount_in_after_fee_scaled = x0_sol.wrapping_sub(reserves_a);

    // Invert the (fee, scale) chain BACKWARDS to the pre-fee native amount.
    // The forward path is:
    //   amount_in_after_fee_pre   = ceil(amount_in · fee_keep / fee_denom)
    //   amount_in_after_fee_scaled = floor(amount_in_after_fee_pre · ONE / decimals_in)
    // so the min `amount_in` with `amount_in_after_fee_scaled ≥ target_scaled`
    // is derived in two ceiling steps:
    //   M         = ceil(target_scaled · decimals_in / ONE)   (min native
    //               post-fee that scales up to ≥ target_scaled)
    //   amount_in = floor((M - 1) · fee_denom / fee_keep) + 1
    // (the min amount_in with `ceil(amount_in · fee_keep / fee_denom) ≥ M`).
    // Mirrors the Uniswap V2 `getAmountIn` rounding-up convention (a
    // ceiling at each rounding site so the realized output is at least the
    // requested amount).
    let m = amount_in_after_fee_scaled
        .wrapping_mul(in_decimals)
        .wrapping_add(ONE)
        .wrapping_sub(U256::from(1u64))
        / ONE;
    // M ≥ 1 for any non-zero target (amount_in_after_fee_scaled > 0 here).
    let m_minus_one = m.checked_sub(U256::from(1u64)).unwrap_or(U256::ZERO);
    let amount_in = m_minus_one
        .wrapping_mul(fee_denom)
        .wrapping_div(fee_keep)
        .wrapping_add(U256::from(1u64));
    Ok(amount_in)
}

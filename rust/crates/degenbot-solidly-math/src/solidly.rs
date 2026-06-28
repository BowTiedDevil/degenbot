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
    decimals_0: U256,
    decimals_1: U256,
) -> Result<U256, SolidlyMathError> {
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
                let k_info = calc_k(x0, y_plus_one, decimals_0, decimals_1)?;
                if k_info > xy {
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

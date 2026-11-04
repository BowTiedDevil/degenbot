//! Camelot-variant stable-pool invariant math — pure-Rust ports of the
//! deployed Camelot stable-pool contracts' `f()` / `k()` / `get_y` math.
//!
//! Camelot is a Solidly fork: same `k`/`D` shape but its own `f()` operation
//! ordering + a simpler convergence check in `get_y_camelot`. Direct 1:1 ports.
//!
//! # Sharing with [`crate::solidly::solidly`]
//!
//! `get_y_camelot` reuses [`crate::solidly::solidly::calc_d`] (the analytic `D` is
//! identical to Solidly's); only `f_camelot` / the convergence check differ.
//! `k_camelot` is byte-identical to [`crate::solidly::solidly::calc_k`] (separate
//! function because the Camelot contract addresses its own `k`).

use alloy::primitives::U256;

use crate::solidly::{solidly::calc_d, SolidlyMathError, ONE};

/// `f(x0, y) = x0 * (y^2 * y // 1e18) // 1e18 + (x0^2 * x0 // 1e18) * y // 1e18`.
///
/// Direct port of the Camelot contract's `f_camelot`. Same mathematical form as
/// [`crate::solidly::solidly::calc_f`] but with the deployed-Camelot contract's
/// operation-ordering (different intermediate-rounding). The contract's
/// left-associative `*`/`//` chains are expressed here as explicit
/// temporaries so the rounding ordering is byte-identical.
#[must_use]
pub fn f_camelot(x0: U256, y: U256) -> U256 {
    // Contract source (left-associative):
    //   x0 * (y * y // 10**18 * y // 10**18) // 10**18
    //   + (x0 * x0 // 10**18 * x0 // 10**18) * y // 10**18
    let yy = y.wrapping_mul(y) / ONE;
    let yyy = yy.wrapping_mul(y) / ONE;
    let left = x0.wrapping_mul(yyy) / ONE;
    let xx = x0.wrapping_mul(x0) / ONE;
    let xxx = xx.wrapping_mul(x0) / ONE;
    let right = xxx.wrapping_mul(y) / ONE;
    left.wrapping_add(right)
}

/// `k = a*b` for unnormalized reserves. Same form as
/// [`crate::solidly::solidly::calc_k`] but surfaced as a separate function
/// (the Camelot contract addresses its own `k`).
///
/// # Errors
///
/// Returns [`SolidlyMathError::Overflow`] if `a * b` exceeds `MAX_UINT256`.
pub fn k_camelot(
    balance_0: U256,
    balance_1: U256,
    decimals_0: U256,
    decimals_1: U256,
) -> Result<U256, SolidlyMathError> {
    if decimals_0.is_zero() || decimals_1.is_zero() {
        return Err(SolidlyMathError::Overflow);
    }
    let x = balance_0.wrapping_mul(ONE) / decimals_0;
    let y = balance_1.wrapping_mul(ONE) / decimals_1;
    let a = x.wrapping_mul(y) / ONE;
    let b = (x.wrapping_mul(x) / ONE).wrapping_add(y.wrapping_mul(y) / ONE);
    let ab = a.checked_mul(b).ok_or(SolidlyMathError::Overflow)?;
    Ok(ab / ONE)
}

/// Solve for `y` in the Camelot invariant using Newton's method.
///
/// Direct port of the Camelot contract's `get_y_camelot`. Uses Solidly's
/// [`calc_d`] (same `D` analytic) but Camelot's `f_camelot` + a simpler
/// convergence check (delta <= 1 in either direction). 255-iteration cap, at
/// which point it returns the last `y` (no revert).
///
/// # Errors
///
/// Only surfaces [`SolidlyMathError::Overflow`] from the inner `calc_d`
/// zero-division; the loop itself never reverts (returns the final `y`).
#[must_use]
pub fn get_y_camelot(x_0: U256, xy: U256, y_seed: U256) -> U256 {
    let mut y = y_seed;
    for _ in 0..255 {
        let y_prev = y;
        let k = f_camelot(x_0, y);
        // calc_d's zero case is impossible (we'd never get here with x0==0
        // since f_camelot would short-circuit). Wrapping div mirrors Solidity
        // unchecked arithmetic.
        let d = calc_d(x_0, y);
        if d.is_zero() {
            // A zero `D` is a division-by-zero revert on the deployed Camelot
            // contract. This leaf returns `y` unchanged instead and leaves the
            // revert mapping to the seam that drives `calc_exact_in_stable`.
            return y;
        }
        if k < xy {
            let dy = xy.wrapping_sub(k).wrapping_mul(ONE) / d;
            y = y.wrapping_add(dy);
        } else {
            let dy = k.wrapping_sub(xy).wrapping_mul(ONE) / d;
            y = y.wrapping_sub(dy);
        }
        if y > y_prev {
            if y.wrapping_sub(y_prev) <= U256::from(1u64) {
                return y;
            }
        } else if y_prev.wrapping_sub(y) <= U256::from(1u64) {
            return y;
        }
    }
    y
}

/// `amountOut` for an exact-input swap to a Camelot stable pool. Pre-baked
/// Camelot variant: uses [`k_camelot`] + [`get_y_camelot`].
///
/// The contract's exact-in-stable path with [`k_camelot`] +
/// [`get_y_camelot`]. Mirrors the Solidly orchestration
/// ([`crate::solidly::solidly::calc_exact_in_stable_solidly`]) but with the Camelot
/// invariant helpers.
///
/// # Errors
///
/// - [`SolidlyMathError::InvalidTokenIn`] if `token_in` is not 0 or 1.
/// - [`SolidlyMathError::Overflow`] on divide-by-zero.
/// - Propagates [`k_camelot`]'s revert on uint256 overflow.
#[expect(clippy::too_many_arguments)]
pub fn calc_exact_in_stable_camelot(
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

    let xy = k_camelot(reserves_0, reserves_1, decimals_0, decimals_1)?;

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

    let y_sol = get_y_camelot(amount_in_after_fee.wrapping_add(reserves_a), xy, reserves_b);
    let y = reserves_b.wrapping_sub(y_sol);
    Ok(y.wrapping_mul(out_decimals) / ONE)
}

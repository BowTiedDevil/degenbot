//! Balancer V2 `WeightedMath` — invariant + swap math + fee helpers.
//! Direct port of `src/degenbot/balancer/libraries/weighted_math.py`.
//!
//! All quantities are non-negative `U256` (balances, weights, amounts, fee
//! percentages). Overflow checks delegate to [`crate::fixed_point`] (matching
//! Python's `MAX_UINT256`-checked Solidity `SafeMath` reverts).

use alloy::primitives::U256;

use crate::constants::{PowVersion, ONE};
use crate::fixed_point::{
    complement, div_down, div_up, mul_down, mul_up, pow_down, pow_up, Result,
};

/// `_MIN_WEIGHT = 0.01e18` — minimum allowed normalized weight.
#[allow(dead_code)]
const MIN_WEIGHT: U256 = U256::from_limbs([10_000_000_000_000_000, 0, 0, 0]);

/// `_MAX_WEIGHTED_TOKENS = 100` — the largest possible pool given the
/// minimum normalized weight.
#[allow(dead_code)]
const MAX_WEIGHTED_TOKENS: u64 = 100;

/// `_MAX_IN_RATIO = 0.3e18` — swap inputs may not exceed 30% of the
/// token's balance.
const MAX_IN_RATIO: U256 = U256::from_limbs([300_000_000_000_000_000, 0, 0, 0]);

/// `_MAX_OUT_RATIO = 0.3e18` — swap outputs may not exceed 30% of the
/// token's balance.
const MAX_OUT_RATIO: U256 = U256::from_limbs([300_000_000_000_000_000, 0, 0, 0]);

// Invariant growth / shrink limits (unused by swap math; ported for fidelity).
#[allow(dead_code)]
const MAX_INVARIANT_RATIO: U256 = U256::from_limbs([3_000_000_000_000_000_000, 0, 0, 0]);
#[allow(dead_code)]
const MIN_INVARIANT_RATIO: U256 = U256::from_limbs([700_000_000_000_000_000, 0, 0, 0]);

/// Calculate the weighted-product invariant
/// `invariant = Π(balance_i ^ weight_i)`.
///
/// `version` selects the `pow_down` implementation (V1 general / V2 fast
/// paths) matching the pool's deployed `FixedPoint` library.
pub fn calculate_invariant(
    normalized_weights: &[U256],
    balances: &[U256],
    version: PowVersion,
) -> Result<U256> {
    let mut invariant = ONE;
    for i in 0..normalized_weights.len() {
        invariant = mul_down(
            invariant,
            pow_down(balances[i], normalized_weights[i], version)?,
        )?;
    }
    if invariant.is_zero() {
        return Err(crate::BalancerMathError::ZeroInvariant);
    }
    Ok(invariant)
}

/// `_calc_out_given_in(balance_in, weight_in, balance_out, weight_out, amount_in, version)`.
///
/// Compute how many tokens can be taken out of a pool if `amount_in` are
/// sent, given the current balances and weights. Rounds down overall
/// (amount out → favor the pool).
///
/// ```text
/// aO = bO * (1 - (bI / (bI + aI)) ^ (wI / wO))
/// ```
pub fn calc_out_given_in(
    balance_in: U256,
    weight_in: U256,
    balance_out: U256,
    weight_out: U256,
    amount_in: U256,
    version: PowVersion,
) -> Result<U256> {
    // Cannot exceed maximum in ratio (30% of balance_in).
    if amount_in > mul_down(balance_in, MAX_IN_RATIO)? {
        return Err(crate::BalancerMathError::MaxInRatio);
    }
    let denominator = balance_in
        .checked_add(amount_in)
        .ok_or(crate::BalancerMathError::AddOverflow)?;
    let base = div_up(balance_in, denominator)?;
    let exponent = div_down(weight_in, weight_out)?;
    let power = pow_up(base, exponent, version)?;
    mul_down(balance_out, complement(power))
}

/// `_calc_in_given_out(balance_in, weight_in, balance_out, weight_out, amount_out, version)`.
///
/// Compute how many tokens must be sent in to take `amount_out`. Rounds up
/// overall (amount in → favor the pool).
///
/// ```text
/// aI = bI * ((bO / (bO - aO)) ^ (wO / wI) - 1)
/// ```
pub fn calc_in_given_out(
    balance_in: U256,
    weight_in: U256,
    balance_out: U256,
    weight_out: U256,
    amount_out: U256,
    version: PowVersion,
) -> Result<U256> {
    // Cannot exceed maximum out ratio (30% of balance_out).
    if amount_out > mul_down(balance_out, MAX_OUT_RATIO)? {
        return Err(crate::BalancerMathError::MaxOutRatio);
    }
    let denominator = balance_out
        .checked_sub(amount_out)
        .ok_or(crate::BalancerMathError::SubOverflow)?;
    let base = div_up(balance_out, denominator)?;
    let exponent = div_up(weight_out, weight_in)?;
    let power = pow_up(base, exponent, version)?;
    // The base is >= 1 (and power rounds up), so `power >= ONE`; the
    // subtraction cannot underflow.
    let ratio = power
        .checked_sub(ONE)
        .ok_or(crate::BalancerMathError::SubOverflow)?;
    mul_up(balance_in, ratio)
}

/// `_subtract_swap_fee_amount(amount, fee_percentage)`.
///
/// Subtract the swap fee from `amount`: `amount - mul_up(amount, fee)`.
/// Rounds the fee up (favoring a higher fee deduction).
pub fn subtract_swap_fee_amount(amount: U256, fee_percentage: U256) -> Result<U256> {
    let fee_amount = mul_up(amount, fee_percentage)?;
    amount
        .checked_sub(fee_amount)
        .ok_or(crate::BalancerMathError::SubOverflow)
}

/// `_add_swap_fee_amount(amount, fee_percentage)`.
///
/// Add the swap fee on top of `amount`: `div_up(amount, ONE - fee)`.
/// Rounds the amount up (favoring a higher fee addition).
pub fn add_swap_fee_amount(amount: U256, fee_percentage: U256) -> Result<U256> {
    div_up(amount, complement(fee_percentage))
}

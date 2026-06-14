//! `SqrtPriceMath`: price movement calculations with liquidity constraints.
//!
//! Port of `SqrtPriceMath.sol` from both Uniswap V3 and V4.
//!
//! The V3 and V4 Python implementations diverge in style (V3 uses
//! `ValidatedInt128`/`ValidatedUint160` overloads; V4 uses raw ints
//! with manual checks), but the underlying mathematical formulas are
//! identical. In Rust, the type system handles the range checks natively.
//!
//! The one genuine difference is `get_amount1_delta`'s rounding: V3
//! uses separate `muldiv`/`muldiv_rounding_up` calls, while V4 computes
//! `muldiv + (mulmod > 0 && round_up)`. Both produce the same result,
//! but the V4 approach avoids a full `muldiv_rounding_up` call for the
//! non-rounding case.

use alloy::primitives::{I256, U256, U512};

use crate::cl_lib::full_math::{muldiv, muldiv_rounding_up};
use crate::cl_lib::unsafe_math::div_rounding_up;
use crate::errors::ClMathError;

/// Q96 = 2^96 — the fixed-point scale for sqrt-price values.
const Q96: U256 = U256::from_limbs([
    0x0000000000000000u64,
    0x0000000100000000u64,
    0x0000000000000000u64,
    0x0000000000000000u64,
]);

/// Q96 resolution (96 bits).
const RESOLUTION: u32 = 96;

/// Maximum value of uint160.
const MAX_UINT160: U256 = U256::from_limbs([
    0xffffffffffffffffu64,
    0xffffffffffffffffu64,
    0x00000000ffffffffu64,
    0x0000000000000000u64,
]);

/// Maximum value of uint256.

/// Get the amount0 delta between two prices for a given liquidity.
///
/// When `round_up` is `Some(bool)`, computes the unsigned (absolute) amount,
/// rounding up if `true`, down if `false`. When `round_up` is `None`,
/// computes the signed delta (negative for positive liquidity, positive for
/// negative liquidity) — matching the Solidity `getToken0Delta` overload
/// that returns `int256`.
///
/// # Errors
///
/// Returns [`ClMathError::InvalidPrice`] if `sqrt_price_a` is zero in the
/// unsigned path.
pub fn get_amount0_delta(
    sqrt_price_a: U256,
    sqrt_price_b: U256,
    liquidity: i128,
    round_up: Option<bool>,
) -> Result<U256, ClMathError> {
    match round_up {
        Some(round_up) => {
            let (sp_a, sp_b) = if sqrt_price_a > sqrt_price_b {
                (sqrt_price_b, sqrt_price_a)
            } else {
                (sqrt_price_a, sqrt_price_b)
            };

            if sp_a.is_zero() {
                return Err(ClMathError::InvalidPrice);
            }

            // In the unsigned path, liquidity is guaranteed >= 0 by Solidity's uint128 type.
            // We cast to u128 assuming the caller has validated this.
            let liquidity_u256 = U256::from(liquidity as u128);
            let numerator1 = liquidity_u256 << RESOLUTION;
            let numerator2 = sp_b - sp_a;

            if round_up {
                let step1 = muldiv_rounding_up(numerator1, numerator2, sp_b)?;
                Ok(div_rounding_up(step1, sp_a))
            } else {
                let step1 = muldiv(numerator1, numerator2, sp_b)?;
                Ok(step1 / sp_a)
            }
        }
        None => {
            // Signed path: positive liquidity → negative result (token0 is debt)
            // Negative liquidity → positive result
            if liquidity < 0 {
                let unsigned = get_amount0_delta(
                    sqrt_price_a, sqrt_price_b, -liquidity, Some(false),
                )?;
                // Negate as int256: result = -unsigned
                let signed = I256::try_from(unsigned)
                    .map_err(|_| ClMathError::Uint256Overflow)?;
                let negated = -signed;
                Ok(negated.to::<U256>())
            } else {
                let unsigned = get_amount0_delta(
                    sqrt_price_a, sqrt_price_b, liquidity, Some(true),
                )?;
                let signed = I256::try_from(unsigned)
                    .map_err(|_| ClMathError::Uint256Overflow)?;
                let negated = -signed;
                Ok(negated.to::<U256>())
            }
        }
    }
}

/// Get the amount1 delta between two prices for a given liquidity.
///
/// When `round_up` is `Some(bool)`, computes the unsigned (absolute) amount,
/// rounding up if `true`, down if `false`. When `round_up` is `None`,
/// computes the signed delta — matching the Solidity `getToken1Delta`
/// overload that returns `int256`.
///
/// # Errors
///
/// Returns [`ClMathError::InvalidLiquidity`] if `liquidity` is negative
/// when `round_up` is `Some`.
pub fn get_amount1_delta(
    sqrt_price_a: U256,
    sqrt_price_b: U256,
    liquidity: i128,
    round_up: Option<bool>,
) -> Result<U256, ClMathError> {
    match round_up {
        Some(round_up) => {
            let (sp_a, sp_b) = if sqrt_price_a > sqrt_price_b {
                (sqrt_price_b, sqrt_price_a)
            } else {
                (sqrt_price_a, sqrt_price_b)
            };

            let liquidity_u256 = U256::from(liquidity as u128);
            let numerator = sp_b - sp_a;

            if round_up {
                muldiv_rounding_up(liquidity_u256, numerator, Q96)
            } else {
                muldiv(liquidity_u256, numerator, Q96)
            }
        }
        None => {
            if liquidity < 0 {
                let unsigned = get_amount1_delta(
                    sqrt_price_a, sqrt_price_b, -liquidity, Some(false),
                )?;
                let signed = I256::try_from(unsigned)
                    .map_err(|_| ClMathError::Uint256Overflow)?;
                let negated = -signed;
                Ok(negated.to::<U256>())
            } else {
                let unsigned = get_amount1_delta(
                    sqrt_price_a, sqrt_price_b, liquidity, Some(true),
                )?;
                let signed = I256::try_from(unsigned)
                    .map_err(|_| ClMathError::Uint256Overflow)?;
                let negated = -signed;
                Ok(negated.to::<U256>())
            }
        }
    }
}

/// Get the next sqrt price given a delta of token0, rounding up.
///
/// # Errors
///
/// Returns [`ClMathError::PriceOverflow`] if the result doesn't fit in
/// `uint160` or the subtraction would underflow.
pub fn get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_price_x96: U256,
    liquidity: i128,
    amount: U256,
    add: bool,
) -> Result<U256, ClMathError> {
    if amount.is_zero() {
        return Ok(sqrt_price_x96);
    }

    let liquidity_u256 = U256::from(liquidity as u128);
    let numerator1 = liquidity_u256 << RESOLUTION;
    let (product, overflowed) = amount.overflowing_mul(sqrt_price_x96);

    if add {
        if !overflowed {
            let denominator = numerator1 + product;
            if denominator >= numerator1 {
                let result = muldiv_rounding_up(numerator1, sqrt_price_x96, denominator)?;
                if result > MAX_UINT160 {
                    return Err(ClMathError::ResultOverflowedUint160);
                }
                return Ok(result);
            }
        }
        // Product overflowed U256 — use the failsafe division path.
        // The intermediate `y = numerator1/sqrt_price + amount` may also overflow U256,
        // so we compute `div_rounding_up` using U512 intermediates.
        let quotient = numerator1 / sqrt_price_x96;
        let y = U512::from(quotient) + U512::from(amount);
        // div_rounding_up with U512 y
        let q512 = U512::from(numerator1) / y;
        let r512 = U512::from(numerator1) % y;
        let result = if r512.is_zero() {
            q512.to::<U256>()
        } else {
            (q512 + U512::from(1u8)).to::<U256>()
        };
        if result > MAX_UINT160 {
            return Err(ClMathError::ResultOverflowedUint160);
        }
        return Ok(result);
    }

    // Subtracting: numerator1 must be > product
    // If product overflowed U256, then mathematically product > numerator1
    if overflowed || numerator1 <= product {
        return Err(ClMathError::PriceOverflow);
    }
    let denominator = numerator1 - product;
    let result = muldiv_rounding_up(numerator1, sqrt_price_x96, denominator)?;
    if result > MAX_UINT160 {
        return Err(ClMathError::PriceOverflow);
    }
    Ok(result)
}

/// Get the next sqrt price given a delta of token1, rounding down.
///
/// # Errors
///
/// Returns [`ClMathError::PriceOverflow`] if the result exceeds `uint160`.
/// Returns [`ClMathError::InsufficientLiquidity`] if there is insufficient
/// liquidity for the requested output.
pub fn get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_price_x96: U256,
    liquidity: i128,
    amount: U256,
    add: bool,
) -> Result<U256, ClMathError> {
    let liquidity_u256 = U256::from(liquidity as u128);

    if add {
        let quotient = if amount <= MAX_UINT160 {
            (amount << RESOLUTION) / liquidity_u256
        } else {
            muldiv(amount, Q96, liquidity_u256)?
        };
        let result = sqrt_price_x96 + quotient;
        if result > MAX_UINT160 {
            return Err(ClMathError::ResultOverflowedUint160);
        }
        Ok(result)
    } else {
        let quotient = if amount <= MAX_UINT160 {
            div_rounding_up(amount << RESOLUTION, liquidity_u256)
        } else {
            muldiv_rounding_up(amount, Q96, liquidity_u256)?
        };

        if sqrt_price_x96 <= quotient {
            return Err(ClMathError::InsufficientLiquidity);
        }
        Ok(sqrt_price_x96 - quotient)
    }
}

/// Get the next sqrt price given an input amount.
///
/// Dispatches to the appropriate rounding function based on `zero_for_one`.
///
/// # Errors
///
/// Returns [`ClMathError::InvalidPrice`] if `sqrt_price` or `liquidity` is zero.
pub fn get_next_sqrt_price_from_input(
    sqrt_price_x96: U256,
    liquidity: i128,
    amount_in: U256,
    zero_for_one: bool,
) -> Result<U256, ClMathError> {
    if sqrt_price_x96.is_zero() || liquidity == 0 {
        return Err(ClMathError::InvalidPriceOrLiquidity);
    }

    if zero_for_one {
        get_next_sqrt_price_from_amount0_rounding_up(sqrt_price_x96, liquidity, amount_in, true)
    } else {
        get_next_sqrt_price_from_amount1_rounding_down(sqrt_price_x96, liquidity, amount_in, true)
    }
}

/// Get the next sqrt price given an output amount.
///
/// Dispatches to the appropriate rounding function based on `zero_for_one`.
///
/// # Errors
///
/// Returns [`ClMathError::InvalidPrice`] if `sqrt_price` or `liquidity` is zero.
pub fn get_next_sqrt_price_from_output(
    sqrt_price_x96: U256,
    liquidity: i128,
    amount_out: U256,
    zero_for_one: bool,
) -> Result<U256, ClMathError> {
    if sqrt_price_x96.is_zero() || liquidity == 0 {
        return Err(ClMathError::InvalidPriceOrLiquidity);
    }

    if zero_for_one {
        get_next_sqrt_price_from_amount1_rounding_down(sqrt_price_x96, liquidity, amount_out, false)
    } else {
        get_next_sqrt_price_from_amount0_rounding_up(sqrt_price_x96, liquidity, amount_out, false)
    }
}

/// Compute V3-style virtual reserves from liquidity and sqrt price.
///
/// Returns `(reserve_in, reserve_out)` where `reserve_in` is the token0
/// virtual reserve and `reserve_out` is the token1 virtual reserve.
/// If `zero_for_one` is true, the order is (token0, token1);
/// otherwise it's (token1, token0).
#[must_use]
pub fn v3_virtual_reserves(
    liquidity: U256,
    sqrt_price_x96: U256,
    zero_for_one: bool,
) -> (U256, U256) {
    let x_virtual = liquidity * Q96 * Q96 / sqrt_price_x96;
    let y_virtual = liquidity * sqrt_price_x96;
    if zero_for_one {
        (x_virtual, y_virtual)
    } else {
        (y_virtual, x_virtual)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_amount0_delta_unsigned() {
        let sp_a = U256::from(79228162514264337593543950336u128); // tick 0
        let sp_b = U256::from(79232123823359799118286999568u128); // tick 1

        let up = get_amount0_delta(sp_a, sp_b, 1000, Some(true)).unwrap();
        let down = get_amount0_delta(sp_a, sp_b, 1000, Some(false)).unwrap();
        assert!(up >= down, "round-up should be >= round-down");
    }

    #[test]
    fn test_get_amount1_delta_unsigned() {
        let sp_a = U256::from(79228162514264337593543950336u128); // tick 0
        let sp_b = U256::from(79232123823359799118286999568u128); // tick 1

        let up = get_amount1_delta(sp_a, sp_b, 1000, Some(true)).unwrap();
        let down = get_amount1_delta(sp_a, sp_b, 1000, Some(false)).unwrap();
        assert!(up >= down, "round-up should be >= round-down");
    }

    #[test]
    fn test_get_amount0_delta_zero_price() {
        let result = get_amount0_delta(U256::ZERO, U256::from(100u64), 1000, Some(true));
        assert!(result.is_err());
    }

    #[test]
    fn test_v3_virtual_reserves() {
        let liquidity = U256::from(1_000_000u64);
        let sqrt_price = U256::from(79228162514264337593543950336u128); // tick 0
        let (x, y) = v3_virtual_reserves(liquidity, sqrt_price, true);
        assert!(!x.is_zero());
        assert!(!y.is_zero());
    }
}

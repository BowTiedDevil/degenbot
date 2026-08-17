//! Aave V3 `PercentageMath` — 4-decimal fixed-point percentage arithmetic.
//!
//! Verbatim port of `src/degenbot/aave/libraries/percentage_math.py` (which is
//! itself a port of the [Aave V3 Solidity `PercentageMath`][sol] library). The
//! §4.2 (U5YIBG) parity cross-check compares these against the Python oracle
//! byte-for-byte, so the rounding + overflow semantics MUST match exactly.
//!
//! [sol]: https://github.com/aave/aave-v3-core/blob/master/contracts/protocol/libraries/math/PercentageMath.sol
//!
//! # Constants
//!
//! - `PERCENTAGE_FACTOR` — 4-decimal fixed-point (`1e4` = 100.00%).
//! - `HALF_PERCENTAGE_FACTOR` — `5e3` (the half-up tie constant).
//!
//! # Rounding
//!
//! The Python library exposes the half-up variants (`percent_mul` /
//! `percent_div`) as the primary operations + floor/ceil variants
//! (`percent_mul_floor` / `percent_mul_ceil` / `percent_div_ceil`). The GHO
//! processor's `accrue_debt_on_action` + `get_discounted_balance` use the
//! half-up `percent_mul` (the discount is `percent_mul(balance_increase,
//! discount_percent)`); the §4.2 cross-check will catch a missed variant.
//!
//! # Overflow
//!
//! The Python oracle raises `EVMRevertError` on overflow. The Rust port
//! returns [`PercentError::Overflow`]; division by zero returns
//! [`PercentError::ZeroDivision`].

use alloy::primitives::U256;
use thiserror::Error;

/// 4-decimal fixed-point precision (`1e4` = 100.00%).
pub const PERCENTAGE_FACTOR: U256 = U256::from_limbs([0x2710, 0, 0, 0]);

/// Half of [`PERCENTAGE_FACTOR`] (`5e3`).
pub const HALF_PERCENTAGE_FACTOR: U256 = U256::from_limbs([0x1388, 0, 0, 0]);

/// Errors from `PercentageMath` operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PercentError {
    /// The product `value * percentage` (or the numerator in `percent_div`)
    /// exceeds `MAX_UINT256`.
    #[error("percentage overflow: product exceeds MAX_UINT256")]
    Overflow,
    /// A division by zero was attempted (`percent_div` with `percentage == 0`).
    #[error("percentage division by zero")]
    ZeroDivision,
}

/// Multiply `value` by `percentage` (in 4-decimal basis points), rounding half
/// up to the nearest integer. Port of `percentage_math.percent_mul`.
///
/// `percentage` is in basis points: `10_000 == 100.00%`. A `percentage` of `0`
/// short-circuits to `0` (no overflow check).
///
/// # Errors
///
/// Returns [`PercentError::Overflow`] if
/// `value * percentage + HALF_PERCENTAGE_FACTOR > MAX_UINT256`.
pub fn percent_mul(value: U256, percentage: U256) -> Result<U256, PercentError> {
    if !percentage.is_zero() {
        // The Python oracle guards: `value > (MAX - HALF) // percentage` → overflow.
        let bound = (crate::MAX_UINT256 - HALF_PERCENTAGE_FACTOR) / percentage;
        if value > bound {
            return Err(PercentError::Overflow);
        }
    }
    let product = value
        .checked_mul(percentage)
        .ok_or(PercentError::Overflow)?;
    let numerator = product
        .checked_add(HALF_PERCENTAGE_FACTOR)
        .ok_or(PercentError::Overflow)?;
    Ok(numerator / PERCENTAGE_FACTOR)
}

/// Divide `value` by `percentage` (in 4-decimal basis points), rounding half
/// up. Port of `percentage_math.percent_div`.
///
/// # Errors
///
/// Returns [`PercentError::ZeroDivision`] if `percentage == 0`, or
/// [`PercentError::Overflow`] if
/// `value * PERCENTAGE_FACTOR + (percentage // 2) > MAX_UINT256`.
pub fn percent_div(value: U256, percentage: U256) -> Result<U256, PercentError> {
    if percentage.is_zero() {
        return Err(PercentError::ZeroDivision);
    }
    let bound = (crate::MAX_UINT256 - (percentage / U256::from(2u8))) / PERCENTAGE_FACTOR;
    if value > bound {
        return Err(PercentError::Overflow);
    }
    let numerator = value
        .checked_mul(PERCENTAGE_FACTOR)
        .ok_or(PercentError::Overflow)?;
    let numerator = numerator
        .checked_add(percentage / U256::from(2u8))
        .ok_or(PercentError::Overflow)?;
    Ok(numerator / percentage)
}

/// Multiply `value` by `percentage`, truncating toward zero. Port of
/// `percentage_math.percent_mul_floor`.
///
/// # Errors
///
/// Returns [`PercentError::Overflow`] if `value * percentage > MAX_UINT256`.
pub fn percent_mul_floor(value: U256, percentage: U256) -> Result<U256, PercentError> {
    if !percentage.is_zero() {
        let bound = crate::MAX_UINT256 / percentage;
        if value > bound {
            return Err(PercentError::Overflow);
        }
    }
    let product = value
        .checked_mul(percentage)
        .ok_or(PercentError::Overflow)?;
    Ok(product / PERCENTAGE_FACTOR)
}

/// Multiply `value` by `percentage`, rounding toward +∞. Port of
/// `percentage_math.percent_mul_ceil`.
///
/// # Errors
///
/// Returns [`PercentError::Overflow`] if `value * percentage > MAX_UINT256`.
pub fn percent_mul_ceil(value: U256, percentage: U256) -> Result<U256, PercentError> {
    if !percentage.is_zero() {
        let bound = crate::MAX_UINT256 / percentage;
        if value > bound {
            return Err(PercentError::Overflow);
        }
    }
    let product = value
        .checked_mul(percentage)
        .ok_or(PercentError::Overflow)?;
    let base = product / PERCENTAGE_FACTOR;
    let rem = product % PERCENTAGE_FACTOR;
    if rem.is_zero() {
        Ok(base)
    } else {
        Ok(base + U256::from(1u8))
    }
}

/// Divide `value` by `percentage`, rounding toward +∞. Port of
/// `percentage_math.percent_div_ceil`.
///
/// # Errors
///
/// Returns [`PercentError::ZeroDivision`] if `percentage == 0`, or
/// [`PercentError::Overflow`] if `value * PERCENTAGE_FACTOR > MAX_UINT256`.
pub fn percent_div_ceil(value: U256, percentage: U256) -> Result<U256, PercentError> {
    if percentage.is_zero() {
        return Err(PercentError::ZeroDivision);
    }
    let bound = crate::MAX_UINT256 / PERCENTAGE_FACTOR;
    if value > bound {
        return Err(PercentError::Overflow);
    }
    let val = value
        .checked_mul(PERCENTAGE_FACTOR)
        .ok_or(PercentError::Overflow)?;
    let base = val / percentage;
    let rem = val % percentage;
    if rem.is_zero() {
        Ok(base)
    } else {
        Ok(base + U256::from(1u8))
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_python_oracle() {
        assert_eq!(PERCENTAGE_FACTOR, U256::from(10_000u64));
        assert_eq!(HALF_PERCENTAGE_FACTOR, U256::from(5_000u64));
        assert_eq!(crate::MAX_UINT256, U256::MAX);
    }

    #[test]
    fn percent_mul_basic() {
        // 50% of 200 = 100.
        assert_eq!(
            percent_mul(U256::from(200u64), U256::from(5_000u64)).unwrap(),
            U256::from(100u64)
        );
        // 0% → 0 (no overflow check, even for large values).
        assert_eq!(
            percent_mul(crate::MAX_UINT256, U256::ZERO).unwrap(),
            U256::ZERO
        );
        // 100% → identity.
        assert_eq!(
            percent_mul(U256::from(123u64), U256::from(10_000u64)).unwrap(),
            U256::from(123u64)
        );
    }

    #[test]
    fn percent_mul_half_up_tie() {
        // percent_mul(value=1, percentage=1) = (1*1 + 5000) // 10000 = 0 (5001/10000 = 0).
        assert_eq!(
            percent_mul(U256::from(1u64), U256::from(1u64)).unwrap(),
            U256::ZERO
        );
        // percent_mul(value=2, percentage=5000) = (10000 + 5000) // 10000 = 1.
        assert_eq!(
            percent_mul(U256::from(2u64), U256::from(5_000u64)).unwrap(),
            U256::from(1u64)
        );
    }

    #[test]
    fn percent_mul_overflow() {
        // value=MAX, percentage=2 → overflow (value > (MAX - HALF) // 2).
        assert_eq!(
            percent_mul(crate::MAX_UINT256, U256::from(2u64)),
            Err(PercentError::Overflow)
        );
    }

    #[test]
    fn percent_div_basic() {
        // percent_div(100, 5000) = (100 * 10000 + 2500) // 5000 = 200.
        assert_eq!(
            percent_div(U256::from(100u64), U256::from(5_000u64)).unwrap(),
            U256::from(200u64)
        );
    }

    #[test]
    fn percent_div_zero_returns_err() {
        assert_eq!(
            percent_div(U256::from(100u64), U256::ZERO),
            Err(PercentError::ZeroDivision)
        );
    }

    #[test]
    fn percent_mul_floor_truncates() {
        // percent_mul_floor(2, 5000) = 10000 // 10000 = 1 (exact here — no remainder).
        assert_eq!(
            percent_mul_floor(U256::from(2u64), U256::from(5_000u64)).unwrap(),
            U256::from(1u64)
        );
        // percent_mul_floor(1, 5000) = 5000 // 10000 = 0 (truncates).
        assert_eq!(
            percent_mul_floor(U256::from(1u64), U256::from(5_000u64)).unwrap(),
            U256::ZERO
        );
    }

    #[test]
    fn percent_mul_ceil_rounds_up() {
        // percent_mul_ceil(1, 5000) = 5000 // 10000 + 1 (remainder) = 1.
        assert_eq!(
            percent_mul_ceil(U256::from(1u64), U256::from(5_000u64)).unwrap(),
            U256::from(1u64)
        );
        // exact: percent_mul_ceil(2, 5000) = 1 (no remainder → no round-up).
        assert_eq!(
            percent_mul_ceil(U256::from(2u64), U256::from(5_000u64)).unwrap(),
            U256::from(1u64)
        );
    }

    #[test]
    fn percent_div_ceil_rounds_up() {
        // percent_div_ceil(1, 3) = 10000 // 3 + 1 (remainder) = 3334.
        assert_eq!(
            percent_div_ceil(U256::from(1u64), U256::from(3u64)).unwrap(),
            U256::from(3_334u64)
        );
        // exact: percent_div_ceil(3, 3) = 10000 // 3 + (rem? 1) = 3334.
        // 3 * 10000 = 30000; 30000 % 3 = 0 → no round-up → 10000.
        assert_eq!(
            percent_div_ceil(U256::from(3u64), U256::from(3u64)).unwrap(),
            U256::from(10_000u64)
        );
    }

    #[test]
    fn floor_vs_ceil_disagree_on_remainder() {
        // A value with a remainder: floor truncates, ceil bumps.
        let value = U256::from(1u64);
        let pct = U256::from(5_000u64);
        let floor = percent_mul_floor(value, pct).unwrap();
        let ceil = percent_mul_ceil(value, pct).unwrap();
        assert_eq!(ceil, floor + U256::from(1u64), "ceil bumps on remainder");
    }
}

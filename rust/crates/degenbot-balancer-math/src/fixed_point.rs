//! Balancer V2 `FixedPoint` arithmetic helpers — direct port of
//! `src/degenbot/balancer/libraries/fixed_point.py`.
//!
//! 18-decimal fixed-point arithmetic with overflow checks vs
//! `MAX_UINT256` (matching the deployed Solidity `SafeMath` reverts). The
//! `MAX_UINT256 = 2**256 - 1` cap is the EVM word-size bound; Python's
//! arbitrary-precision ints would never overflow, so the Python oracle
//! raises `EVMRevertError` to mirror the deployed contract.
//!
//! `pow_down`/`pow_up` take a [`PowVersion`] that selects V1 (general
//! path only) vs V2 (fast paths for `y == ONE`, `TWO`, `FOUR`). The
//! `pow_down`/`pow_up` math routes through [`crate::log_exp_math`] for
//! the general path; the V2 fast paths short-circuit to direct `mul_down`
//! / `mul_up` (no `MAX_POW_RELATIVE_ERROR` correction).

use alloy::primitives::{I256, U256, U512};

use crate::constants::{PowVersion, FOUR, MAX_POW_RELATIVE_ERROR, ONE, TWO};
use crate::log_exp_math;
use crate::BalancerMathError;

/// `MAX_UINT256 = 2**256 - 1` — the EVM word-size bound.
pub const MAX_UINT256: U256 = U256::MAX;

/// `Result` alias for the FixedPoint port.
pub type Result<T> = std::result::Result<T, BalancerMathError>;

/// Integer division that matches Solidity's truncation-toward-zero
/// semantics.
///
/// Solidity's signed `/` truncates toward zero (matching native Rust
/// signed division). Python's `//` floors toward -infinity for negative
/// dividends, which is why the Python oracle has a `_truncated_div`
/// helper. In Rust, signed `Div` already truncates toward zero, so this
/// is just `a / b`. Kept as a named helper for the 1:1 port
/// correspondence + as the single panic-on-overflow seam.
#[inline]
pub(crate) fn truncated_div(a: U256, b: U256) -> U256 {
    // U256 is unsigned, so this is just plain floor division (no sign
    // concerns). The LogExpMath port, which has signed intermediates,
    // uses its own signed `truncated_div`.
    a / b
}

/// Solidity `add(a, b)` — checked `MAX_UINT256` overflow.
pub fn add(a: U256, b: U256) -> Result<U256> {
    a.checked_add(b).ok_or(BalancerMathError::AddOverflow)
}

/// Solidity `sub(a, b)` — checked underflow (`b > a`).
pub fn sub(a: U256, b: U256) -> Result<U256> {
    a.checked_sub(b).ok_or(BalancerMathError::SubOverflow)
}

/// `mul_down(a, b) = a * b / ONE`, rounding down.
///
/// **Note on overflow semantics:** the Python oracle's `mul_down` overflow
/// check (`if not (a == 0 or product // a == b)` — meant to mirror
/// Solidity's uint256-wraparound check) is a big-int no-op: for arbitrary
/// precision ints, `product // a == b` is always true, so the check never
/// triggers. The Rust port mirrors this — it uses `widening_mul` to compute
/// the true product (no wraparound), divides by `ONE`, and returns the
/// low 256 bits as a `U256` (truncation, mirroring Solidity's `uint256`
/// return type when the true result exceeds 2^256 — unreachable in
/// practice). The deployed Solidity contract would revert on `a * b >
/// MAX_UINT256`; the divergence is observable only for adversarial
/// overflow inputs (e.g. `2**128 * 2**128`) the bot never feeds in normal
/// swap math.
pub fn mul_down(a: U256, b: U256) -> Result<U256> {
    let product = a.widening_mul(b); // Uint<512, 8>
    let quotient = product / U512::from(ONE);
    let limbs = quotient.into_limbs();
    // Low 4 limbs — high 4 limbs truncate (wraparound match to
    // Solidity's `uint256` return type for over-large-but-rare inputs).
    Ok(U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]]))
}

/// `mul_up(a, b) = (a * b - 1) / ONE + 1`, rounding up.
///
/// Reverts on `a * b` overflow (except for the zero-product shortcut).
pub fn mul_up(a: U256, b: U256) -> Result<U256> {
    let product = a.checked_mul(b).ok_or(BalancerMathError::MulOverflow)?;
    if product.is_zero() {
        return Ok(U256::ZERO);
    }
    Ok((product - U256::from(1u64)) / ONE + U256::from(1u64))
}

/// `div_down(a, b) = a * ONE / b`, rounding down.
///
/// Reverts on `b == 0` (`ZERO_DIVISION`) and on `a * ONE` overflow
/// (`DIV_INTERNAL`).
pub fn div_down(a: U256, b: U256) -> Result<U256> {
    if b.is_zero() {
        return Err(BalancerMathError::ZeroDivision);
    }
    if a.is_zero() {
        return Ok(U256::ZERO);
    }
    let a_inflated = a.checked_mul(ONE).ok_or(BalancerMathError::DivInternal)?;
    Ok(truncated_div(a_inflated, b))
}

/// `div_up(a, b) = (a * ONE - 1) / b + 1`, rounding up.
///
/// Reverts on `b == 0` and `a * ONE` overflow.
pub fn div_up(a: U256, b: U256) -> Result<U256> {
    if b.is_zero() {
        return Err(BalancerMathError::ZeroDivision);
    }
    if a.is_zero() {
        return Ok(U256::ZERO);
    }
    let a_inflated = a.checked_mul(ONE).ok_or(BalancerMathError::DivInternal)?;
    Ok((a_inflated - U256::from(1u64)) / b + U256::from(1u64))
}

/// `pow_down(x, y) = x^y`, rounding down (the result is guaranteed not
/// to be above the true value).
///
/// `version` controls which deployed-contract path to mirror:
/// - `V1`: general path only (no fast paths). Used by `WeightedPool2Tokens`
///   and other older contracts.
/// - `V2`: fast paths for `y == ONE`, `TWO`, `FOUR` that bypass the
///   `MAX_POW_RELATIVE_ERROR` correction. Used by newer `WeightedPool`
///   contracts.
pub fn pow_down(x: U256, y: U256, version: PowVersion) -> Result<U256> {
    if version == PowVersion::V2 {
        if y == ONE {
            return Ok(x);
        }
        if y == TWO {
            return mul_down(x, x);
        }
        if y == FOUR {
            let square = mul_down(x, x)?;
            return mul_down(square, square);
        }
    }

    let raw = log_exp_math::pow(x, I256::from_raw(y))?;
    let max_error = add(mul_up(raw, MAX_POW_RELATIVE_ERROR)?, U256::from(1u64))?;
    if raw < max_error {
        return Ok(U256::ZERO);
    }
    sub(raw, max_error)
}

/// `pow_up(x, y) = x^y`, rounding up (the result is guaranteed not to be
/// below the true value).
///
/// See [`pow_down`] for the `version` fast-path semantics.
pub fn pow_up(x: U256, y: U256, version: PowVersion) -> Result<U256> {
    if version == PowVersion::V2 {
        if y == ONE {
            return Ok(x);
        }
        if y == TWO {
            return mul_up(x, x);
        }
        if y == FOUR {
            let square = mul_up(x, x)?;
            return mul_up(square, square);
        }
    }

    let raw = log_exp_math::pow(x, I256::from_raw(y))?;
    let max_error = add(mul_up(raw, MAX_POW_RELATIVE_ERROR)?, U256::from(1u64))?;
    add(raw, max_error)
}

/// `complement(x) = ONE - x` if `x < ONE`, else 0.
#[must_use]
pub fn complement(x: U256) -> U256 {
    if x < ONE {
        ONE - x
    } else {
        U256::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_overflow_returns_err() {
        let r = add(MAX_UINT256, U256::from(1u64));
        assert_eq!(r, Err(BalancerMathError::AddOverflow));
    }

    #[test]
    fn add_within_bounds() {
        assert_eq!(add(ONE, ONE), Ok(TWO));
    }

    #[test]
    fn sub_underflow_returns_err() {
        let r = sub(U256::ZERO, ONE);
        assert_eq!(r, Err(BalancerMathError::SubOverflow));
    }

    #[test]
    fn div_by_zero_returns_err() {
        assert_eq!(
            div_down(ONE, U256::ZERO),
            Err(BalancerMathError::ZeroDivision)
        );
        assert_eq!(
            div_up(ONE, U256::ZERO),
            Err(BalancerMathError::ZeroDivision)
        );
    }

    #[test]
    fn complement_zero_above_one() {
        assert_eq!(complement(ONE), U256::ZERO);
        assert_eq!(complement(ONE + U256::from(1u64)), U256::ZERO);
        assert_eq!(complement(U256::ZERO), ONE);
    }

    #[test]
    fn pow_down_v2_fast_paths_match() {
        // y == ONE -> x (V2 only); V1 general path still applies the
        // MAX_POW_RELATIVE_ERROR correction.
        let x = ONE + U256::from(1u64);
        assert_eq!(pow_down(x, ONE, PowVersion::V2).unwrap(), x);
        // y == TWO -> mul_down(x, x) (V2).
        assert_eq!(
            pow_down(x, TWO, PowVersion::V2).unwrap(),
            mul_down(x, x).unwrap()
        );
        // y == FOUR -> mul_down(square, square) (V2).
        let sq = mul_down(x, x).unwrap();
        assert_eq!(
            pow_down(x, FOUR, PowVersion::V2).unwrap(),
            mul_down(sq, sq).unwrap()
        );
    }
}

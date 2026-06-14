//! `SwapMath`: compute swap step amounts and fees.
//!
//! Port of `SwapMath.sol` from both Uniswap V3 and V4.
//!
//! # V3 vs V4 sign convention
//!
//! The **only** genuine mathematical divergence between V3 and V4 concentrated-liquidity
//! math is the sign of `amountSpecified` in `computeSwapStep`:
//!
//! | Mode       | V3 (`amount_remaining`) | V4 (`amount_remaining`) |
//! |------------|-------------------------|--------------------------|
//! | Exact IN   | `>= 0` (positive)       | `< 0` (negative)         |
//! | Exact OUT  | `< 0` (negative)        | `>= 0` (positive)        |
//!
//! For arbitrage (always exact-input mode): V3 uses **positive** values,
//! V4 uses **negative** values. This is verified in `v3_simulator.py:93`.
//!
//! The two `compute_swap_step_v3` / `compute_swap_step_v4` functions handle
//! this difference while sharing the same core logic.
//!
//! Reference: `contract_reference/uniswap/V3/UniswapV3Factory.sol` (`SwapMath`)
//! Reference: `contract_reference/uniswap/V4/PoolManager.sol` (`SwapMath`)

use alloy::primitives::{aliases::I256, U256};

use crate::cl_lib::full_math::{muldiv, muldiv_rounding_up};
use crate::cl_lib::sqrt_price_math::{
    get_amount0_delta, get_amount1_delta, get_next_sqrt_price_from_input,
    get_next_sqrt_price_from_output,
};
use crate::errors::ClMathError;

/// Maximum swap fee in pips (1,000,000 = 100%).
const MAX_SWAP_FEE: U256 = U256::from_limbs([1_000_000u64, 0, 0, 0]);

/// The result of a swap step computation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapStepResult {
    /// The sqrt price after this step.
    pub sqrt_price_next: U256,
    /// The amount of token0 or token1 input.
    pub amount_in: U256,
    /// The amount of token0 or token1 output.
    pub amount_out: U256,
    /// The fee amount charged.
    pub fee_amount: U256,
}

/// Compute the target sqrt price for a swap step.
///
/// For `zero_for_one`: the target is the maximum of `sqrt_price_next`
/// and `sqrt_price_limit`. For `one_for_zero`: the minimum.
#[must_use]
pub fn get_sqrt_price_target(
    zero_for_one: bool,
    sqrt_price_next: U256,
    sqrt_price_limit: U256,
) -> U256 {
    if zero_for_one {
        sqrt_price_next.max(sqrt_price_limit)
    } else {
        sqrt_price_next.min(sqrt_price_limit)
    }
}

/// V3 `computeSwapStep`: positive `amount_remaining` = exact input.
///
/// Returns the next sqrt price, input amount, output amount, and fee amount.
///
/// # Errors
///
/// Returns [`ClMathError::InvalidLiquidity`] if `liquidity` is negative.
pub fn compute_swap_step_v3(
    sqrt_price_current: U256,
    sqrt_price_target: U256,
    liquidity: i128,
    amount_remaining: I256,
    fee_pips: U256,
) -> Result<SwapStepResult, ClMathError> {
    if liquidity < 0 {
        return Err(ClMathError::InvalidLiquidity);
    }

    let zero_for_one = sqrt_price_current >= sqrt_price_target;
    let exact_in = amount_remaining >= I256::ZERO;
    let amount_remaining_u256 = amount_remaining.unsigned_abs();

    if exact_in {
        let amount_remaining_less_fee =
            muldiv(amount_remaining_u256, MAX_SWAP_FEE - fee_pips, MAX_SWAP_FEE)?;

        let amount_in = if zero_for_one {
            get_amount0_delta(sqrt_price_target, sqrt_price_current, liquidity, Some(true))?
        } else {
            get_amount1_delta(sqrt_price_current, sqrt_price_target, liquidity, Some(true))?
        };

        if amount_remaining_less_fee >= amount_in {
            // Target price reachable
            let sqrt_price_next = sqrt_price_target;
            let amount_out = if zero_for_one {
                get_amount1_delta(sqrt_price_next, sqrt_price_current, liquidity, Some(false))?
            } else {
                get_amount0_delta(sqrt_price_current, sqrt_price_next, liquidity, Some(false))?
            };
            let fee_amount =
                muldiv_rounding_up(amount_in, fee_pips, MAX_SWAP_FEE - fee_pips)?;
            Ok(SwapStepResult {
                sqrt_price_next,
                amount_in,
                amount_out,
                fee_amount,
            })
        } else {
            // Can't reach target — exhaust the remaining amount
            let sqrt_price_next = get_next_sqrt_price_from_input(
                sqrt_price_current,
                liquidity,
                amount_remaining_less_fee,
                zero_for_one,
            )?;
            let amount_in = if zero_for_one {
                get_amount0_delta(sqrt_price_next, sqrt_price_current, liquidity, Some(true))?
            } else {
                get_amount1_delta(sqrt_price_current, sqrt_price_next, liquidity, Some(true))?
            };
            let amount_out = if zero_for_one {
                get_amount1_delta(sqrt_price_next, sqrt_price_current, liquidity, Some(false))?
            } else {
                get_amount0_delta(sqrt_price_current, sqrt_price_next, liquidity, Some(false))?
            };
            // Fee is the remainder
            let fee_amount = amount_remaining_u256 - amount_in;
            Ok(SwapStepResult {
                sqrt_price_next,
                amount_in,
                amount_out,
                fee_amount,
            })
        }
    } else {
        // Exact out
        let amount_out = if zero_for_one {
            get_amount1_delta(sqrt_price_target, sqrt_price_current, liquidity, Some(false))?
        } else {
            get_amount0_delta(sqrt_price_current, sqrt_price_target, liquidity, Some(false))?
        };

        if amount_remaining_u256 >= amount_out {
            let sqrt_price_next = sqrt_price_target;
            let amount_in = if zero_for_one {
                get_amount0_delta(sqrt_price_next, sqrt_price_current, liquidity, Some(true))?
            } else {
                get_amount1_delta(sqrt_price_current, sqrt_price_next, liquidity, Some(true))?
            };
            let fee_amount =
                muldiv_rounding_up(amount_in, fee_pips, MAX_SWAP_FEE - fee_pips)?;
            Ok(SwapStepResult {
                sqrt_price_next,
                amount_in,
                amount_out,
                fee_amount,
            })
        } else {
            // Cap the output amount
            let amount_out = amount_remaining_u256;
            let sqrt_price_next = get_next_sqrt_price_from_output(
                sqrt_price_current,
                liquidity,
                amount_out,
                zero_for_one,
            )?;
            let amount_in = if zero_for_one {
                get_amount0_delta(sqrt_price_next, sqrt_price_current, liquidity, Some(true))?
            } else {
                get_amount1_delta(sqrt_price_current, sqrt_price_next, liquidity, Some(true))?
            };
            let fee_amount =
                muldiv_rounding_up(amount_in, fee_pips, MAX_SWAP_FEE - fee_pips)?;
            Ok(SwapStepResult {
                sqrt_price_next,
                amount_in,
                amount_out,
                fee_amount,
            })
        }
    }
}

/// V4 `computeSwapStep`: negative `amount_remaining` = exact input.
///
/// Returns the next sqrt price, input amount, output amount, and fee amount.
///
/// # Errors
///
/// Returns [`ClMathError::InvalidLiquidity`] if `liquidity` is negative.
pub fn compute_swap_step_v4(
    sqrt_price_current: U256,
    sqrt_price_target: U256,
    liquidity: i128,
    amount_remaining: I256,
    fee_pips: U256,
) -> Result<SwapStepResult, ClMathError> {
    if liquidity < 0 {
        return Err(ClMathError::InvalidLiquidity);
    }

    let zero_for_one = sqrt_price_current >= sqrt_price_target;
    let exact_in = amount_remaining < I256::ZERO;
    let amount_remaining_u256 = amount_remaining.unsigned_abs();

    if exact_in {
        let amount_remaining_less_fee =
            muldiv(amount_remaining_u256, MAX_SWAP_FEE - fee_pips, MAX_SWAP_FEE)?;

        let amount_in = if zero_for_one {
            get_amount0_delta(sqrt_price_target, sqrt_price_current, liquidity, Some(true))?
        } else {
            get_amount1_delta(sqrt_price_current, sqrt_price_target, liquidity, Some(true))?
        };

        if amount_remaining_less_fee >= amount_in {
            // Target price reachable
            let sqrt_price_next = sqrt_price_target;
            let fee_amount = if fee_pips == MAX_SWAP_FEE {
                amount_in
            } else {
                muldiv_rounding_up(amount_in, fee_pips, MAX_SWAP_FEE - fee_pips)?
            };
            let amount_out = if zero_for_one {
                get_amount1_delta(sqrt_price_next, sqrt_price_current, liquidity, Some(false))?
            } else {
                get_amount0_delta(sqrt_price_current, sqrt_price_next, liquidity, Some(false))?
            };
            Ok(SwapStepResult {
                sqrt_price_next,
                amount_in,
                amount_out,
                fee_amount,
            })
        } else {
            // Can't reach target — exhaust the remaining amount
            let amount_in = amount_remaining_less_fee;
            let sqrt_price_next = get_next_sqrt_price_from_input(
                sqrt_price_current,
                liquidity,
                amount_remaining_less_fee,
                zero_for_one,
            )?;
            let fee_amount = amount_remaining_u256 - amount_in;
            let amount_out = if zero_for_one {
                get_amount1_delta(sqrt_price_next, sqrt_price_current, liquidity, Some(false))?
            } else {
                get_amount0_delta(sqrt_price_current, sqrt_price_next, liquidity, Some(false))?
            };
            Ok(SwapStepResult {
                sqrt_price_next,
                amount_in,
                amount_out,
                fee_amount,
            })
        }
    } else {
        // Exact out
        let amount_out = if zero_for_one {
            get_amount1_delta(sqrt_price_target, sqrt_price_current, liquidity, Some(false))?
        } else {
            get_amount0_delta(sqrt_price_current, sqrt_price_target, liquidity, Some(false))?
        };

        if amount_remaining_u256 >= amount_out {
            let sqrt_price_next = sqrt_price_target;
            let amount_in = if zero_for_one {
                get_amount0_delta(sqrt_price_next, sqrt_price_current, liquidity, Some(true))?
            } else {
                get_amount1_delta(sqrt_price_current, sqrt_price_next, liquidity, Some(true))?
            };
            let fee_amount =
                muldiv_rounding_up(amount_in, fee_pips, MAX_SWAP_FEE - fee_pips)?;
            Ok(SwapStepResult {
                sqrt_price_next,
                amount_in,
                amount_out,
                fee_amount,
            })
        } else {
            // Cap the output amount
            let amount_out = amount_remaining_u256;
            let sqrt_price_next = get_next_sqrt_price_from_output(
                sqrt_price_current,
                liquidity,
                amount_out,
                zero_for_one,
            )?;
            let amount_in = if zero_for_one {
                get_amount0_delta(sqrt_price_next, sqrt_price_current, liquidity, Some(true))?
            } else {
                get_amount1_delta(sqrt_price_current, sqrt_price_next, liquidity, Some(true))?
            };
            let fee_amount =
                muldiv_rounding_up(amount_in, fee_pips, MAX_SWAP_FEE - fee_pips)?;
            Ok(SwapStepResult {
                sqrt_price_next,
                amount_in,
                amount_out,
                fee_amount,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_sqrt_price_target() {
        let sp_next = U256::from(100u64);
        let sp_limit = U256::from(200u64);

        assert_eq!(get_sqrt_price_target(true, sp_next, sp_limit), U256::from(200u64));
        assert_eq!(get_sqrt_price_target(false, sp_next, sp_limit), U256::from(100u64));
    }
}

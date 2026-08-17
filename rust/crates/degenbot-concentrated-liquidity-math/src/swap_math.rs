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

use crate::full_math::{muldiv, muldiv_rounding_up};
use crate::sqrt_price_math::{
    get_amount0_delta, get_amount1_delta, get_next_sqrt_price_from_input,
    get_next_sqrt_price_from_output,
};
use degenbot_core::errors::ClMathError;

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

/// Exact-output half of `computeSwapStep`.
///
/// Both V3 and V4 share this path verbatim — their only mathematical
/// differences live in the exact-input branch (the `amountSpecified` sign
/// convention, the `fee_pips == MAX_SWAP_FEE` guard, and the can't-reach
/// `amount_in` derivation). See the module docs for the divergence summary.
///
/// # Errors
///
/// Propagates [`ClMathError`] from the delta and price-math computations.
fn compute_swap_step_exact_out(
    zero_for_one: bool,
    sqrt_price_current: U256,
    sqrt_price_target: U256,
    liquidity: i128,
    amount_remaining_u256: U256,
    fee_pips: U256,
) -> Result<SwapStepResult, ClMathError> {
    let amount_out = if zero_for_one {
        get_amount1_delta(
            sqrt_price_target,
            sqrt_price_current,
            liquidity,
            Some(false),
        )?
    } else {
        get_amount0_delta(
            sqrt_price_current,
            sqrt_price_target,
            liquidity,
            Some(false),
        )?
    };

    if amount_remaining_u256 >= amount_out {
        let sqrt_price_next = sqrt_price_target;
        let amount_in = if zero_for_one {
            get_amount0_delta(sqrt_price_next, sqrt_price_current, liquidity, Some(true))?
        } else {
            get_amount1_delta(sqrt_price_current, sqrt_price_next, liquidity, Some(true))?
        };
        let fee_amount = muldiv_rounding_up(amount_in, fee_pips, MAX_SWAP_FEE - fee_pips)?;
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
        let fee_amount = muldiv_rounding_up(amount_in, fee_pips, MAX_SWAP_FEE - fee_pips)?;
        Ok(SwapStepResult {
            sqrt_price_next,
            amount_in,
            amount_out,
            fee_amount,
        })
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
            let fee_amount = muldiv_rounding_up(amount_in, fee_pips, MAX_SWAP_FEE - fee_pips)?;
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
        compute_swap_step_exact_out(
            zero_for_one,
            sqrt_price_current,
            sqrt_price_target,
            liquidity,
            amount_remaining_u256,
            fee_pips,
        )
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
        compute_swap_step_exact_out(
            zero_for_one,
            sqrt_price_current,
            sqrt_price_target,
            liquidity,
            amount_remaining_u256,
            fee_pips,
        )
    }
}

/// The V4 protocol fee packed-`uint24` type: a single direction's
/// protocol fee is the low 12 bits of one half (`getZeroForOneFee` =
/// `protocolFee & 0xFFF`; `getOneForZeroFee` = `protocolFee >> 12`).
///
/// `PoolManager` `Pool.State.slot0.protocolFee` packs both direction fees
/// into one `uint24`; per-direction extraction mirrors
/// `ProtocolFeeLibrary::{getZeroForOneFee, getOneForZeroFee}`.
///
/// Maximum protocol fee is 1000 pips (0.1%) per direction — enforced by
/// `ProtocolFeeLibrary::isValidProtocolFee`.
pub const MAX_PROTOCOL_FEE: u32 = 1000;

/// The denominator V4's `ProtocolFeeLibrary` uses for protocol-fee math
/// ("hundredths of a bip" == pips, 1e6 = 100%).
const PROTOCOL_FEE_PIPS_DENOMINATOR: u32 = 1_000_000;

/// Extract the zero-for-one protocol fee (low 12 bits) from a packed V4
/// `slot0.protocolFee` `uint24`. Mirrors
/// `ProtocolFeeLibrary.getZeroForOneFee` — the fee charged when `token0` is
/// the input (price goes DOWN).
///
/// `protocol_fee_packed` is the raw `uint24` as stored on-chain; this returns
/// the 12-bit zero-for-one direction fee in pips. Caller validates
/// `<= MAX_PROTOCOL_FEE` if needed (V4's `isValidProtocolFee` enforces it at
/// `setProtocolFee` time; out-of-range on-chain state is unreachable for
/// spec-bound pools).
#[must_use]
pub fn protocol_fee_zero_for_one(protocol_fee_packed: u32) -> u32 {
    protocol_fee_packed & 0xFFF
}

/// Extract the one-for-zero protocol fee (high 12 bits) from a packed V4
/// `slot0.protocolFee` `uint24`. Mirrors
/// `ProtocolFeeLibrary.getOneForZeroFee` — the fee charged when `token1` is
/// the input (price goes UP).
#[must_use]
pub fn protocol_fee_one_for_zero(protocol_fee_packed: u32) -> u32 {
    protocol_fee_packed >> 12
}

/// Port of Uniswap V4 `ProtocolFeeLibrary.calculateSwapFee` — the effective
/// swap fee `computeSwapStep` charges when a V4 pool has a non-zero protocol
/// fee set.
///
/// The protocol fee is taken from the input FIRST, then the LP fee is taken
/// from the remaining amount, so the combined fee is HIGHER than the LP fee
/// alone (but capped at 100% = `1_000_000` pips):
/// `swapFee = protocolFee + lpFee - (protocolFee * lpFee / 1_000_000)`
/// with the product term rounded DOWN (`div`, NOT `muldiv_rounding_up` —
/// matches the Solidity `assembly` in `ProtocolFeeLibrary.calculateSwapFee`).
///
/// # Arguments
/// - `direction_protocol_fee` — the per-direction 12-bit protocol fee pips
///   (`protocol_fee_zero_for_one` or `protocol_fee_one_for_zero` of the
///   pool's packed `slot0.protocolFee`).
/// - `lp_fee` — the pool's static `PoolKey.fee` (the LP fee pips, e.g. 3000
///   for 0.3%).
///
/// # Errors
/// Returns [`ClMathError::Uint256Overflow`] if `direction_protocol_fee + lp_fee`
/// does not fit a `u32` (unreachable for spec-bound pools — both are ≤ 1000,
/// sum ≤ 2000 — but the guard mirrors the Solidity `sub` underflow check).
///
/// # Examples
/// ```
/// # use degenbot_concentrated_liquidity_math::swap_math::calculate_swap_fee;
/// // The path=97 fixture pool: proto=500 pips, lpFee=3000 → 3499 pips.
/// assert_eq!(calculate_swap_fee(500, 3_000).unwrap(), 3_499);
/// // No protocol fee → swap fee == LP fee.
/// assert_eq!(calculate_swap_fee(0, 3_000).unwrap(), 3_000);
/// ```
pub fn calculate_swap_fee(direction_protocol_fee: u32, lp_fee: u32) -> Result<u32, ClMathError> {
    // `protocolFee + lpFee - (protocolFee * lpFee / 1_000_000)`, with `div`
    // (round-down) for the product term — matches the `assembly` block in
    // `ProtocolFeeLibrary.calculateSwapFee`.
    let combined = direction_protocol_fee
        .checked_add(lp_fee)
        .ok_or(ClMathError::Uint256Overflow)?;
    let product = muldiv(
        U256::from(direction_protocol_fee),
        U256::from(lp_fee),
        U256::from(PROTOCOL_FEE_PIPS_DENOMINATOR),
    )?;
    let product_u32 = u32::try_from(product).map_err(|_| ClMathError::Uint256Overflow)?;
    combined
        .checked_sub(product_u32)
        .ok_or(ClMathError::Uint256Overflow)
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod protocol_fee_tests {
    use super::*;

    /// The path=97 fixture pool (USDC/WETH V4 0.3%, block 25635461):
    /// `slot0.protocolFee = 0x1f41f4` → 500 pips each direction,
    /// `lpFee = 3000` → effective `swapFee = 3499` pips. Pinned by the
    /// offline replay in
    /// `degenbot-solvers/tests/v4_stale_state_confirmation.rs`.
    #[test]
    fn calculate_swap_fee_path97_fixture_pinned() {
        assert_eq!(calculate_swap_fee(500, 3_000).unwrap(), 3_499);
    }

    #[test]
    fn calculate_swap_fee_zero_protocol_equals_lp_fee() {
        assert_eq!(calculate_swap_fee(0, 3_000).unwrap(), 3_000);
        assert_eq!(calculate_swap_fee(0, 0).unwrap(), 0);
    }

    /// `swapFee = proto + lp - (proto*lp/1e6)`. For proto=500, lp=500: product
    /// = 250000/1e6 = 0 (floor); swapFee = 500 + 500 - 0 = 1000 pips. Pins the
    /// floor-rounding direction (a `muldiv_rounding_up` would give 1, yielding
    /// 999 — wrong).
    #[test]
    fn calculate_swap_fee_product_term_rounds_down_matching_solidity_div() {
        assert_eq!(calculate_swap_fee(500, 500).unwrap(), 1_000);
    }

    /// Max protocol fee (1000 pips) + LP fee 1e6: swapFee caps at 100%
    /// (`1_000_000` pips — the `sub(add(...), ...)` path in
    /// `ProtocolFeeLibrary.calculateSwapFee`).
    #[test]
    fn calculate_swap_fee_max_protocol_and_lp_capped_at_one_million_pips() {
        assert_eq!(calculate_swap_fee(1_000, 1_000_000).unwrap(), 1_000_000);
    }

    #[test]
    fn protocol_fee_zero_for_one_extracts_low_12_bits() {
        assert_eq!(protocol_fee_zero_for_one(0x001f_41f4), 0x1f4); // 500
        assert_eq!(protocol_fee_zero_for_one(0), 0);
        assert_eq!(protocol_fee_zero_for_one(0xFFF), 0xFFF);
    }

    #[test]
    fn protocol_fee_one_for_zero_extracts_high_12_bits() {
        assert_eq!(protocol_fee_one_for_zero(0x001f_41f4), 0x1f4); // 500
        assert_eq!(protocol_fee_one_for_zero(0), 0);
        assert_eq!(protocol_fee_one_for_zero(0xFFF_000), 0xFFF);
    }

    /// `slot0.protocolFee = 0x1f41f4` decodes to 500 pips in BOTH directions
    /// (a symmetric protocol fee) — the value observed on the path=97
    /// mainnet fixture pool.
    #[test]
    fn protocol_fee_decode_path97_fixture_symmetric_500_pips() {
        let packed = 0x001f_41f4_u32;
        assert_eq!(protocol_fee_zero_for_one(packed), 500);
        assert_eq!(protocol_fee_one_for_zero(packed), 500);
    }

    /// The UNI/MATIC V4 pool `0x929b9b09…c2d40` (the reproducible
    /// `empty`-bucket V3-V4-V3 sim-Halt pool, see the no-profit/sim-failure
    /// exploration doc). On-chain `slot0.protocolFee = 0x19019` decodes to 25
    /// pips in BOTH directions; `lpFee = 100`. The effective swap fee is
    /// therefore 125 pips (0.0125%) — NOT 200 and NOT the bare `lpFee` 100.
    ///
    /// This pins the RZKFKR fix (thread `calculateSwapFee(protocol_dir,
    /// lp_fee)` into the V4 swap-step fee): if the protocol fee were omitted
    /// the solver would model 100 pips and diverge from on-chain. The
    /// `fee_bps=2` `[solver-st]` display of this hop is a rounding artifact
    /// of `10000 - (1_000_000 - swap_fee) / 100` (125 → 2), NOT a 200-pip
    /// fee — verified against the on-chain raw `slot0`.
    #[test]
    fn calculate_swap_fee_uni_matic_929b9b09_pool_fixture_125_pips() {
        let packed = 0x0001_9019_u32;
        assert_eq!(packed, 102_425);
        assert_eq!(protocol_fee_zero_for_one(packed), 25);
        assert_eq!(protocol_fee_one_for_zero(packed), 25);
        assert_eq!(calculate_swap_fee(25, 100).unwrap(), 125);
        // fee_bps = 10000 - (1_000_000 - swap_fee) / 100 → 2 for swap_fee=125.
        let gamma = 1_000_000u32 - 125;
        assert_eq!(10000 - (gamma / 100), 2);
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::I256;
    use std::str::FromStr;

    // The integer-exact oracle values below are copied verbatim (decimal
    // strings, parsed at runtime to avoid hex-transcription errors) from the
    // Python reference suite, which is itself ported from the official
    // Uniswap V3/V4 Foundry + TypeScript tests:
    //   tests/uniswap/v4/libraries/test_swap_math.py
    //   tests/uniswap/v3/libraries/test_swap_math.py
    //
    // Both the V3 and V4 Python `compute_swap_step` implementations delegate
    // to these exact Rust functions, so the Python suite is a live oracle —
    // these Rust unit tests pin the same inputs at the source.

    /// Decimal-parse a U256 constant. Panics on malformed input (compile-time
    /// constant strings, so unrecoverable programming error).
    fn u(s: &str) -> U256 {
        U256::from_str_radix(s, 10).expect("hardcoded decimal constant")
    }
    fn i(s: &str) -> I256 {
        I256::from_str(s).expect("hardcoded decimal constant")
    }

    // Reference sqrt prices from `degenbot.uniswap.v4_libraries.constants`.
    fn sqrt_price_1_1() -> U256 {
        u("79228162514264337593543950336")
    } // 2^96
    fn sqrt_price_101_100() -> U256 {
        u("79623317895830914510639640423")
    }
    fn sqrt_price_1000_100() -> U256 {
        u("250541448375047931186413801569")
    }
    fn sqrt_price_10000_100() -> U256 {
        u("792281625142643375935439503360")
    }
    fn sqrt_price_1_4() -> U256 {
        u("39614081257132168796771975168")
    } // 2^95

    #[test]
    fn test_get_sqrt_price_target() {
        let sp_next = U256::from(100u64);
        let sp_limit = U256::from(200u64);

        assert_eq!(
            get_sqrt_price_target(true, sp_next, sp_limit),
            U256::from(200u64)
        );
        assert_eq!(
            get_sqrt_price_target(false, sp_next, sp_limit),
            U256::from(100u64)
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // V4 oracle: 9 deterministic cases ported verbatim from
    // tests/uniswap/v4/libraries/test_swap_math.py (Uniswap V4 SwapMath.t.sol).
    // V4 sign convention: `amount_remaining < 0` = exact input,
    // `amount_remaining > 0` = exact output.
    // ──────────────────────────────────────────────────────────────────────

    /// Python: `test_compute_swap_step_exact_amount_in_one_for_zero_that_gets_capped_at_price_target_in`
    #[test]
    fn v4_exact_in_ofz_capped_at_target() {
        let r = compute_swap_step_v4(
            sqrt_price_1_1(),
            sqrt_price_101_100(),
            2_000_000_000_000_000_000_i128,
            i("-1000000000000000000"),
            U256::from(600u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, u("9975124224178055"));
        assert_eq!(r.amount_out, u("9925619580021728"));
        assert_eq!(r.fee_amount, u("5988667735148"));
        assert!(r.amount_in + r.fee_amount < u("1000000000000000000"));
        assert_eq!(r.sqrt_price_next, sqrt_price_101_100());
    }

    /// Python: `test_compute_swap_step_exact_amount_out_one_for_zero_that_gets_capped_at_price_target_in`
    #[test]
    fn v4_exact_out_ofz_capped_at_target() {
        let r = compute_swap_step_v4(
            sqrt_price_1_1(),
            sqrt_price_101_100(),
            2_000_000_000_000_000_000_i128,
            i("1000000000000000000"),
            U256::from(600u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, u("9975124224178055"));
        assert_eq!(r.amount_out, u("9925619580021728"));
        assert_eq!(r.fee_amount, u("5988667735148"));
        assert!(r.amount_out < u("1000000000000000000"));
        assert_eq!(r.sqrt_price_next, sqrt_price_101_100());
    }

    /// Python: `test_compute_swap_step_exact_amount_in_one_for_zero_that_is_fully_spent_in`
    #[test]
    fn v4_exact_in_ofz_fully_spent() {
        let r = compute_swap_step_v4(
            sqrt_price_1_1(),
            sqrt_price_1000_100(),
            2_000_000_000_000_000_000_i128,
            i("-1000000000000000000"),
            U256::from(600u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, u("999400000000000000"));
        assert_eq!(r.amount_out, u("666399946655997866"));
        assert_eq!(r.fee_amount, u("600000000000000"));
        assert_eq!(r.amount_in + r.fee_amount, u("1000000000000000000"));
        assert!(r.sqrt_price_next < sqrt_price_1000_100());
    }

    /// Python: `test_compute_swap_step_exact_amount_out_one_for_zero_that_is_fully_received_in`
    #[test]
    fn v4_exact_out_ofz_fully_received() {
        let r = compute_swap_step_v4(
            sqrt_price_1_1(),
            sqrt_price_10000_100(),
            2_000_000_000_000_000_000_i128,
            i("1000000000000000000"),
            U256::from(600u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, u("2000000000000000000"));
        assert_eq!(r.fee_amount, u("1200720432259356"));
        assert_eq!(r.amount_out, u("1000000000000000000"));
        assert!(r.sqrt_price_next < sqrt_price_10000_100());
    }

    /// Python: `test_compute_swap_step_amount_out_is_capped_at_the_desired_amount_out`
    #[test]
    fn v4_amount_out_capped_at_desired() {
        let r = compute_swap_step_v4(
            u("417332158212080721273783715441582"),
            u("1452870262520218020823638996"),
            159_344_665_391_607_089_467_575_320_103_i128,
            i("1"),
            U256::from(1u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, u("1"));
        assert_eq!(r.fee_amount, u("1"));
        assert_eq!(r.amount_out, u("1")); // would be 2 if not capped
        assert_eq!(r.sqrt_price_next, u("417332158212080721273783715441581"));
    }

    /// Python: `test_compute_swap_step_target_price_of1_uses_partial_input_amount`
    #[test]
    fn v4_target_price_of1_uses_partial_input() {
        let r = compute_swap_step_v4(
            U256::from(2u64),
            U256::from(1u64),
            1_i128,
            i("-3915081100057732413702495386755767"),
            U256::from(1u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, sqrt_price_1_4());
        assert_eq!(r.fee_amount, u("39614120871253040049813"));
        assert!(r.amount_in + r.fee_amount <= u("3915081100057732413702495386755767"));
        assert_eq!(r.amount_out, U256::ZERO);
        assert_eq!(r.sqrt_price_next, U256::from(1u64));
    }

    /// Python: `test_compute_swap_step_not_entire_input_amount_taken_as_fee`
    #[test]
    fn v4_partial_input_taken_as_fee() {
        let r = compute_swap_step_v4(
            U256::from(2413u64),
            u("79887613182836312"),
            1_985_041_575_832_132_834_610_021_537_970_i128,
            i("-10"),
            U256::from(1872u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, u("9"));
        assert_eq!(r.fee_amount, u("1"));
        assert_eq!(r.amount_out, U256::ZERO);
        assert_eq!(r.sqrt_price_next, U256::from(2413u64));
    }

    /// Python: `test_compute_swap_step_zero_for_one_handles_intermediate_insufficient_liquidity_in_exact_output_case`
    #[test]
    fn v4_zfo_insufficient_liquidity_exact_out() {
        let sqrt_p = u("20282409603651670423947251286016");
        let sqrt_p_target = (sqrt_p * U256::from(11u64)) / U256::from(10u64);
        let r = compute_swap_step_v4(
            sqrt_p,
            sqrt_p_target,
            1024_i128,
            i("4"),
            U256::from(3000u64),
        )
        .unwrap();
        assert_eq!(r.amount_out, U256::ZERO);
        assert_eq!(r.sqrt_price_next, sqrt_p_target);
        assert_eq!(r.amount_in, u("26215"));
        assert_eq!(r.fee_amount, u("79"));
    }

    /// Python: `test_compute_swap_step_one_for_zero_handles_intermediate_insufficient_liquidity_in_exact_output_case`
    #[test]
    fn v4_ofz_insufficient_liquidity_exact_out() {
        let sqrt_p = u("20282409603651670423947251286016");
        let sqrt_p_target = (sqrt_p * U256::from(9u64)) / U256::from(10u64);
        let r = compute_swap_step_v4(
            sqrt_p,
            sqrt_p_target,
            1024_i128,
            i("263000"),
            U256::from(3000u64),
        )
        .unwrap();
        assert_eq!(r.amount_out, u("26214"));
        assert_eq!(r.sqrt_price_next, sqrt_p_target);
        assert_eq!(r.amount_in, u("1"));
        assert_eq!(r.fee_amount, u("1"));
    }

    // ──────────────────────────────────────────────────────────────────────
    // V3 oracle: 5 deterministic cases ported from
    // tests/uniswap/v3/libraries/test_swap_math.py (Uniswap V3 SwapMath.spec.ts).
    // V3 sign convention: `amount_remaining > 0` = exact input,
    // `amount_remaining < 0` = exact output (opposite of V4).
    // ──────────────────────────────────────────────────────────────────────

    /// Python V3: exact amount in, ofz, capped at price target.
    #[test]
    fn v3_exact_in_ofz_capped_at_target() {
        let r = compute_swap_step_v3(
            sqrt_price_1_1(),
            sqrt_price_101_100(),
            2_000_000_000_000_000_000_i128,
            i("1000000000000000000"),
            U256::from(600u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, u("9975124224178055"));
        assert_eq!(r.fee_amount, u("5988667735148"));
        assert_eq!(r.amount_out, u("9925619580021728"));
        assert!(r.amount_in + r.fee_amount < u("1000000000000000000"));
        assert_eq!(r.sqrt_price_next, sqrt_price_101_100());
    }

    /// Python V3: exact amount out, ofz, capped at price target.
    #[test]
    fn v3_exact_out_ofz_capped_at_target() {
        let r = compute_swap_step_v3(
            sqrt_price_1_1(),
            sqrt_price_101_100(),
            2_000_000_000_000_000_000_i128,
            i("-1000000000000000000"),
            U256::from(600u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, u("9975124224178055"));
        assert_eq!(r.fee_amount, u("5988667735148"));
        assert_eq!(r.amount_out, u("9925619580021728"));
        assert!(r.amount_out < u("1000000000000000000"));
        assert_eq!(r.sqrt_price_next, sqrt_price_101_100());
    }

    /// Python V3: exact amount in, ofz, fully spent.
    #[test]
    fn v3_exact_in_ofz_fully_spent() {
        let r = compute_swap_step_v3(
            sqrt_price_1_1(),
            sqrt_price_1000_100(),
            2_000_000_000_000_000_000_i128,
            i("1000000000000000000"),
            U256::from(600u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, u("999400000000000000"));
        assert_eq!(r.fee_amount, u("600000000000000"));
        assert_eq!(r.amount_out, u("666399946655997866"));
        assert_eq!(r.amount_in + r.fee_amount, u("1000000000000000000"));
        assert!(r.sqrt_price_next < sqrt_price_1000_100());
    }

    /// Python V3: exact amount out, ofz, fully received.
    #[test]
    fn v3_exact_out_ofz_fully_received() {
        let r = compute_swap_step_v3(
            sqrt_price_1_1(),
            sqrt_price_10000_100(),
            2_000_000_000_000_000_000_i128,
            i("-1000000000000000000"),
            U256::from(600u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, u("2000000000000000000"));
        assert_eq!(r.fee_amount, u("1200720432259356"));
        assert_eq!(r.amount_out, u("1000000000000000000"));
        assert!(r.sqrt_price_next < sqrt_price_10000_100());
    }

    /// Python V3: amount out is capped at the desired amount out.
    #[test]
    fn v3_amount_out_capped_at_desired() {
        let r = compute_swap_step_v3(
            u("417332158212080721273783715441582"),
            u("1452870262520218020823638996"),
            159_344_665_391_607_089_467_575_320_103_i128,
            i("-1"),
            U256::from(1u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, u("1"));
        assert_eq!(r.fee_amount, u("1"));
        assert_eq!(r.amount_out, u("1")); // would be 2 if not capped
        assert_eq!(r.sqrt_price_next, u("417332158212080721273783715441581"));
    }

    /// Python V3: target price of 1 uses partial input amount.
    #[test]
    fn v3_target_price_of1_uses_partial_input() {
        let r = compute_swap_step_v3(
            U256::from(2u64),
            U256::from(1u64),
            1_i128,
            i("3915081100057732413702495386755767"),
            U256::from(1u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, sqrt_price_1_4());
        assert_eq!(r.fee_amount, u("39614120871253040049813"));
        assert!(r.amount_in + r.fee_amount <= u("3915081100057732413702495386755767"));
        assert_eq!(r.amount_out, U256::ZERO);
        assert_eq!(r.sqrt_price_next, U256::from(1u64));
    }

    /// Python V3: entire input amount taken as fee.
    #[test]
    fn v3_entire_input_taken_as_fee() {
        let r = compute_swap_step_v3(
            U256::from(2413u64),
            u("79887613182836312"),
            1_985_041_575_832_132_834_610_021_537_970_i128,
            i("10"),
            U256::from(1872u64),
        )
        .unwrap();
        assert_eq!(r.amount_in, U256::ZERO);
        assert_eq!(r.fee_amount, u("10"));
        assert_eq!(r.amount_out, U256::ZERO);
        assert_eq!(r.sqrt_price_next, U256::from(2413u64));
    }

    /// Python V3: zero-for-one intermediate insufficient liquidity, exact out.
    #[test]
    fn v3_zfo_insufficient_liquidity_exact_out() {
        let sqrt_p = u("20282409603651670423947251286016");
        let sqrt_p_target = (sqrt_p * U256::from(11u64)) / U256::from(10u64);
        let r = compute_swap_step_v3(
            sqrt_p,
            sqrt_p_target,
            1024_i128,
            i("-4"),
            U256::from(3000u64),
        )
        .unwrap();
        assert_eq!(r.amount_out, U256::ZERO);
        assert_eq!(r.sqrt_price_next, sqrt_p_target);
        assert_eq!(r.amount_in, u("26215"));
        assert_eq!(r.fee_amount, u("79"));
    }

    /// Python V3: one-for-zero intermediate insufficient liquidity, exact out.
    #[test]
    fn v3_ofz_insufficient_liquidity_exact_out() {
        let sqrt_p = u("20282409603651670423947251286016");
        let sqrt_p_target = (sqrt_p * U256::from(9u64)) / U256::from(10u64);
        let r = compute_swap_step_v3(
            sqrt_p,
            sqrt_p_target,
            1024_i128,
            i("-263000"),
            U256::from(3000u64),
        )
        .unwrap();
        assert_eq!(r.amount_out, u("26214"));
        assert_eq!(r.sqrt_price_next, sqrt_p_target);
        assert_eq!(r.amount_in, u("1"));
        assert_eq!(r.fee_amount, u("1"));
    }

    // ──────────────────────────────────────────────────────────────────────
    // Property tests (proptest) — port the Foundry/Uniswap invariant set
    // from Python `test_fuzz_compute_swap_step`. Each function is verified
    // against its OWN contract's invariants — V3 vs V4 are never compared
    // (they port different Solidity implementations).
    // ──────────────────────────────────────────────────────────────────────
    mod proptests {
        use super::super::{compute_swap_step_v3, compute_swap_step_v4};
        use alloy::primitives::aliases::I256;
        use alloy::primitives::U256;
        use proptest::prelude::*;

        /// Smaller-than-uint160 to keep tests tractable and avoid degenerate
        /// overflow cases (matching the spirit of the Foundry fuzz harness).
        fn arb_sqrt_price() -> impl Strategy<Value = U256> {
            (1u64..u64::MAX).prop_map(U256::from)
        }
        fn arb_liquidity() -> impl Strategy<Value = i128> {
            (0i64..=i64::MAX).prop_map(i128::from)
        }
        /// V3: `amount_remaining > 0` = exact in, `< 0` = exact out.
        fn arb_v3_amount() -> impl Strategy<Value = I256> {
            (i64::MIN + 1..=i64::MAX).prop_map(|v| I256::try_from(v).unwrap())
        }
        /// V4: `amount_remaining < 0` = exact in, `> 0` = exact out.
        fn arb_v4_amount() -> impl Strategy<Value = I256> {
            (i64::MIN + 1..=i64::MAX).prop_map(|v| I256::try_from(v).unwrap())
        }
        /// `fee_pips ∈ [0, 1_000_000]` (`MAX_SWAP_FEE` allowed only for exact-in
        /// in V4; for exact-out the contracts forbid it, but the Rust helpers
        /// return an error in that case which our properties tolerate).
        fn arb_fee_pips() -> impl Strategy<Value = U256> {
            (0u32..=1_000_000).prop_map(|f| U256::from(f))
        }

        proptest! {
            /// V3 invariants (from `v3-core/contracts/libraries/SwapMath.sol`):
            /// - `amountIn + feeAmount ≤ amountRemaining` when exact-in
            /// - `amountOut ≤ -amountRemaining` when exact-out
            /// - `sqrtPriceNext` between `sqrtPriceCurrent` and `sqrtPriceTarget`
            /// - If price target reached, full amounts at-target; else input exhausted
            #[test]
            fn v3_invariants(
                sp_current in arb_sqrt_price(),
                sp_target in arb_sqrt_price(),
                liquidity in arb_liquidity(),
                amount in arb_v3_amount(),
                fee_pips in arb_fee_pips(),
            ) {
                let Ok(r) = compute_swap_step_v3(sp_current, sp_target, liquidity, amount, fee_pips) else {
                    return Ok(()); // overflow / div-by-zero — contract would revert
                };
                let exact_in = amount >= I256::ZERO;
                let abs_amount = amount.unsigned_abs();

                // amountIn + feeAmount must not overflow MAX_UINT256 (Foundry invariant).
                prop_assert!(r.amount_in <= U256::MAX - r.fee_amount, "amountIn + fee overflows");

                if exact_in {
                    // "The fee, plus the amount in, will never exceed the amount remaining."
                    prop_assert!(
                        r.amount_in + r.fee_amount <= abs_amount,
                        "exact-in: amountIn+fee {:?} > amount {:?}",
                        r.amount_in + r.fee_amount, abs_amount
                    );
                } else {
                    // Output cannot exceed the requested amount.
                    prop_assert!(
                        r.amount_out <= abs_amount,
                        "exact-out: amountOut {:?} > amount {:?}",
                        r.amount_out, abs_amount
                    );
                }

                // sqrtPriceNext is between current and target.
                if sp_target <= sp_current {
                    prop_assert!(r.sqrt_price_next <= sp_current);
                    prop_assert!(r.sqrt_price_next >= sp_target);
                } else {
                    prop_assert!(r.sqrt_price_next >= sp_current);
                    prop_assert!(r.sqrt_price_next <= sp_target);
                }

                // Zero-price-move input → zero amounts (Foundry edge case).
                if sp_current == sp_target {
                    prop_assert_eq!(r.amount_in, U256::ZERO);
                    prop_assert_eq!(r.amount_out, U256::ZERO);
                    prop_assert_eq!(r.fee_amount, U256::ZERO);
                    prop_assert_eq!(r.sqrt_price_next, sp_target);
                }

                // If target NOT reached, the full input must be consumed
                // (exact-in: amountIn+fee == amountRemaining);
                // (exact-out: amountOut == amountRemaining).
                if r.sqrt_price_next != sp_target {
                    if exact_in {
                        prop_assert_eq!(
                            r.amount_in + r.fee_amount, abs_amount,
                            "exact-in not at target: input not exhausted"
                        );
                    } else {
                        prop_assert_eq!(
                            r.amount_out, abs_amount,
                            "exact-out not at target: output not exhausted"
                        );
                    }
                }
            }

            /// V4 invariants (from `v4-core/src/libraries/SwapMath.sol`).
            /// Mirrors the V4 contract devdoc: "If the swap's amountSpecified is
            /// negative, the combined fee and input amount will never exceed
            /// the absolute value of the remaining amount."
            #[test]
            fn v4_invariants(
                sp_current in arb_sqrt_price(),
                sp_target in arb_sqrt_price(),
                liquidity in arb_liquidity(),
                amount in arb_v4_amount(),
                fee_pips in arb_fee_pips(),
            ) {
                // Exact-OUT requires fee_pips < MAX_SWAP_FEE (Solidity devdoc).
                // Skip the disallowed combation rather than assert an error.
                if amount >= I256::ZERO && fee_pips == U256::from(1_000_000u32) {
                    return Ok(());
                }
                let Ok(r) = compute_swap_step_v4(sp_current, sp_target, liquidity, amount, fee_pips) else {
                    return Ok(());
                };
                let exact_in = amount < I256::ZERO; // V4: negative = exact in
                let abs_amount = amount.unsigned_abs();

                prop_assert!(r.amount_in <= U256::MAX - r.fee_amount, "amountIn + fee overflows");

                if exact_in {
                    prop_assert!(
                        r.amount_in + r.fee_amount <= abs_amount,
                        "V4 exact-in: amountIn+fee {:?} > amount {:?}",
                        r.amount_in + r.fee_amount, abs_amount
                    );
                } else {
                    prop_assert!(
                        r.amount_out <= abs_amount,
                        "V4 exact-out: amountOut {:?} > amount {:?}",
                        r.amount_out, abs_amount
                    );
                }

                if sp_target <= sp_current {
                    prop_assert!(r.sqrt_price_next <= sp_current);
                    prop_assert!(r.sqrt_price_next >= sp_target);
                } else {
                    prop_assert!(r.sqrt_price_next >= sp_current);
                    prop_assert!(r.sqrt_price_next <= sp_target);
                }

                if sp_current == sp_target {
                    prop_assert_eq!(r.amount_in, U256::ZERO);
                    prop_assert_eq!(r.amount_out, U256::ZERO);
                    prop_assert_eq!(r.fee_amount, U256::ZERO);
                    prop_assert_eq!(r.sqrt_price_next, sp_target);
                }

                if r.sqrt_price_next != sp_target {
                    if exact_in {
                        prop_assert_eq!(
                            r.amount_in + r.fee_amount, abs_amount,
                            "V4 exact-in not at target: input not exhausted"
                        );
                    } else {
                        prop_assert_eq!(
                            r.amount_out, abs_amount,
                            "V4 exact-out not at target: output not exhausted"
                        );
                    }
                }
            }
        }
    }
}

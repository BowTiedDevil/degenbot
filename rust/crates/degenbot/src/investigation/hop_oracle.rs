//! Per-hop on-chain oracle comparison for a captured path — feed the solver's
//! hop INPUT into the tier-3-validated oracle twin for that hop's family (V2
//! `getAmountOut` / V3 `v3_simulate_swap` / V4 `v4_simulate_swap`) and return the
//! output the real pool would produce. Used to assert each hop's
//! input→output is correct, isolating a failure to composition/execution rather
//! than per-hop math.
//!
//! The twins here are the SAME functions the tier-3 revm oracles prove byte-exact
//! to the real `UniswapV2Pair` / `UniswapV3Pool` / V4 `PoolManager` bytecode
//! (see `degenbot-pools/tests/tier3_*_swap_vs_revm.rs`).

use alloy::primitives::{I256, U256};

use crate::degenbot_pools::v3_state::{
    v3_simulate_swap, SimulateSwapError, V3PoolState, V3SwapOutcome,
};
use crate::degenbot_pools::v4_state::{v4_simulate_swap, V4PoolState};

/// V2 exact-in `getAmountOut` with `gamma_num`/`fee_denom` fee (Sushi/Uni
/// `997/1000` by default). Byte-identical to the tier-3 `V2SwapOracleHarness`
/// (the tier-3 V2 oracle pins `engine_out + 1` reverting with `UniswapV2: K`).
pub fn v2_get_amount_out(
    amount_in: U256,
    reserve_in: U256,
    reserve_out: U256,
    fee: (u64, u64),
) -> U256 {
    let (gamma_num, fee_denom) = fee;
    let numerator = amount_in * U256::from(gamma_num) * reserve_out;
    let denominator = reserve_in * U256::from(fee_denom) + amount_in * U256::from(gamma_num);
    numerator / denominator
}

/// The exact-in output token amount for a CL swap outcome: the currency that is
/// TAKEN. `zero_for_one` false → token0 is output; true → token1 is output.
pub fn cl_exact_in_output(outcome: &V3SwapOutcome, zero_for_one: bool) -> U256 {
    if zero_for_one {
        outcome.amount1
    } else {
        outcome.amount0
    }
}

/// Outcome of driving a hop through its oracle twin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OracleOutcome {
    /// The output amount the real pool would return for the given input.
    Ok(U256),
    /// The swap cannot fill at that input (range exhaustion / zero liquidity /
    /// invalid state) — mirrors a live on-chain empty/halt path.
    NotComputable,
    /// A sparse-map tick-word miss: fetch word `w` and retry.
    MissingTickWord(i32),
}

impl OracleOutcome {
    /// Whether the pool could evaluate the swap (`Ok(_)`).
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Ok(_))
    }
}

/// Drive a V3 hop at `amount_in` (exact-in, positive) through `v3_simulate_swap`.
pub fn v3_hop_output(
    state: &V3PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_in: U256,
) -> OracleOutcome {
    let amount_specified = I256::try_from(amount_in).expect("v3 input fits i256");
    let limit = V3PoolState::default_sqrt_price_limit(zero_for_one);
    match v3_simulate_swap(
        state,
        fee,
        tick_spacing,
        zero_for_one,
        amount_specified,
        limit,
    ) {
        Ok(o) => OracleOutcome::Ok(cl_exact_in_output(&o, zero_for_one)),
        Err(SimulateSwapError::NotComputable) => OracleOutcome::NotComputable,
        Err(SimulateSwapError::MissingTickWord(w)) => OracleOutcome::MissingTickWord(w),
    }
}

/// Drive a V4 hop at `amount_in` (exact-in) through `v4_simulate_swap`. V4
/// exact-in passes a negative amount, so the input is negated internally.
pub fn v4_hop_output(
    state: &V4PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_in: U256,
) -> OracleOutcome {
    let amount_specified = I256::try_from(amount_in)
        .expect("v4 input fits i256")
        .checked_neg()
        .expect("negate input (V4 exact-in is negative)");
    let limit = V3PoolState::default_sqrt_price_limit(zero_for_one);
    match v4_simulate_swap(
        state,
        fee,
        tick_spacing,
        zero_for_one,
        amount_specified,
        limit,
    ) {
        Ok(o) => OracleOutcome::Ok(cl_exact_in_output(&o, zero_for_one)),
        Err(SimulateSwapError::NotComputable) => OracleOutcome::NotComputable,
        Err(SimulateSwapError::MissingTickWord(w)) => OracleOutcome::MissingTickWord(w),
    }
}

/// Drive a V4 hop at `amount_in` (exact-in) through `v4_simulate_swap`. V4
/// exact-in passes a negative amount, so the input is negated internally.
///
/// Returns the pool's max-convertible input (`input_consumed`) alongside the
/// output, so a caller can derive the CL-hop input clamp (feed
/// `exact_input_clamp_bound` the returned consumption).
pub fn v4_hop_output_consumed(
    state: &V4PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_in: U256,
) -> OracleWithConsumed {
    let amount_specified = I256::try_from(amount_in)
        .expect("v4 input fits i256")
        .checked_neg()
        .expect("negate input (V4 exact-in is negative)");
    let limit = V3PoolState::default_sqrt_price_limit(zero_for_one);
    match v4_simulate_swap(
        state,
        fee,
        tick_spacing,
        zero_for_one,
        amount_specified,
        limit,
    ) {
        Ok(o) => OracleWithConsumed {
            outcome: OracleOutcome::Ok(cl_exact_in_output(&o, zero_for_one)),
            input_consumed: o.input_consumed,
        },
        Err(SimulateSwapError::NotComputable) => OracleWithConsumed {
            outcome: OracleOutcome::NotComputable,
            input_consumed: U256::ZERO,
        },
        Err(SimulateSwapError::MissingTickWord(w)) => OracleWithConsumed {
            outcome: OracleOutcome::MissingTickWord(w),
            input_consumed: U256::ZERO,
        },
    }
}

/// An [`OracleOutcome`] plus the pool's max-convertible input for the queried
/// swap (the `input_consumed` the oracle twin reports), so the caller can
/// derive the CL-hop input clamp without re-querying.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleWithConsumed {
    /// The output / error classification (see [`OracleOutcome`]).
    pub outcome: OracleOutcome,
    /// The gross input-token amount the swap would actually convert (the
    /// pool's max-convertible input for this `amount_in`, in input units).
    pub input_consumed: U256,
}

/// Express a [`OracleOutcome`] as a comparison string for a solver's recorded
/// hop output.
pub fn display_check(outcome: &OracleOutcome, solver_out: U256) -> String {
    match outcome {
        OracleOutcome::Ok(o) => {
            format!(
                "oracle_out={o} solver_out={solver_out} -> {}",
                *o == solver_out
            )
        }
        OracleOutcome::NotComputable => "oracle=NotComputable (swap cannot fill)".to_string(),
        OracleOutcome::MissingTickWord(w) => format!("oracle=MissingTickWord({w})"),
    }
}

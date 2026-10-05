//! The pool-handle ops cluster (ADR-006 D4): the calc / simulate / encode
//! surface `PyLiquidityPool` drives, moved out of the PyO3 shell.
//!
//! Pool handles hold `state_arc + pool_id` (no `Bot`), so these ops are free
//! functions over the shared [`StateLock<BotState>`]: each acquires its guard
//! at [`LockSite::Orchestrator`] (the shell-driven facade acquisitions) and
//! returns a typed result — the PyO3 shell keeps only Python argument
//! extraction and `PyErr` mapping (the message vocabulary lives in the
//! shell's `bot::errmap`). The staged fetch choreography runs its word
//! fetches with NO state lock held; the stored fetcher re-enters Python via
//! `Python::attach`, so the shell drives a whole op inside one `py.detach`
//! (the GIL/`BotState` inversion discipline — guards never held across a
//! Python call).

use std::sync::Arc;

use alloy::primitives::{Address, I256, U256};
use degenbot_pools::curve_data_provider::CurveDataProvider;
use degenbot_substrate::state_lock::{LockSite, StateLock};
use degenbot_substrate::swap_simulation::{
    simulate_balancer_pair_in_given_out, simulate_balancer_pair_out, Caveats, OverrideSwap,
    SwapOutcome, SwapRead, SwapRequest, UnsupportedOverrideFamily,
};
use degenbot_substrate::{BotState, EncodeSwapError};
use degenbot_uniswap::v2_encoding::EncodedCall;

use crate::bot_core::V3SwapOutcome;

/// Typed failure from the swap calc ops — the shell maps each variant to its
/// byte-identical historical Python surface (`bot::errmap`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapOpError {
    /// The request amount did not fit the signed user-perspective delta, or
    /// the constant-product math overflowed a `uint256` intermediate — one
    /// historical `ValueError` surface (the on-chain `getAmountOut` SafeMath
    /// revert parity).
    AmountOverflow,
    /// The pool id is not registered.
    UnknownPool {
        /// The requested pool id.
        pool_id: u64,
    },
    /// The registered family has no implementation for the requested op.
    UnsupportedFamily {
        /// The requested pool id.
        pool_id: u64,
        /// The registered family tag.
        family: &'static str,
    },
}

/// The CL 5-tuple payload the staged fetch-sim seams surface — the legacy
/// `(amount0, amount1, sqrt_price_x96, liquidity, tick)` tuple fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClSwapPayload {
    /// Absolute token0 amount moved.
    pub amount0: U256,
    /// Absolute token1 amount moved.
    pub amount1: U256,
    /// Final `sqrtPriceX96` after the walk.
    pub sqrt_price_x96: U256,
    /// Final active liquidity.
    pub liquidity: u128,
    /// Final tick.
    pub tick: i32,
}

/// Typed result of the staged fetch+retry swap sims. The shell maps each
/// variant per-method: `Computed` → the 5-tuple, `HookedPool` → the archived
/// approximation exception, `NotComputable` → the legacy bare `None`,
/// `UnsupportedFamily` → the exact-output family-gap error (the exact-output
/// seam) or `None` (the exact-input seam).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagedSwapOutcome {
    /// The swap computed; payload per the legacy 5-tuple.
    Computed(ClSwapPayload),
    /// A V4 amount-modifying hook invalidates the standard math — the
    /// approximate deltas ride along for the archived exception text.
    HookedPool {
        /// Taken FROM the user (negative, user-perspective delta).
        consumed: I256,
        /// Given TO the user (unsigned magnitude).
        delivered: U256,
    },
    /// Staging failed, the walk was not computable, or the payload is a
    /// constant-product family the CL 5-tuple seam cannot represent — the
    /// legacy bare-`None` contract.
    NotComputable,
    /// A non-CL family was asked for the CL 5-tuple seam.
    UnsupportedFamily {
        /// The requested pool id.
        pool_id: u64,
        /// The registered family tag.
        family: &'static str,
    },
}

/// The registered family tag for `pool_id` (`None` = not registered).
#[must_use]
pub fn pool_family(state: &StateLock<BotState>, pool_id: u64) -> Option<&'static str> {
    state.read_at(LockSite::Orchestrator).pool_family(pool_id)
}

/// Clone-out the stored `CurveDataProvider` (if any) for `pool_id`'s Curve
/// state, releasing the read guard before a (potentially re-entrant) provider
/// call.
#[must_use]
pub fn curve_data_provider(
    state: &StateLock<BotState>,
    pool_id: u64,
) -> Option<Arc<dyn CurveDataProvider>> {
    let guard = state.read_at(LockSite::Orchestrator);
    guard.get_curve_pool(pool_id)?.data_provider.clone()
}

/// Stage the missing bitmap words a CL swap needs — the bounded 3-pass loop
/// the fetch-sim callers run before entering the disarmed sim: discover (a
/// collect-only transient walk under a READ guard), stage the batch under ONE
/// short write, fetch lock-free (no state lock held; the stored fetcher
/// re-enters Python itself), install under short writes with the fingerprint
/// re-check. A raced install (the pump wrote this pool mid-batch) retries the
/// whole pass. Returns `false` when the id is not registered or the retries
/// are exhausted; `true` when the sim body can run with miss recovery
/// disarmed.
#[must_use]
pub fn stage_missing_words(
    state: &StateLock<BotState>,
    pool_id: u64,
    block: u64,
    request: &SwapRequest,
) -> bool {
    for pass in 0..3u8 {
        let Some(missing) = state
            .read_at(LockSite::Orchestrator)
            .swap_missing_words(block, pool_id, request)
        else {
            // Not registered: the sim body's disarm contract never fetches.
            return false;
        };
        if missing.is_empty() {
            return true;
        }
        // Stage the whole batch under ONE short write (the per-word
        // fingerprints gate the installs individually).
        let Some(staged) = (|| {
            let mut guard = state.write_at(LockSite::Orchestrator);
            missing
                .iter()
                .map(|word| guard.stage_word_fetch_by_pool_id(pool_id, *word, block, pass > 0))
                .collect::<Option<Vec<_>>>()
        })() else {
            return false;
        };
        // Fetches: NO state lock held.
        let mut fetched = Vec::new();
        for staged_word in &staged {
            match staged_word.fetch() {
                Ok(f) => fetched.push(f),
                Err(_) => return false,
            }
        }
        // Installs: short writes, fingerprint-gated. Any Raced means the
        // pump wrote this pool mid-batch: retry the whole pass.
        let raced = (|| {
            let mut guard = state.write_at(LockSite::Orchestrator);
            staged
                .iter()
                .zip(fetched.iter())
                .any(|(staged_word, fetched_word)| {
                    matches!(
                        guard.install_word_fetch(staged_word, fetched_word),
                        degenbot_substrate::InstallWordOutcome::Raced
                    )
                })
        })();
        if !raced {
            return true;
        }
    }
    false
}

/// Exact-input `calculate_tokens_out` — the disarmed sim at the legacy block
/// sentinel `0` with the two error classes kept distinct: a constant-product
/// `uint256` intermediate overflow is [`SwapOpError::AmountOverflow`] (the
/// on-chain-revert parity `ValueError`), while a sparse-map miss recovery
/// failure stays the no-raise zero contract.
///
/// # Errors
/// [`SwapOpError`] per the typed failure vocabulary.
pub fn calculate_tokens_out(
    state: &StateLock<BotState>,
    pool_id: u64,
    zero_for_one: bool,
    amount_in: U256,
) -> Result<U256, SwapOpError> {
    let amount_specified = -I256::try_from(amount_in).map_err(|_| SwapOpError::AmountOverflow)?;
    let request = SwapRequest {
        zero_for_one,
        amount_specified,
        sqrt_price_limit: None,
    };
    let read = state
        .write_at(LockSite::Orchestrator)
        .swap_simulation_disarmed(0, pool_id, &request);
    match read {
        SwapRead::Computed(outcome) => Ok(outcome.delivered_unsigned()),
        SwapRead::NotComputable => Err(SwapOpError::AmountOverflow),
        SwapRead::UnknownPool { pool_id } => Err(SwapOpError::UnknownPool { pool_id }),
        SwapRead::UnsupportedFamily { pool_id, family } => {
            Err(SwapOpError::UnsupportedFamily { pool_id, family })
        }
        // Miss recovery failed: the no-raise contract keeps the zero.
        SwapRead::FetchFailed { .. } | SwapRead::FetchExhausted { .. } => Ok(U256::ZERO),
    }
}

/// Exact-output `calculate_tokens_in` — required input = |consumed|. The
/// legacy silent-0 contract holds for every non-computed read except the
/// typed family gap (a registered family with no exact-output path).
///
/// # Errors
/// [`SwapOpError::AmountOverflow`] on the request sign conversion,
/// [`SwapOpError::UnsupportedFamily`] for the family gap.
pub fn calculate_tokens_in(
    state: &StateLock<BotState>,
    pool_id: u64,
    zero_for_one: bool,
    amount_out: U256,
) -> Result<U256, SwapOpError> {
    let request = SwapRequest {
        zero_for_one,
        amount_specified: I256::try_from(amount_out).map_err(|_| SwapOpError::AmountOverflow)?,
        sqrt_price_limit: None,
    };
    let read = state
        .write_at(LockSite::Orchestrator)
        .swap_simulation_disarmed(0, pool_id, &request);
    match read {
        SwapRead::Computed(outcome) => {
            let consumed = match &outcome {
                SwapOutcome::V2(o) => o.consumed,
                SwapOutcome::V3(o) | SwapOutcome::V4(o) => o.consumed,
            };
            Ok((-consumed).into_raw())
        }
        SwapRead::UnsupportedFamily { pool_id, family } => {
            Err(SwapOpError::UnsupportedFamily { pool_id, family })
        }
        // Legacy silent-0: zero amount, unknown pool, not-computable, and
        // miss-recovery failures all keep the zero sentinel (ADR-037 note:
        // preserved until the Python tail task).
        _ => Ok(U256::ZERO),
    }
}

/// Fetch+retry exact-input `calculate_tokens_out` — stages the missing words
/// first (bounded passes), then runs the disarmed sim at `block`. Every
/// non-computed outcome keeps the legacy zero sentinel.
///
/// # Errors
/// [`SwapOpError::AmountOverflow`] on the request sign conversion.
pub fn calculate_tokens_out_with_fetch(
    state: &StateLock<BotState>,
    pool_id: u64,
    zero_for_one: bool,
    amount_in: U256,
    block: u64,
) -> Result<U256, SwapOpError> {
    let amount_specified = -I256::try_from(amount_in).map_err(|_| SwapOpError::AmountOverflow)?;
    let request = SwapRequest {
        zero_for_one,
        amount_specified,
        sqrt_price_limit: None,
    };
    if !stage_missing_words(state, pool_id, block, &request) {
        return Ok(U256::ZERO);
    }
    let read = state
        .write_at(LockSite::Orchestrator)
        .swap_simulation_disarmed(block, pool_id, &request);
    Ok(match read {
        SwapRead::Computed(outcome) => outcome.delivered_unsigned(),
        _ => U256::ZERO,
    })
}

/// Exact-input swap over an explicit token pair (N-token weighted pools).
/// `None` = not registered, indices invalid, ratio breach, or a `uint256`
/// intermediate overflow (the shell maps every `None` to the same
/// on-chain-revert parity `ValueError`).
#[must_use]
pub fn calculate_tokens_out_for_pair(
    state: &StateLock<BotState>,
    pool_id: u64,
    index_in: usize,
    index_out: usize,
    amount_in: U256,
    override_balances: Option<&[U256]>,
    override_scaling_factors: Option<&[U256]>,
) -> Option<U256> {
    let guard = state.read_at(LockSite::Orchestrator);
    simulate_balancer_pair_out(
        &guard,
        pool_id,
        index_in,
        index_out,
        amount_in,
        override_balances,
        override_scaling_factors,
    )
}

/// Exact-output over an explicit token pair (N-token weighted pools) — the
/// `GIVEN_OUT` arm of [`calculate_tokens_out_for_pair`].
#[must_use]
pub fn calculate_tokens_in_for_pair(
    state: &StateLock<BotState>,
    pool_id: u64,
    index_in: usize,
    index_out: usize,
    amount_out: U256,
    override_balances: Option<&[U256]>,
    override_scaling_factors: Option<&[U256]>,
) -> Option<U256> {
    let guard = state.read_at(LockSite::Orchestrator);
    simulate_balancer_pair_in_given_out(
        &guard,
        pool_id,
        index_in,
        index_out,
        amount_out,
        override_balances,
        override_scaling_factors,
    )
}

/// Staged fetch+retry CL swap — the shared body of the shell's
/// `simulate_swap_with_fetch` / `simulate_exact_output_swap_with_fetch` (the
/// exact-output sign convention rides in the caller-built request). Stages
/// the missing words first, then runs the disarmed sim and maps the CL
/// payload to the legacy 5-tuple fields.
#[must_use]
pub fn simulate_swap_with_fetch(
    state: &StateLock<BotState>,
    pool_id: u64,
    request: &SwapRequest,
    block: u64,
) -> StagedSwapOutcome {
    if !stage_missing_words(state, pool_id, block, request) {
        return StagedSwapOutcome::NotComputable;
    }
    let read = state
        .write_at(LockSite::Orchestrator)
        .swap_simulation_disarmed(block, pool_id, request);
    let payload = match read {
        SwapRead::Computed(SwapOutcome::V3(payload) | SwapOutcome::V4(payload)) => payload,
        SwapRead::UnsupportedFamily { pool_id, family } => {
            return StagedSwapOutcome::UnsupportedFamily { pool_id, family };
        }
        _ => return StagedSwapOutcome::NotComputable,
    };
    // An amount-modifying hook may have invalidated the standard-math result:
    // surface the approximation instead of a silently wrong number.
    if payload.caveats.contains(Caveats::HOOKED_POOL) {
        return StagedSwapOutcome::HookedPool {
            consumed: payload.consumed,
            delivered: payload.delivered.into_raw(),
        };
    }
    let (amount0, amount1) = payload.raw_token_amounts(request.zero_for_one);
    StagedSwapOutcome::Computed(ClSwapPayload {
        amount0,
        amount1,
        sqrt_price_x96: payload.end_sqrt_price_x96,
        liquidity: payload.end_liquidity,
        tick: payload.end_tick,
    })
}

/// Staged fetch+retry override (hypothetical CL state) sim — the bounded
/// 3-pass loop over [`BotState::override_missing_words`] + the stored fetcher
/// (fetched ticks merge into the caller-owned TRANSIENT map, never
/// registered state), then the disarmed override sim.
///
/// # Errors
/// [`UnsupportedOverrideFamily`] when the target pool is registered under a
/// non-CL family (no transient CL state to build).
pub fn simulate_override_with_fetch(
    state: &StateLock<BotState>,
    over: &mut OverrideSwap,
    block: u64,
) -> Result<Option<V3SwapOutcome>, UnsupportedOverrideFamily> {
    for _ in 0..3u8 {
        let staged = {
            let guard = state.read_at(LockSite::Orchestrator);
            let fetcher = guard.stored_fetcher_for_pool(over.pool_id);
            let missing = guard.override_missing_words(over);
            missing.map(|missing| (fetcher, missing))
        };
        let Some((fetcher, missing)) = staged else {
            return Ok(None);
        };
        if missing.is_empty() {
            break;
        }
        // Misses exist but no fetcher is stored: the hypothetical cannot be
        // backfilled — fail as `None` exactly like the disarmed sim's
        // FetchExhausted arm.
        let Some(fetcher) = fetcher else {
            return Ok(None);
        };
        // Fetches: NO state lock held.
        let mut staged_words = Vec::new();
        for word in &missing {
            match fetcher.fetch_missing_tick_word(over.pool_id, *word, block) {
                Ok(f) => staged_words.push(f),
                Err(_) => return Ok(None),
            }
        }
        // Merge fetched ticks into the caller-owned override map.
        for f in &staged_words {
            for (tick, info) in &f.ticks {
                over.tick_data.insert(*tick, info.clone());
            }
        }
    }
    state
        .read_at(LockSite::Orchestrator)
        .simulate_override_disarmed(over, block)
}

/// Resolve the V2 swap-call encoding for `pool_id`. The shell's handle
/// resolves its pool id at construction, so an unregistered id keeps the
/// legacy `Ok(None)` not-found contract; every other encoder error is typed.
///
/// # Errors
/// [`EncodeSwapError`] for a registered family with no swap encoder or a
/// rejected V2 call.
pub fn encode_swap(
    state: &StateLock<BotState>,
    pool_id: u64,
    zero_for_one: bool,
    amount_out: U256,
    recipient: Address,
) -> Result<Option<EncodedCall>, EncodeSwapError> {
    let result = state.read_at(LockSite::Orchestrator).encode_swap(
        pool_id,
        zero_for_one,
        amount_out,
        recipient,
    );
    match result {
        Ok(call) => Ok(Some(call)),
        Err(EncodeSwapError::NotRegistered { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use alloy::primitives::aliases::U112;
    use degenbot_substrate::RegisterV2PoolParams;
    use hashbrown::HashMap;

    const V2_ID: u64 = 1;
    const V3_ID: u64 = 2;
    const CURVE_ID: u64 = 3;

    /// V2 pool with reserves 1000/2000, fee gamma 997/1000 — the classic
    /// `getAmountOut` shape.
    fn fixture() -> StateLock<BotState> {
        let state = StateLock::new(BotState::new());
        {
            let mut guard = state.write_at(LockSite::Core);
            guard
                .register_v2_pool(&RegisterV2PoolParams {
                    address: Address::from([0xccu8; 20]),
                    token0: Address::from([0xa0u8; 20]),
                    token1: Address::from([0xa1u8; 20]),
                    reserve0: U112::from(1_000u64),
                    reserve1: U112::from(2_000u64),
                    fee_token0: (997, 1000),
                    fee_token1: (997, 1000),
                    factory: Address::from([0xf0u8; 20]),
                    update_block: 100,
                    variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                    stable_swap: false,
                    fee_denominator: None,
                    ..Default::default()
                })
                .expect("v2 registration");
            // V3-consistent nets: the ticks below the current tick 0 sum to
            // the registered active liquidity (1M), so the walk's liquidity
            // accounting matches `liquidity` at the seeded price.
            let mut tick_data = HashMap::new();
            for (tick, net) in [(-20i32, 1_000_000i128), (10, -1_000_000), (20, 1_000_000)] {
                tick_data.insert(
                    tick,
                    crate::bot_core::TickInfo {
                        liquidity_gross: alloy::primitives::U128::from(1_000_000u64),
                        liquidity_net: net,
                        block: 0,
                    },
                );
            }
            guard
                .register_v3_pool(&crate::bot_core::RegisterV3PoolParams {
                    address: Address::from([0xddu8; 20]),
                    token0: Address::from([0xa0u8; 20]),
                    token1: Address::from([0xa1u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    factory: Address::from([0xf0u8; 20]),
                    sqrt_price_x96: U256::from(1u128) << 96,
                    liquidity: 1_000_000,
                    tick: 0,
                    tick_data,
                    update_block: 100,
                    coverage: crate::bot_core::PoolTickCoverage::Tracked,
                    fetcher: None,
                    ..Default::default()
                })
                .expect("v3 registration");
            guard.register_curve_pool(&crate::bot_core::RegisterCurvePoolParams {
                address: Address::from([0xeeu8; 20]),
                tokens: vec![Address::from([0xa0u8; 20]), Address::from([0xa1u8; 20])],
                a_coefficient: 100,
                a_precision: 100,
                fee: 4_000_000,
                admin_fee: 5_000_000_000,
                rate_multipliers: vec![U256::from(1_000_000_000_000_000_000u64); 2],
                balances: vec![U256::from(1_000_000_000_000_000_000u64); 2],
                update_block: 100,
                swap_style: 0,
                lending_rate_style: 0,
                d_variant: 0,
                y_variant: 0,
                yd_variant: 0,
                base_pool: None,
                initial_a_coefficient: None,
                future_a_coefficient: None,
                initial_a_coefficient_time: None,
                future_a_coefficient_time: None,
                create_timestamp: None,
                fee_gamma: None,
                mid_fee: None,
                offpeg_fee_multiplier: None,
                out_fee: None,
                gamma: None,
                lp_token: None,
                use_lending: vec![false, false],
                precision_multipliers: vec![U256::from(1_000_000_000_000_000_000u64); 2],
                tokens_underlying: None,
                metapool_rate_style: 0,
                metapool_underlying_style: 0,
                data_provider: None,
            });
        }
        state
    }

    /// `I256` from an unsigned magnitude (the shell's sign-conversion shape).
    fn i256(v: u64) -> I256 {
        I256::try_from(U256::from(v)).unwrap()
    }

    fn exact_in_request(zero_for_one: bool) -> SwapRequest {
        SwapRequest {
            zero_for_one,
            amount_specified: -i256(100),
            sqrt_price_limit: None,
        }
    }

    #[test]
    fn pool_family_resolves_registered_tags_and_none_for_unknown() {
        let state = fixture();
        assert_eq!(pool_family(&state, V2_ID), Some("v2"));
        assert_eq!(pool_family(&state, V3_ID), Some("v3"));
        assert_eq!(pool_family(&state, CURVE_ID), Some("curve"));
        assert_eq!(pool_family(&state, 999), None);
    }

    #[test]
    fn calculate_tokens_out_matches_on_chain_get_amount_out() {
        let state = fixture();
        // (100 * 997 * 2000) / (1000 * 1000 + 100 * 997) = 181 (EVM floor div).
        let out = calculate_tokens_out(&state, V2_ID, true, U256::from(100u64))
            .expect("v2 exact-input computes");
        assert_eq!(out, U256::from(181u64));
        // Opposite direction: (100 * 997 * 1000) / (2000 * 1000 + 100 * 997) = 47.
        let out = calculate_tokens_out(&state, V2_ID, false, U256::from(100u64))
            .expect("v2 exact-input computes");
        assert_eq!(out, U256::from(47u64));
    }

    #[test]
    fn calculate_tokens_out_types_overflow_unknown_and_family_gap() {
        let state = fixture();
        // 2^255 does not fit the signed user-perspective delta.
        let huge = U256::from(1u8) << 255;
        assert_eq!(
            calculate_tokens_out(&state, V2_ID, true, huge),
            Err(SwapOpError::AmountOverflow)
        );
        assert_eq!(
            calculate_tokens_out(&state, 999, true, U256::from(1u64)),
            Err(SwapOpError::UnknownPool { pool_id: 999 })
        );
        // A registered family with no exact-input gap is exercised via the
        // curve fixture in the exact-output test below; the exact-input curve
        // path computes, so the family-gap arm pins on exact-output instead.
        assert_eq!(
            calculate_tokens_in(&state, CURVE_ID, true, U256::from(1u64)),
            Err(SwapOpError::UnsupportedFamily {
                pool_id: CURVE_ID,
                family: "curve",
            })
        );
    }

    #[test]
    fn calculate_tokens_in_matches_on_chain_get_amount_in() {
        let state = fixture();
        // Required input for 181 out: (1000 * 181 * 1000) / (1819 * 997) + 1 = 100.
        let inp = calculate_tokens_in(&state, V2_ID, true, U256::from(181u64))
            .expect("v2 exact-output computes");
        assert_eq!(inp, U256::from(100u64));
        // Unknown pool keeps the legacy silent-0 contract.
        assert_eq!(
            calculate_tokens_in(&state, 999, true, U256::from(1u64)),
            Ok(U256::ZERO)
        );
    }

    #[test]
    fn stage_missing_words_contracts() {
        let state = fixture();
        // Unregistered: nothing can be staged.
        assert!(!stage_missing_words(
            &state,
            999,
            100,
            &exact_in_request(true)
        ));
        // Non-CL family: nothing to stage.
        assert!(stage_missing_words(
            &state,
            V2_ID,
            100,
            &exact_in_request(true)
        ));
        // CL pool with complete tracked coverage: nothing missing.
        assert!(stage_missing_words(
            &state,
            V3_ID,
            100,
            &exact_in_request(true)
        ));
    }

    #[test]
    fn calculate_tokens_out_with_fetch_keeps_zero_contracts() {
        let state = fixture();
        // V2 computes identically (nothing to stage).
        assert_eq!(
            calculate_tokens_out_with_fetch(&state, V2_ID, true, U256::from(100u64), 100),
            Ok(U256::from(181u64))
        );
        // Unregistered pool: staging fails -> the legacy zero sentinel.
        assert_eq!(
            calculate_tokens_out_with_fetch(&state, 999, true, U256::from(100u64), 100),
            Ok(U256::ZERO)
        );
        // Sign overflow stays typed.
        let huge = U256::from(1u8) << 255;
        assert_eq!(
            calculate_tokens_out_with_fetch(&state, V2_ID, true, huge, 100),
            Err(SwapOpError::AmountOverflow)
        );
    }

    #[test]
    fn simulate_swap_with_fetch_returns_cl_payload_for_cl_pool() {
        let state = fixture();
        let outcome = simulate_swap_with_fetch(&state, V3_ID, &exact_in_request(true), 100);
        let payload = match outcome {
            StagedSwapOutcome::Computed(p) => p,
            other => panic!("expected a computed CL payload, got {other:?}"),
        };
        assert_eq!(payload.amount0, U256::from(100u64));
        assert!(payload.amount1 > U256::ZERO);
        assert_eq!(payload.liquidity, 1_000_000);
        // The small swap moves the price down within the seeded range
        // without crossing an initialized tick.
        assert!(
            payload.tick <= 0 && payload.tick > -20,
            "end tick {} in range",
            payload.tick
        );
        assert!(payload.sqrt_price_x96 < U256::from(1u128) << 96);
    }

    #[test]
    fn simulate_swap_with_fetch_exact_output_family_contracts() {
        let state = fixture();
        let request = SwapRequest {
            zero_for_one: true,
            amount_specified: i256(100),
            sqrt_price_limit: None,
        };
        let outcome = simulate_swap_with_fetch(&state, V3_ID, &request, 100);
        let payload = match outcome {
            StagedSwapOutcome::Computed(p) => p,
            other => panic!("expected a computed CL payload, got {other:?}"),
        };
        assert_eq!(payload.amount1, U256::from(100u64));
        assert!(payload.amount0 > U256::ZERO);

        // A constant-product family has no CL 5-tuple: both seams collapse
        // the non-CL payload to the legacy bare-`None` contract (V2 carries
        // an exact-output path, so it computes a V2 payload the CL seam drops).
        assert_eq!(
            simulate_swap_with_fetch(&state, V2_ID, &request, 100),
            StagedSwapOutcome::NotComputable
        );
        let v2_in = exact_in_request(true);
        assert_eq!(
            simulate_swap_with_fetch(&state, V2_ID, &v2_in, 100),
            StagedSwapOutcome::NotComputable
        );
        // The curve fixture pins the typed gap arm for a non-CL family.
        match simulate_swap_with_fetch(&state, CURVE_ID, &request, 100) {
            StagedSwapOutcome::UnsupportedFamily { pool_id, family } => {
                assert_eq!(pool_id, CURVE_ID);
                assert_eq!(family, "curve");
            }
            other => panic!("expected a family gap, got {other:?}"),
        }
    }

    #[test]
    fn simulate_override_with_fetch_contracts() {
        let state = fixture();
        // The hypothetical carries the same V3-consistent tick set as the
        // registered pool — a complete hypothetical needs no fetcher.
        let mut over_ticks = HashMap::new();
        for (tick, net) in [(-20i32, 1_000_000i128), (10, -1_000_000), (20, 1_000_000)] {
            over_ticks.insert(
                tick,
                crate::bot_core::TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(1_000_000u64),
                    liquidity_net: net,
                    block: 0,
                },
            );
        }
        let mut over = OverrideSwap {
            pool_id: V3_ID,
            request: exact_in_request(true),
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: over_ticks,
        };
        let outcome = simulate_override_with_fetch(&state, &mut over, 100)
            .expect("override sim over a CL family");
        let outcome = outcome.expect("complete hypothetical tick data computes");
        assert_eq!(outcome.liquidity, 1_000_000);
        assert!(
            outcome.tick <= 0 && outcome.tick > -20,
            "end tick {} in range",
            outcome.tick
        );
        assert!(outcome.amount1 > U256::ZERO);

        // A non-CL family has no transient CL state: the typed refusal.
        let mut over_v2 = OverrideSwap {
            pool_id: V2_ID,
            request: exact_in_request(true),
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
        };
        let err = simulate_override_with_fetch(&state, &mut over_v2, 100)
            .expect_err("non-CL family refuses the override seam");
        assert_eq!(err.pool_id, V2_ID);
        assert_eq!(err.family, "v2");
    }

    #[test]
    fn encode_swap_contracts() {
        let state = fixture();
        let recipient = Address::from([0x42u8; 20]);
        let call = encode_swap(&state, V2_ID, true, U256::from(100u64), recipient)
            .expect("v2 encodes")
            .expect("registered v2 pool encodes a call");
        assert_eq!(call.to, Address::from([0xccu8; 20]));
        assert_eq!(call.value, U256::ZERO);
        assert!(!call.data.is_empty());

        // Unregistered pool keeps the Ok(None) not-found contract.
        assert!(matches!(
            encode_swap(&state, 999, true, U256::from(100u64), recipient),
            Ok(None)
        ));

        // A non-V2 family has no swap encoder: the typed refusal.
        let err = encode_swap(&state, V3_ID, true, U256::from(100u64), recipient)
            .expect_err("non-v2 family has no encoder");
        assert!(matches!(
            err,
            EncodeSwapError::UnsupportedFamily { pool_id: V3_ID, .. }
        ));
    }
}

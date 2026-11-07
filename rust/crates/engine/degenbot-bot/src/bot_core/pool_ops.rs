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
#[cfg(test)]
use degenbot_core::fee::fee_ratio;
use degenbot_pools::CurveDataProvider;
use degenbot_pools::RegisteredPoolFamily;
use degenbot_pools::{fetch_and_merge, TransientTickMap, WordFetchPolicy};
use degenbot_substrate::state_lock::{LockSite, StateLock};
use degenbot_substrate::swap_simulation::{
    simulate_pair_in_given_out, simulate_pair_out, Caveats, OverrideOutcome, OverrideState,
    OverrideSwap, SwapOutcome, SwapRead, SwapRequest,
};
use degenbot_substrate::{BotState, EncodeSwapError};
use degenbot_uniswap::v2_encoding::EncodedCall;

/// Typed failure from the swap calc ops — the shell maps each variant to its
/// byte-identical historical Python surface (`bot::errmap`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapOpError {
    /// The request amount did not fit the signed user-perspective delta — one
    /// historical `ValueError` surface with [`SwapOpError::NotComputable`] on
    /// the exact-input ops (the on-chain `getAmountOut` `SafeMath` revert
    /// parity).
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
    /// The sim ran but the swap was not computable: a `uint256` intermediate
    /// overflow, an exact-output request the family door refuses (overdraw,
    /// vanishing denominator), or a walk that cannot meet the request. The
    /// exact-input ops render the same historical `ValueError` surface as
    /// [`SwapOpError::AmountOverflow`]; the exact-output op names the class.
    NotComputable,
    /// Miss recovery ran but the fetch itself failed for `word`.
    FetchFailed {
        /// The bitmap word whose fetch failed.
        word: i32,
    },
    /// Miss recovery was impossible or exhausted for `word` (no fetcher
    /// registered, or the same word missed again after a merge).
    FetchExhausted {
        /// The bitmap word whose recovery was exhausted.
        word: i32,
    },
}

/// The concentrated liquidity 5-tuple payload the staged fetch-sim seams surface — the legacy
/// `(amount0, amount1, sqrt_price_x96, liquidity, tick)` tuple fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), expect(dead_code))]
pub struct ConcentratedLiquiditySwapPayload {
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
#[cfg_attr(not(test), expect(dead_code))]
pub enum StagedSwapOutcome {
    /// The swap computed; payload per the legacy 5-tuple.
    Computed(ConcentratedLiquiditySwapPayload),
    /// A V4 amount-modifying hook invalidates the standard math — the
    /// approximate deltas ride along for the archived exception text.
    HookedPool {
        /// Taken FROM the user (negative, user-perspective delta).
        consumed: I256,
        /// Given TO the user (unsigned magnitude).
        delivered: U256,
    },
    /// Staging failed, the walk was not computable, or the payload is a
    /// constant-product family the concentrated liquidity 5-tuple seam cannot represent — the
    /// legacy bare-`None` contract.
    NotComputable,
    /// A non-concentrated-liquidity family was asked for the concentrated liquidity 5-tuple seam.
    UnsupportedFamily {
        /// The requested pool id.
        pool_id: u64,
        /// The registered family tag.
        family: &'static str,
    },
}

/// The registered family tag for `pool_id` (`None` = not registered).
///
/// The string rendering of [`registered_family`], for the surfaces that report
/// a family to Python.
#[must_use]
pub fn pool_family(state: &StateLock<BotState>, pool_id: u64) -> Option<&'static str> {
    state.read_at(LockSite::Orchestrator).pool_family(pool_id)
}

/// The registered family of `pool_id`, typed (`None` = not registered) — what
/// a family dispatch matches on.
#[must_use]
pub fn registered_family(
    state: &StateLock<BotState>,
    pool_id: u64,
) -> Option<RegisteredPoolFamily> {
    state
        .read_at(LockSite::Orchestrator)
        .registered_family(pool_id)
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
    guard
        .pool_slice::<degenbot_pools::CurvePoolState>(pool_id)?
        .data_provider
        .clone()
}

/// Stage the missing bitmap words a concentrated liquidity swap needs — the bounded 3-pass loop
/// the fetch-sim callers run before entering the disarmed sim: discover (a
/// collect-only transient walk under a READ guard), stage the batch under ONE
/// short write, fetch lock-free (no state lock held; the stored fetcher
/// re-enters Python itself), install under short writes with the fingerprint
/// re-check. A raced install (the pump wrote this pool mid-batch) retries the
/// whole pass. The refusal is typed per word — an unregistered id, a fetch
/// that itself failed, or a recovery that was impossible or exhausted — so a
/// caller surfaces which word stalled instead of folding every failure into
/// one answer.
///
/// # Errors
/// [`SwapOpError::UnknownPool`] when the id is not registered,
/// [`SwapOpError::FetchFailed`] when the fetch itself failed for a word, and
/// [`SwapOpError::FetchExhausted`] when recovery is impossible (no stored
/// read handle) or the passes raced out.
pub fn stage_missing_words(
    state: &StateLock<BotState>,
    pool_id: u64,
    block: u64,
    request: &SwapRequest,
) -> Result<(), SwapOpError> {
    let mut last_missing_word = 0i32;
    for pass in 0..3u8 {
        let Some(missing) = state
            .read_at(LockSite::Orchestrator)
            .swap_missing_words(block, pool_id, request)
        else {
            // Not registered: the sim body's disarm contract never fetches.
            return Err(SwapOpError::UnknownPool { pool_id });
        };
        if missing.is_empty() {
            return Ok(());
        }
        last_missing_word = missing.first().copied().unwrap_or(0);
        // Stage the whole batch under ONE short write (the per-word
        // fingerprints gate the installs individually).
        let staged = {
            let guard = state.write_at(LockSite::Orchestrator);
            let mut staged = Vec::with_capacity(missing.len());
            for word in &missing {
                match guard.stage_word_fetch_by_pool_id(pool_id, *word, block, pass > 0) {
                    Some(staged_word) => staged.push(staged_word),
                    // No stored read handle (or the pool cannot receive a
                    // word): recovery is impossible for this word.
                    None => return Err(SwapOpError::FetchExhausted { word: *word }),
                }
            }
            staged
        };
        // Fetches: NO state lock held.
        let mut fetched = Vec::new();
        for staged_word in &staged {
            match staged_word.fetch() {
                Ok(f) => fetched.push(f),
                Err(_) => {
                    return Err(SwapOpError::FetchFailed {
                        word: staged_word.word(),
                    })
                }
            }
        }
        // Installs: short writes, fingerprint-gated. Any Raced means the
        // pump wrote this pool mid-batch: retry the whole pass.
        let raced = {
            let mut guard = state.write_at(LockSite::Orchestrator);
            staged
                .iter()
                .zip(fetched.iter())
                .any(|(staged_word, fetched_word)| {
                    matches!(
                        guard.install_word_fetch(staged_word, fetched_word),
                        degenbot_pools::WordFetchOutcome::Raced
                    )
                })
        };
        if !raced {
            return Ok(());
        }
    }
    // Three passes raced out under the pump: recovery exhausted for the word
    // still missing.
    Err(SwapOpError::FetchExhausted {
        word: last_missing_word,
    })
}

/// Assemble the exact-input request (the shell's sign-conversion shape): the
/// request amount is negated per the user-perspective delta convention.
/// Shared by every exact-input calc op (the disarmed, recovering, and facade
/// doors) so the request logic cannot diverge.
///
/// # Errors
/// [`SwapOpError::AmountOverflow`] when the amount does not fit the signed delta.
pub fn make_exact_in_request(
    zero_for_one: bool,
    amount_in: U256,
) -> Result<SwapRequest, SwapOpError> {
    Ok(SwapRequest {
        zero_for_one,
        amount_specified: -I256::try_from(amount_in).map_err(|_| SwapOpError::AmountOverflow)?,
        sqrt_price_limit: None,
    })
}

/// Assemble the exact-output request (ADR-037: positive user-perspective; the
/// required input rides back as the consumed magnitude). Shared by every
/// exact-output calc op.
///
/// # Errors
/// [`SwapOpError::AmountOverflow`] when the amount does not fit the signed delta.
pub fn make_exact_out_request(
    zero_for_one: bool,
    amount_out: U256,
) -> Result<SwapRequest, SwapOpError> {
    Ok(SwapRequest {
        zero_for_one,
        amount_specified: I256::try_from(amount_out).map_err(|_| SwapOpError::AmountOverflow)?,
        sqrt_price_limit: None,
    })
}

/// The ONE `SwapRead`→result classification every exact-input calc op shares:
/// the computed delivered output, or the typed refusal. Only a successfully
/// computed zero returns `Ok(U256::ZERO)`, so a failing pool cannot masquerade
/// as a zero quote.
///
/// # Errors
/// [`SwapOpError`] per the typed failure vocabulary.
pub fn tokens_out_from_read(read: SwapRead) -> Result<U256, SwapOpError> {
    match read {
        SwapRead::Computed(outcome) => Ok(outcome.delivered_unsigned()),
        SwapRead::NotComputable => Err(SwapOpError::NotComputable),
        SwapRead::UnknownPool { pool_id } => Err(SwapOpError::UnknownPool { pool_id }),
        SwapRead::UnsupportedFamily { pool_id, family } => {
            Err(SwapOpError::UnsupportedFamily { pool_id, family })
        }
        SwapRead::FetchFailed { word } => Err(SwapOpError::FetchFailed { word }),
        SwapRead::FetchExhausted { word } => Err(SwapOpError::FetchExhausted { word }),
    }
}

/// The ONE `SwapRead`→result classification every exact-output calc op shares:
/// the required input = |consumed|, or the typed refusal. Only a successfully
/// computed zero returns `Ok(U256::ZERO)`.
///
/// # Errors
/// [`SwapOpError`] per the typed failure vocabulary.
pub fn tokens_in_from_read(read: SwapRead) -> Result<U256, SwapOpError> {
    match read {
        SwapRead::Computed(outcome) => {
            let consumed = match &outcome {
                SwapOutcome::V2(o) => o.consumed,
                SwapOutcome::V3(o) | SwapOutcome::V4(o) => o.consumed,
            };
            Ok((-consumed).into_raw())
        }
        SwapRead::NotComputable => Err(SwapOpError::NotComputable),
        SwapRead::UnknownPool { pool_id } => Err(SwapOpError::UnknownPool { pool_id }),
        SwapRead::UnsupportedFamily { pool_id, family } => {
            Err(SwapOpError::UnsupportedFamily { pool_id, family })
        }
        SwapRead::FetchFailed { word } => Err(SwapOpError::FetchFailed { word }),
        SwapRead::FetchExhausted { word } => Err(SwapOpError::FetchExhausted { word }),
    }
}

/// Exact-input `calculate_tokens_out` — the disarmed sim at the legacy block
/// sentinel `0`, surfacing every non-computed read as its typed refusal. Only
/// a successfully computed zero returns `Ok(U256::ZERO)`, so a failing pool
/// cannot masquerade as a zero quote.
///
/// # Errors
/// [`SwapOpError`] per the typed failure vocabulary.
pub fn calculate_tokens_out(
    state: &StateLock<BotState>,
    pool_id: u64,
    zero_for_one: bool,
    amount_in: U256,
) -> Result<U256, SwapOpError> {
    let request = make_exact_in_request(zero_for_one, amount_in)?;
    let read = state
        .write_at(LockSite::Orchestrator)
        .swap_simulation_disarmed(0, pool_id, &request);
    tokens_out_from_read(read)
}

/// Exact-output `calculate_tokens_in` — required input = |consumed|, as
/// explicit as [`calculate_tokens_out`]: every non-computed read is a typed
/// refusal, and `Ok(U256::ZERO)` is only the computed zero.
///
/// # Errors
/// [`SwapOpError`] per the typed failure vocabulary.
pub fn calculate_tokens_in(
    state: &StateLock<BotState>,
    pool_id: u64,
    zero_for_one: bool,
    amount_out: U256,
) -> Result<U256, SwapOpError> {
    let request = make_exact_out_request(zero_for_one, amount_out)?;
    let read = state
        .write_at(LockSite::Orchestrator)
        .swap_simulation_disarmed(0, pool_id, &request);
    tokens_in_from_read(read)
}

/// Fetch+retry exact-input `calculate_tokens_out` — stages the missing words
/// first (bounded passes), then runs the disarmed sim at `block`. Every
/// non-computed outcome is a typed refusal (the staging verdict included);
/// `Ok(U256::ZERO)` is only the computed zero.
///
/// # Errors
/// [`SwapOpError`] per the typed failure vocabulary.
pub fn calculate_tokens_out_with_fetch(
    state: &StateLock<BotState>,
    pool_id: u64,
    zero_for_one: bool,
    amount_in: U256,
    block: u64,
) -> Result<U256, SwapOpError> {
    let request = make_exact_in_request(zero_for_one, amount_in)?;
    stage_missing_words(state, pool_id, block, &request)?;
    let read = state
        .write_at(LockSite::Orchestrator)
        .swap_simulation_disarmed(block, pool_id, &request);
    tokens_out_from_read(read)
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
    simulate_pair_out(
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
    simulate_pair_in_given_out(
        &guard,
        pool_id,
        index_in,
        index_out,
        amount_out,
        override_balances,
        override_scaling_factors,
    )
}

/// Staged fetch+retry concentrated liquidity swap — the shared body of the shell's
/// `simulate_swap_with_fetch` / `simulate_exact_output_swap_with_fetch` (the
/// exact-output sign convention rides in the caller-built request). Stages
/// the missing words first, then runs the disarmed sim and maps the concentrated liquidity
/// payload to the legacy 5-tuple fields.
#[must_use]
#[cfg_attr(not(test), expect(dead_code))]
pub fn simulate_swap_with_fetch(
    state: &StateLock<BotState>,
    pool_id: u64,
    request: &SwapRequest,
    block: u64,
) -> StagedSwapOutcome {
    if stage_missing_words(state, pool_id, block, request).is_err() {
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
    StagedSwapOutcome::Computed(ConcentratedLiquiditySwapPayload {
        amount0,
        amount1,
        sqrt_price_x96: payload.end_sqrt_price_x96,
        liquidity: payload.end_liquidity,
        tick: payload.end_tick,
    })
}

/// THE mainline concentrated liquidity swap read (ADR-037): stages the missing bitmap words
/// (bounded pre-pass) then runs the disarmed sim, surfacing the full typed
/// [`SwapRead`] so the shell can tell a compute miss from an exhausted fetch.
/// `request.amount_specified` is signed user-perspective.
#[must_use]
pub fn simulate_mainline(
    state: &StateLock<BotState>,
    pool_id: u64,
    request: &SwapRequest,
    block: u64,
) -> SwapRead {
    let Some(missing) = state
        .read_at(LockSite::Orchestrator)
        .swap_missing_words(block, pool_id, request)
    else {
        return SwapRead::UnknownPool { pool_id };
    };
    if !missing.is_empty() {
        match stage_missing_words(state, pool_id, block, request) {
            Ok(()) => {}
            Err(SwapOpError::UnknownPool { pool_id }) => return SwapRead::UnknownPool { pool_id },
            Err(SwapOpError::FetchFailed { word }) => return SwapRead::FetchFailed { word },
            Err(SwapOpError::FetchExhausted { word }) => return SwapRead::FetchExhausted { word },
            // Staging speaks only the three verdicts above; the calc-op
            // refusals cannot arise here, and a read that could not be armed
            // cannot proceed.
            Err(
                SwapOpError::AmountOverflow
                | SwapOpError::NotComputable
                | SwapOpError::UnsupportedFamily { .. },
            ) => return SwapRead::NotComputable,
        }
    }
    state
        .write_at(LockSite::Orchestrator)
        .swap_simulation_disarmed(block, pool_id, request)
}

/// Staged fetch+retry override (hypothetical state) sim — the bounded
/// 3-pass loop over [`BotState::override_missing_words`] + the pool's stored
/// read handle, merging through the ONE tick-word merge policy into the
/// caller-owned TRANSIENT map (never registered state), then the disarmed
/// override sim.
///
/// Only the concentrated-liquidity family carries a sparse tick map, so only
/// its override state lists words to stage; every other family's override read
/// is a pure transient and enters the door directly.
#[must_use]
pub fn simulate_override_with_fetch(
    state: &StateLock<BotState>,
    over: &mut OverrideSwap,
    block: u64,
) -> Option<OverrideOutcome> {
    for _ in 0..3u8 {
        let staged = {
            let guard = state.read_at(LockSite::Orchestrator);
            let reader = guard.tick_word_reader_for_pool(over.pool_id);
            let missing = guard.override_missing_words(over);
            missing.map(|missing| (reader, missing))
        };
        let (reader, missing) = staged?;
        if missing.is_empty() {
            break;
        }
        // Misses exist but no read handle is stored: the hypothetical cannot be
        // backfilled — fail as `None` exactly like the disarmed sim's
        // FetchExhausted arm.
        let reader = reader?;
        // Fetches: NO state lock held. The transient map is the caller's own, so
        // nothing can write it between the fetch and the merge — the capability's
        // fetch-and-merge is the whole policy, checked-empty included.
        let OverrideState::ConcentratedLiquidity { tick_data, .. } = &mut over.state else {
            return None;
        };
        let mut target = TransientTickMap::new(tick_data);
        for word in &missing {
            if !fetch_and_merge(&mut target, *word, block, WordFetchPolicy::Armed(&reader)).known()
            {
                return None;
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
    #![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use alloy::primitives::aliases::U112;
    use degenbot_pools::{
        DVariant, FetchedTickWord, LendingRateStyle, MetapoolRateStyle, MetapoolUnderlyingStyle,
        SwapStyle, TickWordFetchError, TickWordKey, TickWordSource, YDVariant, YVariant,
    };
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
                    fee_token0: fee_ratio(997, 1000),
                    fee_token1: fee_ratio(997, 1000),
                    factory: Address::from([0xf0u8; 20]),
                    update_block: 100,
                    variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                    stable_swap: false,
                    deployer: Address::ZERO,
                    init_hash: alloy::primitives::B256::ZERO,
                })
                .expect("v2 registration");
            // V3-consistent nets: the ticks below the current tick 0 sum to
            // the registered active liquidity (1M), so the walk's liquidity
            // accounting matches `liquidity` at the seeded price.
            let mut tick_data = HashMap::new();
            for (tick, net) in [(-20i32, 1_000_000i128), (10, -1_000_000), (20, 1_000_000)] {
                tick_data.insert(
                    tick,
                    degenbot_pools::TickInfo {
                        liquidity_gross: alloy::primitives::U128::from(1_000_000u64),
                        liquidity_net: net,
                        block: 0,
                    },
                );
            }
            guard
                .register_v3_pool(&degenbot_pools::RegisterV3PoolParams {
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
                    coverage: degenbot_pools::PoolTickCoverage::Tracked,
                    fetcher: None,
                    ..Default::default()
                })
                .expect("v3 registration");
            guard
                .register_curve_pool(&crate::bot_core::RegisterCurvePoolParams {
                    address: Address::from([0xeeu8; 20]),
                    tokens: vec![Address::from([0xa0u8; 20]), Address::from([0xa1u8; 20])],
                    a_coefficient: 100,
                    a_precision: 100,
                    fee: 4_000_000,
                    admin_fee: 5_000_000_000,
                    rate_multipliers: vec![U256::from(1_000_000_000_000_000_000u64); 2],
                    balances: vec![U256::from(1_000_000_000_000_000_000u64); 2],
                    update_block: 100,
                    swap_style: SwapStyle::Standard,
                    lending_rate_style: LendingRateStyle::None,
                    d_variant: DVariant::Standard,
                    y_variant: YVariant::Standard,
                    yd_variant: YDVariant::Standard,
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
                    metapool_rate_style: MetapoolRateStyle::Standard,
                    metapool_underlying_style: MetapoolUnderlyingStyle::Standard,
                    data_provider: None,
                })
                .expect("curve registration");
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

    /// A sparse concentrated liquidity pool (empty tick data — the walk
    /// misses the starting bitmap word) with the stored word source the
    /// recovery leg needs. Returns the state and the assigned pool id.
    fn sparse_v3_state(fetcher: Option<Arc<dyn TickWordSource>>) -> (StateLock<BotState>, u64) {
        let state = StateLock::new(BotState::new());
        let pool_id = {
            let mut guard = state.write_at(LockSite::Core);
            guard
                .register_v3_pool(&degenbot_pools::RegisterV3PoolParams {
                    address: Address::from([0xddu8; 20]),
                    token0: Address::from([0xa0u8; 20]),
                    token1: Address::from([0xa1u8; 20]),
                    fee: 3000,
                    tick_spacing: 60,
                    factory: Address::from([0xf0u8; 20]),
                    sqrt_price_x96: U256::from(1u128) << 96,
                    liquidity: 1_000_010_000_000,
                    tick: 0,
                    tick_data: HashMap::new(),
                    update_block: 100,
                    coverage: degenbot_pools::PoolTickCoverage::Sparse,
                    fetcher,
                    ..Default::default()
                })
                .expect("sparse v3 registration")
        };
        (state, pool_id)
    }

    /// A stored word source whose fetch always fails.
    #[derive(Debug)]
    struct FailingWordSource;

    impl TickWordSource for FailingWordSource {
        fn fetch_tick_word(
            &self,
            _key: &TickWordKey,
            _word: i32,
            _block: u64,
        ) -> Result<Option<FetchedTickWord>, TickWordFetchError> {
            Err(TickWordFetchError::FetchFailed)
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
    }

    #[test]
    fn calculate_tokens_in_types_every_refusal_and_keeps_computed_zero() {
        let state = fixture();
        // The zero request is the row's computed 0 -> 0: a computed zero is
        // the one zero that stays a success.
        assert_eq!(
            calculate_tokens_in(&state, V2_ID, true, U256::ZERO),
            Ok(U256::ZERO)
        );
        // The exact-output overdraw (amount_out >= the output reserve) is a
        // typed refusal — never a zero quote.
        assert_eq!(
            calculate_tokens_in(&state, V2_ID, true, U256::from(2_000u64)),
            Err(SwapOpError::NotComputable)
        );
        // An unknown pool refuses in the exact-output direction too.
        assert_eq!(
            calculate_tokens_in(&state, 999, true, U256::from(1u64)),
            Err(SwapOpError::UnknownPool { pool_id: 999 })
        );
    }

    #[test]
    fn calculate_tokens_out_types_miss_refusals_and_keeps_computed_zero() {
        let state = fixture();
        // The zero request is the row's computed 0 -> 0.
        assert_eq!(
            calculate_tokens_out(&state, V2_ID, true, U256::ZERO),
            Ok(U256::ZERO)
        );
        // 2**250 fits the signed request but overflows the constant-product
        // multiply: the not-computable class, not a zero quote.
        assert_eq!(
            calculate_tokens_out(&state, V2_ID, true, U256::from(1u8) << 250),
            Err(SwapOpError::NotComputable)
        );
        // A sparse miss with no fetcher is an exhausted recovery, never 0.
        let (state, pool_id) = sparse_v3_state(None);
        assert_eq!(
            calculate_tokens_out(&state, pool_id, true, U256::from(1_000u64)),
            Err(SwapOpError::FetchExhausted { word: 0 })
        );
    }

    #[test]
    fn stage_missing_words_contracts() {
        let state = fixture();
        // Unregistered: the typed unknown-pool refusal.
        assert_eq!(
            stage_missing_words(&state, 999, 100, &exact_in_request(true)),
            Err(SwapOpError::UnknownPool { pool_id: 999 })
        );
        // Non-concentrated-liquidity family: nothing to stage.
        assert_eq!(
            stage_missing_words(&state, V2_ID, 100, &exact_in_request(true)),
            Ok(())
        );
        // concentrated liquidity pool with complete tracked coverage: nothing missing.
        assert_eq!(
            stage_missing_words(&state, V3_ID, 100, &exact_in_request(true)),
            Ok(())
        );
    }

    #[test]
    fn calculate_tokens_out_with_fetch_types_refusals_and_keeps_computed_zero() {
        let state = fixture();
        // V2 computes identically (nothing to stage).
        assert_eq!(
            calculate_tokens_out_with_fetch(&state, V2_ID, true, U256::from(100u64), 100),
            Ok(U256::from(181u64))
        );
        // The zero request is the row's computed 0 -> 0.
        assert_eq!(
            calculate_tokens_out_with_fetch(&state, V2_ID, true, U256::ZERO, 100),
            Ok(U256::ZERO)
        );
        // Unregistered pool: staging refuses with the typed verdict.
        assert_eq!(
            calculate_tokens_out_with_fetch(&state, 999, true, U256::from(100u64), 100),
            Err(SwapOpError::UnknownPool { pool_id: 999 })
        );
        // Sign overflow stays typed.
        let huge = U256::from(1u8) << 255;
        assert_eq!(
            calculate_tokens_out_with_fetch(&state, V2_ID, true, huge, 100),
            Err(SwapOpError::AmountOverflow)
        );
        // Sparse with no fetcher: recovery is impossible for the missed word.
        let (state, pool_id) = sparse_v3_state(None);
        assert_eq!(
            calculate_tokens_out_with_fetch(&state, pool_id, true, U256::from(1_000u64), 100),
            Err(SwapOpError::FetchExhausted { word: 0 })
        );
        // Sparse with a failing fetcher: the fetch itself failed for the word.
        let (state, pool_id) = sparse_v3_state(Some(Arc::new(FailingWordSource)));
        assert_eq!(
            calculate_tokens_out_with_fetch(&state, pool_id, true, U256::from(1_000u64), 100),
            Err(SwapOpError::FetchFailed { word: 0 })
        );
    }

    #[test]
    fn simulate_swap_with_fetch_returns_concentrated_liquidity_payload_for_concentrated_liquidity_pool(
    ) {
        let state = fixture();
        let outcome = simulate_swap_with_fetch(&state, V3_ID, &exact_in_request(true), 100);
        let payload = match outcome {
            StagedSwapOutcome::Computed(p) => p,
            other => panic!("expected a computed concentrated liquidity payload, got {other:?}"),
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
            other => panic!("expected a computed concentrated liquidity payload, got {other:?}"),
        };
        assert_eq!(payload.amount1, U256::from(100u64));
        assert!(payload.amount0 > U256::ZERO);

        // A constant-product family has no concentrated liquidity 5-tuple: both seams collapse
        // the non-concentrated-liquidity payload to the legacy bare-`None` contract (V2 carries
        // an exact-output path, so it computes a V2 payload the concentrated liquidity seam drops).
        assert_eq!(
            simulate_swap_with_fetch(&state, V2_ID, &request, 100),
            StagedSwapOutcome::NotComputable
        );
        let v2_in = exact_in_request(true);
        assert_eq!(
            simulate_swap_with_fetch(&state, V2_ID, &v2_in, 100),
            StagedSwapOutcome::NotComputable
        );
        // The curve fixture pins the typed gap arm for a non-concentrated-liquidity family.
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
                degenbot_pools::TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(1_000_000u64),
                    liquidity_net: net,
                    block: 0,
                },
            );
        }
        let mut over = OverrideSwap {
            pool_id: V3_ID,
            request: exact_in_request(true),
            state: OverrideState::ConcentratedLiquidity {
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: over_ticks,
            },
        };
        let outcome = simulate_override_with_fetch(&state, &mut over, 100)
            .expect("complete hypothetical tick data computes");
        let OverrideOutcome::ConcentratedLiquidity(walk) = outcome else {
            panic!("the state variant selects the family: {outcome:?}")
        };
        assert_eq!(walk.liquidity, 1_000_000);
        assert!(
            walk.tick <= 0 && walk.tick > -20,
            "end tick {} in range",
            walk.tick
        );
        assert!(walk.amount1 > U256::ZERO);
        // The concentrated-liquidity family is the only one with words to
        // stage; a complete hypothetical stages none.
        assert_eq!(
            state
                .read_at(LockSite::Orchestrator)
                .override_missing_words(&over),
            Some(Vec::new())
        );

        // The reserve-pair family computes from the caller's TRANSIENT
        // reserves: no fetch pass exists for it (nothing to stage, and no
        // fetcher could be consulted).
        let mut over_v2 = OverrideSwap {
            pool_id: V2_ID,
            request: exact_in_request(true),
            state: OverrideState::ReservePair {
                reserves_token0: U112::from(1_000u64),
                reserves_token1: U112::from(2_000u64),
            },
        };
        let outcome = simulate_override_with_fetch(&state, &mut over_v2, 100)
            .expect("the reserve-pair override computes");
        let OverrideOutcome::ReservePair(payload) = outcome else {
            panic!("the state variant selects the family: {outcome:?}")
        };
        // (100 * 997 * 2000) / (1000 * 1000 + 100 * 997) = 181 (EVM floor div).
        assert_eq!(payload.consumed, -i256(100));
        assert_eq!(payload.delivered, i256(181));
        assert_eq!(payload.end_reserves_token0, U112::from(1_100u64));
        assert_eq!(payload.end_reserves_token1, U112::from(1_819u64));

        // A state the registration cannot carry is the door's refusal — the
        // shell marshalls the variant from the registered family, so this
        // pairing is unreachable from Python.
        let mut mismatch = OverrideSwap {
            pool_id: V3_ID,
            request: exact_in_request(true),
            state: OverrideState::ReservePair {
                reserves_token0: U112::from(1_000u64),
                reserves_token1: U112::from(2_000u64),
            },
        };
        assert_eq!(
            simulate_override_with_fetch(&state, &mut mismatch, 100),
            None
        );
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

//! V4 concentrated-liquidity pool state — the single BotState-owned home for
//! V4 pool data (ADR-003). Supersedes the engine-side `V4PoolState` that lived
//! in `solvers/v4_block_engine.rs`; that engine's path/solve subsystem is
//! deleted as orphan dead code (the unified `UniswapEngine` already handles V4
//! paths via `HopType::V4` in its `register_path`/`solve_path`).
//!
//! V4 shares identical CL math with V3 (same tick structure, same
//! `sqrtPriceX96`, same liquidity tracking). The `build_int_v4_sequence`
//! produces the same `IntV3TickRangeSequence` the V3 path solver consumes.

use std::collections::HashMap;
use std::sync::Arc;

use alloy::primitives::{Address, I256, U160, U256};

use crate::bot_core::state_history::{ReorgJournal, V3BlockDelta};
use crate::bot_core::tick_bitmap::{compute_tick_ranges, gen_ticks, V3TickRangeForSolver};
use crate::bot_core::v3_state::{PoolTickCoverage, V3SwapOutcome};
use crate::bot_core::TickInfo;
use crate::solvers::liquidity_event_buffer::LiquidityEvent;
use crate::solvers::mobius_v3_int::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_cl_math::cl_lib::swap_math::compute_swap_step_v4;
use degenbot_cl_math::cl_lib::tick_math::{
    get_sqrt_ratio_at_tick_internal, get_tick_at_sqrt_ratio_internal, MAX_SQRT_RATIO,
    MIN_SQRT_RATIO,
};
use degenbot_decoders::v4_swap_decoder::PoolId;

// ---------------------------------------------------------------------------
// Hook filtering constants
// ---------------------------------------------------------------------------

/// Bitmask of V4 hook flags that can modify swap amounts.
///
/// Pools with any of these bits set are excluded from arbitrage because the
/// solver assumes standard V3 math — hooked pools can produce arbitrary deltas
/// that violate this assumption.
/// - `BEFORE_SWAP` (1<<7 = 0x80)
/// - `AFTER_SWAP` (1<<6 = 0x40)
/// - `BEFORE_SWAP_RETURNS_DELTA` (1<<3 = 0x08)
/// - `AFTER_SWAP_RETURNS_DELTA` (1<<2 = 0x04)
pub const AMOUNT_MODIFYING_HOOK_MASK: u16 = 0x80 | 0x40 | 0x08 | 0x04; // = 0xCC

/// V4 dynamic-fee flag. Pools with `fee == 0x0010_0000` have swap fees that
/// change between blocks; the solver assumes a fixed fee, so these are
/// excluded at registration.
pub const V4_DYNAMIC_FEE_FLAG: u32 = 0x0010_0000;

// ---------------------------------------------------------------------------
// V4 pool key
// ---------------------------------------------------------------------------

/// Identifies a V4 pool immutably (matches V4's `PoolKey` struct).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V4PoolKey {
    pub currency0: Address,
    pub currency1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub hooks: Address,
}

// ---------------------------------------------------------------------------
// Buffered liquidity update for unregistered V4 pools
// ---------------------------------------------------------------------------

/// A buffered `ModifyLiquidity` event for an unregistered V4 pool awaiting
/// registration. V4 uses a single `ModifyLiquidity` event covering both Mint
/// (positive delta) and Burn (negative delta).
#[derive(Clone, Debug)]
pub struct BufferedV4LiquidityUpdate {
    pub tick_lower: i32,
    pub tick_upper: i32,
    /// Signed liquidity delta (V4 emits `int256`; stored as `I256` then
    /// narrowed to `i128` at apply time — valid on-chain data always fits).
    pub liquidity_delta: I256,
    pub block_number: u64,
}

impl LiquidityEvent for BufferedV4LiquidityUpdate {
    fn block_number(&self) -> u64 {
        self.block_number
    }
}

// ---------------------------------------------------------------------------
// Registration params + swap update type
// ---------------------------------------------------------------------------

/// Parameters for registering a V4 pool with `BotState`.
#[derive(Clone, Debug)]
pub struct RegisterV4PoolParams {
    pub pool_manager: Address,
    pub pool_id: PoolId,
    pub pool_key: V4PoolKey,
    /// Pre-decoded hook flags bitmask. Pools with amount-modifying hooks
    /// (`hook_flags & 0xCC != 0`) or dynamic fees (`fee == 0x100000`) are
    /// rejected at registration — enforced by the Rust core
    /// (`BotState::register_v4_pool`) as a correctness floor, surfacing as
    /// `HookedPoolRejectedError` / `DynamicFeePoolRejectedError` at the seam.
    pub hook_flags: u16,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub tick_data: HashMap<i32, TickInfo>,
    pub update_block: u64,
    pub coverage: PoolTickCoverage,
}

/// Typed rejection from [`crate::bot_core::BotState::register_v4_pool`].
///
/// Pool admission is a *correctness floor*: the solver's V3-CL math assumes
/// no hook intervention, and a fixed fee. Per ADR-005 the standalone-core
/// target means a Rust consumer (no Python) must be protected, so the refusal
/// lives in the Rust core and surfaces at the `PyO3` seam as typed Python
/// exceptions (subclassing `ValueError` — a recoverable, per-candidate
/// decision) so Python classifies by type, not string matching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegisterV4PoolError {
    /// Pool carries an amount-modifying hook (`hook_flags & 0xCC != 0`).
    HookedPool { hook_flags: u16 },
    /// Pool uses a dynamic fee (`fee == 0x100000`).
    DynamicFee { fee: u32 },
    /// A pool with the same `(pool_manager, pool_id)` is already registered —
    /// a wiring/programming error, distinct from the two admission
    /// categories. Surfaces as a plain `PyValueError`.
    AlreadyRegistered {
        pool_manager: Address,
        pool_id: PoolId,
    },
}

/// Full V4 state overwrite applied by [`crate::bot_core::BotState::sync_v4_pool_state`].
///
/// Groups the five mutable state fields so `sync_v4_pool_state` stays under
/// clippy's argument limit — the pool is identified separately by
/// `(pool_manager, pool_id)`.
#[derive(Clone, Debug)]
pub struct V4StateSync {
    /// New sqrt price (Q64.96).
    pub sqrt_price_x96: U256,
    /// New active in-range liquidity.
    pub liquidity: u128,
    /// Current tick.
    pub tick: i32,
    /// Full tick-data overwrite (`liquidity_gross`/`liquidity_net`).
    pub tick_data: HashMap<i32, TickInfo>,
    /// Block number of this state update.
    pub update_block: u64,
}

/// A pre-decoded V4 Swap update for testing without log decoding.
#[derive(Clone, Debug)]
pub struct V4SwapUpdate {
    pub pool_manager: Address,
    pub pool_id: PoolId,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub tick_priors: Vec<(i32, TickInfo)>,
}

// ---------------------------------------------------------------------------
// V4 pool state
// ---------------------------------------------------------------------------

/// Cached tick ranges for a single pool, keyed by direction.
#[derive(Clone, Debug, Default)]
struct TickRangeCache {
    zfo: Option<Arc<[V3TickRangeForSolver]>>,
    ofz: Option<Arc<[V3TickRangeForSolver]>>,
}

/// V4 concentrated-liquidity pool state owned by [`crate::bot_core::BotState`].
/// Carries authoritative mutable state plus a per-pool reorg journal (same
/// `V3BlockDelta` shape — V4 `ModifyLiquidity` carries a signed liquidity
/// delta, but the journal records the same scalar + per-tick priors as V3).
#[derive(Debug)]
pub struct V4PoolState {
    pub pool_manager: Address,
    pub pool_id: PoolId,
    pub pool_key: V4PoolKey,

    // --- Mutable state (authoritative) ---
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub update_block: u64,

    /// Initialized ticks: tick index → (`liquidity_gross`, `liquidity_net`).
    pub tick_data: HashMap<i32, TickInfo>,
    pub coverage: PoolTickCoverage,

    /// Reorg journal — scalar priors + per-tick priors for rollback (V4 uses
    /// the same `V3BlockDelta` shape; `tick_priors` store `ModifyLiquidity` reversal
    /// data).
    pub journal: ReorgJournal<V3BlockDelta>,

    cached_tick_ranges: parking_lot::Mutex<TickRangeCache>,
}

impl Clone for V4PoolState {
    fn clone(&self) -> Self {
        Self {
            pool_manager: self.pool_manager,
            pool_id: self.pool_id,
            pool_key: self.pool_key.clone(),
            sqrt_price_x96: self.sqrt_price_x96,
            liquidity: self.liquidity,
            tick: self.tick,
            update_block: self.update_block,
            tick_data: self.tick_data.clone(),
            coverage: self.coverage,
            journal: self.journal.clone(),
            cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
        }
    }
}

impl V4PoolState {
    /// Construct from registration params with a journal of the given depth.
    #[must_use]
    pub fn from_params(params: RegisterV4PoolParams, journal_depth: usize) -> Self {
        Self {
            pool_manager: params.pool_manager,
            pool_id: params.pool_id,
            pool_key: params.pool_key,
            sqrt_price_x96: params.sqrt_price_x96,
            liquidity: params.liquidity,
            tick: params.tick,
            update_block: params.update_block,
            tick_data: params.tick_data,
            coverage: params.coverage,
            journal: ReorgJournal::<V3BlockDelta>::new(journal_depth),
            cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
        }
    }

    /// Invalidate the cached tick ranges (call after any state mutation).
    pub fn invalidate_tick_range_cache(&self) {
        let mut cache = self.cached_tick_ranges.lock();
        cache.zfo = None;
        cache.ofz = None;
    }

    fn get_cached_tick_ranges(&self, zero_for_one: bool) -> Option<Arc<[V3TickRangeForSolver]>> {
        {
            let cache = self.cached_tick_ranges.lock();
            let slot = if zero_for_one { &cache.zfo } else { &cache.ofz };
            if let Some(ranges) = slot {
                return Some(Arc::clone(ranges));
            }
        }

        let ranges = compute_tick_ranges(
            &self.tick_data,
            self.tick,
            self.pool_key.tick_spacing,
            self.liquidity,
            zero_for_one,
            15,
        )
        .map(|(ranges, _)| Arc::<[V3TickRangeForSolver]>::from(ranges));

        if let Some(ref r) = ranges {
            let mut cache = self.cached_tick_ranges.lock();
            if zero_for_one {
                cache.zfo = Some(Arc::clone(r));
            } else {
                cache.ofz = Some(Arc::clone(r));
            }
        }

        ranges
    }

    /// Build an integer tick-range sequence for the V4 pool. Identical CL math
    /// to V3's `build_int_v3_sequence`; the type is named `IntV3TickRange-
    /// Sequence` but applies equally to V4 hops.
    ///
    /// Returns `None` if insufficient tick data.
    #[must_use]
    pub fn build_int_v4_sequence(
        &self,
        zero_for_one: bool,
        max_ranges: usize,
    ) -> Option<IntV3TickRangeSequence> {
        let ranges = self.get_cached_tick_ranges(zero_for_one)?;
        let use_ranges = ranges.get(..ranges.len().min(max_ranges))?;

        let gamma_numer = u64::from(1_000_000 - self.pool_key.fee);
        let fee_denom = 1_000_000u64;

        let mut int_ranges = Vec::with_capacity(use_ranges.len());
        for (i, r) in use_ranges.iter().enumerate() {
            let sqrt_price_x96 = if i == 0 {
                self.sqrt_price_x96
            } else if zero_for_one {
                use_ranges[i - 1].sqrt_price_upper
            } else {
                use_ranges[i - 1].sqrt_price_lower
            };

            let range_liquidity = if i == 0 {
                self.liquidity
            } else {
                let mut l = self.liquidity.cast_signed();
                for prev_range in &use_ranges[..i] {
                    let net = prev_range.liquidity_net;
                    if zero_for_one {
                        l -= net;
                    } else {
                        l += net;
                    }
                }
                if l.is_negative() {
                    0u128
                } else {
                    l.cast_unsigned()
                }
            };

            int_ranges.push(IntV3TickRangeHop {
                liquidity: range_liquidity,
                sqrt_price_x96,
                sqrt_price_lower_x96: r.sqrt_price_lower,
                sqrt_price_upper_x96: r.sqrt_price_upper,
                gamma_numer,
                fee_denom,
                zero_for_one,
            });
        }

        IntV3TickRangeSequence::new(int_ranges).ok()
    }
}

// ---------------------------------------------------------------------------
// V4 single-pool swap simulation (ADR-003: "Pool's authority over its own math")
// ---------------------------------------------------------------------------
//
// V4 uses identical CL math to V3 with one sign-convention divergence: V3
// uses `amountSpecified > 0` for exact-in, V4 uses `amountSpecified < 0`.
// (Verified in `v3_simulator.py:93` — `exact_input = amount_specified > 0`
// for V3.) The V4 `compute_swap_step_v4` clamp + flow match V3 after the
// sign flip, so the simulator mirrors `v3_simulate_swap` delegating to
// `compute_swap_step_v4`. We reuse V3SwapOutcome (same amount0/amount1 shape).

/// Simulate a V4 swap, mirroring [`v3_simulate_swap`] but using V4's
/// `compute_swap_step_v4` (which takes the V4 sign convention).
///
/// `amount_specified` uses the V4 sign convention supplied by the caller:
/// negative = exact input, positive = exact output (opposite to V3). The
/// `BotState` `calculate_tokens_*` callers flip before calling so this stays
/// a pure V4 port.
#[must_use]
#[allow(clippy::too_many_lines)] // faithful port of V3/V4's `_calculate_swap`; mirroring `v3_simulate_swap`.
#[allow(unused_assignments)] // `tick` tracks the contract's post-step tick; faithful to the loop.
pub fn v4_simulate_swap(
    state: &V4PoolState,
    zero_for_one: bool,
    amount_specified: I256,
) -> Option<V3SwapOutcome> {
    if amount_specified.is_zero() {
        return None; // AS: zero amount (V3 reverts)
    }
    // V4 sign convention: `amount_specified < 0` = exact INPUT, `> 0` = exact OUTPUT
    // (opposite to V3). Matches Solidity V4 `Pool.sol:295`
    // `bool exactInput = params.amountSpecified < 0;` and `compute_swap_step_v4`'s
    // internal `amount_remaining < I256::ZERO` check. Verified against the
    // integer-exact oracle suite in `cl_lib::swap_math::tests`.
    let exact_in = amount_specified.is_negative();
    let sqrt_price_limit = if zero_for_one {
        U256::from(MIN_SQRT_RATIO) + U256::from(1u64) // 4295128740
    } else {
        U256::from(MAX_SQRT_RATIO) - U256::from(1u64)
    };

    let mut amount_specified_remaining = amount_specified;
    let mut amount_calculated = I256::ZERO;
    let mut sqrt_price_x96 = state.sqrt_price_x96;
    let mut tick = state.tick;
    let mut liquidity = i128::try_from(state.liquidity).ok()?;

    let fee_pips = U256::from(state.pool_key.fee);
    let tick_spacing = state.pool_key.tick_spacing;

    let ticks = gen_ticks(&state.tick_data, tick, tick_spacing, zero_for_one, 30_000).ok()?;

    for tick_along_path in ticks {
        if amount_specified_remaining.is_zero() || sqrt_price_x96 == sqrt_price_limit {
            break;
        }

        let mut tick_next = tick_along_path.tick;
        let initialized = tick_along_path.is_initialized;

        tick_next = if zero_for_one {
            tick_next.max(-887_272)
        } else {
            tick_next.min(887_272)
        };

        let sqrt_price_next = U256::from(get_sqrt_ratio_at_tick_internal(tick_next).ok()?);

        let sqrt_price_target = if (zero_for_one && sqrt_price_next < sqrt_price_limit)
            || (!zero_for_one && sqrt_price_next > sqrt_price_limit)
        {
            sqrt_price_limit
        } else {
            sqrt_price_next
        };

        let sqrt_price_start = sqrt_price_x96;
        let step = compute_swap_step_v4(
            sqrt_price_x96,
            sqrt_price_target,
            liquidity,
            amount_specified_remaining,
            fee_pips,
        )
        .ok()?;

        sqrt_price_x96 = step.sqrt_price_next;

        // V4 accounting — matches Uniswap V4 `Pool.sol::swap()` (lines 371-381):
        //   exactInput  (amountSpecified < 0): remaining += (amountIn + feeAmount); calculated += amountOut
        //   exactOutput (amountSpecified > 0): remaining -= amountOut;              calculated -= (amountIn + feeAmount)
        // The earlier code had both the flag polarity inverted (is_positive vs
        // V4's is_negative) AND the quantities swapped between remaining/
        // calculated — they partially cancelled, producing a ~1-wei over-count
        // instead of total breakage. See `contract_reference/uniswap/V4/PoolManager.sol`.
        if exact_in {
            let consumed = I256::try_from(step.amount_in.saturating_add(step.fee_amount)).ok()?;
            amount_specified_remaining = amount_specified_remaining.checked_add(consumed)?;
            amount_calculated =
                amount_calculated.checked_add(I256::try_from(step.amount_out).ok()?)?;
        } else {
            amount_specified_remaining =
                amount_specified_remaining.checked_sub(I256::try_from(step.amount_out).ok()?)?;
            let gross_input =
                I256::try_from(step.amount_in.saturating_add(step.fee_amount)).ok()?;
            amount_calculated = amount_calculated.checked_sub(gross_input)?;
        }

        if sqrt_price_x96 == sqrt_price_next {
            if initialized {
                if let Some(info) = state.tick_data.get(&tick_next) {
                    let liquidity_net_i256 = info.liquidity_net;
                    let liquidity_net: i128 = i128::try_from(liquidity_net_i256).ok()?;
                    let net = if zero_for_one {
                        -liquidity_net
                    } else {
                        liquidity_net
                    };
                    liquidity = liquidity.checked_add(net)?;
                    if liquidity < 0 {
                        return None; // LO: invariant violated
                    }
                }
            }
            tick = if zero_for_one {
                tick_next - 1
            } else {
                tick_next
            };
        } else if sqrt_price_x96 != sqrt_price_start {
            tick = get_tick_at_sqrt_ratio_internal(sqrt_price_x96.to::<U160>())
                .ok()?
                .as_i32();
        }
    }

    let input_consumed = amount_specified.checked_sub(amount_specified_remaining)?;

    // Solidity V4 `Pool.swap()` final delta assembly:
    //   if (zeroForOne != (amountSpecified < 0)) { (amount0, amount1) = (calculated,   consumed) }
    //   else                                    { (amount0, amount1) = (consumed, calculated) }
    let (amount0_signed, amount1_signed) = if zero_for_one == exact_in {
        (input_consumed, amount_calculated)
    } else {
        (amount_calculated, input_consumed)
    };

    Some(V3SwapOutcome {
        amount0: amount0_signed.unsigned_abs(),
        amount1: amount1_signed.unsigned_abs(),
    })
}

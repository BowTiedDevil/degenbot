//! V4 concentrated-liquidity pool state — the single BotState-owned home for
//! V4 pool data (ADR-003). Supersedes the engine-side `V4PoolState` that lived
//! in `solvers/v4_block_engine.rs`; that engine's path/solve subsystem is
//! deleted as orphan dead code (the unified `ArbitrageEngine` already handles V4
//! paths via `HopType::V4` in its `register_path`/`solve_path`).
//!
//! V4 shares identical CL math with V3 (same tick structure, same
//! `sqrtPriceX96`, same liquidity tracking). The `build_int_v4_sequence`
//! produces the same `IntV3TickRangeSequence` the V3 path solver consumes.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use alloy::primitives::{Address, I256, U160, U256};

use crate::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use crate::liquidity_event::LiquidityEvent;
use crate::state_history::{JournalError, ReorgJournal, ReorgPoolState, V3BlockDelta};
use crate::tick_bitmap::{compute_tick_ranges, gen_ticks, V3TickRangeForSolver};
use crate::tick_fetch::TickWordFetcher;
use crate::v3_state::{PoolTickCoverage, SimulateSwapError, V3SwapOutcome};
use crate::TickInfo;
use degenbot_cl_math::cl_lib::functions::tick_position;
use degenbot_cl_math::cl_lib::swap_math::compute_swap_step_v4;
use degenbot_cl_math::cl_lib::tick_math::{
    get_sqrt_ratio_at_tick_internal, get_tick_at_sqrt_ratio_internal,
};
use degenbot_decoders::v4_swap_decoder::V4PoolId;

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
    pub pool_id: V4PoolId,
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
    /// Sparse-tick backfill fetcher (stored on `V4PoolState` at registration;
    /// `None` for `Tracked` pools or when no Python fetcher was supplied).
    /// ADR-006 I/O trait object — same shape as V3.
    pub fetcher: Option<Arc<dyn TickWordFetcher>>,
}

/// Typed rejection from [`crate::BotState::register_v4_pool`].
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
        pool_id: V4PoolId,
    },
    /// An out-of-spec field (`sqrt_price_x96` / `tick` / V4 `fee` /
    /// `tick_spacing`) — wraps a [`spec_bounds::SpecViolation`] from the
    /// validator helpers (`validate_sqrt_price` / `validate_tick` /
    /// `validate_v4_fee` / `validate_tick_spacing`); the four V4 spec checks
    /// fire together ahead of the hooked / dynamic-fee rejections. Mirrors
    /// the V2 (`RegisterV2PoolError::SpecViolation`, MSTAT2) and V3
    /// (`RegisterV3PoolError::SpecViolation`, 24KNGF) twins.
    SpecViolation(crate::spec_bounds::SpecViolation),
}

impl From<crate::spec_bounds::SpecViolation> for RegisterV4PoolError {
    fn from(v: crate::spec_bounds::SpecViolation) -> Self {
        Self::SpecViolation(v)
    }
}

/// Full V4 state overwrite applied by [`crate::BotState::sync_v4_pool_state`].
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
    pub pool_id: V4PoolId,
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

/// Immutable V4 registration identity (ADR-005 identity slice).
///
/// Pure registration data — the pool manager, on-chain `pool_id`, and
/// the V4 pool key (`currency0/currency1/fee/tick_spacing/hooks`). Set once at
/// `register_v4_pool` and never mutated. Mirrors `V3PoolIdentity`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V4PoolIdentity {
    pub pool_manager: Address,
    pub pool_id: V4PoolId,
    pub pool_key: V4PoolKey,
}

/// V4 concentrated-liquidity pool state owned by [`crate::BotState`].
/// Carries authoritative mutable state plus a per-pool reorg journal (same
/// `V3BlockDelta` shape — V4 `ModifyLiquidity` carries a signed liquidity
/// delta, but the journal records the same scalar + per-tick priors as V3).
/// Immutable identity lives on [`V4PoolIdentity`]; look it up via
/// [`crate::BotState::get_v4_identity`].
#[derive(Debug)]
pub struct V4PoolState {
    // --- Mutable state (authoritative) ---
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub update_block: u64,

    /// Initialized ticks: tick index → (`liquidity_gross`, `liquidity_net`).
    pub tick_data: HashMap<i32, TickInfo>,
    pub coverage: PoolTickCoverage,

    /// The pinned snapshot seed (CBCH6H — see `V3PoolState::snapshot_seed`).
    /// A copy of the registration `tick_data`, immutable across pump
    /// `ModifyLiquidity` events, retained so step-1 verify compares the seed
    /// vs on-chain@snapshot_block. `Some` only for `Tracked` pools; cleared by
    /// `take_v4_snapshot_seed` after step-1 verify.
    pub snapshot_seed: Option<HashMap<i32, TickInfo>>,

    /// The pinned **post-drain** `(tick_data, block)` pair (step-2 rolling-start
    /// race fix, V4 twin of `V3PoolState::post_drain_snapshot`). Captured
    /// atomically with `apply_buffer_v4`'s final drain by
    /// `pin_v4_post_drain_snapshot`, consumed once by
    /// `take_v4_post_drain_snapshot` for step-2 verify. The pair carries the
    /// frozen `tick_data` AND the `update_block` it was computed at; step-2
    /// verify compares against on-chain@**the pinned block**, not a
    /// start()-time `verify_backfill_block` constant (see
    /// `V3PoolState::post_drain_snapshot` for the full rationale). `Some` only
    /// for `Tracked` pools.
    pub post_drain_snapshot: Option<(HashMap<i32, TickInfo>, u64)>,

    /// Tick-bitmap word positions known to be fetched. See
    /// [`V3PoolState::known_bitmap_words`] — V4 shares the same sparse
    /// miss-detection discipline (ADR-005 sparse-map feature parity).
    pub known_bitmap_words: HashSet<i32>,

    /// The sparse-tick backfill fetcher (ADR-006 I/O trait object). Set once
    /// at `register_v4_pool`; the fetch+retry loop in
    /// `calculate_tokens_out_with_fetch` / `simulate_*_with_fetch` clones
    /// this `Arc` out and calls it on a `MissingTickWord` miss. `None` for
    /// `Tracked` pools (complete tick data) or when no Python fetcher was
    /// supplied at registration.
    pub fetcher: Option<Arc<dyn TickWordFetcher>>,

    /// Reorg journal — scalar priors + per-tick priors for rollback (V4 uses
    /// the same `V3BlockDelta` shape; `tick_priors` store `ModifyLiquidity` reversal
    /// data).
    pub journal: ReorgJournal<V3BlockDelta>,

    cached_tick_ranges: parking_lot::Mutex<TickRangeCache>,
}

impl Clone for V4PoolState {
    fn clone(&self) -> Self {
        Self {
            sqrt_price_x96: self.sqrt_price_x96,
            liquidity: self.liquidity,
            tick: self.tick,
            update_block: self.update_block,
            tick_data: self.tick_data.clone(),
            coverage: self.coverage,
            known_bitmap_words: self.known_bitmap_words.clone(),
            fetcher: self.fetcher.clone(),
            journal: self.journal.clone(),
            // Clones (e.g. `v4_pools_snapshot()`) do NOT carry the pinned seed or
            // the pinned post-drain snapshot (CBCH6H + step-2 race fix — see
            // V3PoolState::Clone).
            snapshot_seed: None,
            post_drain_snapshot: None,
            cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
        }
    }
}

impl V4PoolState {
    /// Word position holding `tick` (V3/V4-shared helper).
    #[must_use]
    pub fn word_of(tick: i32, tick_spacing: i32) -> i32 {
        i32::from(tick_position(tick.div_euclid(tick_spacing)).0)
    }

    /// Seed `known_bitmap_words` from the current `tick_data` keys' word
    /// positions (Sparse pools only; Tracked bypasses detection).
    pub fn seed_known_bitmap_words(&mut self, tick_spacing: i32) {
        self.known_bitmap_words = self
            .tick_data
            .keys()
            .map(|&t| Self::word_of(t, tick_spacing))
            .collect();
    }

    // `merge_tick_word` lives on the `ConcentratedLiquidityPoolMut` trait
    // (ADR-017 slice 1) — the body was the byte-identical twin of
    // `V3PoolState::merge_tick_word`; the trait dedups the two. See
    // `impl ConcentratedLiquidityPoolMut for V4PoolState` in `registry.rs`.

    /// Construct from registration params with a journal of the given depth.
    #[must_use]
    pub fn from_params(
        params: RegisterV4PoolParams,
        journal_depth: usize,
    ) -> (V4PoolIdentity, V4PoolState) {
        let tick_spacing = params.pool_key.tick_spacing;
        let mut state = Self {
            sqrt_price_x96: params.sqrt_price_x96,
            liquidity: params.liquidity,
            tick: params.tick,
            update_block: params.update_block,
            tick_data: params.tick_data,
            coverage: params.coverage,
            known_bitmap_words: HashSet::new(),
            fetcher: params.fetcher,
            journal: ReorgJournal::<V3BlockDelta>::new(journal_depth),
            snapshot_seed: None,
            post_drain_snapshot: None,
            cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
        };
        let identity = V4PoolIdentity {
            pool_manager: params.pool_manager,
            pool_id: params.pool_id,
            pool_key: params.pool_key,
        };
        // Seed the known-bitmap-word set from the tick_data keys for EVERY
        // coverage (mirrors `sync_tick_data_by_pool_id`). Pre-fix this only ran
        // for `Sparse`, so a `Tracked` inline seed (ADR-006 race closure)
        // relied on a later separate `update_tick_data` — the clobber the
        // closure removes. Seeding here makes the inline seed complete.
        state.seed_known_bitmap_words(tick_spacing);
        // CBCH6H: pin the snapshot seed for Tracked pools so step-1 verify
        // compares the seed (not pump-mutated `tick_data`) against
        // on-chain@snapshot_block. Sparse pools have no complete seed.
        state.snapshot_seed = if params.coverage == PoolTickCoverage::Tracked {
            Some(state.tick_data.clone())
        } else {
            None
        };
        (identity, state)
    }

    // `apply_swap` + `apply_liquidity_update` live on the
    // `ConcentratedLiquidityPoolMut` trait (ADR-017 slice 2) — the bodies
    // were byte-identical twins across V3/V4; the trait dedups the two.
    // See `impl ConcentratedLiquidityPoolMut for V4PoolState` in `registry.rs`.

    /// Invalidate the cached tick ranges (call after any state mutation).
    pub fn invalidate_tick_range_cache(&self) {
        let mut cache = self.cached_tick_ranges.lock();
        cache.zfo = None;
        cache.ofz = None;
    }

    fn get_cached_tick_ranges(
        &self,
        tick_spacing: i32,
        zero_for_one: bool,
    ) -> Option<Arc<[V3TickRangeForSolver]>> {
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
            tick_spacing,
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
        tick_spacing: i32,
        fee: u32,
        zero_for_one: bool,
        max_ranges: usize,
    ) -> Option<IntV3TickRangeSequence> {
        let ranges = self.get_cached_tick_ranges(tick_spacing, zero_for_one)?;
        let use_ranges = ranges.get(..ranges.len().min(max_ranges))?;

        let gamma_numer = u64::from(1_000_000 - fee);
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

// ADR-016 — pool-owned reorg rollback for the CL family. The field-write
// previously duplicated across `BotState::v3_restore_before_block` /
// `v4_restore_before_block` (scalar-priors write-back + tick reverse-apply)
// is absorbed into the state struct; `V3RestoreResult` stays internal to
// this impl and never escapes. The CL journal's `restore_before_block`
// returns `V3RestoreResult` directly (panics on empty — the empty-journal
// case is an invariant violation for a registered pool, not a recoverable
// error), so this impl returns `Ok(())` after applying the landed-at state.
// Byte-identical to the V3 impl modulo the struct name. See ADR-016 D4.
impl ReorgPoolState for V4PoolState {
    fn restore_before_block(&mut self, block: u64) -> Result<(), JournalError> {
        let result = self.journal.restore_before_block(block);

        // Sync scalar fields if the rolled-back range had scalar changes.
        // If scalar_priors is None (tick-only event(s) rolled back), the
        // current slot0 scalars were never changed by the rolled-back events
        // and are already correct — skip the write-back. See ADR-004.
        if let Some(p) = &result.scalar_priors {
            self.sqrt_price_x96 = p.sqrt_price_x96_before;
            self.liquidity = p.liquidity_before;
            self.tick = p.tick_before;
        }
        self.update_block = result.block;
        self.invalidate_tick_range_cache();

        // Reverse-apply tick priors accumulated across all popped deltas.
        for (tick_idx, tick_before) in &result.tick_priors {
            match tick_before.liquidity_gross_before {
                Some(gross_before) => {
                    // Tick existed before — restore its prior values.
                    self.tick_data.insert(
                        *tick_idx,
                        TickInfo {
                            liquidity_gross: gross_before,
                            liquidity_net: tick_before.liquidity_net_before,
                            block: 0,
                        },
                    );
                }
                None => {
                    // Tick was newly initialized in this block — remove it.
                    self.tick_data.remove(tick_idx);
                }
            }
        }

        Ok(())
    }

    fn discard_before_block(&mut self, block: u64) -> Result<(), JournalError> {
        self.journal.discard_before_block(block)
    }

    fn journal_len(&self) -> usize {
        self.journal.len()
    }

    fn newest_block(&self) -> Option<u64> {
        self.journal.newest_block()
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
///
/// # Errors
///
/// Same discipline as [`v3_simulate_swap`]: [`SimulateSwapError::NotComputable`]
/// for zero amount / overflow / invariant violations;
/// [`SimulateSwapError::MissingTickWord(w)`] when `coverage == Sparse` and the
/// walk entered an unfetched tick-bitmap word (caller fetches + retries).
#[allow(clippy::too_many_lines)] // faithful port of V3/V4's `_calculate_swap`; mirroring `v3_simulate_swap`.
#[allow(unused_assignments)] // `tick` tracks the contract's post-step tick; faithful to the loop.
pub fn v4_simulate_swap(
    state: &V4PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: U256,
) -> Result<V3SwapOutcome, SimulateSwapError> {
    if amount_specified.is_zero() {
        return Err(SimulateSwapError::NotComputable); // AS: zero amount (V3 reverts)
    }
    // V4 sign convention: `amount_specified < 0` = exact INPUT, `> 0` = exact OUTPUT
    // (opposite to V3). Matches Solidity V4 `Pool.sol:295`
    // `bool exactInput = params.amountSpecified < 0;` and `compute_swap_step_v4`'s
    // internal `amount_remaining < I256::ZERO` check. Verified against the
    // integer-exact oracle suite in `cl_lib::swap_math::tests`.
    let exact_in = amount_specified.is_negative();

    let mut amount_specified_remaining = amount_specified;
    let mut amount_calculated = I256::ZERO;
    let mut sqrt_price_x96 = state.sqrt_price_x96;
    let mut tick = state.tick;
    let mut liquidity =
        i128::try_from(state.liquidity).map_err(|_| SimulateSwapError::NotComputable)?;

    let fee_pips = U256::from(fee);
    // `tick_spacing` is the immutable-config parameter (threaded from identity).

    // Sparse-map miss detection (V3/V4-shared; see `v3_simulate_swap`).
    let sparse = state.coverage == PoolTickCoverage::Sparse;
    if sparse
        && !state
            .known_bitmap_words
            .contains(&V4PoolState::word_of(tick, tick_spacing))
    {
        return Err(SimulateSwapError::MissingTickWord(V4PoolState::word_of(
            tick,
            tick_spacing,
        )));
    }

    let ticks = gen_ticks(&state.tick_data, tick, tick_spacing, zero_for_one, 30_000)
        .map_err(|_| SimulateSwapError::NotComputable)?;

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

        let sqrt_price_next = U256::from(
            get_sqrt_ratio_at_tick_internal(tick_next)
                .map_err(|_| SimulateSwapError::NotComputable)?,
        );

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
        .map_err(|_| SimulateSwapError::NotComputable)?;

        sqrt_price_x96 = step.sqrt_price_next;

        // V4 accounting — matches Uniswap V4 `Pool.sol::swap()` (lines 371-381):
        //   exactInput  (amountSpecified < 0): remaining += (amountIn + feeAmount); calculated += amountOut
        //   exactOutput (amountSpecified > 0): remaining -= amountOut;              calculated -= (amountIn + feeAmount)
        // The earlier code had both the flag polarity inverted (is_positive vs
        // V4's is_negative) AND the quantities swapped between remaining/
        // calculated — they partially cancelled, producing a ~1-wei over-count
        // instead of total breakage. See `contract_reference/uniswap/V4/PoolManager.sol`.
        if exact_in {
            let consumed = I256::try_from(step.amount_in.saturating_add(step.fee_amount))
                .map_err(|_| SimulateSwapError::NotComputable)?;
            amount_specified_remaining = amount_specified_remaining
                .checked_add(consumed)
                .ok_or(SimulateSwapError::NotComputable)?;
            amount_calculated = amount_calculated
                .checked_add(
                    I256::try_from(step.amount_out)
                        .map_err(|_| SimulateSwapError::NotComputable)?,
                )
                .ok_or(SimulateSwapError::NotComputable)?;
        } else {
            amount_specified_remaining = amount_specified_remaining
                .checked_sub(
                    I256::try_from(step.amount_out)
                        .map_err(|_| SimulateSwapError::NotComputable)?,
                )
                .ok_or(SimulateSwapError::NotComputable)?;
            let gross_input = I256::try_from(step.amount_in.saturating_add(step.fee_amount))
                .map_err(|_| SimulateSwapError::NotComputable)?;
            amount_calculated = amount_calculated
                .checked_sub(gross_input)
                .ok_or(SimulateSwapError::NotComputable)?;
        }

        if sqrt_price_x96 == sqrt_price_next {
            // Gated sparse miss-detection — see `v3_simulate_swap`: only fire
            // when the walk actually crosses into `tick_next`'s word, so an
            // unreached proposed boundary does not false-trigger.
            if sparse
                && !state
                    .known_bitmap_words
                    .contains(&V4PoolState::word_of(tick_next, tick_spacing))
            {
                return Err(SimulateSwapError::MissingTickWord(V4PoolState::word_of(
                    tick_next,
                    tick_spacing,
                )));
            }
            if initialized {
                if let Some(info) = state.tick_data.get(&tick_next) {
                    let liquidity_net_i256 = info.liquidity_net;
                    let liquidity_net: i128 = i128::try_from(liquidity_net_i256)
                        .map_err(|_| SimulateSwapError::NotComputable)?;
                    let net = if zero_for_one {
                        -liquidity_net
                    } else {
                        liquidity_net
                    };
                    liquidity = liquidity
                        .checked_add(net)
                        .ok_or(SimulateSwapError::NotComputable)?;
                    if liquidity < 0 {
                        return Err(SimulateSwapError::NotComputable); // LO: invariant violated
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
                .map_err(|_| SimulateSwapError::NotComputable)?
                .as_i32();
            // Slice-4 fix: an amount-capped step (the ELSE branch — the step
            // moved the price but did not reach `sqrt_price_next`) can land
            // inside an UNFETCHED word whose initialized ticks `gen_ticks`
            // never proposed (they are absent from `tick_data`). The CROSS
            // branch above guards its `tick_next`, but this branch derived
            // `tick` from the price with no word-knownness check — so the walk
            // could terminate having skipped that word's liquidity-nets,
            // producing a short output (the V4 `test_cached_calculations`
            // divergence on multi-word ofz swaps). Raise `MissingTickWord` so
            // the fetch+retry loop backfills the word; on re-run `gen_ticks`
            // proposes its ticks as `init=true` and they are crossed + applied
            // like the dense path. Proven by the captured corpus fixture
            // (`test_rust_v4_sparse_fetch_corpus_diverges_from_dense`).
            if sparse
                && !state
                    .known_bitmap_words
                    .contains(&V4PoolState::word_of(tick, tick_spacing))
            {
                return Err(SimulateSwapError::MissingTickWord(V4PoolState::word_of(
                    tick,
                    tick_spacing,
                )));
            }
        }
    }

    let input_consumed = amount_specified
        .checked_sub(amount_specified_remaining)
        .ok_or(SimulateSwapError::NotComputable)?;

    // Solidity V4 `Pool.swap()` final delta assembly:
    //   if (zeroForOne != (amountSpecified < 0)) { (amount0, amount1) = (calculated,   consumed) }
    //   else                                    { (amount0, amount1) = (consumed, calculated) }
    let (amount0_signed, amount1_signed) = if zero_for_one == exact_in {
        (input_consumed, amount_calculated)
    } else {
        (amount_calculated, input_consumed)
    };

    Ok(V3SwapOutcome {
        amount0: amount0_signed.unsigned_abs(),
        amount1: amount1_signed.unsigned_abs(),
        sqrt_price_x96,
        liquidity: u128::try_from(liquidity.max(0)).unwrap_or(0),
        tick,
    })
}

// ===========================================================================
// Tests for the relocated apply methods (ADR-014 D1 — CL half of Q1).
// V4 shares identical CL math + the V3BlockDelta journal type with V3, so the
// apply contract is the same shape (scalars-on-swap, tick-only-on-mint). These
// exercise `V4PoolState::apply_swap` / `apply_liquidity_update` directly.
// ===========================================================================
#[cfg(test)]
mod apply_inherent_tests {
    #![allow(unused_imports)]
    use super::*;
    use crate::registry::ConcentratedLiquidityPoolMut;
    use crate::state_history::{ReorgJournal, V3BlockDelta};
    use crate::v3_state::PoolTickCoverage;
    use crate::TickInfo;
    use alloy::primitives::{Address, I256, U128, U256};
    use std::collections::{HashMap, HashSet};

    /// Minimal V4 state at tick 0, 1:1 price, liquidity `liq`, with a
    /// [-60, +60] position. The journal is fresh (depth 8); snapshot fields
    /// are `None`. `tick_spacing` 60 (read off the constructed `pool_key`).
    fn state_with_position(liq: u128) -> V4PoolState {
        let sp_0 = U256::from(1u128) << 96;
        let liq_u128 = U256::from(liq).to::<U128>();
        let mut tick_data = HashMap::new();
        tick_data.insert(
            -60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: I256::try_from(i128::try_from(liq).unwrap()).unwrap(),
                block: 0,
            },
        );
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: I256::try_from(-i128::try_from(liq).unwrap()).unwrap(),
                block: 0,
            },
        );
        V4PoolState {
            sqrt_price_x96: sp_0,
            liquidity: liq,
            tick: 0,
            update_block: 0,
            tick_data,
            coverage: PoolTickCoverage::Tracked,
            known_bitmap_words: HashSet::new(),
            fetcher: None,
            journal: ReorgJournal::<V3BlockDelta>::new(8),
            snapshot_seed: None,
            post_drain_snapshot: None,
            cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
        }
    }

    #[test]
    fn apply_swap_updates_scalars_advances_block_and_journals_priors() {
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);

        let new_tick_info = TickInfo {
            liquidity_gross: U256::from(500u64).to::<U128>(),
            liquidity_net: I256::try_from(500i128).unwrap(),
            block: 7,
        };
        let tick_priors = vec![(100, new_tick_info.clone())];

        let new_sqrt = U256::from(2u128) << 96;
        let before_len = state.journal.len();

        state.apply_swap(new_sqrt, liq + 1, 1, 7, &tick_priors);

        assert_eq!(state.sqrt_price_x96, new_sqrt);
        assert_eq!(state.liquidity, liq + 1);
        assert_eq!(state.tick, 1);
        assert_eq!(state.update_block, 7);
        assert_eq!(state.tick_data.get(&100), Some(&new_tick_info));
        assert_eq!(state.journal.len(), before_len + 1);
        assert_eq!(state.journal.newest_block(), Some(7));
        {
            let cache = state.cached_tick_ranges.lock();
            assert!(cache.zfo.is_none());
            assert!(cache.ofz.is_none());
        }
    }

    #[test]
    fn merge_tick_word_via_trait_merges_ticks_marks_word_known_invalidates_cache() {
        // ADR-017 slice 1: `merge_tick_word` lives on the
        // `ConcentratedLiquidityPoolMut` trait. Verify the trait-dispatch path
        // lands ticks + marks the word known + invalidates the cached ranges
        // (the same observable behavior the prior inherent method had, now
        // reached through `&mut dyn ConcentratedLiquidityPoolMut`).
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        // Seed the cache so the invalidate assertion is observable.
        let _ = state.build_int_v4_sequence(60, 3_000, true, 15);
        let fetched = crate::tick_fetch::FetchedTickWord {
            word: 7,
            ticks: {
                let mut m = HashMap::new();
                m.insert(
                    420,
                    TickInfo {
                        liquidity_gross: U256::from(500u64).to::<U128>(),
                        liquidity_net: I256::try_from(500i128).unwrap(),
                        block: 9,
                    },
                );
                m
            },
        };
        let before_len = state.journal.len();

        let cl: &mut dyn ConcentratedLiquidityPoolMut = &mut state;
        let applied = cl.merge_tick_word(&fetched);

        assert!(applied, "trait merge_tick_word returns true on a CL pool");
        assert_eq!(
            state.tick_data.get(&420),
            Some(&TickInfo {
                liquidity_gross: U256::from(500u64).to::<U128>(),
                liquidity_net: I256::try_from(500i128).unwrap(),
                block: 9,
            }),
            "fetched tick landed in tick_data"
        );
        assert!(
            state.known_bitmap_words.contains(&7),
            "fetched word marked known"
        );
        assert_eq!(
            state.journal.len(),
            before_len,
            "merge_tick_word never journals (no rollback delta)"
        );
        {
            let cache = state.cached_tick_ranges.lock();
            assert!(
                cache.zfo.is_none() && cache.ofz.is_none(),
                "cache invalidated"
            );
        }
    }

    #[test]
    fn apply_liquidity_update_mutates_ticks_advances_block_without_changing_scalars() {
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);

        let sp_before = state.sqrt_price_x96;
        let liq_before = state.liquidity;
        let tick_before = state.tick;
        let prior_lower = state.tick_data.get(&-60).cloned().unwrap();
        let prior_upper = state.tick_data.get(&60).cloned().unwrap();

        let delta = 123_456i128;
        let before_len = state.journal.len();

        state.apply_liquidity_update(-60, 60, delta, 9);

        let after_lower = state.tick_data.get(&-60).unwrap();
        let after_upper = state.tick_data.get(&60).unwrap();
        assert_eq!(
            after_lower.liquidity_gross,
            prior_lower.liquidity_gross + U256::from(u128::try_from(delta).unwrap()).to::<U128>()
        );
        assert_eq!(
            after_upper.liquidity_gross,
            prior_upper.liquidity_gross + U256::from(u128::try_from(delta).unwrap()).to::<U128>()
        );
        assert_eq!(
            after_lower.liquidity_net,
            prior_lower.liquidity_net + I256::try_from(delta).unwrap()
        );
        assert_eq!(
            after_upper.liquidity_net,
            prior_upper.liquidity_net - I256::try_from(delta).unwrap()
        );
        assert_eq!(state.update_block, 9);
        assert_eq!(state.sqrt_price_x96, sp_before);
        assert_eq!(state.liquidity, liq_before);
        assert_eq!(state.tick, tick_before);
        assert_eq!(state.journal.len(), before_len + 1);
        assert_eq!(state.journal.newest_block(), Some(9));
    }

    #[test]
    fn replace_tick_data_swaps_map_advances_block_and_invalidates_cache() {
        // V4 twin of the V3 test — proves the trait method dispatches onto
        // V4PoolState through dyn ConcentratedLiquidityPoolMut.
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        assert_eq!(state.update_block, 0);

        let mut new_data = std::collections::HashMap::new();
        new_data.insert(
            120,
            TickInfo {
                liquidity_gross: U256::from(7u64).to::<U128>(),
                liquidity_net: I256::try_from(7i128).unwrap(),
                block: 5,
            },
        );

        state.replace_tick_data(new_data, 5, 60);

        assert_eq!(state.update_block, 5);
        assert!(state.tick_data.contains_key(&120));
        assert!(!state.tick_data.contains_key(&-60));
        {
            let cache = state.cached_tick_ranges.lock();
            assert!(cache.zfo.is_none());
            assert!(cache.ofz.is_none());
        }
    }

    // ------------------------------------------------------------------
    // ReorgPoolState trait (ADR-016 D4 — CL family adopts the trait).
    // `restore_before_block` returns `()`; the landed-at state lives in the
    // struct's own fields (read-after-restore). `V3RestoreResult` stays
    // internal to the impl and never escapes. Byte-identical to the V3 impl.
    // ------------------------------------------------------------------

    #[test]
    fn restore_before_block_writes_landed_at_scalars_and_restore_point_block() {
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        let pre_sqrt = state.sqrt_price_x96;
        let pre_liq = state.liquidity;
        let pre_tick = state.tick;

        let new_sqrt = U256::from(2u128) << 96;
        state.apply_swap(new_sqrt, liq + 1, 1, 7, &[]);
        assert_eq!(state.update_block, 7);

        let result: Result<(), JournalError> = state.restore_before_block(7);
        assert!(result.is_ok());
        assert_eq!(state.sqrt_price_x96, pre_sqrt);
        assert_eq!(state.liquidity, pre_liq);
        assert_eq!(state.tick, pre_tick);
        assert_eq!(
            state.update_block, 7,
            "update_block set to the restore-point block (result.block)"
        );
        assert_eq!(state.journal_len(), 0, "swap delta popped");
    }

    #[test]
    fn restore_before_block_no_op_when_newest_delta_before_target() {
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        let new_sqrt = U256::from(2u128) << 96;
        state.apply_swap(new_sqrt, liq + 1, 1, 7, &[]);
        let post_sqrt = state.sqrt_price_x96;

        let result: Result<(), JournalError> = state.restore_before_block(8);
        assert!(result.is_ok());
        assert_eq!(
            state.sqrt_price_x96, post_sqrt,
            "no rollback when newest delta is before the target"
        );
        assert_eq!(state.journal_len(), 1, "delta not popped");
    }

    #[test]
    fn restore_before_block_removes_newly_initialized_tick_on_rollback() {
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        let new_tick = TickInfo {
            liquidity_gross: U256::from(500u64).to::<U128>(),
            liquidity_net: I256::try_from(500i128).unwrap(),
            block: 7,
        };
        let new_sqrt = U256::from(2u128) << 96;
        state.apply_swap(new_sqrt, liq + 1, 1, 7, &[(100, new_tick)]);
        assert!(
            state.tick_data.contains_key(&100),
            "new tick seeded by swap"
        );

        let result: Result<(), JournalError> = state.restore_before_block(7);
        assert!(result.is_ok());
        assert!(
            !state.tick_data.contains_key(&100),
            "newly-initialized tick removed on rollback"
        );
    }

    #[test]
    fn discard_before_block_drops_old_deltas_without_mutating_live_state() {
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        state.journal.push_delta(V3BlockDelta {
            block: 0,
            scalar_priors: None,
            tick_priors: Vec::new(),
        });
        let new_sqrt = U256::from(2u128) << 96;
        state.apply_swap(new_sqrt, liq + 1, 1, 7, &[]);
        assert_eq!(state.journal_len(), 2);
        let post_sqrt = state.sqrt_price_x96;

        let result: Result<(), JournalError> = state.discard_before_block(7);
        assert!(result.is_ok());
        assert_eq!(state.journal_len(), 1, "genesis dropped, swap kept");
        assert_eq!(
            state.sqrt_price_x96, post_sqrt,
            "discard trims history only — live state untouched"
        );
    }

    #[test]
    fn journal_len_reports_delta_count() {
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        assert_eq!(state.journal_len(), 0);
        state.apply_swap(U256::from(2u128) << 96, liq + 1, 1, 7, &[]);
        assert_eq!(state.journal_len(), 1);
    }
}

//! V3 concentrated-liquidity pool state — the single BotState-owned home for
//! V3 pool data (ADR-003). Supersedes the engine-side `V3PoolState` that lived
//! in `solvers/v3_block_engine.rs`; that engine is dissolved and V3 state
//! is owned by [`crate::bot_core::BotState`], peer to `UniswapEngine`.
//!
//! This struct carries both the authoritative mutable state (`sqrt_price_x96`,
//! `liquidity`, `tick`, `tick_data`), the snapshot-coverage flag, the lazy
//! tick-range derivation cache (`cached_tick_ranges`, shared infra consumed
//! by `build_int_v3_sequence`), and the per-pool reorg journal
//! ([`ReorgJournal`] of [`V3BlockDelta`]).

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use alloy::primitives::{Address, I256, U160, U256};

use crate::bot_core::state_history::{ReorgJournal, V3BlockDelta};
use crate::bot_core::tick_bitmap::{compute_tick_ranges, gen_ticks, V3TickRangeForSolver};
use crate::bot_core::TickInfo;
use crate::solvers::mobius_v3_int::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_cl_math::cl_lib::functions::tick_position;
use degenbot_cl_math::cl_lib::swap_math::compute_swap_step_v3;
use degenbot_cl_math::cl_lib::tick_math::{
    get_sqrt_ratio_at_tick_internal, get_tick_at_sqrt_ratio_internal, MAX_SQRT_RATIO,
    MIN_SQRT_RATIO,
};

// ---------------------------------------------------------------------------
// Coverage flag
// ---------------------------------------------------------------------------

/// Describes the completeness of tick data for a registered V3 pool.
///
/// `Tracked` means the snapshot provided complete tick data (may be empty =
/// genuinely illiquid). `Sparse` means no snapshot data exists for this pool
/// — solver results may contain errors or phantom profits.
///
/// Moved from `solvers/uniswap_engine/mod.rs` to live with V3 state under
/// ADR-003; re-exported from `uniswap_engine` for back-compat with callers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolTickCoverage {
    /// Snapshot provided complete tick data. Solver results are trustworthy.
    Tracked,
    /// No snapshot data exists. Solver results may be inaccurate.
    Sparse,
}

// ---------------------------------------------------------------------------
// Buffered liquidity update for unregistered V3 pools
// ---------------------------------------------------------------------------

/// A buffered liquidity update (Mint or Burn) for an unregistered V3 pool
/// awaiting registration. Stores raw event data (not collapsed) so future
/// reorg handling can reverse-apply.
#[derive(Clone, Debug)]
pub struct BufferedV3LiquidityUpdate {
    /// The tick lower boundary of the position.
    pub tick_lower: i32,
    /// The tick upper boundary of the position.
    pub tick_upper: i32,
    /// The signed liquidity delta: positive for Mint, negative for Burn.
    pub liquidity_delta: i128,
    /// The block number of this event.
    pub block_number: u64,
}

impl crate::solvers::liquidity_event_buffer::LiquidityEvent for BufferedV3LiquidityUpdate {
    fn block_number(&self) -> u64 {
        self.block_number
    }
}

// ---------------------------------------------------------------------------
// Registration params
// ---------------------------------------------------------------------------

/// Parameters for registering a V3 pool with `BotState`.
///
/// Bundles all fields to satisfy `clippy::too_many_arguments`.
#[derive(Clone, Debug)]
pub struct RegisterV3PoolParams {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub factory: Address,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub tick_data: HashMap<i32, TickInfo>,
    pub update_block: u64,
    /// Whether tick data came from the snapshot (`Tracked`) or has no
    /// snapshot coverage (`Sparse`). The buffer is always applied — the
    /// snapshot is always stale data from the DB.
    pub coverage: PoolTickCoverage,
}

/// A pre-decoded V3 Swap update for testing without log decoding.
#[derive(Clone, Debug)]
pub struct V3SwapUpdate {
    pub pool_address: Address,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub tick_priors: Vec<(i32, TickInfo)>,
}

// ---------------------------------------------------------------------------
// V3 pool state
// ---------------------------------------------------------------------------

/// Cached tick ranges for a single pool, keyed by direction.
#[derive(Clone, Debug, Default)]
struct TickRangeCache {
    zfo: Option<Arc<[V3TickRangeForSolver]>>,
    ofz: Option<Arc<[V3TickRangeForSolver]>>,
}

/// V3 concentrated-liquidity pool state owned by [`crate::bot_core::BotState`].
///
/// Carries authoritative mutable state plus a per-pool reorg journal. Swap
/// calculations read current mutable fields directly (never touch the journal);
/// `apply_swap`/`apply_liquidity_update` push reverse-apply deltas.
#[derive(Debug)]
pub struct V3PoolState {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub factory: Address,

    // --- Mutable state (authoritative) ---
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub update_block: u64,

    /// Initialized ticks: tick index → (`liquidity_gross`, `liquidity_net`).
    pub tick_data: HashMap<i32, TickInfo>,

    /// Whether the snapshot provided complete tick data for this pool.
    pub coverage: PoolTickCoverage,

    /// Tick-bitmap word positions (`tick_position(tick.div_euclid(tick_spacing))`)
    /// whose bitmap has been fetched and is therefore "known". Only consulted
    /// when `coverage == Sparse` (Tracked pools have complete data and bypass
    /// miss-detection). Seeded at registration from the initial `tick_data`
    /// keys' word positions; grown by the fetch-write seam (`update_tick_data`)
    /// as the registered tick-data fetcher fills unknown regions. Mirrors the
    /// Python `LiquidityMapSnapshot.tick_bitmap` key-presence rule: in sparse
    /// mode a region is unknown unless its word key is in this set (a
    /// fetched-but-empty word is recorded here as `known`).
    pub known_bitmap_words: HashSet<i32>,

    /// Reorg journal — scalar priors + per-tick priors for rollback.
    pub journal: ReorgJournal<V3BlockDelta>,

    // Cached tick ranges (interior mutability for lazy computation from &self).
    // Invalidated on apply_swap / apply_liquidity_update. Consumed only by
    // `build_int_v3_sequence` (gen-3 integer solver).
    cached_tick_ranges: parking_lot::Mutex<TickRangeCache>,
}

impl Clone for V3PoolState {
    fn clone(&self) -> Self {
        Self {
            address: self.address,
            token0: self.token0,
            token1: self.token1,
            fee: self.fee,
            tick_spacing: self.tick_spacing,
            factory: self.factory,
            sqrt_price_x96: self.sqrt_price_x96,
            liquidity: self.liquidity,
            tick: self.tick,
            update_block: self.update_block,
            tick_data: self.tick_data.clone(),
            coverage: self.coverage,
            known_bitmap_words: self.known_bitmap_words.clone(),
            // Clones start with no cached ranges — the cache is invalidated on
            // mutation anyway, and a fresh Mutex avoids aliasing the source's.
            journal: self.journal.clone(),
            cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
        }
    }
}

impl V3PoolState {
    /// Word position (`tick_position(tick.div_euclid(tick_spacing))`) holding
    /// the given tick. Shared by V3/V4 sparse miss-detection.
    #[must_use]
    pub fn word_of(tick: i32, tick_spacing: i32) -> i32 {
        i32::from(tick_position(tick.div_euclid(tick_spacing)).0)
    }

    /// Seed `known_bitmap_words` from the current `tick_data` keys' word
    /// positions. Called at registration for Sparse pools (the keys a partial
    /// snapshot carries are known); a no-op for Tracked pools (detection is
    /// bypassed when `coverage != Sparse`).
    pub fn seed_known_bitmap_words(&mut self) {
        self.known_bitmap_words = self
            .tick_data
            .keys()
            .map(|&t| Self::word_of(t, self.tick_spacing))
            .collect();
    }

    /// Construct from registration params with a journal of the given depth.
    #[must_use]
    pub fn from_params(params: RegisterV3PoolParams, journal_depth: usize) -> Self {
        let mut out = Self {
            address: params.address,
            token0: params.token0,
            token1: params.token1,
            fee: params.fee,
            tick_spacing: params.tick_spacing,
            factory: params.factory,
            sqrt_price_x96: params.sqrt_price_x96,
            liquidity: params.liquidity,
            tick: params.tick,
            update_block: params.update_block,
            tick_data: params.tick_data,
            coverage: params.coverage,
            known_bitmap_words: HashSet::new(),
            journal: ReorgJournal::<V3BlockDelta>::new(journal_depth),
            cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
        };
        // A partial snapshot's tick_data keys are known regions.
        if params.coverage == PoolTickCoverage::Sparse {
            out.seed_known_bitmap_words();
        }
        out
    }

    /// Invalidate the cached tick ranges (call after any state mutation).
    pub fn invalidate_tick_range_cache(&self) {
        let mut cache = self.cached_tick_ranges.lock();
        cache.zfo = None;
        cache.ofz = None;
    }

    /// Get cached tick ranges for the given direction, computing and caching
    /// if absent. Uses `max_ranges=15` so all callers can slice the result.
    fn get_cached_tick_ranges(&self, zero_for_one: bool) -> Option<Arc<[V3TickRangeForSolver]>> {
        {
            let cache = self.cached_tick_ranges.lock();
            let slot = if zero_for_one { &cache.zfo } else { &cache.ofz };
            if let Some(ranges) = slot {
                return Some(Arc::clone(ranges));
            }
        }

        // Not cached — compute and store
        let ranges = compute_tick_ranges(
            &self.tick_data,
            self.tick,
            self.tick_spacing,
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

    /// Build an integer V3 tick range sequence with up to `max_ranges` ranges,
    /// using original U256 sqrt prices and i128→u128 liquidity (no f64 conversion).
    ///
    /// Produces an [`IntV3TickRangeSequence`] suitable for the integer-exact
    /// V3-V3 solver, preserving full precision. Returns `None` if insufficient
    /// tick data.
    #[must_use]
    pub fn build_int_v3_sequence(
        &self,
        zero_for_one: bool,
        max_ranges: usize,
    ) -> Option<IntV3TickRangeSequence> {
        let ranges = self.get_cached_tick_ranges(zero_for_one)?;
        let use_ranges = ranges.get(..ranges.len().min(max_ranges))?;

        let gamma_numer = u64::from(1_000_000 - self.fee);
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
// V3 single-pool swap simulation (ADR-003: "Pool's authority over its own math")
// ---------------------------------------------------------------------------

/// Result of a single-pool V3 swap simulation.
///
/// `amount0` is the token0 delta (positive = pool received token0, i.e. the
/// swapper paid token0). `amount1` is the token1 delta. The sign convention
/// matches Uniswap V3's `Swap` callback: for an exact-input swap,
/// `zero_for_one` swaps pay token0 (`amount0 > 0`) and receive token1
/// (`amount1 < 0`); both magnitudes are returned unsigned here.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct V3SwapOutcome {
    /// Absolute token0 amount moved (input for zfo swaps, output for ofz).
    pub amount0: U256,
    /// Absolute token1 amount moved (output for zfo swaps, input for ofz).
    pub amount1: U256,
    /// Final `sqrtPriceX96` after the swap walk (ADR-005 slice 3b: the
    /// companion's `simulate_exact_input_swap` builds `final_state` from this).
    pub sqrt_price_x96: U256,
    /// Final active liquidity after the swap walk.
    pub liquidity: u128,
    /// Final tick after the swap walk.
    pub tick: i32,
}

/// Why a [`v3_simulate_swap`] / [`v4_simulate_swap`] call could not produce a
/// trustworthy outcome. Shared by V3 and V4.
///
/// `NotComputable` covers the non-fetchable failures the previous
/// `Option::None` return encoded (zero amount, arithmetic overflow, invariant
/// violation). `MissingTickWord` is the new fetchable sparse-map miss
/// (ADR-005 sparse-map feature parity): the walk entered a tick-bitmap word
/// whose bitmap has not been fetched, so the caller must fetch it via the
/// registered tick-data fetcher and retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimulateSwapError {
    /// The swap is not computable (zero amount, arithmetic overflow, invariant).
    NotComputable,
    /// Sparse coverage: the walk entered tick-bitmap word `i32` that has not
    /// been fetched. Mirrors Python's `MissingLiquidityData(word=...)`.
    MissingTickWord(i32),
}

/// Simulate a V3 exact-input or exact-output swap against a pool's state.
///
/// Port of `UniswapV3Pool._calculate_swap` / `degenbot.uniswap.concentrated.
/// v3_simulator.calculate_swap`. Walks initialized ticks via [`gen_ticks`],
/// runs [`compute_swap_step_v3`] per tick range, and crosses liquidity-net at
/// initialized boundaries. Pure (no state mutation — reads `&V3PoolState`);
/// the solve engine + the calc API both read by reference per ADR-003.
///
/// `amount_specified` uses the V3 sign convention: positive = exact input,
/// negative = exact output.
///
/// # Errors
///
/// Returns [`SimulateSwapError::NotComputable`] if the amount is zero, the
/// swap cannot be computed (tick math / swap-step error, liquidity overflow),
/// or an invariant is violated. Returns [`SimulateSwapError::MissingTickWord`]
/// when `coverage == Sparse` and the walk entered a tick-bitmap word whose
/// bitmap has not been fetched — the caller must fetch word `w` via the
/// registered tick-data fetcher and retry (ADR-005 sparse-map parity).
///
/// See: `contract_reference/uniswap/V3/UniswapV3Factory.sol` (`SwapMath`,
/// `TickBitmap`, `TickMath`).
#[allow(unused_assignments)]
// `tick` tracks the contract's post-step tick; kept faithful to the V3
// `_calculate_swap` loop even though this pure simulator returns only amounts.
#[allow(clippy::too_many_lines)] // faithful port of V3's `_calculate_swap`; splitting would obscure the loop.
pub fn v3_simulate_swap(
    state: &V3PoolState,
    zero_for_one: bool,
    amount_specified: I256,
) -> Result<V3SwapOutcome, SimulateSwapError> {
    if amount_specified.is_zero() {
        return Err(SimulateSwapError::NotComputable); // AS: zero amount (V3 reverts)
    }
    let exact_in = amount_specified.is_positive();

    // V3 price-limit bounds. The swap cannot cross these regardless of amount.
    let sqrt_price_limit = if zero_for_one {
        U256::from(MIN_SQRT_RATIO) + U256::from(1u64) // 4295128740
    } else {
        U256::from(MAX_SQRT_RATIO) - U256::from(1u64)
    };

    let mut amount_specified_remaining = amount_specified;
    let mut amount_calculated = I256::ZERO;
    let mut sqrt_price_x96 = state.sqrt_price_x96;
    let mut tick = state.tick;
    let mut liquidity =
        i128::try_from(state.liquidity).map_err(|_| SimulateSwapError::NotComputable)?;

    let fee_pips = U256::from(state.fee);
    let tick_spacing = state.tick_spacing;

    // Sparse-map miss detection (ADR-005 feature parity). In sparse mode a
    // region is unknown unless its word key is in `known_bitmap_words`. The
    // Python simulator checks the starting word first via
    // `next_initialized_tick_within_one_word`; mirror that: if the word
    // containing the current tick is unknown, signal a fetchable miss before
    // trusting any per-step result derived from `gen_ticks` (which, lacking a
    // bitmap store, would otherwise silently produce a wrong amount here).
    let sparse = state.coverage == PoolTickCoverage::Sparse;
    if sparse
        && !state
            .known_bitmap_words
            .contains(&V3PoolState::word_of(tick, tick_spacing))
    {
        return Err(SimulateSwapError::MissingTickWord(V3PoolState::word_of(
            tick,
            tick_spacing,
        )));
    }

    // Walk initialized + word-boundary ticks in the swap direction. gen_ticks
    // yields ticks in swap order (descending for zfo, ascending for ofz).
    // Cap iterations to bound pathological loops; the V3 contract's loop also
    // terminates at MIN/MAX_TICK.
    let ticks = gen_ticks(
        &state.tick_data,
        tick,
        tick_spacing,
        zero_for_one, // less_than_or_equal matches swap direction
        30_000,
    )
    .map_err(|_| SimulateSwapError::NotComputable)?;

    for tick_along_path in ticks {
        if amount_specified_remaining.is_zero() || sqrt_price_x96 == sqrt_price_limit {
            break;
        }

        let mut tick_next = tick_along_path.tick;
        let initialized = tick_along_path.is_initialized;

        // Clamp to the V3 tick bounds (mirrors `_calculate_swap`).
        tick_next = if zero_for_one {
            tick_next.max(-887_272)
        } else {
            tick_next.min(887_272)
        };

        let sqrt_price_next = U256::from(
            get_sqrt_ratio_at_tick_internal(tick_next)
                .map_err(|_| SimulateSwapError::NotComputable)?,
        );

        // Target price: clamp to the swap price-limit if the next tick would
        // cross it.
        let sqrt_price_target = if (zero_for_one && sqrt_price_next < sqrt_price_limit)
            || (!zero_for_one && sqrt_price_next > sqrt_price_limit)
        {
            sqrt_price_limit
        } else {
            sqrt_price_next
        };

        let sqrt_price_start = sqrt_price_x96;
        let step = compute_swap_step_v3(
            sqrt_price_x96,
            sqrt_price_target,
            liquidity,
            amount_specified_remaining,
            fee_pips,
        )
        .map_err(|_| SimulateSwapError::NotComputable)?;

        sqrt_price_x96 = step.sqrt_price_next;

        if exact_in {
            // Gross input consumed this step = amount_in + fee_amount.
            let consumed = I256::try_from(step.amount_in.saturating_add(step.fee_amount))
                .map_err(|_| SimulateSwapError::NotComputable)?;
            amount_specified_remaining = amount_specified_remaining
                .checked_sub(consumed)
                .ok_or(SimulateSwapError::NotComputable)?;
            amount_calculated = amount_calculated
                .checked_sub(
                    I256::try_from(step.amount_out)
                        .map_err(|_| SimulateSwapError::NotComputable)?,
                )
                .ok_or(SimulateSwapError::NotComputable)?;
        } else {
            amount_specified_remaining = amount_specified_remaining
                .checked_add(
                    I256::try_from(step.amount_out)
                        .map_err(|_| SimulateSwapError::NotComputable)?,
                )
                .ok_or(SimulateSwapError::NotComputable)?;
            let gross_input = I256::try_from(step.amount_in.saturating_add(step.fee_amount))
                .map_err(|_| SimulateSwapError::NotComputable)?;
            amount_calculated = amount_calculated
                .checked_add(gross_input)
                .ok_or(SimulateSwapError::NotComputable)?;
        }

        if sqrt_price_x96 == sqrt_price_next {
            // The walk reached `tick_next` — crossing into its word. In sparse
            // mode a region is unknown unless its word key is in
            // `known_bitmap_words`; an unfetched word means the result past
            // this crossing would be untrustworthy, so signal a fetchable miss
            // (caller fetches + retries) rather than reading potentially-absent
            // tick_data or proceeding on an unknown region. Mirrors Python's
            // per-step `MissingLiquidityData` raise on word entry. The check is
            // gated on an actual crossing so a merely-proposed (unreached)
            // boundary tick in a neighbouring word does not false-trigger.
            if sparse
                && !state
                    .known_bitmap_words
                    .contains(&V3PoolState::word_of(tick_next, tick_spacing))
            {
                return Err(SimulateSwapError::MissingTickWord(V3PoolState::word_of(
                    tick_next,
                    tick_spacing,
                )));
            }
            // Reached the next tick — cross it if initialized.
            if initialized {
                if let Some(info) = state.tick_data.get(&tick_next) {
                    let liquidity_net_i256 = info.liquidity_net;
                    // Crossing direction: zfo crosses from above (subtract net);
                    // ofz crosses from below (add net). Matches V3's
                    // `liquidity = liquidity - liquidityNet` (zfo) branch.
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
            // Price moved but didn't reach tick_next — recompute tick from price.
            tick = get_tick_at_sqrt_ratio_internal(sqrt_price_x96.to::<U160>())
                .map_err(|_| SimulateSwapError::NotComputable)?
                .as_i32();
        }
    }

    // Derive amount0 / amount1 per the V3 Swap callback convention.
    // input_consumed = amount_specified - amount_remaining (positive for
    // both exact-in and exact-out).
    let input_consumed = amount_specified
        .checked_sub(amount_specified_remaining)
        .ok_or(SimulateSwapError::NotComputable)?;

    let (amount0_signed, amount1_signed) = if zero_for_one == exact_in {
        // zfo + exact_in  → pool receives token0 (input),  sends token1 (output)
        // ofz + exact_out → pool sends token0 (output),  receives token1 (input)
        (input_consumed, -amount_calculated)
    } else {
        (-amount_calculated, input_consumed)
    };

    Ok(V3SwapOutcome {
        amount0: amount0_signed.unsigned_abs(),
        amount1: amount1_signed.unsigned_abs(),
        sqrt_price_x96,
        liquidity: u128::try_from(liquidity.max(0)).unwrap_or(0),
        tick,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{I256, U128, U256};
    use degenbot_cl_math::cl_lib::swap_math::compute_swap_step_v3;

    /// Build a V3 pool at tick 0 (1:1 price, `sqrt_price` = 2^96), liquidity `liq`,
    /// fee 0.3% (3000 pips), `tick_spacing` 60, with a single position spanning
    /// [-60, +60] so the active range bounded by ±60 matches
    /// `make_v3_hop_at_1to1`. The ticks -60 and +60 are initialized with the
    /// position's `liquidity_net` (+L at lower, -L at upper) and matching gross.
    fn pool_1to1_with_position(liq: u128) -> V3PoolState {
        let sp_0 = U256::from(1u128) << 96;
        let mut tick_data = HashMap::new();
        // Position [-60, +60] with liquidity `liq`.
        let liq_u128 = U256::from(liq).to::<U128>();
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
        V3PoolState {
            address: Address::ZERO,
            token0: Address::ZERO,
            token1: Address::ZERO,
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: sp_0,
            liquidity: liq,
            tick: 0,
            update_block: 0,
            tick_data,
            coverage: PoolTickCoverage::Tracked,
            known_bitmap_words: HashSet::new(),
            journal: ReorgJournal::<V3BlockDelta>::new(8),
            cached_tick_ranges: parking_lot::Mutex::new(super::TickRangeCache::default()),
        }
    }

    #[test]
    fn zfo_small_swap_matches_single_compute_swap_step() {
        // What: a small zfo exact-input swap on a 1:1 V3 pool with a [-60,+60]
        // position stays inside the range (no tick crossing), so the outcome
        // must equal a single `compute_swap_step_v3` call with the same bounds.
        // Why: pins the V3 simulator's first-step behavior against the already-
        // tested swap-step primitive as the oracle (zero hand-computed math).
        let liq = 10_000_000_000_000u128;
        let state = pool_1to1_with_position(liq);
        let amount_in = U256::from(1000u64);

        let outcome = v3_simulate_swap(&state, true, I256::try_from(amount_in).unwrap())
            .expect("small swap should produce an outcome");

        // Oracle: the single-step target is tick -60's sqrt price (the range
        // lower bound), which a small input does not reach. amount_remaining is
        // the full positive input (V3 exact-in convention).
        let sp_lower = U256::from(get_sqrt_ratio_at_tick_internal(-60).unwrap());
        let step = compute_swap_step_v3(
            state.sqrt_price_x96,
            sp_lower,
            i128::try_from(liq).unwrap(),
            I256::try_from(amount_in).unwrap(),
            U256::from(state.fee),
        )
        .unwrap();

        assert_eq!(
            outcome.amount1, step.amount_out,
            "zfo exact-in: token1 output must equal the single swap-step amount_out"
        );
        assert_eq!(
            outcome.amount0,
            step.amount_in + step.fee_amount,
            "zfo exact-in: token0 input consumed must equal amount_in + fee_amount"
        );
        assert!(
            outcome.amount1 < amount_in,
            "on a 1:1 pool with fees, output must be < input (got {} >= {})",
            outcome.amount1,
            amount_in
        );
    }

    #[test]
    fn ofz_small_swap_matches_single_compute_swap_step() {
        // Mirrors the zfo test for the one_for_zero direction — oracle target
        // is tick +60's sqrt price (the range upper bound).
        let liq = 10_000_000_000_000u128;
        let state = pool_1to1_with_position(liq);
        let amount_in = U256::from(1000u64);

        let outcome = v3_simulate_swap(&state, false, I256::try_from(amount_in).unwrap())
            .expect("small ofz swap should produce an outcome");

        let sp_upper = U256::from(get_sqrt_ratio_at_tick_internal(60).unwrap());
        let step = compute_swap_step_v3(
            state.sqrt_price_x96,
            sp_upper,
            i128::try_from(liq).unwrap(),
            I256::try_from(amount_in).unwrap(),
            U256::from(state.fee),
        )
        .unwrap();

        assert_eq!(outcome.amount0, step.amount_out);
        assert_eq!(outcome.amount1, step.amount_in + step.fee_amount);
        assert!(outcome.amount0 < amount_in);
    }

    #[test]
    fn zero_amount_is_not_computable() {
        let state = pool_1to1_with_position(1_000_000u128);
        let outcome = v3_simulate_swap(&state, true, I256::ZERO);
        assert_eq!(
            outcome,
            Err(SimulateSwapError::NotComputable),
            "zero amount_specified should be NotComputable (V3 AS revert)"
        );
    }

    #[test]
    fn output_scales_monotonically_with_input() {
        // Larger exact-input swaps produce larger outputs (within the same
        // tick range, pre-crossing).
        let state = pool_1to1_with_position(10_000_000_000_000u128);
        let small = v3_simulate_swap(&state, true, I256::try_from(U256::from(100u64)).unwrap())
            .unwrap()
            .amount1;
        let large = v3_simulate_swap(&state, true, I256::try_from(U256::from(10_000u64)).unwrap())
            .unwrap()
            .amount1;
        assert!(
            large > small,
            "larger input must produce larger output (small={small}, large={large})"
        );
    }

    #[test]
    fn sparse_unknown_word_signals_fetchable_miss() {
        // ADR-005 sparse-map feature parity. In sparse mode a region is unknown
        // unless its word key is in `known_bitmap_words`. A pool constructed
        // sparse with no known words must therefore signal a fetchable miss on
        // the starting word (mirrors Python's `MissingLiquidityData(word=0)`
        // first-step raise), NOT a silently-wrong computed amount.
        let liq = 10_000_000_000_000u128;
        let mut state = pool_1to1_with_position(liq);
        state.coverage = PoolTickCoverage::Sparse;
        state.known_bitmap_words.clear(); // fully sparse: no regions known

        let res = v3_simulate_swap(&state, true, I256::try_from(1_000u64).unwrap());
        assert_eq!(
            res,
            Err(SimulateSwapError::MissingTickWord(0)),
            "sparse pool with unknown starting word must signal MissingTickWord, \
             not a computed outcome nor NotComputable"
        );
    }

    #[test]
    fn tracked_pool_bypasses_miss_detection() {
        // ADR-005 sparse-map feature parity. Miss detection is gated on
        // `coverage == Sparse`: a Tracked pool (complete tick data) must
        // compute normally even when `known_bitmap_words` is empty — it never
        // consults the set. Confirms detection is sparse-only.
        let liq = 10_000_000_000_000u128;
        let mut state = pool_1to1_with_position(liq);
        // Tracked + empty known set — must NOT miss.
        state.known_bitmap_words.clear();
        assert_eq!(state.coverage, PoolTickCoverage::Tracked);

        let res = v3_simulate_swap(&state, true, I256::try_from(1_000u64).unwrap());
        assert!(
            res.is_ok(),
            "Tracked pool must compute regardless of known_bitmap_words, got {res:?}"
        );
    }

    #[test]
    fn sparse_unreached_boundary_does_not_false_miss() {
        // The complement of `sparse_unknown_word_signals_fetchable_miss`.
        // gen_ticks proposes candidate boundary ticks along the path; a swap
        // that does NOT reach a proposed tick in an unknown neighbor word must
        // still compute (no false miss). Mirrors Python's per-word miss: the
        // word is only consulted when the walk actually enters it.
        let liq = 10_000_000_000_000u128;
        let mut state = pool_1to1_with_position(liq);
        state.coverage = PoolTickCoverage::Sparse;
        // Word 0 (tick 0, the start) is known; word −1 (containing the tick −60
        // boundary of the position) is NOT known. A small swap stays near tick 0
        // and never crosses −60, so word −1 is merely proposed, not entered.
        state.known_bitmap_words.clear();
        state.known_bitmap_words.insert(0);

        let res = v3_simulate_swap(&state, true, I256::try_from(100u64).unwrap());
        assert!(
            res.is_ok(),
            "small swap that never enters the unknown word −1 should compute, got {res:?}"
        );
    }

    #[test]
    fn v3_simulate_swap_outcome_caries_final_state() {
        // ADR-005 slice 3b: the companion's simulate_exact_input_swap builds
        // final_state from the outcome, so v3_simulate_swap must return the
        // post-walk sqrt_price_x96 / liquidity / tick (not just the amounts).
        let state = pool_1to1_with_position(10_000_000_000_000u128);
        let amount_in = I256::try_from(1_000u128).unwrap();
        let outcome = v3_simulate_swap(&state, true, amount_in).expect("computes");
        // zfo small swap below +60 (no crossing): price drops (sqrt_price_x96 <
        // start) but liquidity + tick stay within the active range.
        assert!(
            outcome.sqrt_price_x96 < state.sqrt_price_x96,
            "zfo swap must drop the price below the start value"
        );
        assert_eq!(
            outcome.liquidity, state.liquidity,
            "liquidity unchanged (no crossing)"
        );
        assert!(
            (-60..60).contains(&outcome.tick),
            "tick stays within the active range (no crossing): got {}",
            outcome.tick
        );
        // A crossing swap (large zfo) must move the state off the start values.
        let big = I256::try_from(100_000_000_000_000_000u128).unwrap();
        let crossed = v3_simulate_swap(&state, true, big).expect("computes");
        assert_ne!(
            crossed.sqrt_price_x96, state.sqrt_price_x96,
            "a crossing swap must move the price off the start value"
        );
    }
}

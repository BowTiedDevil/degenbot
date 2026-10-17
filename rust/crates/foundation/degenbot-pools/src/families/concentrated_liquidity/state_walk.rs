//! The concentrated liquidity family's swap-walk state verbs (ADR-080 D2, step 3) — the
//! state-side half of the family tree, beside the read door.
//!
//! The V3 and V4 state structs (`crate::v3_state` / `crate::v4_state`) carry
//! the same slot0 scalars, the same initialized-tick map, the same two-stamp
//! clocks, and the same lazy tick-range cache, so the directional viability
//! gate, the clock advances, the cache invalidation/lookup, and the integer
//! tick-range sequence walks are written once here — data-generically over
//! the [`WalkHost`] field seam — and the per-design files keep only thin
//! forwarders. What genuinely differs stays per-adapter: the per-design
//! event application, the hook-keyed V4 identity, and V4's `slot0.protocolFee`
//! scalar (passed into [`build_int_v4_sequence`] by its adapter — V3 has no
//! protocol fee).

use std::sync::Arc;

use alloy::primitives::{U160, U256};
use hashbrown::HashMap;

use crate::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use crate::tick_bitmap::{compute_tick_ranges, V3TickRangeForSolver};
use crate::TickInfo;
use degenbot_math::concentrated_liquidity::swap_math::{
    calculate_swap_fee, protocol_fee_one_for_zero, protocol_fee_zero_for_one,
};
use degenbot_math::concentrated_liquidity::tick_math::{
    get_sqrt_ratio_at_tick_internal, MAX_SQRT_RATIO, MIN_SQRT_RATIO,
};

/// Cached state of a tick-range computation for one direction.
///
/// Three states: `StillEmpty` (not computed since last invalidation),
/// `Hit` (`compute_tick_ranges` returned `Some`), `Miss` (returned `None`).
/// Caching `Miss` prevents re-walking pools whose state hasn't changed
/// since the last failed walk — the dominant cost in live solve cycles
/// (1300-9600 `SequenceUnavailable` rejections per cycle, each re-walking
/// O(tick-walk) from scratch). `invalidate_tick_range_cache` resets both
/// slots to `StillEmpty` on every Swap/Mint/Burn .
#[derive(Clone, Debug, Default)]
pub enum CachedTickRanges {
    /// Not computed since the last invalidation — the next call must walk.
    #[default]
    StillEmpty,
    /// `compute_tick_ranges` returned `Some` — cached for reuse.
    Hit(Arc<[V3TickRangeForSolver]>),
    /// `compute_tick_ranges` returned `None` — cached to avoid re-walking.
    Miss,
}

/// Cached tick ranges for a single pool, keyed by direction.
#[derive(Clone, Debug, Default)]
pub struct TickRangeCache {
    pub(crate) zfo: CachedTickRanges,
    pub(crate) ofz: CachedTickRanges,
}

/// The field seam the shared walk verbs read and write through — implemented
/// by each design's state struct (`V3PoolState`, `V4PoolState`), whose slot0
/// scalars, tick map, two-stamp clocks, and lazy tick-range cache are
/// shape-identical. The accessors are the family-specific field access; the
/// verb logic below is written once over this seam.
pub(crate) trait WalkHost {
    /// The pool's current `sqrt_price_x96` slot0 scalar.
    fn sqrt_price_x96(&self) -> U256;

    /// The pool's current active `liquidity` scalar.
    fn liquidity(&self) -> u128;

    /// The pool's current active `tick`.
    fn tick(&self) -> i32;

    /// The pool's initialized-tick map.
    fn tick_data(&self) -> &HashMap<i32, TickInfo>;

    /// The pool's lazy tick-range cache (interior mutability for lazy
    /// computation from `&self`).
    fn tick_range_cache(&self) -> &parking_lot::Mutex<TickRangeCache>;

    /// The pool's price clock (`update_block`).
    fn update_block(&self) -> u64;

    /// The pool's liquidity clock (`tick_data_block`).
    fn tick_data_block(&self) -> u64;

    /// Write the price clock (the monotonic advance's forward write).
    fn set_update_block(&mut self, block: u64);

    /// Write the liquidity clock (the monotonic advance's forward write).
    fn set_tick_data_block(&mut self, block: u64);
}

/// `V3PoolState::swap_is_viable` / `V4PoolState::swap_is_viable` — directional
/// swap viability: can this pool host a swap in the given direction with its
/// CURRENT state?
///
/// O(1) using the tick-map extremes; ported from the archived Python
/// `v3_liquidity_pool.py::swap_is_viable`, keeping its directional
/// precision (Python's coarse empty-map early-out is subsumed by the
/// directional test, which an empty map also fails).
///
/// Checks, in order:
/// 1. uninitialized price (`sqrt_price_x96 == 0`) → not viable;
/// 2. price at the `MIN`/`MAX_SQRT_RATIO` protocol boundary in the swap's
///    direction → not viable (the swap would drive price past the limit);
/// 3. no initialized tick strictly ahead in the walk direction → not
///    viable (a zfo walk descends and needs a position below; ofz ascends
///    and needs one above).
///
/// NOTE: unlike `build_int_v3_sequence`, this does NOT consult or populate
/// the tick-range cache — it is safe to call under contention and costs
/// two hashmap-extreme scans at worst (amortized O(1) via BTreeMap-free
/// iteration over keys; tick maps are small).
#[must_use]
pub(crate) fn swap_is_viable<H: WalkHost>(host: &H, zero_for_one: bool) -> bool {
    if host.sqrt_price_x96().is_zero() {
        return false;
    }
    let sp = host.sqrt_price_x96();
    if zero_for_one {
        if sp <= U256::from(MIN_SQRT_RATIO) + U256::from(1u64) {
            return false;
        }
        // Viable iff some initialized tick sits strictly below the price.
        match host.tick_data().keys().min() {
            Some(&min_tick) => match get_sqrt_ratio_at_tick_internal(min_tick) {
                Ok(t) => U256::from(t) < sp,
                Err(_) => false,
            },
            None => false,
        }
    } else {
        if sp >= U256::from(MAX_SQRT_RATIO) - U256::from(1u64) {
            return false;
        }
        match host.tick_data().keys().max() {
            Some(&max_tick) => match get_sqrt_ratio_at_tick_internal(max_tick) {
                Ok(t) => U256::from(t) > sp,
                Err(_) => false,
            },
            None => false,
        }
    }
}

/// `V3PoolState::advance_update_block` / `V4PoolState::advance_update_block`
/// — monotonic advance of the **price** clock (`update_block`).
///
/// Sets it to `block` when `block > cur`; an equal `block` is an
/// idempotent no-op. A `block < cur` is a BACKWARD STAMP — an invariant
/// violation outside a reorg — and panics loudly with a stable, grep-able
/// literal (ADR-021 fail-fast discipline; two-stamp rule). The only
/// sanctioned rewind is `ReorgPoolState::restore_before_block`, which sets
/// both clocks directly from the journal priors and must NOT call this.
///
/// # Panics
///
/// Panics with `PoolState monotonicity violated: update_block attempted
/// {block} < current {cur} outside a reorg — ABORT` when `block < cur`.
pub(crate) fn advance_update_block<H: WalkHost>(host: &mut H, block: u64) {
    assert!(
        block >= host.update_block(),
        "PoolState monotonicity violated: update_block attempted {block} < current {} outside a reorg — ABORT",
        host.update_block()
    );
    if block > host.update_block() {
        host.set_update_block(block);
    }
}

/// `V3PoolState::advance_tick_data_block` / `V4PoolState::advance_tick_data_block`
/// — monotonic advance of the **liquidity** clock (`tick_data_block`).
///
/// Same contract as [`advance_update_block`] for the tick-map clock.
///
/// # Panics
///
/// Panics with `PoolState monotonicity violated: tick_data_block attempted
/// {block} < current {cur} outside a reorg — ABORT` when `block < cur`.
pub(crate) fn advance_tick_data_block<H: WalkHost>(host: &mut H, block: u64) {
    assert!(
        block >= host.tick_data_block(),
        "PoolState monotonicity violated: tick_data_block attempted {block} < current {} outside a reorg — ABORT",
        host.tick_data_block()
    );
    if block > host.tick_data_block() {
        host.set_tick_data_block(block);
    }
}

/// `V3PoolState::invalidate_tick_range_cache` /
/// `V4PoolState::invalidate_tick_range_cache` — reset both direction slots
/// to `StillEmpty` (call after any state mutation).
pub(crate) fn invalidate_tick_range_cache<H: WalkHost>(host: &H) {
    let mut cache = host.tick_range_cache().lock();
    cache.zfo = CachedTickRanges::StillEmpty;
    cache.ofz = CachedTickRanges::StillEmpty;
}

/// `V3PoolState::get_cached_tick_ranges` / `V4PoolState::get_cached_tick_ranges`
/// — get cached tick ranges for the given direction, computing and caching
/// if absent. The walk visits every initialized tick in the swap direction.
/// Results are cached per pool+direction so the walk
/// amortizes to zero per cycle.
pub(crate) fn get_cached_tick_ranges<H: WalkHost>(
    host: &H,
    tick_spacing: i32,
    zero_for_one: bool,
) -> Option<Arc<[V3TickRangeForSolver]>> {
    {
        let cache = host.tick_range_cache().lock();
        let slot = if zero_for_one { &cache.zfo } else { &cache.ofz };
        match slot {
            CachedTickRanges::Hit(ranges) => return Some(Arc::clone(ranges)),
            CachedTickRanges::Miss => return None, // cached negative
            CachedTickRanges::StillEmpty => {}     // fall through to compute
        }
    }

    // Not cached — compute and store
    let ranges = compute_tick_ranges(
        host.tick_data(),
        host.tick(),
        tick_spacing,
        host.liquidity(),
        zero_for_one,
    )
    .map(|(ranges, _)| Arc::<[V3TickRangeForSolver]>::from(ranges));

    // Cache the result regardless of Some/None (caching None
    // avoids re-walking pools whose state hasn't changed since the last
    // failed walk).
    let mut cache = host.tick_range_cache().lock();
    match ranges {
        Some(ref r) => {
            if zero_for_one {
                cache.zfo = CachedTickRanges::Hit(Arc::clone(r));
            } else {
                cache.ofz = CachedTickRanges::Hit(Arc::clone(r));
            }
        }
        None => {
            if zero_for_one {
                cache.zfo = CachedTickRanges::Miss;
            } else {
                cache.ofz = CachedTickRanges::Miss;
            }
        }
    }

    ranges
}

/// `V3PoolState::build_int_v3_sequence` — build an integer V3 tick range
/// sequence using original U256 sqrt prices and i128→u128 liquidity (no f64
/// conversion).
///
/// Produces an [`IntV3TickRangeSequence`] suitable for the integer-exact
/// V3-V3 solver, preserving full precision. Returns `None` if insufficient
/// tick data. The walk visits every initialized tick in the swap direction;
/// the solver's own bounds (`iteration_cap`, `prune`, `REFINE_GRID_POINTS`)
/// handle pathological cost post-hoc.
#[must_use]
pub(crate) fn build_int_v3_sequence<H: WalkHost>(
    host: &H,
    tick_spacing: i32,
    fee: u32,
    zero_for_one: bool,
) -> Option<IntV3TickRangeSequence> {
    let ranges = get_cached_tick_ranges(host, tick_spacing, zero_for_one)?;
    let use_ranges: &[V3TickRangeForSolver] = &ranges;

    let gamma_numer = u64::from(1_000_000 - fee);
    let fee_denom = 1_000_000u64;

    // Net at the current tick (zfo only). For a zfo swap the V3 swap
    // loop's first `nextInitializedTickWithinOneWord(lte=true)` resolves
    // the CURRENT tick when it is initialized, so the swap sweeps the
    // leading segment [current, sqrt(currentTick)] at the PRE-drain stored
    // liquidity; only upon REACHING sqrt(currentTick) does it cross the
    // tick and apply `if (zeroForOne) liquidityNet = -liquidityNet;` →
    // `state.liquidity -= net(currentTick)` (UniswapV3Pool::swap →
    // Tick.cross → LiquidityMath.addDelta).
    //
    // This function models that leading segment as a dedicated hop at the
    // stored liquidity (below) and applies the net at the crossing into the
    // below ranges via `base = stored - net(currentTick)`. Prior to this
    // the net was folded into range 0's liquidity unconditionally — a
    // compression faithful only for DEEP swaps that reach the boundary. For
    // a SHALLOW swap that partial-fills ABOVE sqrt(currentTick) (e.g. path
    // 13827 DAI/USDC: current tick -276324 carries net -4.27e17, inflating
    // the modeled starting liquidity and over-predicting `v3_simulate_swap`
    // by 1 wei → the `V3_TAKE` overdraft / "IIA" revert class), that
    // compression incorrectly governed the output by the post-drain
    // liquidity. ofz uses `gt` (exclusive) and does NOT re-cross the
    // current tick, so no net applies there.
    let current_tick_net: i128 = if zero_for_one {
        // liquidity_net is I256; the low-128-bit int128 projection
        // (the shared `TickInfo::liquidity_net_i128` — the extraction
        // `compute_tick_ranges` uses too; documents the path-13827
        // 1-wei over-prediction trap).
        host.tick_data()
            .get(&host.tick())
            .map_or(0, TickInfo::liquidity_net_i128)
    } else {
        0
    };
    let base_liquidity: i128 = host.liquidity().cast_signed() - current_tick_net;

    let has_leading_current_segment = zero_for_one && host.tick_data().contains_key(&host.tick());

    let mut int_ranges = Vec::with_capacity(use_ranges.len() + 1);

    // Contract-faithful leading segment (zfo + initialized current tick
    // only): the swap's first step targets sqrt(currentTick) while
    // `state.liquidity` is still the stored (pre-drain) value, so model
    // [current, sqrt(currentTick)] as a leading hop at stored liquidity.
    // The below computed ranges then apply the net at the crossing.
    if has_leading_current_segment {
        let tick_sqrt =
            U256::from(get_sqrt_ratio_at_tick_internal(host.tick()).unwrap_or(U160::ZERO));
        int_ranges.push(IntV3TickRangeHop {
            liquidity: host.liquidity(),
            sqrt_price_x96: host.sqrt_price_x96(),
            sqrt_price_lower_x96: tick_sqrt,
            sqrt_price_upper_x96: tick_sqrt,
            gamma_numer,
            fee_denom,
            zero_for_one,
            word_boundary_prices: Vec::new(),
        });
    }

    for (i, r) in use_ranges.iter().enumerate() {
        let sqrt_price_x96 = int_range_entry_sqrt_price(
            i,
            has_leading_current_segment,
            zero_for_one,
            host.sqrt_price_x96(),
            host.tick(),
            use_ranges,
        );
        let range_liquidity = int_range_liquidity(i, base_liquidity, zero_for_one, use_ranges);

        int_ranges.push(IntV3TickRangeHop {
            liquidity: range_liquidity,
            sqrt_price_x96,
            sqrt_price_lower_x96: r.sqrt_price_lower,
            sqrt_price_upper_x96: r.sqrt_price_upper,
            gamma_numer,
            fee_denom,
            zero_for_one,
            word_boundary_prices: word_boundary_prices(r),
        });
    }

    IntV3TickRangeSequence::new(int_ranges).ok()
}

/// `V4PoolState::build_int_v4_sequence` — build an integer tick-range
/// sequence for the V4 pool. Identical concentrated liquidity math to [`build_int_v3_sequence`];
/// the type is named `IntV3TickRangeSequence` but applies equally to V4
/// hops. `protocol_fee` is V4's packed `slot0.protocolFee`, passed by the
/// V4 adapter (V3 has no protocol fee).
///
/// Returns `None` if insufficient tick data.
#[must_use]
pub(crate) fn build_int_v4_sequence<H: WalkHost>(
    host: &H,
    protocol_fee: u32,
    tick_spacing: i32,
    fee: u32,
    zero_for_one: bool,
) -> Option<IntV3TickRangeSequence> {
    let ranges = get_cached_tick_ranges(host, tick_spacing, zero_for_one)?;
    let use_ranges: &[V3TickRangeForSolver] = &ranges;

    // V4 charges the COMBINED `swapFee = calculateSwapFee(protocol_fee_dir,
    // lp_fee)`, NOT `lp_fee` alone, when `slot0.protocolFee > 0`. See
    // `v4_simulate_swap` + the sim_v4_swap_step_rounding diagnosis (removed
    // in the stale-docs cleanup `71ec78b2`).
    let direction_protocol_fee = if zero_for_one {
        protocol_fee_zero_for_one(protocol_fee)
    } else {
        protocol_fee_one_for_zero(protocol_fee)
    };
    let swap_fee = calculate_swap_fee(direction_protocol_fee, fee).ok()?;
    let gamma_numer = u64::from(1_000_000u32.checked_sub(swap_fee)?);
    let fee_denom = 1_000_000u64;

    let mut int_ranges = Vec::with_capacity(use_ranges.len());
    for (i, r) in use_ranges.iter().enumerate() {
        let sqrt_price_x96 = if i == 0 {
            host.sqrt_price_x96()
        } else if zero_for_one {
            use_ranges[i - 1].sqrt_price_upper
        } else {
            use_ranges[i - 1].sqrt_price_lower
        };

        // V4 step-1 current-tick drain (zfo only) — same semantics as the
        // V3 case: the PoolManager swap loop resolves `tickNext = current
        // tick` (lte, inclusive) for zfo, a zero-amount step that crosses
        // the tick and applies `liquidity -= liquidityNet(currentTick)`.
        // See the V3 sibling in `build_int_v3_sequence` and the mainnet
        // fixture logs/fixtures/v2_v3_v3_solver_divergence_25641093.md.
        // ofz uses `gt` (exclusive) and does NOT re-cross the current tick.
        let current_tick_drain: i128 = if zero_for_one {
            // The low-128-bit int128 projection (shared
            // `TickInfo::liquidity_net_i128`).
            host.tick_data()
                .get(&host.tick())
                .map_or(0, TickInfo::liquidity_net_i128)
        } else {
            0
        };
        let base_liquidity: i128 = host.liquidity().cast_signed() - current_tick_drain;

        let range_liquidity = int_range_liquidity(i, base_liquidity, zero_for_one, use_ranges);

        int_ranges.push(IntV3TickRangeHop {
            liquidity: range_liquidity,
            sqrt_price_x96,
            sqrt_price_lower_x96: r.sqrt_price_lower,
            sqrt_price_upper_x96: r.sqrt_price_upper,
            gamma_numer,
            fee_denom,
            zero_for_one,
            word_boundary_prices: word_boundary_prices(r),
        });
    }

    IntV3TickRangeSequence::new(int_ranges).ok()
}

/// Entry sqrt price for computed range `i` of an integer V3 walk.
///
/// Range 0 enters at sqrt(currentTick) when a leading current-tick segment
/// was modeled (the leading hop already covers [current, sqrt(currentTick)]);
/// otherwise it enters at the pool's current sqrt price. Later ranges enter
/// at the previous range's swap-direction boundary.
fn int_range_entry_sqrt_price(
    i: usize,
    has_leading_current_segment: bool,
    zero_for_one: bool,
    current_sqrt_price_x96: U256,
    current_tick: i32,
    use_ranges: &[V3TickRangeForSolver],
) -> U256 {
    if i == 0 {
        if has_leading_current_segment {
            // leading hop already starts at current; this first computed
            // range enters at sqrt(currentTick)
            U256::from(get_sqrt_ratio_at_tick_internal(current_tick).unwrap_or(U160::ZERO))
        } else {
            current_sqrt_price_x96
        }
    } else if zero_for_one {
        use_ranges[i - 1].sqrt_price_upper
    } else {
        use_ranges[i - 1].sqrt_price_lower
    }
}

/// Stored-liquidity-adjusted liquidity of computed range `i`.
///
/// Range 0 carries the leading-segment-corrected base liquidity; each later
/// range applies every prior range's boundary net in swap order, flooring
/// negative results at zero.
fn int_range_liquidity(
    i: usize,
    base_liquidity: i128,
    zero_for_one: bool,
    use_ranges: &[V3TickRangeForSolver],
) -> u128 {
    if i == 0 {
        if base_liquidity < 0 {
            0
        } else {
            base_liquidity.cast_unsigned()
        }
    } else {
        let mut l = base_liquidity;
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
    }
}

/// Convert the interior word-boundary ticks `compute_tick_ranges` collapsed
/// out of this range into sqrt prices (swap order) so the solver's
/// `compute_crossing` / `simulate_v3_range_swap` can re-walk them per
/// boundary, restoring the per-step `computeSwapStep` flooring.
fn word_boundary_prices(r: &V3TickRangeForSolver) -> Vec<U256> {
    r.interior_boundaries
        .iter()
        .map(|&t| U256::from(get_sqrt_ratio_at_tick_internal(t).unwrap_or(U160::ZERO)))
        .collect()
}

/// The merged walk-verb test suite (ADR-080 D4) — the former V3/V4 twin
/// batteries, converged at this module's interface.
#[cfg(test)]
mod tests;

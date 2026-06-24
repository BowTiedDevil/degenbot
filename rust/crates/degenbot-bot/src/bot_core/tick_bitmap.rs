//! V3 tick bitmap walk — produces ordered initialized and boundary ticks.
//!
//! Port of `gen_ticks()` from `uniswap/v3_libraries/tick_bitmap.py`.
//!
//! The tick bitmap is a `HashMap<i16, U256>` mapping word positions to
//! 256-bit bitmap values. Each bit in the bitmap corresponds to an
//! initialized tick at `word_pos * 256 + bit_pos` positions (scaled by
//! `tick_spacing`).
//!
//! [`gen_ticks`] yields ticks along a swap path (descending for `zero_for_one`,
//! ascending for `one_for_zero`), interleaving boundary ticks (at word edges)
//! with initialized ticks from the liquidity mapping.

use std::collections::HashMap;

use alloy::primitives::{I256, U128, U256};

use degenbot_cl_math::cl_lib::functions::tick_position;
use degenbot_cl_math::cl_lib::tick_math::{MAX_TICK, MIN_TICK};

use crate::bot_core::TickInfo;

/// A tick along the swap path, with an initialization flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TickAlongPath {
    /// The tick value.
    pub tick: i32,
    /// Whether this tick has initialized liquidity data.
    pub is_initialized: bool,
}

/// Error type for [`gen_ticks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenTicksError {
    /// `tick_spacing` must be non-zero.
    ZeroTickSpacing,
}

/// Yield ticks at word boundaries and initialized ticks in the liquidity mapping.
///
/// Ticks are yielded in descending order when `less_than_or_equal` is `true`
/// (`zero_for_one` direction), else ascending order.
///
/// This is a direct port of the Python `gen_ticks()` generator, implemented
/// as a function that collects ticks into a `Vec` with a maximum count.
///
/// # Arguments
///
/// * `tick_data` - Map of initialized tick index → `TickInfo` (only keys are used)
/// * `starting_tick` - The current pool tick
/// * `tick_spacing` - Tick spacing for this pool (e.g., 60 for 0.3%)
/// * `less_than_or_equal` - `true` for `zero_for_one` direction (descending)
/// * `max_ticks` - Maximum number of ticks to yield (prevents unbounded iteration)
///
/// # Errors
///
/// Returns [`GenTicksError::ZeroTickSpacing`] if `tick_spacing` is zero.
pub fn gen_ticks<S: std::hash::BuildHasher>(
    tick_data: &HashMap<i32, TickInfo, S>,
    starting_tick: i32,
    tick_spacing: i32,
    less_than_or_equal: bool,
    max_ticks: usize,
) -> Result<Vec<TickAlongPath>, GenTicksError> {
    if tick_spacing == 0 {
        return Err(GenTicksError::ZeroTickSpacing);
    }

    // Compressed tick (Python: starting_tick // tick_spacing, rounds toward -inf)
    let compressed = starting_tick.div_euclid(tick_spacing);
    let (word_pos, _) = tick_position(compressed);

    // Pre-generate boundary ticks on demand
    // For descending: start at 0th bit of current word, step = -256 * tick_spacing
    // For ascending: start at 255th bit of current word, step = +256 * tick_spacing
    let (first_boundary, step): (i64, i64) = if less_than_or_equal {
        let first = i64::from(tick_spacing) * 256 * i64::from(word_pos);
        (first, -i64::from(tick_spacing) * 256)
    } else {
        let mut first = i64::from(tick_spacing) * (256 * i64::from(word_pos) + 255);
        if i64::from(starting_tick) >= first {
            first += i64::from(tick_spacing) * 256;
        }
        (first, i64::from(tick_spacing) * 256)
    };

    // Collect initialized ticks in the relevant direction
    let mut initialized_ticks: Vec<i32> = if less_than_or_equal {
        tick_data
            .keys()
            .filter(|&&tick| tick <= starting_tick)
            .copied()
            .collect()
    } else {
        tick_data
            .keys()
            .filter(|&&tick| tick > starting_tick)
            .copied()
            .collect()
    };

    // Sort for merge: descending for less_than_or_equal, ascending otherwise
    if less_than_or_equal {
        initialized_ticks.sort_by(|a, b| b.cmp(a));
    } else {
        initialized_ticks.sort_unstable();
    }

    // Merge boundary ticks and initialized ticks
    let mut result = Vec::with_capacity(max_ticks);
    let mut boundary_tick = first_boundary;
    let mut ii: usize = 0; // initialized tick index

    // First phase: interleave while initialized ticks remain
    while ii < initialized_ticks.len() && result.len() < max_ticks {
        let init_tick = i64::from(initialized_ticks[ii]);

        let (next_tick, is_init) = match (less_than_or_equal, init_tick.cmp(&boundary_tick)) {
            // The initialized tick is closer in the swap direction
            (true, std::cmp::Ordering::Greater) | (false, std::cmp::Ordering::Less) => {
                (init_tick, true)
            }
            // The boundary tick is closer in the swap direction
            (true, std::cmp::Ordering::Less) | (false, std::cmp::Ordering::Greater) => {
                (boundary_tick, false)
            }
            // Both on the boundary — advance both iterators.
            // Python yields the current boundary first, then advances.
            // We advance boundary_tick here (post-push won't do it since
            // is_init=true), and let the post-push `if is_init { ii += 1 }`
            // handle the initialized tick advancement.
            (_, std::cmp::Ordering::Equal) => {
                let tick = boundary_tick;
                boundary_tick += step;
                (tick, true)
            }
        };

        // Clamp to valid tick range
        if less_than_or_equal {
            if next_tick < i64::from(MIN_TICK) {
                break;
            }
        } else if next_tick > i64::from(MAX_TICK) {
            break;
        }

        // SAFETY: next_tick is within i32 range because it was clamped
        // to [MIN_TICK, MAX_TICK] which are both i32 values.
        #[allow(clippy::cast_possible_truncation)]
        let tick_i32 = next_tick as i32;

        result.push(TickAlongPath {
            tick: tick_i32,
            is_initialized: is_init,
        });

        if is_init {
            ii += 1;
        } else {
            boundary_tick += step;
        }
    }

    // Second phase: yield remaining boundary ticks.
    //
    // When ``boundary_tick`` crosses past MIN_TICK / MAX_TICK, CLAMP it to
    // the bound + push the clamped tick (then stop) rather than just
    // breaking. A bare ``break`` leaves the swap loop (in `v3_simulate_swap` /
    // `v4_simulate_swap`) with an exhausted tick list while
    // ``amount_specified_remaining`` is still positive + the price-limit
    // unreached — the walk strands in the last fully-fitting word + never
    // enters the final word (the one containing MIN/MAX_TICK) whose
    // boundary tick drives the price to the limit. Python's `gen_ticks`
    // yields boundary ticks *forever* (`while True: yield`), so clamping to
    // the bound matches that behaviour at the extremity. Mirrors the V3/V4
    // Solity swap's MIN/MAX_TICK termination.
    while result.len() < max_ticks {
        let clamped;
        if less_than_or_equal {
            if boundary_tick < i64::from(MIN_TICK) {
                clamped = Some(i64::from(MIN_TICK));
            } else {
                clamped = None;
            }
        } else if boundary_tick > i64::from(MAX_TICK) {
            clamped = Some(i64::from(MAX_TICK));
        } else {
            clamped = None;
        }

        let push_tick = clamped.unwrap_or(boundary_tick);

        // SAFETY: push_tick is within i32 range because it is clamped to
        // [MIN_TICK, MAX_TICK] which are both i32 values, or an unclamped
        // boundary still inside them.
        #[allow(clippy::cast_possible_truncation)]
        result.push(TickAlongPath {
            tick: push_tick as i32,
            is_initialized: false,
        });

        if clamped.is_some() {
            // The last boundary tick was at the MIN/MAX bound — stop.
            break;
        }
        boundary_tick += step;
    }

    Ok(result)
}

/// Compute V3 tick ranges for the solver.
///
/// Given the current pool state and swap direction, produces a sequence of
/// [`V3TickRangeForSolver`] objects representing adjacent tick ranges, plus the
/// index of the range containing the current price.
///
/// Unlike the Python `_compute_tick_ranges()` which walks the bitmap in the
/// *opposite* direction from the swap (producing "inverted" ranges where
/// `tick_lower > tick_upper`), this implementation walks in the **same**
/// direction as the swap, ensuring `tick_lower < tick_upper` and
/// `sqrt_price_lower < sqrt_price_upper` for every range — the invariant
/// required by the Möbius solver.
///
/// # Direction logic
///
/// * `zero_for_one = true` (price goes DOWN): walk ticks going DOWN from
///   current. `gen_ticks` with `less_than_or_equal = true` yields descending
///   ticks including the current tick. Since ticks are in descending order
///   (e.g., `[0, -60, -120]`), `tick_lower = [i+1]` and `tick_upper = [i]`
///   gives `tick_lower < tick_upper`.
///
/// * `zero_for_one = false` (price goes UP): walk ticks going UP from
///   current. `gen_ticks` with `less_than_or_equal = false` yields ascending
///   ticks *above* current (e.g., `[60, 120]`). The current tick must be
///   prepended. Since ticks are in ascending order (e.g., `[0, 60, 120]`),
///   `tick_lower = [i]` and `tick_upper = [i+1]` gives
///   `tick_lower < tick_upper`.
///
/// # Arguments
///
/// * `tick_data` - Map of initialized tick index → `TickInfo`
/// * `current_tick` - Current pool tick
/// * `tick_spacing` - Tick spacing for this pool
/// * `liquidity` - Current active liquidity
/// * `zero_for_one` - Swap direction
/// * `max_ranges` - Maximum number of tick ranges to produce (default 3)
///
/// # Returns
///
/// A tuple `(ranges, current_range_index)` or `None` if insufficient ranges.
#[must_use]
pub fn compute_tick_ranges<S: std::hash::BuildHasher>(
    tick_data: &HashMap<i32, TickInfo, S>,
    current_tick: i32,
    tick_spacing: i32,
    liquidity: u128,
    zero_for_one: bool,
    max_ranges: usize,
) -> Option<(Vec<V3TickRangeForSolver>, usize)> {
    // Walk in the SAME direction as the swap — this is the key difference
    // from the Python code, which walks the opposite direction and then
    // assigns tick_lower/tick_upper "backwards", producing ranges where
    // tick_lower > tick_upper (invalid for the solver).
    let less_than_or_equal = zero_for_one;

    let ticks = gen_ticks(
        tick_data,
        current_tick,
        tick_spacing,
        less_than_or_equal,
        max_ranges + 10,
    )
    .ok()?;

    // Collect initialized ticks (and the current tick), clamping to MIN/MAX tick
    let mut initialized_ticks: Vec<i32> = Vec::new();
    for tp in &ticks {
        let tick = tp.tick;

        // Clamp to valid tick range
        let clamped = if less_than_or_equal {
            tick.max(MIN_TICK)
        } else {
            tick.min(MAX_TICK)
        };

        if clamped != tick {
            break;
        }

        if initialized_ticks.len() > max_ranges {
            break;
        }

        if tp.is_initialized || tick == current_tick {
            initialized_ticks.push(tick);
        }
    }

    // gen_ticks does NOT yield the current tick when it is neither
    // initialized nor at a word boundary. In both swap directions, the
    // current tick must be present as one boundary of the first range.
    // Prepend it if missing.
    //   Descending (zfo=true):  [current_tick, next_below, next_next_below, ...]
    //   Ascending  (zfo=false): [current_tick, next_above, next_next_above, ...]
    if initialized_ticks.first() != Some(&current_tick) {
        initialized_ticks.insert(0, current_tick);
    }

    if initialized_ticks.len() < 2 {
        return None;
    }

    let mut ranges: Vec<V3TickRangeForSolver> = Vec::new();

    for i in 0..(initialized_ticks.len() - 1) {
        // Ticks are ordered in the swap direction:
        //   Descending (zfo=true):  [0, -60, -120]  → lower=[i+1], upper=[i]
        //   Ascending  (zfo=false): [0, 60, 120]    → lower=[i], upper=[i+1]
        let (tick_lower, tick_upper) = if zero_for_one {
            // Descending: [i+1] is the numerically lower tick
            (initialized_ticks[i + 1], initialized_ticks[i])
        } else {
            // Ascending: [i] is the numerically lower tick
            (initialized_ticks[i], initialized_ticks[i + 1])
        };

        // The liquidity_net comes from the boundary tick being crossed
        // in the swap direction:
        //   zfo=true (going DOWN): cross tick_lower from above → use tick_lower's net
        //   zfo=false (going UP): cross tick_upper from below → use tick_upper's net
        let tick_key = if zero_for_one { tick_lower } else { tick_upper };
        let range_liquidity = tick_data.get(&tick_key).map_or_else(
            || i128::try_from(liquidity).unwrap_or(0),
            |info| {
                // liquidity_net is I256 — extract the low 128 bits as i128
                let bytes = info.liquidity_net.to_be_bytes::<32>();
                let low_bytes: [u8; 16] = bytes[16..32].try_into().unwrap_or([0u8; 16]);
                i128::from_be_bytes(low_bytes)
            },
        );

        let sqrt_price_lower =
            degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal(tick_lower)
                .ok()?;
        let sqrt_price_upper =
            degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal(tick_upper)
                .ok()?;

        debug_assert!(
            tick_lower < tick_upper,
            "tick_lower {tick_lower} >= tick_upper {tick_upper}"
        );
        debug_assert!(
            sqrt_price_lower <= sqrt_price_upper,
            "sqrt_price_lower > sqrt_price_upper for ticks [{tick_lower}, {tick_upper}]"
        );

        ranges.push(V3TickRangeForSolver {
            tick_lower,
            tick_upper,
            liquidity_net: range_liquidity,
            sqrt_price_lower: U256::from(sqrt_price_lower),
            sqrt_price_upper: U256::from(sqrt_price_upper),
        });

        // Note: we do NOT search for current_idx here. The Python code's
        // `tick_lower <= current_tick < tick_upper` check never matches
        // because the Python ranges are inverted (tick_lower > tick_upper).
        // In practice, Python always uses current_idx=0. In our implementation,
        // range 0 always brackets the current price by construction (we start
        // from the current tick and walk outward), so current_idx=0 is correct.
    }

    if ranges.is_empty() {
        return None;
    }

    // Range 0 always contains the current price: we start the walk from
    // the current tick and it becomes one boundary of the first range.
    Some((ranges, 0))
}

/// A V3 tick range for the solver.
///
/// Mirrors the Python `V3TickRangeInfo` dataclass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V3TickRangeForSolver {
    /// Lower tick boundary of this range.
    pub tick_lower: i32,
    /// Upper tick boundary of this range.
    pub tick_upper: i32,
    /// `liquidity_net` at the boundary tick (determines range liquidity).
    pub liquidity_net: i128,
    /// Sqrt price at the lower tick boundary (Q128.96 as U256).
    pub sqrt_price_lower: U256,
    /// Sqrt price at the upper tick boundary (Q128.96 as U256).
    pub sqrt_price_upper: U256,
}

/// Update a single tick's `liquidity_gross` and `liquidity_net` in-place.
///
/// Matches the Uniswap V3 `Tick.update()` logic:
/// - `liquidity_gross += delta` (always, for both lower and upper ticks)
/// - `liquidity_net += delta` for the lower tick
/// - `liquidity_net -= delta` for the upper tick
///
/// The `is_lower_tick` parameter controls whether to add or subtract the
/// delta from `liquidity_net` (matches Solidity's `if (upper)` check).
///
/// If a tick doesn't exist yet, it's inserted with appropriate initial values.
/// Ticks with `liquidity_gross == 0` after the update should be removed by the caller.
pub fn update_tick_liquidity<S: std::hash::BuildHasher>(
    tick_data: &mut HashMap<i32, TickInfo, S>,
    tick: i32,
    delta: i128,
    is_lower_tick: bool,
    block: u64,
) {
    let entry = tick_data.entry(tick).or_insert(TickInfo {
        liquidity_gross: U128::ZERO,
        liquidity_net: I256::ZERO,
        block,
    });
    // Update the per-tick block to the event block — mirrors the Python
    // ``LiquidityAtTick`` replacement (a touched tick's block reflects the
    // last mutation).
    entry.block = block;

    // Update liquidity_gross: += delta (always the same direction for both ticks)
    let current_gross = entry.liquidity_gross.to::<u128>();
    let new_gross_i128 = current_gross.cast_signed() + delta;
    // liquidity_gross is always >= 0 in valid state; negative means an underflow bug
    let new_gross = if new_gross_i128 < 0 {
        U128::ZERO
    } else {
        U128::from(new_gross_i128.cast_unsigned())
    };
    entry.liquidity_gross = new_gross;

    // Update liquidity_net: += delta for lower tick, -= delta for upper tick
    let delta_i256 = I256::try_from(delta).unwrap_or(I256::ZERO);
    let current_net = entry.liquidity_net;
    entry.liquidity_net = if is_lower_tick {
        current_net.checked_add(delta_i256).unwrap_or(I256::ZERO)
    } else {
        current_net.checked_sub(delta_i256).unwrap_or(I256::ZERO)
    };
}

/// Apply a liquidity delta to a tick range [`tick_lower`, `tick_upper`].
///
/// Updates both boundary ticks and removes ticks whose `liquidity_gross`
/// has dropped to zero. This is the standard `Tick.update()` pattern for
/// Mint/Burn/ModifyLiquidity events.
///
/// # Panics
///
/// Never panics in normal operation. Uses `checked_add`/`checked_sub` for
/// `liquidity_net` and saturates at zero for `liquidity_gross`.
pub fn apply_liquidity_to_tick_range<S: std::hash::BuildHasher>(
    tick_data: &mut HashMap<i32, TickInfo, S>,
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: i128,
    block: u64,
) {
    update_tick_liquidity(tick_data, tick_lower, liquidity_delta, true, block);
    update_tick_liquidity(tick_data, tick_upper, liquidity_delta, false, block);
    // Match Uniswap V3 `Tick.update`: only the two touched boundary ticks can
    // have transitioned to zero `liquidity_gross` on this call, so only those
    // need a gross-zero removal check. A full `HashMap::retain` here would be
    // O(N) in the number of initialized ticks (mainnet V3 pools routinely
    // have hundreds–thousands), turning every Mint/Burn/ModifyLiquidity event
    // into an O(N) map walk + rehash on the hot path.
    //
    // By the `apply_liquidity_to_tick_range` invariant, no zero-gross tick is
    // left in `tick_data` at the end of a call, so any zero-gross tick present
    // at entry must be one we just touched this call.
    for tick in [tick_lower, tick_upper] {
        if tick_data
            .get(&tick)
            .is_some_and(|info| info.liquidity_gross.is_zero())
        {
            tick_data.remove(&tick);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{I256, U128};

    fn make_tick_info(liquidity_gross: u128, liquidity_net: i128) -> TickInfo {
        TickInfo {
            liquidity_gross: U128::from(liquidity_gross),
            liquidity_net: I256::try_from(liquidity_net).unwrap_or(I256::ZERO),
            block: 0,
        }
    }

    #[test]
    fn test_gen_ticks_initialized_on_word_boundary_descending() {
        // Regression test: when an initialized tick sits exactly on a word
        // boundary (tick 0 is at word_pos 0, bit 0), the Equal branch must
        // yield the boundary tick BEFORE advancing, not after.
        let mut tick_data = HashMap::new();
        // tick 0 is at word boundary (word_pos=0, bit_pos=0)
        tick_data.insert(0, make_tick_info(100, 50));
        tick_data.insert(-60, make_tick_info(200, 100));

        let ticks = gen_ticks(&tick_data, 1, 60, true, 10).unwrap();

        // Descending from tick 1, tick_spacing=60
        // compressed = 1 / 60 = 0, word_pos = 0
        // first_boundary_tick = 60 * 256 * 0 = 0
        // Initialized ticks (desc, ≤ 1): [0, -60]
        // Merge: init=0, boundary=0 → equal → yield (0, true), advance both
        // Then boundary = -15360, init = -60
        // init(-60) > boundary(-15360) → yield (-60, true)
        let init_ticks: Vec<(i32, bool)> = ticks
            .iter()
            .take(4)
            .map(|t| (t.tick, t.is_initialized))
            .collect();
        assert_eq!(
            init_ticks[0],
            (0, true),
            "first tick should be 0 (initialized boundary)"
        );
        assert_eq!(init_ticks[1], (-60, true), "second tick should be -60");
    }

    #[test]
    fn test_gen_ticks_initialized_on_word_boundary_ascending() {
        // Ascending: tick at 15360 (word boundary at word 0, bit 255 with spacing 60)
        let mut tick_data = HashMap::new();
        tick_data.insert(15300, make_tick_info(100, 50));
        tick_data.insert(15360, make_tick_info(200, 100));
        tick_data.insert(16020, make_tick_info(300, 150));

        let ticks = gen_ticks(&tick_data, 15200, 60, false, 10).unwrap();

        // Ascending from tick 15200, tick_spacing=60
        // compressed = 15200 / 60 = 253, word_pos = 0
        // first_boundary_tick = 60 * (256*0 + 255) = 60*255 = 15300
        // If 15200 < 15300, starting from boundary at same level
        // init=15300 < boundary=15300... depends on exact ordering
        let init_ticks: Vec<(i32, bool)> = ticks
            .iter()
            .take(5)
            .map(|t| (t.tick, t.is_initialized))
            .collect();
        // First tick should be at the boundary (initialized or not)
        assert!(
            init_ticks[0].0 <= 15360,
            "first ticks should be near current tick"
        );
    }

    #[test]
    fn test_gen_ticks_empty_tick_data_ascending() {
        let tick_data = HashMap::new();
        let ticks = gen_ticks(&tick_data, 0, 60, false, 5).unwrap();

        // No initialized ticks — should return boundary ticks only
        // Starting tick = 0, tick_spacing = 60, ascending
        // compressed = 0 / 60 = 0
        // word_pos = 0 >> 8 = 0
        // first_boundary_tick = 60 * (256 * 0 + 255) = 60 * 255 = 15300
        // step = 60 * 256 = 15360
        assert_eq!(ticks.len(), 5);
        assert_eq!(ticks[0].tick, 15300);
        assert!(!ticks[0].is_initialized);
        assert_eq!(ticks[1].tick, 15300 + 15360);
    }

    #[test]
    fn test_gen_ticks_empty_tick_data_descending() {
        let tick_data = HashMap::new();
        let ticks = gen_ticks(&tick_data, 0, 60, true, 5).unwrap();

        // No initialized ticks — descending from tick 0
        // compressed = 0 / 60 = 0
        // word_pos = 0 >> 8 = 0
        // first_boundary_tick = 60 * 256 * 0 = 0
        // step = -60 * 256 = -15360
        assert_eq!(ticks.len(), 5);
        assert_eq!(ticks[0].tick, 0);
        assert!(!ticks[0].is_initialized);
        assert_eq!(ticks[1].tick, -15360);
    }

    #[test]
    fn test_gen_ticks_with_initialized_ticks_descending() {
        let mut tick_data = HashMap::new();
        tick_data.insert(-120, make_tick_info(100, 50));
        tick_data.insert(-60, make_tick_info(200, 100));
        tick_data.insert(0, make_tick_info(300, 150));
        tick_data.insert(60, make_tick_info(400, 200));

        // Descending from tick 100, tick_spacing = 60
        // initialized ticks <= 100: [60, 0, -60, -120] (sorted desc)
        let ticks = gen_ticks(&tick_data, 100, 60, true, 10).unwrap();

        // Should interleave initialized ticks and boundary ticks
        // Starting tick = 100, less_than_or_equal = true
        // compressed = 100.div_euclid(60) = 1
        // word_pos = 1 >> 8 = 0
        // first_boundary_tick = 60 * 256 * 0 = 0
        // step = -60 * 256 = -15360
        // Initialized ticks (desc): 60, 0, -60, -120
        // Boundary ticks (desc): 0, -15360, ...
        // Merge:
        //   init=60 > boundary=0 → yield (60, true)
        //   init=0 == boundary=0 → yield boundary tick=true, advance both
        //     But wait: after yielding equal, boundary advances to 0 + (-15360) = -15360
        //     and ii advances past 0
        //   init=-60 > boundary=-15360 → yield (-60, true)
        //   init=-120 > boundary=-15360 → yield (-120, true)
        //   Then boundary ticks only: -15360, ...

        assert!(ticks.len() >= 5);

        // First tick should be initialized tick 60
        let first_init = ticks.iter().find(|t| t.is_initialized);
        assert!(first_init.is_some());
        assert_eq!(first_init.unwrap().tick, 60);
    }

    #[test]
    fn test_gen_ticks_with_initialized_ticks_ascending() {
        let mut tick_data = HashMap::new();
        tick_data.insert(60, make_tick_info(400, 200));
        tick_data.insert(120, make_tick_info(500, 250));
        tick_data.insert(180, make_tick_info(600, 300));

        // Ascending from tick 0, tick_spacing = 60
        // initialized ticks > 0: [60, 120, 180] (sorted asc)
        let ticks = gen_ticks(&tick_data, 0, 60, false, 10).unwrap();

        // compressed = 0 / 60 = 0, word_pos = 0
        // first_boundary_tick = 60 * (256 * 0 + 255) = 15300
        // All initialized ticks (60, 120, 180) are less than 15300
        // So they should all be yielded as initialized ticks first
        let init_ticks: Vec<i32> = ticks
            .iter()
            .filter(|t| t.is_initialized)
            .map(|t| t.tick)
            .collect();
        assert_eq!(init_ticks, vec![60, 120, 180]);
    }

    #[test]
    fn test_gen_ticks_rejects_zero_tick_spacing() {
        let tick_data = HashMap::new();
        let result = gen_ticks(&tick_data, 0, 0, false, 10);
        assert_eq!(result, Err(GenTicksError::ZeroTickSpacing));
    }

    #[test]
    fn test_gen_ticks_clamps_to_max_tick_ascending() {
        let tick_data = HashMap::new();
        let ticks = gen_ticks(&tick_data, 887_000, 60, false, 100).unwrap();

        // Should not include any tick above MAX_TICK (887272)
        for tp in &ticks {
            assert!(tp.tick <= MAX_TICK, "tick {} exceeds MAX_TICK", tp.tick);
        }
    }

    #[test]
    fn test_gen_ticks_clamps_to_min_tick_descending() {
        let tick_data = HashMap::new();
        let ticks = gen_ticks(&tick_data, -887_000, 60, true, 100).unwrap();

        // Should not include any tick below MIN_TICK (-887272)
        for tp in &ticks {
            assert!(tp.tick >= MIN_TICK, "tick {} below MIN_TICK", tp.tick);
        }
    }

    fn assert_ranges_well_formed(ranges: &[V3TickRangeForSolver]) {
        for r in ranges {
            assert!(
                r.tick_lower < r.tick_upper,
                "tick_lower {} >= tick_upper {}",
                r.tick_lower,
                r.tick_upper
            );
            assert!(
                r.sqrt_price_lower <= r.sqrt_price_upper,
                "sqrt_price_lower > sqrt_price_upper for ticks [{}, {}]",
                r.tick_lower,
                r.tick_upper
            );
        }
    }

    /// Helper: verify `current_idx` is always 0 (range 0 always contains
    /// the current price by construction).
    fn assert_current_idx_is_zero(current_idx: usize) {
        assert_eq!(current_idx, 0, "current_idx should always be 0");
    }

    #[test]
    fn test_compute_tick_ranges_zfo_descending() {
        let mut tick_data = HashMap::new();
        // current_tick=0, tick_spacing=60
        // For zfo=true: gen_ticks(descending) yields [0, -60, -120, ...]
        // current tick 0 is not initialized but is included as current_tick
        tick_data.insert(-120, make_tick_info(100, 50));
        tick_data.insert(-60, make_tick_info(200, 100));
        tick_data.insert(60, make_tick_info(300, 150));
        tick_data.insert(120, make_tick_info(400, 200));

        let result = compute_tick_ranges(&tick_data, 0, 60, 1_000_000, true, 3);
        let (ranges, current_idx) = result.expect("should produce ranges for zfo=true");

        assert_ranges_well_formed(&ranges);

        // Descending ticks: [0 (current), -60, -120]
        // Range 0: tick_lower=-60, tick_upper=0 (swap crosses -60 going down)
        // Range 1: tick_lower=-120, tick_upper=-60 (swap crosses -120 going down)
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].tick_lower, -60);
        assert_eq!(ranges[0].tick_upper, 0);
        assert_eq!(ranges[1].tick_lower, -120);
        assert_eq!(ranges[1].tick_upper, -60);

        // Current tick (0) is in range [-60, 0) — that's range 0
        assert_eq!(current_idx, 0);
    }

    #[test]
    fn test_compute_tick_ranges_ofz_ascending() {
        let mut tick_data = HashMap::new();
        // current_tick=0, tick_spacing=60
        // For zfo=false: gen_ticks(ascending) yields [60, 120, ...]
        // Current tick 0 is prepended: [0, 60, 120]
        tick_data.insert(-120, make_tick_info(100, 50));
        tick_data.insert(-60, make_tick_info(200, 100));
        tick_data.insert(60, make_tick_info(300, 150));
        tick_data.insert(120, make_tick_info(400, 200));

        let result = compute_tick_ranges(&tick_data, 0, 60, 1_000_000, false, 3);
        let (ranges, current_idx) = result.expect("should produce ranges for zfo=false");

        assert_ranges_well_formed(&ranges);

        // Ascending ticks (with current prepended): [0, 60, 120]
        // Range 0: tick_lower=0, tick_upper=60 (swap crosses 60 going up)
        // Range 1: tick_lower=60, tick_upper=120 (swap crosses 120 going up)
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].tick_lower, 0);
        assert_eq!(ranges[0].tick_upper, 60);
        assert_eq!(ranges[1].tick_lower, 60);
        assert_eq!(ranges[1].tick_upper, 120);

        // Current tick (0) is in range [0, 60) — that's range 0
        assert_eq!(current_idx, 0);
    }

    #[test]
    fn test_compute_tick_ranges_liquidity_net_selection() {
        let mut tick_data = HashMap::new();
        tick_data.insert(-60, make_tick_info(200, -100));
        tick_data.insert(60, make_tick_info(300, 150));

        // zfo=true: tick_lower = -60, we use tick_lower's liquidity_net = -100
        let result = compute_tick_ranges(&tick_data, 0, 60, 1_000_000, true, 3);
        let (ranges, _) = result.expect("should produce ranges");
        assert_ranges_well_formed(&ranges);
        assert_eq!(ranges[0].liquidity_net, -100);

        // zfo=false: tick_upper = 60, we use tick_upper's liquidity_net = 150
        let result = compute_tick_ranges(&tick_data, 0, 60, 1_000_000, false, 3);
        let (ranges, _) = result.expect("should produce ranges");
        assert_ranges_well_formed(&ranges);
        assert_eq!(ranges[0].liquidity_net, 150);
    }

    #[test]
    fn test_compute_tick_ranges_insufficient_ticks() {
        let mut tick_data = HashMap::new();
        // Only one initialized tick on each side — not enough for both
        // directions at once
        tick_data.insert(60, make_tick_info(300, 150));

        // zfo=true needs initialized ticks <= 0. None exist except
        // the current tick itself — only 1 entry, not enough.
        let result = compute_tick_ranges(&tick_data, 0, 60, 1_000_000, true, 3);
        assert_eq!(result, None);
    }

    #[test]
    fn test_compute_tick_ranges_current_at_initialized_tick() {
        let mut tick_data = HashMap::new();
        // Current tick IS an initialized tick
        tick_data.insert(0, make_tick_info(500, 250));
        tick_data.insert(60, make_tick_info(300, 150));
        tick_data.insert(120, make_tick_info(400, 200));
        tick_data.insert(-60, make_tick_info(200, -100));

        // zfo=false: ascending ticks above 0 → [0, 60, 120]
        // (tick 0 is initialized, so gen_ticks includes it)
        let result = compute_tick_ranges(&tick_data, 0, 60, 1_000_000, false, 3);
        let (ranges, current_idx) = result.expect("should produce ranges");
        assert_ranges_well_formed(&ranges);
        assert_eq!(ranges[0].tick_lower, 0);
        assert_eq!(ranges[0].tick_upper, 60);
        assert_eq!(current_idx, 0);
    }

    #[test]
    fn test_compute_tick_ranges_spike_tick_spacing() {
        let mut tick_data = HashMap::new();
        // tick_spacing=1 ( spike pools)
        tick_data.insert(1, make_tick_info(100, 50));
        tick_data.insert(2, make_tick_info(200, 100));
        tick_data.insert(-1, make_tick_info(150, 75));

        // zfo=false: ascending from 0 → [0(current), 1, 2]
        let result = compute_tick_ranges(&tick_data, 0, 1, 1_000_000, false, 3);
        let (ranges, current_idx) = result.expect("should produce ranges");
        assert_ranges_well_formed(&ranges);
        assert_eq!(ranges[0].tick_lower, 0);
        assert_eq!(ranges[0].tick_upper, 1);
        assert_eq!(ranges[1].tick_lower, 1);
        assert_eq!(ranges[1].tick_upper, 2);
        assert_eq!(current_idx, 0);
    }

    /// Property: for any tick data with enough initialized ticks on both sides,
    /// `compute_tick_ranges` always produces well-formed ranges where
    /// `tick_lower < tick_upper` and `sqrt_price_lower <= sqrt_price_upper`,
    /// and `current_range_index` points to a range that contains the
    /// current tick.
    #[test]
    fn test_compute_tick_ranges_invariant_property() {
        // Test a grid of tick spacings and current tick positions
        let tick_spacings = [1, 10, 60, 200];
        let current_ticks = [-1000, -60, -1, 0, 1, 60, 1000];

        for &tick_spacing in &tick_spacings {
            for &current_tick in &current_ticks {
                // Build tick data with initialized ticks at multiples of tick_spacing
                // within a reasonable range around current_tick
                let mut tick_data = HashMap::new();
                let range_start = (current_tick - 5 * tick_spacing) / tick_spacing * tick_spacing;
                let range_end = (current_tick + 5 * tick_spacing) / tick_spacing * tick_spacing;
                let mut t = range_start;
                while t <= range_end {
                    if t != current_tick {
                        tick_data.insert(t, make_tick_info(100, 50));
                    }
                    t += tick_spacing;
                }

                for &zero_for_one in &[true, false] {
                    let result = compute_tick_ranges(
                        &tick_data,
                        current_tick,
                        tick_spacing,
                        1_000_000,
                        zero_for_one,
                        3,
                    );

                    if let Some((ranges, current_idx)) = result {
                        // Invariant 1: all ranges well-formed
                        assert_ranges_well_formed(&ranges);

                        // Invariant 2: current_idx is always 0 by construction
                        assert_current_idx_is_zero(current_idx);

                        // Invariant 3: current price must be at a boundary
                        // of the first range (by construction — the walk starts
                        // at current_tick, which becomes one boundary of range 0)
                        let r0 = &ranges[0];
                        let at_boundary =
                            current_tick == r0.tick_lower || current_tick == r0.tick_upper;
                        assert!(
                            at_boundary,
                            "current_tick {current_tick} not at boundary of first range [{}, {}]",
                            r0.tick_lower, r0.tick_upper
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_apply_liquidity_removes_touched_tick_when_gross_zero() {
        // Mint then burn the same liquidity: both boundary ticks should drop
        // to zero gross and be removed (matches Solidity `Tick.update`).
        let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
        apply_liquidity_to_tick_range(&mut tick_data, 100, 200, 1_000_000, 0);
        assert!(tick_data.contains_key(&100));
        assert!(tick_data.contains_key(&200));

        apply_liquidity_to_tick_range(&mut tick_data, 100, 200, -1_000_000, 0);
        assert!(
            !tick_data.contains_key(&100),
            "lower tick not pruned at zero gross"
        );
        assert!(
            !tick_data.contains_key(&200),
            "upper tick not pruned at zero gross"
        );
    }

    #[test]
    fn test_apply_liquidity_only_prunes_touched_ticks() {
        // A pre-existing zero-gross tick *outside* the touched pair is left
        // alone — we no longer run a full `retain`. This is the O(1) contract:
        // apply touches only `tick_lower` / `tick_upper`.
        let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
        // Seed an initialized tick we will not touch.
        tick_data.insert(50, make_tick_info(100, 50));
        // Seed a (transient, out-of-contract) zero-gross tick elsewhere.
        tick_data.insert(12345, make_tick_info(0, 0));

        apply_liquidity_to_tick_range(&mut tick_data, 100, 200, 1_000_000, 0);

        assert!(
            tick_data.contains_key(&50),
            "untouched live tick must survive"
        );
        // The pre-existing zero-gross tick is NOT pruned by the touched-pair
        // fast path; it persists until its own range is touched (matching
        // Solidity's per-tick `Tick.update`).
        assert!(
            tick_data.contains_key(&12345),
            "untouched zero-gross tick must survive (no full retain)"
        );
    }

    #[test]
    fn test_apply_liquidity_is_o1_in_pool_size() {
        // A single apply must not scale with the number of initialized ticks.
        // We don't benchmark wall-clock (flaky in CI); instead we assert the
        // post-apply map size grows by exactly 2 regardless of starting size —
        // i.e. no wholesale scan/rehash touched the rest of the map.
        // (seed_count, untouched range to add)
        for (seed_count, lower, upper) in [
            (0usize, 100i32, 200i32),
            (100usize, 1_000i32, 1_100i32),
            (1000usize, 10_000i32, 10_100i32),
        ] {
            let mut tick_data: HashMap<i32, TickInfo> = HashMap::with_capacity(seed_count + 2);
            for t in 0..seed_count {
                tick_data.insert(i32::try_from(t).unwrap(), make_tick_info(100, 50));
            }
            apply_liquidity_to_tick_range(&mut tick_data, lower, upper, 1_000_000, 0);
            assert_eq!(
                tick_data.len(),
                seed_count + 2,
                "apply must add exactly the two touched ticks, regardless of pool size (n={seed_count})"
            );
        }
    }
}

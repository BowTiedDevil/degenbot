//! Liquidity-mapping tick-data mutation math.
//!
//! Canonical Rust home for `apply_liquidity_mapping_update` +
//! `get_tick_word_and_bit_position`.
//!
//! This is the pure computation core of the DB-aware pool updater: given the
//! current tick-data map and a single liquidity event (mint or burn with
//! `tick_lower` / `tick_upper`), it produces the next tick-data state and the
//! updated active `liquidity`. No I/O, no allocation beyond the map copy — the
//! caller owns the map.
//!
//! The typed struct [`LiquidityAtTick`] mirrors the Python
//! `concentrated/types.py` models and `degenbot-bot`'s `TickInfo`:
//! `liquidity_gross: U128`, `liquidity_net: I256`, `block: u64`. `liquidity_net`
//! uses `I256` (not `I128`) so the intermediate `current_net + delta` never
//! overflows for valid inputs — arbitrary-precision arithmetic; the Solidity
//! `int128` bound is the caller's concern.
//!
//! The word-position helper [`get_tick_word_and_bit_position`] survives the
//! word-*value* retirement as load-bearing word-**key** math: it computes the
//! mapping key for the on-chain staged word fetch and the sparse known-word set.
//!
//! # Parity
//!
//! `#[cfg(test)]` parity fixtures (see `tests/liquidity_mapping_fixtures.json`
//! and the `apply_liquidity_mapping_update` step-chains) assert byte-identical
//! `tick_data` / `liquidity` output across in-range adjustments, out-of-range
//! events, tick initialization, gross-to-zero deletion, net-sign handling at
//! the upper tick, large deltas, and the
//! `update_block == initial_state_block` no-op path.

use hashbrown::hash_map::Entry;
use hashbrown::HashMap;

use alloy::primitives::{I256, U128};

use crate::cl::functions::tick_position;

/// Liquidity data at an initialized tick.
///
/// Mirrors the Python `LiquidityAtTick` (`concentrated/types.py`) and
/// `degenbot-bot`'s `TickInfo`. `liquidity_net` is `I256` (wide) so the
/// `current_net ± delta` intermediate never overflows for valid inputs,
/// matching the Python oracle's arbitrary-precision arithmetic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiquidityAtTick {
    /// Signed liquidity delta for ticks crossed left-to-right.
    /// Positive at the lower tick, negative at the upper tick.
    pub liquidity_net: I256,
    /// Total liquidity referencing this tick (always non-negative).
    pub liquidity_gross: U128,
    /// Block at which this tick was last mutated.
    pub block: u64,
}

/// Result of applying a liquidity update to the tick data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiquidityUpdateResult {
    /// The updated tick data (tick → liquidity info).
    pub tick_data: HashMap<i32, LiquidityAtTick>,
    /// The updated active (in-range) liquidity.
    pub liquidity: U128,
}

/// Compute the word position and bit position for a tick, accounting for tick
/// spacing.
///
/// Mirrors the Python `get_tick_word_and_bit_position`:
/// `position(evm_divide(tick, tick_spacing))` where `evm_divide` truncates
/// toward zero (matching EVM / Solidity `int` division and Rust's `/`).
/// `position(t) = (t >> 8, t % 256)` with `%` non-negative (matching Python's
/// modulo and Rust's `rem_euclid`).
///
/// The returned `word` is the mapping key; `bit` is in `0..=255`.
#[must_use]
pub fn get_tick_word_and_bit_position(tick: i32, tick_spacing: i32) -> (i32, u8) {
    // EVM division truncates toward zero — Rust's `/` for signed ints does too.
    let compressed = tick / tick_spacing;
    let (word, bit) = tick_position(compressed);
    (i32::from(word), bit)
}

/// Widen a `U128` to `I256` (zero-extends; always fits since `U128` ≤ 2^128-1
/// &lt; 2^255-1). Done via big-endian bytes to dodge the missing
/// `From<U128> for U256` impl.
#[inline]
fn gross_to_i256(gross: U128) -> I256 {
    let mut full = [0u8; 32];
    full[16..32].copy_from_slice(&gross.to_be_bytes::<16>());
    I256::from_be_bytes::<32>(full)
}

/// Narrow a non-negative `I256` to `U128`, taking the low 128 bits.
///
/// # Panics (debug only)
///
/// `debug_assert!`s the value is in `[0, 2^128 - 1]`, matching the Python
/// `ValidatedUint128` field bound. Release builds truncate to the low 128 bits.
fn narrow_to_u128(v: I256) -> U128 {
    debug_assert!(
        v >= I256::ZERO,
        "liquidity value must be non-negative for uint128"
    );
    let max_u128 = {
        let mut f = [0u8; 32];
        f[16..32].fill(0xFF);
        I256::from_be_bytes::<32>(f)
    };
    debug_assert!(v <= max_u128, "liquidity value exceeds uint128");
    let bytes = v.to_be_bytes::<32>();
    // `bytes[16..32]` is exactly 16 bytes (fixed `[u8; 32]` source), so this
    // slice→array copy is infallible — no `try_into().expect` needed.
    let mut low = [0u8; 16];
    low.copy_from_slice(&bytes[16..32]);
    U128::from_be_bytes(low)
}

/// Apply a liquidity-mapping update to the tick-data map.
///
/// This is the canonical Rust computation core for
/// `UniswapV3Pool.update_liquidity_map` / `UniswapV4Pool.update_liquidity_map`.
/// The input map is consumed
/// (moved) — the caller passes a clone if it needs to retain the prior state.
///
/// # Invariants (debug-checked, mirroring the Python `assert`s)
///
/// - `liquidity` starts non-negative (always true for `U128`).
/// - The in-range `liquidity + liquidity_delta` stays non-negative.
/// - `liquidity_gross + liquidity_delta` stays non-negative (and fits `uint128`).
/// - Both boundary ticks are grid-aligned (`tick % tick_spacing == 0`).
///
/// # Panics
///
/// Panics (debug builds only) if an invariant is violated — matching the
/// Python `assert`s, which are stripped under `python -O`. Release builds do
/// not check.
#[must_use = "the updated state must be used"]
#[expect(clippy::too_many_arguments)]
pub fn apply_liquidity_mapping_update(
    mut tick_data: HashMap<i32, LiquidityAtTick>,
    tick_spacing: i32,
    tick: i32,
    liquidity: U128,
    initial_state_block: u64,
    update_block: u64,
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: I256,
) -> LiquidityUpdateResult {
    let state_block = update_block;
    let mut working_liquidity = liquidity;

    // Both boundary ticks must be grid-aligned, mirroring the Python
    // `ValueError("Invalid tick or spacing")` guard the retired bitmap path
    // enforced.
    debug_assert!(
        tick_lower.rem_euclid(tick_spacing) == 0 && tick_upper.rem_euclid(tick_spacing) == 0,
        "Invalid tick or spacing"
    );

    // Adjust in-range liquidity if the modified region includes the active tick.
    if tick_lower <= tick && tick < tick_upper && state_block > initial_state_block {
        let new_liquidity = gross_to_i256(working_liquidity) + liquidity_delta;
        debug_assert!(
            new_liquidity >= I256::ZERO,
            "in-range liquidity adjustment violated invariant"
        );
        working_liquidity = narrow_to_u128(new_liquidity);
    }

    for current_tick in [tick_lower, tick_upper] {
        // If the tick is uninitialized, seed it empty.
        if let Entry::Vacant(slot) = tick_data.entry(current_tick) {
            slot.insert(LiquidityAtTick {
                liquidity_net: I256::ZERO,
                liquidity_gross: U128::ZERO,
                block: state_block,
            });
        }

        let (current_liquidity_net, current_liquidity_gross) = {
            // `current_tick` is seeded with a placeholder immediately above when
            // absent, so the map is guaranteed populated here; a targeted
            // (fulfilled) expect surfaces that invariant loudly if it ever breaks.
            #[expect(clippy::expect_used)]
            let info = tick_data
                .get(&current_tick)
                .expect("tick just seeded if absent");
            (info.liquidity_net, info.liquidity_gross)
        };

        let new_liquidity_gross_signed = gross_to_i256(current_liquidity_gross) + liquidity_delta;
        debug_assert!(
            new_liquidity_gross_signed >= I256::ZERO,
            "negative gross liquidity"
        );

        let new_liquidity_gross = narrow_to_u128(new_liquidity_gross_signed);

        if new_liquidity_gross == U128::ZERO {
            // No remaining liquidity references this tick — drop it.
            tick_data.remove(&current_tick);
            continue;
        }

        // Positions include the lower tick but exclude the upper tick: the lower
        // tick's net gets +delta, the upper tick's net gets -delta.
        let new_liquidity_net = if current_tick == tick_lower {
            current_liquidity_net + liquidity_delta
        } else {
            current_liquidity_net - liquidity_delta
        };

        tick_data.insert(
            current_tick,
            LiquidityAtTick {
                liquidity_net: new_liquidity_net,
                liquidity_gross: new_liquidity_gross,
                block: state_block,
            },
        );
    }

    LiquidityUpdateResult {
        tick_data,
        liquidity: working_liquidity,
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {

    use super::*;
    use alloy::primitives::{I256, U128, U256};

    /// Parse the fixtures JSON embedded at compile time.
    fn fixtures() -> serde_json::Value {
        serde_json::from_str(include_str!("testdata/liquidity_mapping_fixtures.json"))
            .expect("fixtures JSON parses")
    }

    fn u256_from_json(v: &serde_json::Value) -> U256 {
        U256::from_str_radix(v.as_str().expect("u256 string"), 10).expect("valid u256 decimal")
    }

    fn u128_from_json(v: &serde_json::Value) -> U128 {
        let big = u256_from_json(v);
        let bytes = big.to_be_bytes::<32>();
        let low: [u8; 16] = bytes[16..32].try_into().expect("16 bytes");
        U128::from_be_bytes(low)
    }

    fn i256_from_json(v: &serde_json::Value) -> I256 {
        let s = v.as_str().expect("i256 string");
        if let Some(neg) = s.strip_prefix('-') {
            -I256::try_from(U256::from_str_radix(neg, 10).expect("valid decimal"))
                .expect("fits I256")
        } else {
            I256::try_from(U256::from_str_radix(s, 10).expect("valid decimal")).expect("fits I256")
        }
    }

    /// Assert a tick-data map equals the fixture's `tick_data` object, per tick.
    fn assert_tick_data(data: &HashMap<i32, LiquidityAtTick>, expected: &serde_json::Value) {
        assert_eq!(data.len(), expected.as_object().unwrap().len());
        for (t_str, tv) in expected.as_object().unwrap() {
            let t: i32 = t_str.parse().unwrap();
            assert_eq!(data[&t].liquidity_net, i256_from_json(&tv["liquidity_net"]));
            assert_eq!(
                data[&t].liquidity_gross,
                u128_from_json(&tv["liquidity_gross"])
            );
            assert_eq!(data[&t].block, tv["block"].as_u64().unwrap());
        }
    }

    // ─── get_tick_word_and_bit_position ────────────────────────────────

    #[test]
    fn word_and_bit_for_positive_tick() {
        // tick=1, spacing=1 → compressed=1 → word 0, bit 1
        assert_eq!(get_tick_word_and_bit_position(1, 1), (0, 1));
        // tick=256 → word 1, bit 0
        assert_eq!(get_tick_word_and_bit_position(256, 1), (1, 0));
        // tick=257 → word 1, bit 1
        assert_eq!(get_tick_word_and_bit_position(257, 1), (1, 1));
    }

    #[test]
    fn word_and_bit_for_negative_tick_toward_zero() {
        // tick=-230, spacing=1 → evm_divide(-230,1) = -230 (toward zero)
        // word = -230 >> 8 = -1 ; bit = (-230).rem_euclid(256) = 26
        assert_eq!(get_tick_word_and_bit_position(-230, 1), (-1, 26));
        // tick=-230, spacing=10 → evm_divide=-23 → word = -23>>8 = -1 ; bit = (-23).rem_euclid(256)=233
        assert_eq!(get_tick_word_and_bit_position(-230, 10), (-1, 233));
        // tick=-60, spacing=60 → evm_divide(-60,60)=-1 → word -1, bit 255
        assert_eq!(get_tick_word_and_bit_position(-60, 60), (-1, 255));
    }

    #[test]
    fn word_and_bit_spacing_sixty() {
        // tick=120, spacing=60 → compressed=2 → word 0, bit 2
        assert_eq!(get_tick_word_and_bit_position(120, 60), (0, 2));
        // tick=0 → word 0, bit 0
        assert_eq!(get_tick_word_and_bit_position(0, 60), (0, 0));
    }

    // ─── apply_liquidity_mapping_update parity ─────────────────────────

    #[test]
    fn apply_chain_a_in_range_burn_to_zero() {
        // tick=0 active, ticks [-1000, 1000], spacing 10, init_block 10.
        // Steps: +1M, +250K, -750K, -500K → burns both ticks to zero (deleted),
        // active liquidity back to 0.
        let fix = fixtures();
        let expected = &fix["apply"]["A"];

        let mut data: HashMap<i32, LiquidityAtTick> = HashMap::new();
        let mut liq = U128::ZERO;
        let deltas = [
            (I256::try_from(1_000_000u64).unwrap(), 11u64, 10u64),
            (I256::try_from(250_000u64).unwrap(), 12, 10),
            (I256::try_from(-750_000i64).unwrap(), 13, 10),
            (I256::try_from(-500_000i64).unwrap(), 14, 10),
        ];
        for (delta, update_block, init_block) in deltas {
            let r = apply_liquidity_mapping_update(
                std::mem::take(&mut data),
                10,
                0,
                liq,
                init_block,
                update_block,
                -1000,
                1000,
                delta,
            );
            data = r.tick_data;
            liq = r.liquidity;
        }
        assert_eq!(
            liq,
            u128_from_json(&expected["liquidity"]),
            "chain A active liquidity"
        );
        assert!(
            data.is_empty(),
            "chain A tick_data should be empty (burned to zero)"
        );
    }

    #[test]
    fn apply_chain_b_large_delta_out_of_range() {
        // tick=5000 active, position ticks [-120,-60] then [300,600], spacing 60.
        // Active tick 5000 is never in range → liquidity stays 5_000_000.
        // Steps: +2^100, -2^100 (zero/delete), +123456789 (re-init), +999 (new ticks, out of range).
        let fix = fixtures();
        let expected = &fix["apply"]["B"];

        let mut data: HashMap<i32, LiquidityAtTick> = HashMap::new();
        let mut liq = U128::try_from(5_000_000u64).unwrap();
        // (delta, tick_lower, tick_upper, update_block, init_block)
        let steps: [(I256, i32, i32, u64, u64); 4] = [
            (
                I256::try_from(U256::from(1u128) << 100).unwrap(),
                -120,
                -60,
                101,
                100,
            ),
            (
                -I256::try_from(U256::from(1u128) << 100).unwrap(),
                -120,
                -60,
                102,
                100,
            ),
            (I256::try_from(123_456_789u64).unwrap(), -120, -60, 103, 100),
            (I256::try_from(999u64).unwrap(), 300, 600, 104, 100),
        ];
        for (delta, tl, tu, update_block, init_block) in steps {
            let r = apply_liquidity_mapping_update(
                std::mem::take(&mut data),
                60,
                5000,
                liq,
                init_block,
                update_block,
                tl,
                tu,
                delta,
            );
            data = r.tick_data;
            liq = r.liquidity;
        }
        assert_eq!(
            liq,
            u128_from_json(&expected["liquidity"]),
            "chain B active liquidity (out-of-range, unchanged)"
        );
        assert_tick_data(&data, &expected["tick_data"]);
    }

    #[test]
    fn apply_chain_c_net_signs_and_reinit_delete() {
        // ticks [10,20], tick=0 (out of range: 10<=0 is false) → active stays 0.
        // Steps: +1000, +1000, -1000, +500 (new tick 5), -500 (tick 5 deleted).
        let fix = fixtures();
        let expected = &fix["apply"]["C"];

        let mut data: HashMap<i32, LiquidityAtTick> = HashMap::new();
        let mut liq = U128::ZERO;
        let steps: [(I256, i32, i32, u64, u64); 5] = [
            (I256::try_from(1000u64).unwrap(), 10, 20, 201, 200),
            (I256::try_from(1000u64).unwrap(), 10, 20, 202, 200),
            (I256::try_from(-1000i64).unwrap(), 10, 20, 203, 200),
            (I256::try_from(500u64).unwrap(), 5, 20, 204, 200),
            (I256::try_from(-500i64).unwrap(), 5, 20, 205, 200),
        ];
        for (delta, tl, tu, update_block, init_block) in steps {
            let r = apply_liquidity_mapping_update(
                std::mem::take(&mut data),
                1,
                0,
                liq,
                init_block,
                update_block,
                tl,
                tu,
                delta,
            );
            data = r.tick_data;
            liq = r.liquidity;
        }
        assert_eq!(liq, u128_from_json(&expected["liquidity"]));
        assert_tick_data(&data, &expected["tick_data"]);
    }

    #[test]
    fn apply_chain_d_block_equals_init_no_inrange_change() {
        // update_block (300) == initial_state_block (300) → in-range branch skipped,
        // active liquidity stays at the initial 0 even though tick=0 is in range.
        let fix = fixtures();
        let expected = &fix["apply"]["D"];

        let data_in: HashMap<i32, LiquidityAtTick> = HashMap::new();
        let r = apply_liquidity_mapping_update(
            data_in,
            10,
            0,
            U128::ZERO,
            300,
            300,
            -100,
            100,
            I256::try_from(1000u64).unwrap(),
        );
        assert_eq!(
            r.liquidity,
            u128_from_json(&expected["liquidity"]),
            "chain D liquidity (no in-range change)"
        );
        assert_tick_data(&r.tick_data, &expected["tick_data"]);
    }

    #[test]
    fn apply_chain_e_in_range_nonzero_residual() {
        // tick=0 active within [-100,100], spacing 10, init_block 400.
        // Steps: +1M (active→1M), +500K (→1.5M), -200K (→1.3M).
        let fix = fixtures();
        let expected = &fix["apply"]["E"];

        let mut data: HashMap<i32, LiquidityAtTick> = HashMap::new();
        let mut liq = U128::ZERO;
        let steps: [(I256, u64); 3] = [
            (I256::try_from(1_000_000u64).unwrap(), 401),
            (I256::try_from(500_000u64).unwrap(), 402),
            (I256::try_from(-200_000i64).unwrap(), 403),
        ];
        for (delta, update_block) in steps {
            let r = apply_liquidity_mapping_update(
                std::mem::take(&mut data),
                10,
                0,
                liq,
                400,
                update_block,
                -100,
                100,
                delta,
            );
            data = r.tick_data;
            liq = r.liquidity;
        }
        assert_eq!(
            liq,
            u128_from_json(&expected["liquidity"]),
            "chain E active liquidity (in-range residual)"
        );
        assert_tick_data(&data, &expected["tick_data"]);
    }

    #[test]
    fn apply_does_not_mutate_inputs() {
        // The fn consumes (moves) its input; passing a clone leaves the original intact.
        let data: HashMap<i32, LiquidityAtTick> = HashMap::new();
        let r = apply_liquidity_mapping_update(
            data.clone(),
            10,
            0,
            U128::ZERO,
            10,
            11,
            -100,
            100,
            I256::try_from(500u64).unwrap(),
        );
        assert!(data.is_empty());
        // the result mutated its own copy
        assert!(!r.tick_data.is_empty());
    }
}

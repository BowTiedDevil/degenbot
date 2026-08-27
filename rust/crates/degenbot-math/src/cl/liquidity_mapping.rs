//! Liquidity-mapping + tick-bitmap mutation math.
//!
//! Pure-Rust port of the Python `apply_liquidity_mapping_update`
//! (`src/degenbot/calculations/concentrated_liquidity.py`) and `flip_tick` /
//! `get_tick_word_and_bit_position` (`src/degenbot/uniswap/v3_libraries/tick_bitmap.py`
//! + `src/degenbot/uniswap/v3_functions.py`).
//!
//! These are the pure computation core of the DB-aware pool updater: given the
//! current tick bitmap / tick-data maps and a single liquidity event (mint or
//! burn with `tick_lower` / `tick_upper`), they produce the next bitmap / data
//! state and the updated active `liquidity`. No I/O, no allocation beyond the
//! map copies — the caller owns the dicts.
//!
//! The typed structs [`BitmapAtWord`], [`LiquidityAtTick`] mirror the Python
//! `concentrated/types.py` models and `degenbot-bot`'s `TickInfo`:
//! `liquidity_gross: U128`, `liquidity_net: I256`, `block: u64`. `liquidity_net`
//! uses `I256` (not `I128`) so the intermediate `current_net + delta` never
//! overflows for valid inputs — matching the Python oracle's arbitrary-precision
//! arithmetic; the Solidity `int128` bound is the caller's concern.
//!
//! # Parity
//!
//! `#[cfg(test)]` parity fixtures (see `tests/liquidity_mapping_fixtures.json`
//! and the `apply_liquidity_mapping_update` step-chains) are generated directly
//! from the Python oracle and assert byte-identical `tick_bitmap` / `tick_data`
//! / `liquidity` output across in-range adjustments, out-of-range events,
//! tick initialization (flip on), gross-to-zero deletion (flip off), net-sign
//! handling at the upper tick, large deltas, and the
//! `update_block == initial_state_block` no-op path.

use hashbrown::hash_map::Entry;
use hashbrown::HashMap;

use alloy::primitives::{I256, U128, U256};

use crate::cl::functions::tick_position;

/// Bitmap value at a tick-bitmap word position.
///
/// Mirrors the Python `BitmapAtWord` (`degenbot/uniswap/concentrated/types.py`)
/// and the `bitmap` / `block` pair carried by `degenbot-bot`'s tick state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitmapAtWord {
    /// The 256-bit bitmap word. Bit `bit_pos` is set when the tick at
    /// `word_pos * 256 + bit_pos` (scaled by spacing) is initialized.
    pub bitmap: U256,
    /// Block at which this word was last mutated.
    pub block: u64,
}

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

/// Result of applying a liquidity-mapping update to the tick bitmap and tick
/// data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiquidityMapUpdateResult {
    /// The updated tick bitmap (word → bitmap word).
    pub tick_bitmap: HashMap<i32, BitmapAtWord>,
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

/// Flip the initialized state for a tick in the bitmap (non-sparse path).
///
/// Mirrors the Python `flip_tick` with `sparse=False`: if the word is absent it
/// is created empty, then the tick's bit is XOR-toggled and the word's `block`
/// is stamped to `update_block`.
///
/// # Panics
///
/// Panics if `tick` is not a multiple of `tick_spacing`, matching the Python
/// `ValueError("Invalid tick or spacing")`.
pub fn flip_tick(
    tick_bitmap: &mut HashMap<i32, BitmapAtWord>,
    tick: i32,
    tick_spacing: i32,
    update_block: u64,
) {
    // Python's `%` is non-negative for a positive divisor — `rem_euclid` matches.
    assert!(
        tick.rem_euclid(tick_spacing) == 0,
        "Invalid tick or spacing"
    );
    let (word_pos, bit_pos) = get_tick_word_and_bit_position(tick, tick_spacing);
    let bit_mask = U256::from(1u64) << bit_pos;
    tick_bitmap
        .entry(word_pos)
        .and_modify(|entry| {
            entry.bitmap ^= bit_mask;
            entry.block = update_block;
        })
        .or_insert(BitmapAtWord {
            bitmap: bit_mask,
            block: update_block,
        });
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

/// Apply a liquidity-mapping update to the tick bitmap and tick data maps.
///
/// This is the pure computation core extracted from
/// `UniswapV3Pool.update_liquidity_map` / `UniswapV4Pool.update_liquidity_map`
/// (see `calculations/concentrated_liquidity.py`). The input maps are consumed
/// (moved) — the caller passes clones if it needs to retain the prior state.
///
/// # Invariants (debug-checked, mirroring the Python `assert`s)
///
/// - `liquidity` starts non-negative (always true for `U128`).
/// - The in-range `liquidity + liquidity_delta` stays non-negative.
/// - `liquidity_gross + liquidity_delta` stays non-negative (and fits `uint128`).
///
/// # Panics
///
/// Panics (debug builds only) if an invariant is violated — matching the
/// Python `assert`s, which are stripped under `python -O`. Release builds do
/// not check.
#[must_use = "the updated state must be used"]
#[expect(clippy::too_many_arguments)]
pub fn apply_liquidity_mapping_update(
    mut tick_bitmap: HashMap<i32, BitmapAtWord>,
    mut tick_data: HashMap<i32, LiquidityAtTick>,
    tick_spacing: i32,
    tick: i32,
    liquidity: U128,
    initial_state_block: u64,
    update_block: u64,
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: I256,
) -> LiquidityMapUpdateResult {
    let state_block = update_block;
    let mut working_liquidity = liquidity;

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
        let tick_word = get_tick_word_and_bit_position(current_tick, tick_spacing).0;

        // Ensure the word exists so flip_tick's non-sparse create-and-toggle works.
        tick_bitmap.entry(tick_word).or_insert(BitmapAtWord {
            bitmap: U256::ZERO,
            block: state_block,
        });

        // If the tick is uninitialized, seed it empty and flip the bitmap bit on.
        if let Entry::Vacant(slot) = tick_data.entry(current_tick) {
            slot.insert(LiquidityAtTick {
                liquidity_net: I256::ZERO,
                liquidity_gross: U128::ZERO,
                block: state_block,
            });
            flip_tick(&mut tick_bitmap, current_tick, tick_spacing, state_block);
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
            // No remaining liquidity references this tick — drop it and flip the
            // bitmap bit back off.
            tick_data.remove(&current_tick);
            flip_tick(&mut tick_bitmap, current_tick, tick_spacing, state_block);
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

    LiquidityMapUpdateResult {
        tick_bitmap,
        tick_data,
        liquidity: working_liquidity,
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation
)]
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

    // ─── flip_tick parity ─────────────────────────────────────────────

    #[test]
    fn flip_tick_parity_vs_python_oracle() {
        let fix = fixtures();
        for case in fix["flip"].as_array().expect("flip array") {
            let ticks: Vec<i32> = case["ticks"]
                .as_array()
                .expect("ticks array")
                .iter()
                .map(|v| v.as_i64().expect("i32 tick") as i32)
                .collect();
            let spacing = case["spacing"].as_i64().expect("spacing") as i32;

            let mut bitmap: HashMap<i32, BitmapAtWord> = HashMap::new();
            for (i, &t) in ticks.iter().enumerate() {
                flip_tick(&mut bitmap, t, spacing, i as u64);
            }

            let expected = case["result"].as_object().expect("result object");
            assert_eq!(
                bitmap.len(),
                expected.len(),
                "word count mismatch for ticks={ticks:?} spacing={spacing}"
            );
            for (word_str, bitmap_val) in expected {
                let word: i32 = word_str.parse().expect("word key");
                let expected_bitmap = u256_from_json(bitmap_val);
                let got = bitmap
                    .get(&word)
                    .unwrap_or_else(|| panic!("missing word {word} for ticks={ticks:?}"));
                assert_eq!(
                    got.bitmap, expected_bitmap,
                    "bitmap mismatch at word {word} for ticks={ticks:?} spacing={spacing}"
                );
            }
        }
    }

    // ─── flip_tick error path ─────────────────────────────────────────

    #[test]
    #[should_panic(expected = "Invalid tick or spacing")]
    fn flip_tick_rejects_non_multiple_tick() {
        let mut bmp = HashMap::new();
        // tick=2 is not a multiple of spacing=3
        flip_tick(&mut bmp, 2, 3, 0);
    }

    // ─── apply_liquidity_mapping_update parity ─────────────────────────

    #[test]
    fn apply_chain_a_in_range_burn_to_zero() {
        // tick=0 active, ticks [-1000, 1000], spacing 10, init_block 10.
        // Steps: +1M, +250K, -750K, -500K → burns both ticks to zero (deleted,
        // flipped off), active liquidity back to 0.
        let fix = fixtures();
        let expected = &fix["apply"]["A"];

        let mut bitmap: HashMap<i32, BitmapAtWord> = HashMap::new();
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
                std::mem::take(&mut bitmap),
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
            bitmap = r.tick_bitmap;
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
        assert_eq!(
            bitmap.len(),
            expected["tick_bitmap"].as_object().unwrap().len(),
            "chain A word count"
        );
        for (word_str, wv) in expected["tick_bitmap"].as_object().unwrap() {
            let word: i32 = word_str.parse().unwrap();
            assert_eq!(bitmap[&word].bitmap, u256_from_json(&wv["bitmap"]));
            assert_eq!(bitmap[&word].block, wv["block"].as_u64().unwrap());
        }
    }

    #[test]
    fn apply_chain_b_large_delta_out_of_range() {
        // tick=5000 active, position ticks [-120,-60] then [300,600], spacing 60.
        // Active tick 5000 is never in range → liquidity stays 5_000_000.
        // Steps: +2^100, -2^100 (zero/delete), +123456789 (re-init), +999 (new ticks, out of range).
        let fix = fixtures();
        let expected = &fix["apply"]["B"];

        let mut bitmap: HashMap<i32, BitmapAtWord> = HashMap::new();
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
                std::mem::take(&mut bitmap),
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
            bitmap = r.tick_bitmap;
            data = r.tick_data;
            liq = r.liquidity;
        }
        assert_eq!(
            liq,
            u128_from_json(&expected["liquidity"]),
            "chain B active liquidity (out-of-range, unchanged)"
        );
        assert_eq!(data.len(), expected["tick_data"].as_object().unwrap().len());
        assert_eq!(
            bitmap.len(),
            expected["tick_bitmap"].as_object().unwrap().len()
        );
        // Verify each tick_data entry
        for (t_str, tv) in expected["tick_data"].as_object().unwrap() {
            let t: i32 = t_str.parse().unwrap();
            assert_eq!(
                data[&t].liquidity_net,
                i256_from_json(&tv["liquidity_net"]),
                "chain B net at {t}"
            );
            assert_eq!(
                data[&t].liquidity_gross,
                u128_from_json(&tv["liquidity_gross"]),
                "chain B gross at {t}"
            );
            assert_eq!(
                data[&t].block,
                tv["block"].as_u64().unwrap(),
                "chain B block at {t}"
            );
        }
        for (w_str, wv) in expected["tick_bitmap"].as_object().unwrap() {
            let w: i32 = w_str.parse().unwrap();
            assert_eq!(
                bitmap[&w].bitmap,
                u256_from_json(&wv["bitmap"]),
                "chain B bitmap at {w}"
            );
            assert_eq!(bitmap[&w].block, wv["block"].as_u64().unwrap());
        }
    }

    #[test]
    fn apply_chain_c_net_signs_and_reinit_delete() {
        // ticks [10,20], tick=0 (out of range: 10<=0 is false) → active stays 0.
        // Steps: +1000, +1000, -1000, +500 (new tick 5), -500 (tick 5 deleted+flipped).
        let fix = fixtures();
        let expected = &fix["apply"]["C"];

        let mut bitmap: HashMap<i32, BitmapAtWord> = HashMap::new();
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
                std::mem::take(&mut bitmap),
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
            bitmap = r.tick_bitmap;
            data = r.tick_data;
            liq = r.liquidity;
        }
        assert_eq!(liq, u128_from_json(&expected["liquidity"]));
        assert_eq!(data.len(), expected["tick_data"].as_object().unwrap().len());
        assert_eq!(
            bitmap.len(),
            expected["tick_bitmap"].as_object().unwrap().len()
        );
        for (t_str, tv) in expected["tick_data"].as_object().unwrap() {
            let t: i32 = t_str.parse().unwrap();
            assert_eq!(data[&t].liquidity_net, i256_from_json(&tv["liquidity_net"]));
            assert_eq!(
                data[&t].liquidity_gross,
                u128_from_json(&tv["liquidity_gross"])
            );
            assert_eq!(data[&t].block, tv["block"].as_u64().unwrap());
        }
        for (w_str, wv) in expected["tick_bitmap"].as_object().unwrap() {
            let w: i32 = w_str.parse().unwrap();
            assert_eq!(bitmap[&w].bitmap, u256_from_json(&wv["bitmap"]));
            assert_eq!(bitmap[&w].block, wv["block"].as_u64().unwrap());
        }
    }

    #[test]
    fn apply_chain_d_block_equals_init_no_inrange_change() {
        // update_block (300) == initial_state_block (300) → in-range branch skipped,
        // active liquidity stays at the initial 0 even though tick=0 is in range.
        let fix = fixtures();
        let expected = &fix["apply"]["D"];

        let bitmap_in: HashMap<i32, BitmapAtWord> = HashMap::new();
        let data_in: HashMap<i32, LiquidityAtTick> = HashMap::new();
        let r = apply_liquidity_mapping_update(
            bitmap_in,
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
        assert_eq!(
            r.tick_data.len(),
            expected["tick_data"].as_object().unwrap().len()
        );
        assert_eq!(
            r.tick_bitmap.len(),
            expected["tick_bitmap"].as_object().unwrap().len()
        );
        for (t_str, tv) in expected["tick_data"].as_object().unwrap() {
            let t: i32 = t_str.parse().unwrap();
            assert_eq!(
                r.tick_data[&t].liquidity_net,
                i256_from_json(&tv["liquidity_net"])
            );
            assert_eq!(
                r.tick_data[&t].liquidity_gross,
                u128_from_json(&tv["liquidity_gross"])
            );
            assert_eq!(r.tick_data[&t].block, tv["block"].as_u64().unwrap());
        }
        for (w_str, wv) in expected["tick_bitmap"].as_object().unwrap() {
            let w: i32 = w_str.parse().unwrap();
            assert_eq!(r.tick_bitmap[&w].bitmap, u256_from_json(&wv["bitmap"]));
            assert_eq!(r.tick_bitmap[&w].block, wv["block"].as_u64().unwrap());
        }
    }

    #[test]
    fn apply_chain_e_in_range_nonzero_residual() {
        // tick=0 active within [-100,100], spacing 10, init_block 400.
        // Steps: +1M (active→1M), +500K (→1.5M), -200K (→1.3M).
        let fix = fixtures();
        let expected = &fix["apply"]["E"];

        let mut bitmap: HashMap<i32, BitmapAtWord> = HashMap::new();
        let mut data: HashMap<i32, LiquidityAtTick> = HashMap::new();
        let mut liq = U128::ZERO;
        let steps: [(I256, u64); 3] = [
            (I256::try_from(1_000_000u64).unwrap(), 401),
            (I256::try_from(500_000u64).unwrap(), 402),
            (I256::try_from(-200_000i64).unwrap(), 403),
        ];
        for (delta, update_block) in steps {
            let r = apply_liquidity_mapping_update(
                std::mem::take(&mut bitmap),
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
            bitmap = r.tick_bitmap;
            data = r.tick_data;
            liq = r.liquidity;
        }
        assert_eq!(
            liq,
            u128_from_json(&expected["liquidity"]),
            "chain E active liquidity (in-range residual)"
        );
        assert_eq!(data.len(), expected["tick_data"].as_object().unwrap().len());
        assert_eq!(
            bitmap.len(),
            expected["tick_bitmap"].as_object().unwrap().len()
        );
        for (t_str, tv) in expected["tick_data"].as_object().unwrap() {
            let t: i32 = t_str.parse().unwrap();
            assert_eq!(data[&t].liquidity_net, i256_from_json(&tv["liquidity_net"]));
            assert_eq!(
                data[&t].liquidity_gross,
                u128_from_json(&tv["liquidity_gross"])
            );
            assert_eq!(data[&t].block, tv["block"].as_u64().unwrap());
        }
        for (w_str, wv) in expected["tick_bitmap"].as_object().unwrap() {
            let w: i32 = w_str.parse().unwrap();
            assert_eq!(bitmap[&w].bitmap, u256_from_json(&wv["bitmap"]));
            assert_eq!(bitmap[&w].block, wv["block"].as_u64().unwrap());
        }
    }

    #[test]
    fn apply_does_not_mutate_inputs() {
        // The fn consumes (moves) its inputs; passing a clone leaves the original intact.
        let mut bitmap: HashMap<i32, BitmapAtWord> = HashMap::new();
        bitmap.insert(
            0,
            BitmapAtWord {
                bitmap: U256::ZERO,
                block: 0,
            },
        );
        let data: HashMap<i32, LiquidityAtTick> = HashMap::new();
        let r = apply_liquidity_mapping_update(
            bitmap.clone(),
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
        // original untouched (still has the seeded word 0)
        assert!(bitmap.contains_key(&0));
        assert_eq!(bitmap[&0].bitmap, U256::ZERO);
        assert!(data.is_empty());
        // the result mutated its own copy
        assert!(!r.tick_bitmap.is_empty());
    }
}

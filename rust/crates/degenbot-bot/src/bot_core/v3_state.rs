//! **Partially relocated.** The lib code (state structs + inherent value
//! methods) now lives in `degenbot_pools::v3_state`; re-exported here at the
//! historical `bot_core::v3_state` path so consumers resolve unchanged. The
//! `#[cfg(test)]` integration-test mod stays here (it exercises the state
//! through the `BotState` registry, which stays in bot). Transient re-export —
//! repointed at `degenbot_pools::v3_state` natively by USPN7M/P2CKRL.

pub use ::degenbot_pools::v3_state::*;

#[cfg(test)]
mod tests {
    #![allow(unused_imports)]
    use super::*;
    use crate::bot_core::state_history::{ReorgJournal, V3BlockDelta};
    use crate::bot_core::tick_bitmap::{compute_tick_ranges, gen_ticks, V3TickRangeForSolver};
    use crate::bot_core::tick_fetch::TickWordFetcher;
    use crate::bot_core::TickInfo;
    use crate::solvers::mobius_v3_int::{IntV3TickRangeHop, IntV3TickRangeSequence};
    use alloy::primitives::{Address, B256, U160};
    use alloy::primitives::{I256, U128, U256};
    use degenbot_cl_math::cl_lib::functions::tick_position;
    use degenbot_cl_math::cl_lib::swap_math::compute_swap_step_v3;
    use degenbot_cl_math::cl_lib::tick_math::{
        get_sqrt_ratio_at_tick_internal, get_tick_at_sqrt_ratio_internal, MAX_SQRT_RATIO,
        MIN_SQRT_RATIO,
    };
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::sync::Arc;

    /// Build a V3 pool at tick 0 (1:1 price, `sqrt_price` = 2^96), liquidity `liq`,
    /// fee 0.3% (3000 pips), `tick_spacing` 60, with a single position spanning
    /// [-60, +60] so the active range bounded by ±60 matches
    /// `make_v3_hop_at_1to1`. The ticks -60 and +60 are initialized with the
    /// position's `liquidity_net` (+L at lower, -L at upper) and matching gross.
    fn pool_1to1_with_position(liq: u128) -> (V3PoolIdentity, V3PoolState) {
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
        (
            V3PoolIdentity {
                address: Address::ZERO,
                token0: Address::ZERO,
                token1: Address::ZERO,
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                deployer: Address::ZERO,
                init_hash: alloy::primitives::B256::ZERO,
            },
            V3PoolState {
                sqrt_price_x96: sp_0,
                liquidity: liq,
                tick: 0,
                update_block: 0,
                tick_data,
                coverage: PoolTickCoverage::Tracked,
                known_bitmap_words: HashSet::new(),
                fetcher: None,
                journal: ReorgJournal::<V3BlockDelta>::new(8),
                cached_tick_ranges: parking_lot::Mutex::new(super::TickRangeCache::default()),
                snapshot_seed: None,
                post_drain_snapshot: None,
            },
        )
    }

    #[test]
    fn zfo_small_swap_matches_single_compute_swap_step() {
        // What: a small zfo exact-input swap on a 1:1 V3 pool with a [-60,+60]
        // position stays inside the range (no tick crossing), so the outcome
        // must equal a single `compute_swap_step_v3` call with the same bounds.
        // Why: pins the V3 simulator's first-step behavior against the already-
        // tested swap-step primitive as the oracle (zero hand-computed math).
        let liq = 10_000_000_000_000u128;
        let (identity, state) = pool_1to1_with_position(liq);
        let amount_in = U256::from(1000u64);

        let outcome = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::try_from(amount_in).unwrap(),
            V3PoolState::default_sqrt_price_limit(true),
        )
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
            U256::from(identity.fee),
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
        let (identity, state) = pool_1to1_with_position(liq);
        let amount_in = U256::from(1000u64);

        let outcome = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            false,
            I256::try_from(amount_in).unwrap(),
            V3PoolState::default_sqrt_price_limit(false),
        )
        .expect("small ofz swap should produce an outcome");

        let sp_upper = U256::from(get_sqrt_ratio_at_tick_internal(60).unwrap());
        let step = compute_swap_step_v3(
            state.sqrt_price_x96,
            sp_upper,
            i128::try_from(liq).unwrap(),
            I256::try_from(amount_in).unwrap(),
            U256::from(identity.fee),
        )
        .unwrap();

        assert_eq!(outcome.amount0, step.amount_out);
        assert_eq!(outcome.amount1, step.amount_in + step.fee_amount);
        assert!(outcome.amount0 < amount_in);
    }

    #[test]
    fn zero_amount_is_not_computable() {
        let (identity, state) = pool_1to1_with_position(1_000_000u128);
        let outcome = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::ZERO,
            V3PoolState::default_sqrt_price_limit(true),
        );
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
        let (identity, state) = pool_1to1_with_position(10_000_000_000_000u128);
        let small = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::try_from(U256::from(100u64)).unwrap(),
            V3PoolState::default_sqrt_price_limit(true),
        )
        .unwrap()
        .amount1;
        let large = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::try_from(U256::from(10_000u64)).unwrap(),
            V3PoolState::default_sqrt_price_limit(true),
        )
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
        let (identity, mut state) = pool_1to1_with_position(liq);
        state.coverage = PoolTickCoverage::Sparse;
        state.known_bitmap_words.clear(); // fully sparse: no regions known

        let res = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::try_from(1_000u64).unwrap(),
            V3PoolState::default_sqrt_price_limit(true),
        );
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
        let (identity, mut state) = pool_1to1_with_position(liq);
        // Tracked + empty known set — must NOT miss.
        state.known_bitmap_words.clear();
        assert_eq!(state.coverage, PoolTickCoverage::Tracked);

        let res = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::try_from(1_000u64).unwrap(),
            V3PoolState::default_sqrt_price_limit(true),
        );
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
        //
        // Uses an ofz (price-rising) swap from tick 0: the position's lower
        // tick −60 lives in word −1 (unknown here), but ofz walks UPWARD into
        // word 0 (known) toward +60, so word −1 is merely proposed, never
        // entered — no miss. (A zfo swap would move the tick into word −1, the
        // endpoint-in-unknown-word case covered by
        // `sparse_endpoint_in_unknown_word_signals_miss`.)
        let liq = 10_000_000_000_000u128;
        let (identity, mut state) = pool_1to1_with_position(liq);
        state.coverage = PoolTickCoverage::Sparse;
        // Word 0 (tick 0, the start) is known; word −1 (containing the tick −60
        // boundary of the position) is NOT known.
        state.known_bitmap_words.clear();
        state.known_bitmap_words.insert(0);

        let res = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            false,
            I256::try_from(100u64).unwrap(),
            V3PoolState::default_sqrt_price_limit(false),
        );
        assert!(
            res.is_ok(),
            "ofz swap staying in the known word 0 should compute, got {res:?}"
        );
    }

    #[test]
    fn sparse_endpoint_in_unknown_word_signals_miss() {
        // Slice-4 fix (V3 mirror of the V4 ELSE-branch miss check). A swap
        // whose price endpoint lands in an UNFETCHED word must signal
        // `MissingTickWord` — the word may contain initialized ticks the walk
        // crossed but `gen_ticks` never proposed (they are absent from
        // `tick_data`). This is the divergence that made V4 `test_cached_
        // calculations` undercount on multi-word swaps: without this check the
        // walk committed a result computed with stale liquidity, having skipped
        // the unknown word's liquidity-nets. Mirrors Python's
        // `next_initialized_tick_within_one_word`, which raises for the current
        // tick's word at every step — so Python fetches the endpoint word; Rust
        // must too.
        let liq = 10_000_000_000_000u128;
        let (identity, mut state) = pool_1to1_with_position(liq);
        state.coverage = PoolTickCoverage::Sparse;
        state.known_bitmap_words.clear();
        state.known_bitmap_words.insert(0); // word 0 known; word −1 unknown

        // zfo drops the price below tick 0 into word −1 (unknown).
        let res = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::try_from(100u64).unwrap(),
            V3PoolState::default_sqrt_price_limit(true),
        );
        assert_eq!(
            res,
            Err(SimulateSwapError::MissingTickWord(-1)),
            "zfo swap whose endpoint enters the unknown word −1 must signal a \
             fetchable miss, got {res:?}"
        );
    }

    #[test]
    fn v3_simulate_swap_outcome_caries_final_state() {
        // ADR-005 slice 3b: the companion's simulate_exact_input_swap builds
        // final_state from the outcome, so v3_simulate_swap must return the
        // post-walk sqrt_price_x96 / liquidity / tick (not just the amounts).
        let (identity, state) = pool_1to1_with_position(10_000_000_000_000u128);
        let amount_in = I256::try_from(1_000u128).unwrap();
        let outcome = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            amount_in,
            V3PoolState::default_sqrt_price_limit(true),
        )
        .expect("computes");
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
        let crossed = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            big,
            V3PoolState::default_sqrt_price_limit(true),
        )
        .expect("computes");
        assert_ne!(
            crossed.sqrt_price_x96, state.sqrt_price_x96,
            "a crossing swap must move the price off the start value"
        );
    }

    /// Word-of parity: the Rust sparse miss-detection model (`word_of`, used by
    /// both V3 + V4 `vX_simulate_swap` to decide whether the current tick's
    /// bitmap word is "known") must match the Python companion's bitmap word
    /// computation (`position(tick // tick_spacing)[0]` in
    /// `v3_libraries/tick_bitmap.py`). Both use floored division (`div_euclid`
    /// == Python `//`) + arithmetic right-shift (`>> 8`), so they agree for
    /// negative non-multiple current ticks — the regime a crossing swap's
    /// post-step price lives in. This test locks that equivalence so the V4
    /// crossing-swap divergence under the fetch seam (slice 4) is NOT
    /// mis-attributed to the miss-detection model. See the slice-3 diagnosis
    /// recorded on ergo task `2ZG6XO`: the models match, so V4 routing's fork
    /// divergence lives elsewhere (fee accounting / boundary-tick walk / fetch
    /// merge semantics) and must be fork-validated.
    #[test]
    fn word_of_matches_python_bitmap_word_position_for_edge_ticks() {
        // (tick, tick_spacing) covering: positive + negative, multiples +
        // non-multiples of spacing, and cross-word boundaries.
        let cases: &[(i32, i32)] = &[
            (0, 60),
            (60, 60),
            (-60, 60),
            (-10, 60), // negative non-multiple: div_euclid floors to -1
            (-1, 60),
            (59, 60),
            (-61, 60),
            (-255, 1),
            (-256, 1), // word boundary (-1 → -2)
            (-257, 1),
            (255, 1),
            (256, 1), // word boundary (0 → 1)
            (887_272, 60),
            (-887_272, 60),
            (887_270, 60), // negative mirror of a non-multiple
            (-887_270, 60),
        ];
        for &(tick, spacing) in cases {
            let rust_word = V3PoolState::word_of(tick, spacing);
            // Python: compressed = tick // spacing (floored); word = compressed >> 8.
            let compressed = tick.div_euclid(spacing);
            let py_word = compressed >> 8;
            assert_eq!(
                rust_word, py_word,
                "word_of({tick}, {spacing}) = {rust_word} != python {py_word}"
            );
        }
    }
}

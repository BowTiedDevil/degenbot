//! WalkMemo behavioral tests: the three-state probe, negative/hit replay,
//! census-scoped eviction across epochs, the monotone-epoch reorg-rewind
//! contract, and the negative-entries gauge - plus the mixed V2+concentrated liquidity memo
//! twins and the `walk_concentrated_liquidity_path_fingerprint` separation tests that pin the
//! memo key's content stability.

//! Split out of `concentrated_liquidity::tests` (the `refinements` precedent); shared helpers
//! (`make_v3_hop_at_1to1`, `multi_range_sequence`, the mixed memo fixtures)
//! stay in `super`.

use crate::int_v3_hop::IntV3TickRangeSequence;
use alloy::primitives::U256;
use degenbot_math::v2::IntHopState;

use crate::solve::concentrated_liquidity::memo::{
    walk_concentrated_liquidity_path_fingerprint, walk_mixed_path_fingerprint,
    walk_views_fingerprint, MemoProbe, WalkMemo,
};
use crate::solve::concentrated_liquidity::{
    solve_concentrated_liquidity_piecewise, solve_piecewise, MixedHopView,
    PreparedConcentratedLiquidityHop, WalkRefusal,
};
use crate::solve::runtime::SolveRuntimeConfig;

use super::{
    make_v3_hop_at_1to1, mixed_memo_fixture, mixed_memo_fixture_with_late_liq, multi_range_sequence,
};

#[test]
fn walk_fingerprint_is_content_stable_and_separates_compositions() {
    let hop1 = make_v3_hop_at_1to1(10_000_000_000_000u128, true);
    let hop2 = make_v3_hop_at_1to1(8_000_000_000_000u128, false);
    let seq1 = IntV3TickRangeSequence::new(vec![hop1.clone()]).unwrap();
    let seq2 = IntV3TickRangeSequence::new(vec![hop2]).unwrap();
    let sv = [&seq1, &seq2];

    let fp_a = walk_concentrated_liquidity_path_fingerprint(&sv);
    let fp_b = walk_concentrated_liquidity_path_fingerprint(&sv);
    assert_eq!(fp_a, fp_b, "fingerprint must be a pure function of content");

    // Reversed hop order is a different composition.
    let fp_rev = walk_concentrated_liquidity_path_fingerprint(&[&seq2, &seq1]);
    assert_ne!(fp_a, fp_rev, "hop order must separate compositions");

    // A single-field state change must change the key.
    let mut h1 = hop1.clone();
    h1.liquidity += 1;
    let seq1b = IntV3TickRangeSequence::new(vec![h1]).unwrap();
    let fp_changed = walk_concentrated_liquidity_path_fingerprint(&[&seq1b, &seq2]);
    assert_ne!(fp_a, fp_changed, "a liquidity delta must change the key");
}

#[test]
fn walk_fingerprint_separates_swap_direction() {
    let hop = make_v3_hop_at_1to1(10_000_000_000_000u128, true);
    let mut flipped = hop.clone();
    flipped.zero_for_one = !hop.zero_for_one;
    let seq = IntV3TickRangeSequence::new(vec![hop]).unwrap();
    let seq_flipped = IntV3TickRangeSequence::new(vec![flipped]).unwrap();
    assert_ne!(
        walk_concentrated_liquidity_path_fingerprint(&[&seq]),
        walk_concentrated_liquidity_path_fingerprint(&[&seq_flipped]),
        "a direction flip must change the key"
    );
}

#[test]
fn walk_fingerprint_separates_word_boundary_lists() {
    let hop = make_v3_hop_at_1to1(10_000_000_000_000u128, true);
    let mut tail_a = hop.clone();
    let mut tail_b = hop.clone();
    // Same length, same first boundary — only a later boundary differs.
    tail_a.word_boundary_prices = vec![U256::from(123u64), U256::from(456u64)];
    tail_b.word_boundary_prices = vec![U256::from(123u64), U256::from(789u64)];
    let seq_a = IntV3TickRangeSequence::new(vec![tail_a]).unwrap();
    let seq_b = IntV3TickRangeSequence::new(vec![tail_b]).unwrap();
    assert_ne!(
        walk_concentrated_liquidity_path_fingerprint(&[&seq_a]),
        walk_concentrated_liquidity_path_fingerprint(&[&seq_b]),
        "a later word boundary must change the key"
    );
}

// ── Cross-block composition memo (WalkMemo) ───────────────────

/// The probe's three states: a stored `None` must come back as `Negative`
/// (no longer indistinguishable from a `Miss`), a stored solution as `Hit`,
/// an absent key as `Miss`. Covers both the stats-on and stats-off arms.
#[test]
fn memo_probe_distinguishes_hit_negative_miss() {
    let sol = (
        U256::from(11u64),
        U256::from(22u64),
        vec![U256::from(33u64)],
    );

    // Stats-on arm.
    let memo = WalkMemo::new(true, true);
    memo.store(0xA11CEu128, None);
    memo.store(0xB0Bu128, Some(&sol));
    assert_eq!(memo.probe(0xA11CEu128), MemoProbe::Negative);
    assert_eq!(memo.probe(0xB0Bu128), MemoProbe::Hit(sol.clone()));
    assert_eq!(memo.probe(0xC1C1u128), MemoProbe::Miss);

    // Stats-off arm: same cache semantics; the census is still maintained
    // (it runs whenever the memo is active, not just under stats), only
    // the heavier counters stay dark.
    let memo_off_stats = WalkMemo::new(true, false);
    memo_off_stats.store(0xA11CEu128, None);
    memo_off_stats.store(0xB0Bu128, Some(&sol));
    assert_eq!(memo_off_stats.probe(0xA11CEu128), MemoProbe::Negative);
    assert_eq!(memo_off_stats.probe(0xB0Bu128), MemoProbe::Hit(sol.clone()));
    assert_eq!(memo_off_stats.probe(0xC1C1u128), MemoProbe::Miss);

    // Memo disabled: store is a no-op and every probe is a Miss.
    let memo_off = WalkMemo::new(false, true);
    memo_off.store(0xA11CEu128, None);
    assert_eq!(memo_off.probe(0xA11CEu128), MemoProbe::Miss);
}

/// The point of the three-state probe: an unprofitable composition is
/// walked ONCE (first solve = Miss, the walk runs and stores the negative);
/// the replay is answered from the cached `Negative` without re-entering
/// the inner walk, while the census still records the fingerprint.
///
/// `stats.sims` is the direct walk-ran proof but flushes to zero without
/// the crate's default-off `telemetry` feature — every `WalkStats` field is
/// feature-gated (counter policy documented at `WalkStats` in
/// `concentrated_liquidity/active_set.rs`; the bounded-counts test enforces the zero flush);
/// `negatives_played` is the feature-independent instrument.
#[test]
fn memo_cached_negative_skips_the_walk_on_replay() {
    // Two same-price 1:1 pools (zfo + ofz): fees dominate, always None.
    let hop1 = make_v3_hop_at_1to1(10_000_000_000_000u128, true);
    let hop2 = make_v3_hop_at_1to1(10_000_000_000_000u128, false);
    let seq1 = IntV3TickRangeSequence::new(vec![hop1]).unwrap();
    let seq2 = IntV3TickRangeSequence::new(vec![hop2]).unwrap();
    let prepared = [
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(seq1.clone())),
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(seq2.clone())),
    ];
    let cfg = SolveRuntimeConfig::default();
    let memo = WalkMemo::new(true, true);

    // First solve: Miss → the walk runs and stores the negative.
    let first = solve_concentrated_liquidity_piecewise(&prepared, Some(&memo), None, &cfg, None);
    assert!(first.result.is_none(), "fixture must be unprofitable");
    #[cfg(feature = "telemetry")]
    assert!(first.stats.sims > 0, "first solve must run the walk");
    let st1 = memo.take_stats();
    assert_eq!(st1.cache_plays, 1, "first solve consulted the cache");
    assert_eq!(
        st1.negatives_played, 0,
        "first solve was a Miss, not a play"
    );
    assert_eq!(
        st1.negative_entries, 1,
        "the unprofitable composition is cached"
    );

    // Epoch swap so the replay's census insert is observable on its own.
    memo.begin_block(2);

    // Second solve: the cached negative answers without the walk.
    let second = solve_concentrated_liquidity_piecewise(&prepared, Some(&memo), None, &cfg, None);
    assert!(second.result.is_none());
    assert_eq!(
        second.stats.sims, 0,
        "the walk must not run on a negative play"
    );
    let st2 = memo.take_stats();
    assert_eq!(st2.cache_plays, 1);
    assert_eq!(st2.negatives_played, 1, "the negative must be played");
    assert_eq!(st2.negative_entries, 1, "no second store");
    assert_eq!(st2.distinct, 1, "the negative play still lands fp in curr");
    assert_eq!(
        st2.hits, 1,
        "the composition recurred from the previous epoch"
    );
}

/// A cached profitable solution replays byte-identical through the entry,
/// and the replay does not re-run the walk.
#[test]
fn memo_cached_hit_replays_the_solution_without_the_walk() {
    // The late-liquidity fixture the corner tests pin as profitable.
    let seq1 = multi_range_sequence(750, 1300, true, &[1_000_000_000_000_000]);
    let mut liquidities = vec![1_000_000_000u128; 10];
    liquidities.push(10_000_000_000_000u128);
    liquidities.push(1_000_000_000u128);
    let seq2 = multi_range_sequence(0, 60, false, &liquidities);
    let prepared = [
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(seq1.clone())),
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(seq2.clone())),
    ];
    let cfg = SolveRuntimeConfig::default();
    let memo = WalkMemo::new(true, true);

    let first = solve_concentrated_liquidity_piecewise(&prepared, Some(&memo), None, &cfg, None);
    let first_result = first.result.expect("fixture must be profitable");
    let st1 = memo.take_stats();
    assert_eq!(st1.negatives_played, 0);
    assert_eq!(
        st1.negative_entries, 0,
        "profitable compositions cache Some"
    );

    let second = solve_concentrated_liquidity_piecewise(&prepared, Some(&memo), None, &cfg, None);
    assert_eq!(
        second.result,
        Some(first_result),
        "cached hit replays exactly"
    );
    assert_eq!(
        second.stats.sims, 0,
        "the walk must not run on a hit replay"
    );
    let st2 = memo.take_stats();
    assert_eq!(st2.cache_plays, 1);
    assert_eq!(st2.negatives_played, 0, "a hit is not a negative play");
}

/// The 4096 wholesale clear is gone: a burst of new distinct compositions
/// in one epoch must not wipe previously cached entries. The cache is
/// bounded by the census working set, so every fingerprint probed in the
/// epoch survives the epoch swap (driven through probe + store, the real
/// flow, so each entry joins the census).
#[test]
fn memo_cache_survives_a_burst_beyond_the_old_4096_cap() {
    let memo = WalkMemo::new(true, true);

    // One epoch, 5000 distinct compositions: probe (Miss) then store the
    // negative — exactly what a solve does on a fresh composition.
    let first_fp = 1u128;
    for i in 0u128..5000 {
        let fp = first_fp + i;
        assert_eq!(memo.probe(fp), MemoProbe::Miss);
        memo.store(fp, None);
    }

    // Old behavior: the 4097th store cleared the cache, so the earliest
    // fingerprint would be a Miss after the epoch swap. Census-scoped
    // eviction keeps every probed fingerprint instead.
    memo.begin_block(1);
    assert_eq!(
        memo.probe(first_fp),
        MemoProbe::Negative,
        "the earliest composition must survive; no mid-epoch clear"
    );
    let late_fp = first_fp + 4999;
    assert_eq!(memo.probe(late_fp), MemoProbe::Negative);

    let st = memo.take_stats();
    assert_eq!(
        st.negative_entries, 5000,
        "all 5000 cached negatives are live; the wholesale clear is gone"
    );
}

/// Stale eviction: an entry survives one epoch advance (it is still in the
/// previous epoch's census) but is dropped once it goes unprobed across a
/// full epoch — the census is the reuse window, and the negative gauge
/// reflects the survivors, not the evicted.
#[test]
fn memo_stale_unprobed_entry_is_evicted_by_the_census() {
    let memo = WalkMemo::new(true, true);

    // Epoch 1: A is probed + stored negative (the real flow).
    memo.begin_block(1);
    assert_eq!(memo.probe(0xAAAAu128), MemoProbe::Miss);
    memo.store(0xAAAAu128, None);
    assert_eq!(memo.take_stats().negative_entries, 1);

    // Epoch 2: A is never probed. It survives this advance (still in the
    // previous epoch's census) while B joins the census.
    memo.begin_block(2);
    assert_eq!(memo.probe(0xBBBBu128), MemoProbe::Miss);
    memo.store(0xBBBBu128, None);
    assert_eq!(
        memo.take_stats().negative_entries,
        2,
        "A survived the first advance"
    );

    // Epoch 3: A is two epochs stale — not in curr or prev — so the census
    // retain drops it; B survives (probed last epoch).
    memo.begin_block(3);
    assert_eq!(
        memo.probe(0xAAAAu128),
        MemoProbe::Miss,
        "stale A was evicted"
    );
    assert_eq!(
        memo.probe(0xBBBBu128),
        MemoProbe::Negative,
        "probed B survived"
    );
    let st = memo.take_stats();
    assert_eq!(
        st.negative_entries, 1,
        "the gauge reflects survivors only, not evicted entries"
    );
}

/// The monotone-epoch contract, exercised by the reorg scenario it guards:
/// the block pump rewinds to a LOWER block number after a reorg, so
/// `begin_block(4)` arrives after `begin_block(5)`. The regression must
/// warn-and-hold (the same early-return shape as the equal-epoch path),
/// NOT swap the census backwards: the higher epoch's state — epoch counter,
/// census, costs, cache — survives untouched, so a probe of the epoch-5
/// fingerprint still answers from the live cache. A backwards swap would
/// evict the fingerprint (after the swap it sits in neither `prev` nor
/// `curr`), downgrade the probe to a `Miss`, and drop the negative gauge —
/// the memo would serve entries stamped from a future epoch. No panic on
/// the regression path.
#[test]
fn memo_reorg_rewind_holds_the_higher_epoch_state() {
    let fp = 0x4E1Du128;
    let memo = WalkMemo::new(true, true);

    // Epoch 1: probe + store a negative — the real solve flow.
    memo.begin_block(1);
    assert_eq!(memo.probe(fp), MemoProbe::Miss);
    memo.store(fp, None);
    assert_eq!(memo.take_stats().negative_entries, 1);

    // Advance forward to epoch 5: the fingerprint joins the previous
    // epoch's census and survives the retain — it is epoch-5 live state.
    memo.begin_block(5);

    // REGRESSION: reorg rewind to a lower block number.
    memo.begin_block(4);

    let st = memo.take_stats();
    assert_eq!(
        st.epoch, 5,
        "the epoch must stay at the higher value; no backwards swap"
    );
    assert_eq!(
        st.negative_entries, 1,
        "the cached negative survived; a backwards swap would have evicted it"
    );

    // The memo still serves the epoch-5 state: the probe answers from the
    // live cache (not a swapped-backwards census) and counts as a
    // previous-epoch census recurrence.
    assert_eq!(
        memo.probe(fp),
        MemoProbe::Negative,
        "the epoch-5 cached entry must still answer after the rewind"
    );
    let st = memo.take_stats();
    assert_eq!(st.epoch, 5, "still epoch 5 after the probe");
    assert_eq!(
        st.hits, 1,
        "fp is still a previous-epoch census member at epoch 5"
    );
    assert_eq!(
        st.negatives_played, 1,
        "answered from the cached negative, not a Miss"
    );
}

/// Census without stats: a memo_on/stats_off engine maintains the census
/// (the probe insert runs whenever the memo is active), so cross-epoch
/// retention and `distinct` work with the counters dark.
#[test]
fn memo_retains_across_epochs_without_stats() {
    let memo = WalkMemo::new(true, false);

    assert_eq!(memo.probe(0x00C0_FFEE_u128), MemoProbe::Miss);
    memo.store(0x00C0_FFEE_u128, None);

    memo.begin_block(1);
    assert_eq!(
        memo.probe(0x00C0_FFEE_u128),
        MemoProbe::Negative,
        "retention works without stats_on: the census was maintained"
    );

    let st = memo.take_stats();
    assert_eq!(
        st.distinct, 1,
        "distinct is maintained in memo-on/stats-off runs too"
    );
    assert_eq!(st.negative_entries, 1);
    assert_eq!(st.probes, 0, "heavy counters stay gated on stats_on");
    assert_eq!(st.hits, 0, "heavy counters stay gated on stats_on");
}

/// The negative gauge tracks every cache transition exactly: fresh None +1,
/// Some→None +1, None→Some −1, and census eviction −1. `take_stats` reports
/// the gauge without resetting it (it mirrors live cache state).
#[test]
fn memo_negative_entries_gauge_mirrors_the_cache() {
    let sol = (
        U256::from(44u64),
        U256::from(55u64),
        vec![U256::from(66u64)],
    );
    let memo = WalkMemo::new(true, true);

    memo.probe(0xD1CEu128);
    memo.store(0xD1CEu128, None);
    assert_eq!(memo.take_stats().negative_entries, 1);

    memo.store(0xD1CEu128, Some(&sol));
    assert_eq!(
        memo.take_stats().negative_entries,
        0,
        "None flipped to Some"
    );

    memo.store(0xD1CEu128, None);
    assert_eq!(memo.take_stats().negative_entries, 1);
    assert_eq!(
        memo.take_stats().negative_entries,
        1,
        "take_stats must not reset the gauge"
    );

    // Two epoch advances with no probes: the entry survives the first
    // (still in the previous epoch's census) and is evicted at the second.
    memo.begin_block(2);
    memo.begin_block(3);
    assert_eq!(
        memo.take_stats().negative_entries,
        0,
        "the evicted negative left the gauge"
    );
}

/// The mixed twin of `memo_cached_hit_replays_the_solution_without_the_walk`:
/// a recurring mixed composition (one V2 hop + one concentrated liquidity hop, pools unchanged)
/// walks once and replays the identical result from the memo without the
/// walk. Pure-V2 compositions route through the same entry, so the probe
/// path here is theirs too.
#[test]
fn memo_mixed_cached_hit_replays_the_solution_without_the_walk() {
    let (v2, concentrated_liquidity_seq) = mixed_memo_fixture();
    let concentrated_liquidity_prepared =
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(concentrated_liquidity_seq));
    let hops = [
        MixedHopView::V2(&v2),
        MixedHopView::ConcentratedLiquidity(&concentrated_liquidity_prepared),
    ];
    let cfg = SolveRuntimeConfig::default();
    let memo = WalkMemo::new(true, true);

    // First solve: Miss → the walk runs and stores the solution.
    let first = solve_piecewise(&hops, Some(&memo), None, &cfg, None);
    let first_result = first.result.expect("mixed fixture must be profitable");
    #[cfg(feature = "telemetry")]
    assert!(first.stats.sims > 0, "first solve must run the walk");
    let st1 = memo.take_stats();
    assert_eq!(st1.cache_plays, 1, "first solve consulted the cache");
    assert_eq!(st1.negatives_played, 0);
    assert_eq!(
        st1.negative_entries, 0,
        "profitable compositions cache Some"
    );

    // Epoch swap so the replay's census hit is observable on its own.
    memo.begin_block(2);

    // Second solve: the cached hit replays byte-identically, no walk.
    let second = solve_piecewise(&hops, Some(&memo), None, &cfg, None);
    assert_eq!(
        second.result,
        Some(first_result),
        "cached mixed hit replays exactly"
    );
    assert_eq!(
        second.stats.sims, 0,
        "the walk must not run on a hit replay"
    );
    let st2 = memo.take_stats();
    assert_eq!(st2.cache_plays, 1);
    assert_eq!(st2.negatives_played, 0, "a hit is not a negative play");
    assert_eq!(
        st2.hits, 1,
        "the composition recurred from the previous epoch"
    );
}

/// The mixed twin of `memo_cached_negative_skips_the_walk_on_replay`: an
/// unprofitable mixed composition walks once; the replay is answered from
/// the cached `Negative` with the walk skipped.
#[test]
fn memo_mixed_cached_negative_skips_the_walk_on_replay() {
    // Two same-price 1:1 pools (V2 then concentrated liquidity): fees dominate, always None.
    let v2 = IntHopState::new(
        U256::from(1_000_000_000_000_000u128),
        U256::from(1_000_000_000_000_000u128),
        997,
        1000,
    );
    let concentrated_liquidity_hop = make_v3_hop_at_1to1(10_000_000_000_000u128, true);
    let concentrated_liquidity_seq =
        IntV3TickRangeSequence::new(vec![concentrated_liquidity_hop]).unwrap();
    let concentrated_liquidity_prepared =
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(concentrated_liquidity_seq));
    let hops = [
        MixedHopView::V2(&v2),
        MixedHopView::ConcentratedLiquidity(&concentrated_liquidity_prepared),
    ];
    let cfg = SolveRuntimeConfig::default();
    let memo = WalkMemo::new(true, true);

    let first = solve_piecewise(&hops, Some(&memo), None, &cfg, None);
    assert!(first.result.is_none(), "fixture must be unprofitable");
    #[cfg(feature = "telemetry")]
    assert!(first.stats.sims > 0, "first solve must run the walk");
    let st1 = memo.take_stats();
    assert_eq!(st1.cache_plays, 1);
    assert_eq!(
        st1.negatives_played, 0,
        "first solve was a Miss, not a play"
    );
    assert_eq!(
        st1.negative_entries, 1,
        "the unprofitable composition is cached"
    );

    memo.begin_block(2);

    let second = solve_piecewise(&hops, Some(&memo), None, &cfg, None);
    assert!(second.result.is_none());
    assert_eq!(
        second.stats.sims, 0,
        "the walk must not run on a negative play"
    );
    let st2 = memo.take_stats();
    assert_eq!(st2.cache_plays, 1);
    assert_eq!(st2.negatives_played, 1, "the negative must be played");
    assert_eq!(st2.negative_entries, 1, "no second store");
    assert_eq!(st2.distinct, 1, "the negative play still lands fp in curr");
    assert_eq!(
        st2.hits, 1,
        "the composition recurred from the previous epoch"
    );
}

/// Key completeness, V2 side: changing ONLY the V2 reserve of a cached
/// mixed composition must Miss (no stale replay). The stale-replay proof is
/// feature-independent — under the SAME memo, the changed composition must
/// solve to the memo-less result of the changed composition; a key
/// collision would replay the cached base result instead.
#[test]
fn memo_mixed_changed_v2_reserve_misses_instead_of_replaying() {
    let cfg = SolveRuntimeConfig::default();
    let (v2, concentrated_liquidity_seq) = mixed_memo_fixture();

    // Only reserve_in changes (doubled); reserve_out, the fee params, and
    // the concentrated liquidity sequence stay identical.
    let v2_changed = IntHopState::new(v2.reserve_in * U256::from(2u64), v2.reserve_out, 997, 1000);
    assert_ne!(
        walk_mixed_path_fingerprint(
            &[Some(v2.clone()), None],
            &[None, Some(&concentrated_liquidity_seq)],
            &[true, false]
        ),
        walk_mixed_path_fingerprint(
            &[Some(v2_changed.clone()), None],
            &[None, Some(&concentrated_liquidity_seq)],
            &[true, false]
        ),
        "a V2 reserve change must change the mixed key"
    );

    let concentrated_liquidity_prepared =
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(concentrated_liquidity_seq));
    let memo = WalkMemo::new(true, true);
    let base = solve_piecewise(
        &[
            MixedHopView::V2(&v2),
            MixedHopView::ConcentratedLiquidity(&concentrated_liquidity_prepared),
        ],
        Some(&memo),
        None,
        &cfg,
        None,
    )
    .result
    .expect("mixed fixture must be profitable");
    memo.begin_block(2);

    let replayed = solve_piecewise(
        &[
            MixedHopView::V2(&v2_changed),
            MixedHopView::ConcentratedLiquidity(&concentrated_liquidity_prepared),
        ],
        Some(&memo),
        None,
        &cfg,
        None,
    );
    let fresh = solve_piecewise(
        &[
            MixedHopView::V2(&v2_changed),
            MixedHopView::ConcentratedLiquidity(&concentrated_liquidity_prepared),
        ],
        None,
        None,
        &cfg,
        None,
    )
    .result;
    assert_ne!(
        replayed.result,
        Some(base),
        "the changed composition must not replay the cached base result"
    );
    assert_eq!(
        replayed.result, fresh,
        "the Miss re-solved the changed composition exactly"
    );
    #[cfg(feature = "telemetry")]
    assert!(
        replayed.stats.sims > 0,
        "the changed composition must re-run the walk"
    );
}

/// Key completeness, concentrated liquidity side: changing ONLY the concentrated liquidity sequence of a cached
/// mixed composition must Miss too — mirrored argument to the V2 side.
#[test]
fn memo_mixed_changed_concentrated_liquidity_sequence_misses_instead_of_replaying() {
    let cfg = SolveRuntimeConfig::default();
    let (v2, concentrated_liquidity_seq) = mixed_memo_fixture();
    let (_, concentrated_liquidity_seq_changed) =
        mixed_memo_fixture_with_late_liq(20_000_000_000_000u128);

    assert_ne!(
        walk_mixed_path_fingerprint(
            &[Some(v2.clone()), None],
            &[None, Some(&concentrated_liquidity_seq)],
            &[true, false]
        ),
        walk_mixed_path_fingerprint(
            &[Some(v2.clone()), None],
            &[None, Some(&concentrated_liquidity_seq_changed)],
            &[true, false]
        ),
        "a concentrated liquidity sequence change must change the mixed key"
    );

    let concentrated_liquidity_prepared =
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(concentrated_liquidity_seq));
    let concentrated_liquidity_prepared_changed = PreparedConcentratedLiquidityHop::derive(
        std::sync::Arc::new(concentrated_liquidity_seq_changed),
    );
    let memo = WalkMemo::new(true, true);
    let base = solve_piecewise(
        &[
            MixedHopView::V2(&v2),
            MixedHopView::ConcentratedLiquidity(&concentrated_liquidity_prepared),
        ],
        Some(&memo),
        None,
        &cfg,
        None,
    )
    .result
    .expect("mixed fixture must be profitable");
    memo.begin_block(2);

    let replayed = solve_piecewise(
        &[
            MixedHopView::V2(&v2),
            MixedHopView::ConcentratedLiquidity(&concentrated_liquidity_prepared_changed),
        ],
        Some(&memo),
        None,
        &cfg,
        None,
    );
    let fresh = solve_piecewise(
        &[
            MixedHopView::V2(&v2),
            MixedHopView::ConcentratedLiquidity(&concentrated_liquidity_prepared_changed),
        ],
        None,
        None,
        &cfg,
        None,
    )
    .result;
    assert_ne!(
        replayed.result,
        Some(base),
        "the changed composition must not replay the cached base result"
    );
    assert_eq!(
        replayed.result, fresh,
        "the Miss re-solved the changed composition exactly"
    );
    #[cfg(feature = "telemetry")]
    assert!(
        replayed.stats.sims > 0,
        "the changed composition must re-run the walk"
    );
}

/// Unification pin: the all-concentrated-liquidity intake (`solve_concentrated_liquidity_piecewise`) is a forwarding
/// convenience over the ONE solve body — its outcomes must be byte-equal to
/// the unified entry's on the same prepared hops, in every memo stance.
#[test]
fn all_concentrated_liquidity_intake_forwards_to_the_unified_entry_byte_exact() {
    // The profitable late-liquidity fixture.
    let seq1 = multi_range_sequence(750, 1300, true, &[1_000_000_000_000_000]);
    let mut liquidities = vec![1_000_000_000u128; 10];
    liquidities.push(10_000_000_000_000u128);
    liquidities.push(1_000_000_000u128);
    let seq2 = multi_range_sequence(0, 60, false, &liquidities);
    let prepared = [
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(seq1.clone())),
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(seq2.clone())),
    ];
    let views: Vec<MixedHopView<'_>> = prepared
        .iter()
        .map(MixedHopView::ConcentratedLiquidity)
        .collect();
    let cfg = SolveRuntimeConfig::default();

    // Stance 1: no memo handle.
    let via_intake = solve_concentrated_liquidity_piecewise(&prepared, None, None, &cfg, None);
    let direct = solve_piecewise(&views, None, None, &cfg, None);
    assert!(via_intake.result.is_some(), "fixture must be profitable");
    assert_eq!(
        via_intake.result, direct.result,
        "no-memo results must match"
    );
    assert_eq!(
        via_intake.stats.sims, direct.stats.sims,
        "no-memo walk cost must match"
    );

    // Stance 2: memo handle present, inactive.
    let off = WalkMemo::new(false, false);
    let via_off = solve_concentrated_liquidity_piecewise(&prepared, Some(&off), None, &cfg, None);
    let direct_off = solve_piecewise(&views, Some(&off), None, &cfg, None);
    assert_eq!(via_off.result, direct_off.result);

    // Stance 3: active memos — SEPARATE handles so both routes Miss and walk
    // (one shared handle would turn the second call into a cache hit).
    let memo_a = WalkMemo::new(true, true);
    memo_a.begin_block(1);
    let memo_b = WalkMemo::new(true, true);
    memo_b.begin_block(1);
    let via_active =
        solve_concentrated_liquidity_piecewise(&prepared, Some(&memo_a), None, &cfg, None);
    let direct_active = solve_piecewise(&views, Some(&memo_b), None, &cfg, None);
    assert_eq!(via_active.result, direct_active.result);
    assert_eq!(via_active.stats.sims, direct_active.stats.sims);
    // The cost-parity half is only meaningful over real walks — without the
    // telemetry feature the counters read zero and the equality above is
    // vacuous.
    #[cfg(feature = "telemetry")]
    {
        assert!(
            via_active.stats.sims > 0,
            "the intake arm's Miss path must walk"
        );
        assert!(
            direct_active.stats.sims > 0,
            "the direct arm's Miss path must walk"
        );
    }
}

/// The two key front doors must fold identically for the same composition —
/// the no-fork claim the memo module's docs make, pinned.
#[test]
fn key_front_doors_agree_for_concentrated_liquidity_only_compositions() {
    let seq1 = multi_range_sequence(750, 1300, true, &[1_000_000_000_000_000]);
    let seq2 = multi_range_sequence(0, 60, false, &[1_000_000_000u128]);
    let prepared = [
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(seq1.clone())),
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(seq2.clone())),
    ];
    let views: Vec<MixedHopView<'_>> = prepared
        .iter()
        .map(MixedHopView::ConcentratedLiquidity)
        .collect();
    let seqs = [&seq1, &seq2];
    assert_eq!(
        walk_views_fingerprint(&views),
        walk_concentrated_liquidity_path_fingerprint(&seqs),
        "the views door and the capture door must fold the same key"
    );
}

/// Contract pin: both solve surfaces refuse paths shorter than two hops (the
/// unified entry's `len < 2` guard; the all-concentrated-liquidity intake forwards that contract).
#[test]
fn both_solve_entries_refuse_paths_shorter_than_two_hops() {
    let hop = make_v3_hop_at_1to1(10_000_000_000_000u128, true);
    let seq = IntV3TickRangeSequence::new(vec![hop]).unwrap();
    let prepared = [PreparedConcentratedLiquidityHop::derive(
        std::sync::Arc::new(seq.clone()),
    )];
    let views = [MixedHopView::ConcentratedLiquidity(&prepared[0])];
    let cfg = SolveRuntimeConfig::default();

    let via_intake = solve_concentrated_liquidity_piecewise(&prepared, None, None, &cfg, None);
    let direct = solve_piecewise(&views, None, None, &cfg, None);
    assert_eq!(
        via_intake.refusal,
        Some(WalkRefusal::TooShort),
        "the all-concentrated-liquidity intake must name its single-hop refusal"
    );
    assert_eq!(
        direct.refusal,
        Some(WalkRefusal::TooShort),
        "the unified entry must name its single-hop refusal"
    );
    assert!(via_intake.result.is_none() && direct.result.is_none());

    // Refusal is not unprofitability: a walked negative carries no refusal
    // (fees dominate this two-hop fixture — the walk runs and finds nothing).
    let hop2 = make_v3_hop_at_1to1(10_000_000_000_000u128, false);
    let seq2 = IntV3TickRangeSequence::new(vec![hop2]).unwrap();
    let negative = [
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(seq.clone())),
        PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(seq2)),
    ];
    let walked = solve_concentrated_liquidity_piecewise(&negative, None, None, &cfg, None);
    assert!(walked.result.is_none(), "fixture must walk to None");
    assert_eq!(
        walked.refusal, None,
        "a walked negative must not report a refusal"
    );

    // The empty path names the same arity refusal.
    let empty = solve_concentrated_liquidity_piecewise(&[], None, None, &cfg, None);
    assert_eq!(empty.refusal, Some(WalkRefusal::TooShort));
}

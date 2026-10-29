//! The merged state-mutation test suite (ADR-080 D4): the five write verbs —
//! `replace_tick_data`, `merge_tick_word`, `mark_bitmap_words_known`,
//! `apply_swap`, and `apply_liquidity_update` — written ONCE against the
//! [`ClStateFixture`] seam and run on BOTH adapters, so a per-design drift in
//! the write path (the V3/V4 nonce split this battery now forbids) is
//! unrepresentable rather than merely untested.
//!
//! The `apply_swap`/`apply_liquidity_update` journal and two-stamp contracts
//! live in the journal battery (`state_journal/tests.rs`); what this suite adds
//! is the per-verb contract every mutator owes the staleness gate
//! (`state_nonce`), the caches, and the coverage bookkeeping.

use alloy::primitives::{U128, U256};
use hashbrown::{HashMap, HashSet};

use crate::families::concentrated_liquidity::state_fixture::{ClStateFixture, LIQ};
use crate::families::concentrated_liquidity::state_journal::JournalHost;
use crate::families::concentrated_liquidity::state_seeding::SeedingHost;
use crate::families::concentrated_liquidity::state_walk::{
    get_cached_tick_ranges, CachedTickRanges, WalkHost,
};
use crate::sim::tick_fetch::FetchedTickWord;
use crate::state::v3_state::{PoolTickCoverage, V3PoolState};
use crate::state::v4_state::V4PoolState;
use crate::TickInfo;

fn tick(gross: u128, net: i128, block: u64) -> TickInfo {
    TickInfo {
        liquidity_gross: U256::from(gross).to::<U128>(),
        liquidity_net: net,
        block,
    }
}

fn word(word: i32, ticks: &[(i32, TickInfo)]) -> FetchedTickWord {
    FetchedTickWord::new(word, ticks.iter().cloned().collect())
}

/// Warm the `zero_for_one` tick-range cache slot so a later invalidation is
/// observable. Both designs cache a walk MISS as well as a hit, so "not
/// `StillEmpty`" is the warm predicate.
fn warm_cache<F: ClStateFixture>(state: &F, tag: &str) {
    let _ = get_cached_tick_ranges(state, 60, true);
    assert!(
        !matches!(state.cached_slot(true), CachedTickRanges::StillEmpty),
        "{tag}: cache is warm before the mutation"
    );
}

fn assert_cache_invalidated<F: ClStateFixture>(state: &F, tag: &str) {
    assert!(
        matches!(state.cached_slot(true), CachedTickRanges::StillEmpty),
        "{tag}: zfo cache invalidated"
    );
    assert!(
        matches!(state.cached_slot(false), CachedTickRanges::StillEmpty),
        "{tag}: ofz cache invalidated"
    );
}

fn assert_cache_preserved<F: ClStateFixture>(state: &F, tag: &str) {
    assert!(
        !matches!(state.cached_slot(true), CachedTickRanges::StillEmpty),
        "{tag}: cache preserved"
    );
}

#[test]
fn replace_tick_data_replaces_the_map_and_bumps_the_nonce_on_both_adapters() {
    fn case<F: ClStateFixture>(tag: &str) {
        // What: `replace_tick_data` (1) wholesale-replaces the tick map,
        // (2) advances ONLY the liquidity clock (`tick_data_block`) — the
        // scalars and the price clock are untouched (two-stamp rule),
        // (3) re-seeds `known_bitmap_words` from the new keys,
        // (4) invalidates both cached directions, and (5) bumps `state_nonce`.
        // Why (5): the tick map IS observable state, so a solver result
        // computed against the pre-replace map is stale — the staleness gate
        // (`Observe::StateNonce`) only rejects it if the nonce moves.
        let mut state = F::with_position(LIQ);
        state.set_coverage(PoolTickCoverage::Sparse);
        warm_cache(&state, tag);
        let nonce_before = state.state_nonce();
        let sqrt_before = WalkHost::sqrt_price_x96(&state);
        let journal_before = JournalHost::journal(&state).len();

        let mut replacement = HashMap::new();
        replacement.insert(15_360, tick(700, 700, 12));
        replacement.insert(-15_360, tick(700, -700, 12));
        assert!(
            state.replace_tick_data(replacement, 12, 60),
            "{tag}: the write door reports success"
        );

        // (1) wholesale replacement — the old [-60, +60] position is gone.
        let ticks = SeedingHost::tick_data(&state);
        assert_eq!(ticks.len(), 2, "{tag}: map replaced, not merged");
        assert!(
            ticks.contains_key(&15_360) && ticks.contains_key(&-15_360),
            "{tag}: replacement keys present"
        );
        // (2) two-stamp rule: liquidity clock only; clocks/scalars clean.
        assert_eq!(
            WalkHost::tick_data_block(&state),
            12,
            "{tag}: liquidity clock"
        );
        assert_eq!(WalkHost::update_block(&state), 0, "{tag}: price clock");
        assert_eq!(
            WalkHost::sqrt_price_x96(&state),
            sqrt_before,
            "{tag}: scalars untouched"
        );
        // (3) known words re-seeded from the new keys: word ±1 at spacing 60.
        let known = SeedingHost::known_bitmap_words(&state);
        assert_eq!(
            known,
            &HashSet::from([-1, 1]),
            "{tag}: known words re-seeded from the replacement keys"
        );
        // (4) cache invalidated.
        assert_cache_invalidated(&state, tag);
        // (5) exactly one nonce bump, and no journal delta (a wholesale
        // replace has undefined rollback semantics).
        assert_eq!(
            state.state_nonce(),
            nonce_before.wrapping_add(1),
            "{tag}: exactly one nonce bump per tick-map replace"
        );
        assert_eq!(
            JournalHost::journal(&state).len(),
            journal_before,
            "{tag}: no journal delta"
        );
    }
    case::<V3PoolState>("V3");
    case::<V4PoolState>("V4");
}

#[test]
fn merge_tick_word_merges_the_word_and_bumps_the_nonce_on_both_adapters() {
    fn case<F: ClStateFixture>(tag: &str) {
        // What: `merge_tick_word` (1) lands the fetched rows, (2) marks the
        // word known, (3) invalidates the cached directions, and (4) bumps
        // `state_nonce`.
        // Why (4): a merged word changes observable state — a candidate solved
        // against the pre-merge map must be rejected at the dispatch seam.
        let mut state = F::with_position(LIQ);
        state.set_coverage(PoolTickCoverage::Sparse);
        warm_cache(&state, tag);
        let nonce_before = state.state_nonce();
        let journal_before = JournalHost::journal(&state).len();

        let fetched = word(7, &[(420, tick(500, 500, 9))]);
        assert!(
            state.merge_tick_word(&fetched),
            "{tag}: the write door reports success"
        );

        // (1) the fetched rows landed next to the resident position.
        let ticks = SeedingHost::tick_data(&state);
        assert_eq!(
            ticks.get(&420),
            Some(&tick(500, 500, 9)),
            "{tag}: fetched row landed"
        );
        assert!(
            ticks.contains_key(&-60) && ticks.contains_key(&60),
            "{tag}: a merge never drops resident ticks"
        );
        // (2) the whole word is known.
        assert!(
            SeedingHost::known_bitmap_words(&state).contains(&7),
            "{tag}: merged word marked known"
        );
        // (3) cache invalidated.
        assert_cache_invalidated(&state, tag);
        // (4) exactly one nonce bump; a fetched merge never journals.
        assert_eq!(
            state.state_nonce(),
            nonce_before.wrapping_add(1),
            "{tag}: exactly one nonce bump per word merge"
        );
        assert_eq!(
            JournalHost::journal(&state).len(),
            journal_before,
            "{tag}: no journal delta"
        );
    }
    case::<V3PoolState>("V3");
    case::<V4PoolState>("V4");
}

#[test]
fn merge_tick_word_keeps_a_fresher_resident_row_and_overwrites_a_stale_one_on_both_adapters() {
    fn case<F: ClStateFixture>(tag: &str) {
        // What: the two-stamp discipline at the insert — a fetched word is a
        // block-stamped snapshot, so it must never regress a resident row
        // stamped LATER (the fetch was in flight while the pump applied a newer
        // event), while a resident row stamped at or before the fetch's block
        // is superseded.
        // Why: a regression here would silently rewind a tick the pump already
        // advanced, corrupting the walk without any journal delta to undo it.
        let mut state = F::with_position(LIQ);
        JournalHost::tick_data_mut(&mut state).insert(60, tick(900, -900, 40));
        JournalHost::tick_data_mut(&mut state).insert(-60, tick(800, 800, 30));

        // Word 0: tick 60 resident at block 40 vs fetched at 30 (resident
        // fresher — keep it); tick -60 resident at 30 vs fetched at 30 (equal
        // stamp — the fetch wins), tick 90 is new.
        let fetched = word(
            0,
            &[
                (60, tick(700, -700, 30)),
                (-60, tick(700, 700, 30)),
                (90, tick(1, 1, 30)),
            ],
        );
        assert!(
            state.merge_tick_word(&fetched),
            "{tag}: the write door reports success"
        );

        let ticks = SeedingHost::tick_data(&state);
        assert_eq!(
            ticks.get(&60),
            Some(&tick(900, -900, 40)),
            "{tag}: a fresher resident row survives the merge"
        );
        assert_eq!(
            ticks.get(&-60),
            Some(&tick(700, 700, 30)),
            "{tag}: an equal-stamp fetched row supersedes the resident"
        );
        assert_eq!(
            ticks.get(&90),
            Some(&tick(1, 1, 30)),
            "{tag}: new fetched rows land"
        );
    }
    case::<V3PoolState>("V3");
    case::<V4PoolState>("V4");
}

#[test]
fn mark_bitmap_words_known_writes_sparse_only_and_touches_no_clock_on_both_adapters() {
    fn case<F: ClStateFixture>(tag: &str) {
        // What: `mark_bitmap_words_known` extends the known set for a Sparse
        // pool, is a no-op for a Tracked one (a complete bitmap's absent word
        // is known-empty — writing the set would be dead state), and moves
        // neither the clocks, the caches, nor the nonce.
        // Why: the known set is a miss-detection fact, not a state mutation —
        // bumping the nonce here would invalidate every in-flight candidate for
        // a fact the solver never reads.
        let mut state = F::with_position(LIQ);
        state.set_coverage(PoolTickCoverage::Sparse);
        warm_cache(&state, tag);
        let nonce_before = state.state_nonce();

        state.mark_words_known(&[5, 9]);
        let known = SeedingHost::known_bitmap_words(&state);
        assert!(
            known.contains(&5) && known.contains(&9),
            "{tag}: sparse words marked"
        );

        state.mark_words_known(&[11]);
        assert!(
            !matches!(state.cached_slot(true), CachedTickRanges::StillEmpty),
            "{tag}: the known set does not feed the tick-range cache"
        );
        assert_eq!(
            WalkHost::update_block(&state),
            0,
            "{tag}: price clock untouched"
        );
        assert_eq!(
            WalkHost::tick_data_block(&state),
            0,
            "{tag}: liquidity clock untouched"
        );
        assert_eq!(
            state.state_nonce(),
            nonce_before,
            "{tag}: marking a word known is not a state mutation"
        );

        let mut tracked = F::with_position(LIQ);
        tracked.set_coverage(PoolTickCoverage::Tracked);
        tracked.mark_words_known(&[5, 9]);
        assert!(
            SeedingHost::known_bitmap_words(&tracked).is_empty(),
            "{tag}: a Tracked pool never writes the known set"
        );
    }
    case::<V3PoolState>("V3");
    case::<V4PoolState>("V4");
}

#[test]
fn apply_swap_bumps_the_nonce_once_and_advances_both_clocks_on_both_adapters() {
    fn case<F: ClStateFixture>(tag: &str) {
        // What: a Swap rewrites the slot0 head and may cross ticks, so it
        // advances BOTH clocks and bumps the nonce exactly once.
        let mut state = F::seeded_state();
        let nonce_before = state.state_nonce();

        state.apply_swap(
            U256::from((1u128 << 96) + 1),
            LIQ + 1,
            60,
            97,
            &[(120, tick(500, 500, 97))],
        );

        assert_eq!(
            WalkHost::update_block(&state),
            97,
            "{tag}: price clock advanced"
        );
        assert_eq!(
            WalkHost::tick_data_block(&state),
            97,
            "{tag}: liquidity clock advanced"
        );
        assert_eq!(
            state.state_nonce(),
            nonce_before.wrapping_add(1),
            "{tag}: exactly one nonce bump per swap"
        );
    }
    case::<V3PoolState>("V3");
    case::<V4PoolState>("V4");
}

#[test]
fn apply_liquidity_update_bumps_the_nonce_once_on_both_adapters() {
    fn case<F: ClStateFixture>(tag: &str) {
        let mut state = F::seeded_state();
        let nonce_before = state.state_nonce();

        state.apply_liquidity_update(-60, 60, 7_000, 97);

        assert_eq!(
            state.state_nonce(),
            nonce_before.wrapping_add(1),
            "{tag}: exactly one nonce bump per liquidity update"
        );
        assert_cache_invalidated(&state, tag);
    }
    case::<V3PoolState>("V3");
    case::<V4PoolState>("V4");
}

/// A backward stamp on the liquidity clock is an invariant violation outside a
/// reorg: replacing the tick map with data from a block BELOW the current
/// liquidity clock must panic loudly (two-stamp rule). Both adapters must be
/// exercised, and `#[should_panic]` stops at the first, so each gets its own
/// wrapper.
#[test]
#[should_panic(expected = "monotonicity violated: tick_data_block")]
fn replace_tick_data_backward_block_panics_v3() {
    let mut state = V3PoolState::with_position(LIQ);
    assert!(state.replace_tick_data(HashMap::new(), 10, 60));
    let _ = state.replace_tick_data(HashMap::new(), 5, 60);
}

#[test]
#[should_panic(expected = "monotonicity violated: tick_data_block")]
fn replace_tick_data_backward_block_panics_v4() {
    let mut state = V4PoolState::with_position(LIQ);
    assert!(state.replace_tick_data(HashMap::new(), 10, 60));
    let _ = state.replace_tick_data(HashMap::new(), 5, 60);
}

#[test]
fn an_in_range_swap_that_moves_neither_the_tick_nor_the_map_preserves_the_cache_on_both() {
    fn case<F: ClStateFixture>(tag: &str) {
        // What: the walk inputs are the tick map and the current tick, so a
        // swap that stays at the same tick and crosses nothing leaves the
        // cached ranges valid — the dominant case for an active pool, which
        // swaps inside its current range every block.
        let mut state = F::seeded_state();
        warm_cache(&state, tag);

        state.apply_swap(WalkHost::sqrt_price_x96(&state), LIQ, 0, 97, &[]);

        assert_cache_preserved(&state, tag);
        assert_eq!(
            WalkHost::update_block(&state),
            97,
            "{tag}: the price clock still advances"
        );
    }
    case::<V3PoolState>("V3");
    case::<V4PoolState>("V4");
}

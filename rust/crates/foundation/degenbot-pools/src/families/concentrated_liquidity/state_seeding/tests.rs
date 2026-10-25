//! The merged seeding-verb test suite (ADR-080 D4): the word-position
//! arithmetic and bitmap-word seeding the two adapters share. The V3/V4
//! `replace_tick_data` re-seeding probes stay on the adapters (the swap-map
//! method is per-adapter surface); what lives here is the shared verb
//! contract itself.

use super::{seed_checked_bitmap_words, seed_known_bitmap_words, word_of, SeedingHost};
use crate::families::concentrated_liquidity::state_fixture::ClStateFixture;
use crate::families::concentrated_liquidity::state_journal::JournalHost;
use crate::state::v3_state::{PoolTickCoverage, V3PoolState};
use crate::state::v4_state::V4PoolState;
use crate::TickInfo;
use alloy::primitives::U128;
use hashbrown::HashSet;

/// Word-of parity: the sparse miss-detection model (`word_of`, used by
/// both V3 + V4 `vX_simulate_swap` to decide whether the current tick's
/// bitmap word is "known") must match the bitmap word computation
/// (`position(tick // tick_spacing)[0]`, the canonical word-position math
/// in [`crate::tick_bitmap`]). Both use floored division (`div_euclid` ==
/// `//`) + arithmetic right-shift (`>> 8`), so they agree for
/// negative non-multiple current ticks — the regime a crossing swap's
/// post-step price lives in. This test locks that equivalence so the V4
/// crossing-swap divergence under the fetch seam is NOT
/// mis-attributed to the miss-detection model.
#[test]
fn word_of_matches_bitmap_word_position_for_edge_ticks() {
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
        let rust_word = word_of(tick, spacing);
        // Python: compressed = tick // spacing (floored); word = compressed >> 8.
        let compressed = tick.div_euclid(spacing);
        let py_word = compressed >> 8;
        assert_eq!(
            rust_word, py_word,
            "word_of({tick}, {spacing}) = {rust_word} != python {py_word}"
        );
    }
}

#[test]
fn seed_known_bitmap_words_seeds_the_word_positions_of_the_tick_keys_on_both_adapters() {
    fn seed_known_bitmap_words_seeds_the_word_positions_of_the_tick_keys<F: ClStateFixture>(
        tag: &str,
    ) {
        // The sparse miss-detection set is exactly the word positions of the
        // initialized tick keys — a region is "known" iff one of its ticks
        // was seeded. Ticks 0 / ±15360 at spacing 60 land in words 0 / ±1
        // (compressed = tick.div_euclid(60), word = compressed >> 8).
        let mut state = F::with_position(1_000_000u128);
        {
            let ticks = JournalHost::tick_data_mut(&mut state);
            ticks.clear();
            for (tick, gross, net) in [
                (0, 500u128, 500i128),
                (15_360, 700, 700),
                (-15_360, 700, -700),
            ] {
                ticks.insert(
                    tick,
                    TickInfo {
                        liquidity_gross: U128::from(gross),
                        liquidity_net: net,
                        block: 0,
                    },
                );
            }
        }

        seed_known_bitmap_words(&mut state, 60);

        let known = SeedingHost::known_bitmap_words_mut(&mut state);
        assert_eq!(
            known,
            &HashSet::from([0, 1, -1]),
            "{tag}: the seeded set is exactly the keys' word positions"
        );
    }
    seed_known_bitmap_words_seeds_the_word_positions_of_the_tick_keys::<V3PoolState>("V3");
    seed_known_bitmap_words_seeds_the_word_positions_of_the_tick_keys::<V4PoolState>("V4");
}

#[test]
fn seed_checked_bitmap_words_extends_the_sparse_set_and_ignores_tracked() {
    fn case<F: ClStateFixture>(tag: &str) {
        // Sparse: a checked-empty word (absent from the tick map, so absent
        // from the derived set) joins the known set — the registration-time
        // on-chain check is never repeated as a fetch.
        let mut state = F::with_position(1_000_000u128);
        F::set_coverage(&mut state, PoolTickCoverage::Sparse);
        seed_known_bitmap_words(&mut state, 60);
        let derived = SeedingHost::known_bitmap_words_mut(&mut state).clone();
        assert!(
            !derived.contains(&9),
            "{tag}: fixture precondition: word 9 has no tick"
        );

        seed_checked_bitmap_words(&mut state, &[9], PoolTickCoverage::Sparse);

        let known = SeedingHost::known_bitmap_words_mut(&mut state).clone();
        assert!(
            known.contains(&9),
            "{tag}: the registration-checked word joins the sparse set: {known:?}"
        );
        assert!(
            derived.is_subset(&known),
            "{tag}: the tick-derived words survive the checked-word seed"
        );

        // Tracked: the bitmap is complete, so the known set is never written
        // (the same coverage gate as `mark_bitmap_words_known`).
        let mut state = F::with_position(1_000_000u128);
        F::set_coverage(&mut state, PoolTickCoverage::Tracked);
        seed_known_bitmap_words(&mut state, 60);
        let before = SeedingHost::known_bitmap_words_mut(&mut state).clone();

        seed_checked_bitmap_words(&mut state, &[9], PoolTickCoverage::Tracked);

        assert_eq!(
            SeedingHost::known_bitmap_words_mut(&mut state),
            &before,
            "{tag}: a Tracked pool ignores registration-checked words"
        );
    }

    case::<V3PoolState>("V3");
    case::<V4PoolState>("V4");
}

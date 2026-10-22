//! The concentrated liquidity family's bitmap-word seeding and registration-coverage bookkeeping
//! (ADR-080 D1, step 2) — the state-side half of the family tree, beside the
//! read door.
//!
//! The V3 and V4 state structs (`crate::v3_state` / `crate::v4_state`) carry
//! the same word-position arithmetic, the same sparse miss-detection set
//! (`known_bitmap_words`), the same genesis-anchor shape, and the same
//! coverage-driven registration decisions, so those verbs are written once
//! here — data-generically over the [`SeedingHost`] field seam — and the
//! per-design files keep only thin forwarders. What genuinely differs stays
//! per-adapter: the registration seeding scope (V3 seeds known words for
//! `Sparse` pools only; V4 seeds for every coverage — the ADR-006 inline-seed
//! closure) and the hook-keyed V4 identity live in the adapters' `from_params`.

use hashbrown::{HashMap, HashSet};

use alloy::primitives::U256;

use crate::state_history::{ReorgJournal, ScalarPriors, V3BlockDelta};
use crate::v3_state::{PoolTickCoverage, RegistrationLifecycle};
use crate::TickInfo;
use degenbot_math::concentrated_liquidity::functions::tick_position;

/// The field seam the shared seeding/coverage verbs read and write through —
/// implemented by each design's state struct (`V3PoolState`, `V4PoolState`),
/// whose word set, tick map, slot0 scalars, clocks, and journal are
/// shape-identical. The accessors are the family-specific field access; the
/// verb logic below is written once over this seam.
pub(crate) trait SeedingHost {
    /// The pool's initialized-tick map.
    fn tick_data(&self) -> &HashMap<i32, TickInfo>;

    /// The checked tick-bitmap words (the sparse miss-detection set), mutable.
    fn known_bitmap_words_mut(&mut self) -> &mut HashSet<i32>;

    /// The pool's current `sqrt_price_x96` slot0 scalar.
    fn sqrt_price_x96(&self) -> U256;

    /// The pool's current active `liquidity` scalar.
    fn liquidity(&self) -> u128;

    /// The pool's current active `tick`.
    fn tick(&self) -> i32;

    /// The pool's price clock (`update_block`).
    fn update_block(&self) -> u64;

    /// The pool's liquidity clock (`tick_data_block`).
    fn tick_data_block(&self) -> u64;

    /// The pool's reorg journal, mutable.
    fn journal_mut(&mut self) -> &mut ReorgJournal<V3BlockDelta>;
}

/// `V3PoolState::word_of` / `V4PoolState::word_of` — the word position
/// (`tick_position(tick.div_euclid(tick_spacing))`) holding the given tick,
/// shared by the V3/V4 sparse miss-detection.
#[must_use]
pub(crate) fn word_of(tick: i32, tick_spacing: i32) -> i32 {
    i32::from(tick_position(tick.div_euclid(tick_spacing)).0)
}

/// `V3PoolState::seed_known_bitmap_words` / `V4PoolState::seed_known_bitmap_words`
/// — seed `known_bitmap_words` from the current `tick_data` keys' word
/// positions. Callers decide the coverage scope: at registration V3 seeds for
/// `Sparse` pools (the keys a partial snapshot carries are known) while V4
/// seeds for every coverage; `replace_tick_data` re-seeds from the
/// replacement map.
pub(crate) fn seed_known_bitmap_words<H: SeedingHost>(host: &mut H, tick_spacing: i32) {
    let seeded: HashSet<i32> = host
        .tick_data()
        .keys()
        .map(|&t| word_of(t, tick_spacing))
        .collect();
    *host.known_bitmap_words_mut() = seeded;
}

/// `V3PoolState::from_params` / `V4PoolState::from_params` — record the
/// checked bitmap words a registration caller supplied (each word was looked
/// up on-chain and came back EMPTY). Extends the set seeded from the
/// `tick_data` keys, so a registration-time check is never repeated as a
/// fetch. A no-op for `Tracked` pools, whose bitmap is complete — the same
/// coverage gate as `ConcentratedLiquidityPoolMut::mark_bitmap_words_known`.
pub(crate) fn seed_checked_bitmap_words<H: SeedingHost>(
    host: &mut H,
    words: &[i32],
    coverage: PoolTickCoverage,
) {
    if coverage == PoolTickCoverage::Sparse && !words.is_empty() {
        host.known_bitmap_words_mut().extend(words.iter().copied());
    }
}

/// `ConcentratedLiquidityPoolMut::seed_genesis` (one impl per family) — the
/// registration/seed genesis anchor (two-stamp rule fresh-read builder).
///
/// Pushes a `before == after` journal delta at `block` so the reorg journal is
/// non-empty from registration — keeping `has_state_prior_to` true so a
/// mid-window reorg restores to the seeded state instead of the graceful
/// `NoStatePriorToBlock` pump shutdown that an empty journal would trigger.
/// The delta records the current scalars + both clocks as its "before" priors,
/// so a restore pops to the exact seeded state.
///
/// It deliberately advances no clock: it is the split-seed replacement for the
/// builder's old `apply_swap`-genesis, which (a) would backward-panic the
/// price clock when registration seeds `update_block` at HEAD past the DB map
/// block, and (b) would advance `tick_data_block` and falsely claim the tick
/// map reaches head. Because `before == after` it is safe; call once at
/// registration time, mirroring the V2/Curve genesis-anchor pattern.
pub(crate) fn seed_genesis<H: SeedingHost>(host: &mut H, block: u64) {
    let scalars = ScalarPriors {
        sqrt_price_x96_before: host.sqrt_price_x96(),
        liquidity_before: host.liquidity(),
        tick_before: host.tick(),
    };
    let update_block_before = host.update_block();
    let tick_data_block_before = host.tick_data_block();
    host.journal_mut().push_delta(V3BlockDelta {
        block,
        scalar_priors: Some(scalars),
        update_block_before: Some(update_block_before),
        tick_data_block_before: Some(tick_data_block_before),
        tick_priors: Vec::new(),
    });
}

/// The registration lifecycle a coverage starts in: a `Tracked` pool (complete
/// map, pinned verify seed) starts `Quarantined` so no live event
/// direct-applies before the two-step verify; a `Sparse` pool stays
/// `Live`/direct-apply.
#[must_use]
pub(crate) fn registration_lifecycle_for(coverage: PoolTickCoverage) -> RegistrationLifecycle {
    if coverage == PoolTickCoverage::Tracked {
        RegistrationLifecycle::Quarantined
    } else {
        RegistrationLifecycle::Live
    }
}

/// The pinned snapshot verify seed: a clone of the complete registration tick
/// map for `Tracked` pools (step-1 verify compares it — not the pump-mutated
/// live map — against on-chain@snapshot_block); `None` for `Sparse` pools (no
/// complete seed).
#[must_use]
pub(crate) fn snapshot_seed_for(
    tick_data: &HashMap<i32, TickInfo>,
    coverage: PoolTickCoverage,
) -> Option<HashMap<i32, TickInfo>> {
    if coverage == PoolTickCoverage::Tracked {
        Some(tick_data.clone())
    } else {
        None
    }
}

/// The merged seeding-verb test suite (ADR-080 D4) — the former V3/V4 twin
/// batteries, converged at this module's interface.
#[cfg(test)]
mod tests;

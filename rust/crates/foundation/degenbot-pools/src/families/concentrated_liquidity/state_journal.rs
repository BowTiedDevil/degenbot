//! The concentrated liquidity family's reorg-journal state verbs (ADR-080 D1, step 1) — the
//! state-side half of the family tree, beside the read door.
//!
//! The V3 and V4 state structs (`crate::v3_state` / `crate::v4_state`) carry
//! the same journal shape (`ReorgJournal<V3BlockDelta>`), the same slot0
//! scalars, the same two-stamp clocks, and the same tick map, so the
//! restore/trim/inspect verbs are written once here — data-generically over
//! the [`JournalHost`] field seam — and the per-design files keep only the
//! thin `ReorgPoolState` forwarding impls. The family-specific field-write
//! stays inside each struct's impl; the verb logic above it lives here.

use hashbrown::HashMap;

use crate::state_history::{JournalError, ReorgJournal, ScalarPriors, V3BlockDelta};
use crate::TickInfo;

/// The field seam the shared journal verbs write through — implemented by
/// each design's state struct (`V3PoolState`, `V4PoolState`), whose journal
/// and touched fields are shape-identical. The accessors are the
/// family-specific field-write; the verb logic below is written once over
/// this seam.
pub(crate) trait JournalHost {
    /// The pool's reorg journal.
    fn journal(&self) -> &ReorgJournal<V3BlockDelta>;

    /// The pool's reorg journal, mutable.
    fn journal_mut(&mut self) -> &mut ReorgJournal<V3BlockDelta>;

    /// The initialized-tick map the restore reverse-applies into.
    fn tick_data_mut(&mut self) -> &mut HashMap<i32, TickInfo>;

    /// Write back the slot0 scalar priors (`sqrt_price_x96`, `liquidity`,
    /// `tick`) of the rolled-back range.
    fn apply_scalar_priors(&mut self, priors: &ScalarPriors);

    /// Rewind the two-stamp clocks to their exact pre-target values; `None`
    /// means that clock was not advanced by the rolled-back events, so its
    /// current value is already correct.
    fn apply_clock_priors(
        &mut self,
        update_block_before: Option<u64>,
        tick_data_block_before: Option<u64>,
    );

    /// Bump the per-mutation nonce and invalidate the tick-range cache — the
    /// post-restore bookkeeping every rewind carries.
    fn bump_state_nonce_and_invalidate_cache(&mut self);
}

/// `ReorgPoolState::restore_before_block` — written once for both designs.
///
/// Pops the rolled-back range from the journal and reverse-applies its priors
/// into the live fields through the [`JournalHost`] seam. The journal's own
/// `restore_before_block` panics on an empty journal (an invariant violation
/// for a registered pool, not a recoverable error), so the verb itself cannot
/// fail — the trait's `Result` is wrapped by the forwarding impls.
pub(crate) fn restore_before_block<H: JournalHost>(host: &mut H, block: u64) {
    let result = host.journal_mut().restore_before_block(block);

    // Sync scalar fields if the rolled-back range had scalar changes.
    // If scalar_priors is None (tick-only event(s) rolled back), the
    // current slot0 scalars were never changed by the rolled-back events
    // and are already correct — skip the write-back. See ADR-004.
    if let Some(p) = &result.scalar_priors {
        host.apply_scalar_priors(p);
    }
    // Reorg is the sole sanctioned rewind of both clocks: restore each to
    // its exact pre-target value from the rolled-back range's priors
    // (two-stamp rule).
    host.apply_clock_priors(result.update_block_before, result.tick_data_block_before);
    host.bump_state_nonce_and_invalidate_cache();

    // Reverse-apply tick priors accumulated across all popped deltas. Each
    // prior carries the tick's full pre-range row, its `block` stamp
    // included, so the restored map is bit-identical to a fresh replay of
    // every event before the target — a tick last mutated at 97 says 97
    // again even when a sibling tick moved the liquidity clock to 99 in
    // between.
    for (tick_idx, tick_before) in &result.tick_priors {
        match tick_before.liquidity_gross_before {
            Some(gross_before) => {
                // Tick existed before — restore its prior row.
                host.tick_data_mut().insert(
                    *tick_idx,
                    TickInfo {
                        liquidity_gross: gross_before,
                        liquidity_net: tick_before.liquidity_net_before,
                        block: tick_before.block_before,
                    },
                );
            }
            None => {
                // Tick was newly initialized in this block — remove it.
                host.tick_data_mut().remove(tick_idx);
            }
        }
    }
}

/// `ReorgPoolState::discard_before_block` — trims old deltas without
/// mutating the live state fields (only the journal front).
pub(crate) fn discard_before_block<H: JournalHost>(
    host: &mut H,
    block: u64,
) -> Result<(), JournalError> {
    host.journal_mut().discard_before_block(block)
}

/// `ReorgPoolState::journal_len` — the number of deltas currently held.
#[must_use]
pub(crate) fn journal_len<H: JournalHost>(host: &H) -> usize {
    host.journal().len()
}

/// `ReorgPoolState::newest_block` — the newest delta's block, or `None` if
/// the journal is empty.
#[must_use]
pub(crate) fn newest_block<H: JournalHost>(host: &H) -> Option<u64> {
    host.journal().newest_block()
}

/// The merged journal-verb test suite (ADR-080 D4) — the former V3/V4 twin
/// batteries, converged at this module's interface.
#[cfg(test)]
mod tests;

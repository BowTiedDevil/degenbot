//! The unified reorg-journal dispatch over every registered family (ADR-016).
//!
//! One trait-dispatching method per journal op over every `PoolEntry` variant,
//! through `PoolEntry::as_reorg_state(_mut)`. The per-family state structs own
//! the journal mechanics; this module owns the dispatch, so a caller never
//! names a family to roll back. `restore` returns the journal's own typed error
//! rather than a per-family restore-result type, which keeps those types inside
//! the impls that produce them.

use super::{BotState, JournalError, PoolEntry, ReorgPoolState};

impl BotState {
    /// Restore **every** registered pool's state to just before `target`.
    ///
    /// Bulk restore helper. The engine's reorg path is per-event
    /// `ReorgCoordinator::dispatch_reorg_log` (per-pool `restore_before_block`),
    /// not this bulk call: this helper survives as a `BotState` API (used by
    /// tests + available for ad-hoc bulk rollback), not on the hot path.
    ///
    /// Pools with no journal delta at/after `target` are left as-is (idempotent
    /// a reorg touches only a subset of pools). Returns the count of pools
    /// that were rolled back.
    pub fn restore_all_pools_before_block(&mut self, target: u64) -> usize {
        let pool_ids: Vec<u64> = self.pools.keys().collect();
        let mut restored = 0usize;
        for pool_id in pool_ids {
            // Peek the per-pool newest delta block without a mutable borrow
            // (ADR-016). Only pools with a delta at/after the reorg target
            // need rollback; untouched pools keep their current state
            // (idempotent restore). The peek also guards the concentrated liquidity family's
            // panic-on-empty journal: an empty journal reports `None` → skip.
            let needs_restore = self
                .pools
                .get(pool_id)
                .and_then(PoolEntry::as_reorg_state)
                .and_then(ReorgPoolState::newest_block)
                .is_some_and(|b| b >= target);
            if !needs_restore {
                continue;
            }

            // Dispatch through the unified trait path. On `Ok`, the trait
            // impl wrote the landed-at state into the struct's own fields; on
            // `Err` (target at/before registration), skip the pool
            // (idempotent — a reorg doesn't touch pools that didn't exist
            // before the fork target).
            let did_restore = self
                .restore_pool_before_block(pool_id, target)
                .is_some_and(|r| r.is_ok());
            if did_restore {
                restored += 1;
            }
        }
        restored
    }

    /// Restore `pool_id`'s state to the landed-at state strictly before
    /// `block`, dispatching through `ReorgPoolState::restore_before_block`.
    /// Returns `None` if the pool is not registered.
    ///
    /// A caller needing the restored values (the `PyO3` wrapper, which marshals
    /// a tuple to Python) reads the struct's current fields after restore —
    /// the post-restore fields ARE the landed-at (before) values the
    /// per-family return types previously carried.
    ///
    /// # Errors
    ///
    /// `NoStatePriorToBlock` if the target is at/before the registration
    /// (genesis) delta. The concentrated liquidity family's journal panics on empty instead —
    /// callers must pre-check [`has_state_prior_to`](Self::has_state_prior_to).
    pub fn restore_pool_before_block(
        &mut self,
        pool_id: u64,
        block: u64,
    ) -> Option<Result<(), JournalError>> {
        Some(
            self.pools
                .get_mut(pool_id)?
                .as_reorg_state_mut()?
                .restore_before_block(block),
        )
    }

    /// Discard reorg journal deltas earlier than `block`, dispatching through
    /// `ReorgPoolState::discard_before_block`. Returns `None` if the pool is
    /// not registered. Does NOT mutate the live state fields (only trims old
    /// history).
    ///
    /// # Errors
    ///
    /// `NoStateAtOrAfterBlock` if the target is past the newest delta.
    pub fn discard_pool_before_block(
        &mut self,
        pool_id: u64,
        block: u64,
    ) -> Option<Result<(), JournalError>> {
        Some(
            self.pools
                .get_mut(pool_id)?
                .as_reorg_state_mut()?
                .discard_before_block(block),
        )
    }

    /// Number of deltas in the reorg journal, dispatching through
    /// `ReorgPoolState::journal_len`. Returns `None` if the pool is not
    /// registered.
    #[must_use]
    pub fn pool_journal_len(&self, pool_id: u64) -> Option<usize> {
        Some(self.pools.get(pool_id)?.as_reorg_state()?.journal_len())
    }

    /// Peek the newest reorg-journal delta block for `pool_id` without
    /// mutating anything. `None` when unregistered or the journal is empty.
    /// Used by `ReorgCoordinator` to label idempotent no-op restores
    /// (newest delta strictly below the reorg target) in its
    /// `degenbot.reorg.restore` spans.
    #[must_use]
    pub fn newest_journal_block(&self, pool_id: u64) -> Option<u64> {
        self.pools
            .get(pool_id)
            .and_then(PoolEntry::as_reorg_state)
            .and_then(ReorgPoolState::newest_block)
    }

    /// Does `pool_id`'s journal have state at or before `block`?
    /// `false` → a too-deep reorg; `ReorgCoordinator` returns
    /// `Err(NoStatePriorToBlock)` and the pump shuts down gracefully.
    ///
    /// The predicate is **family-dependent** because the journals differ in
    /// whether they carry a genesis anchor:
    ///
    /// - **V2** carries a genesis delta (pushed at registration, `before ==
    ///   after`). There is genuinely no state *prior to* the genesis block, so
    ///   a target at or before the earliest (genesis) delta is too-deep:
    ///   `earliest < block`.
    /// - **V3/V4** push **no** genesis delta at registration. The "before"
    ///   values of the first forward event ARE the registration state, so
    ///   `restore_before_block(B)` handles a single delta at `B` (and any
    ///   target below the earliest delta) by popping down to registration
    ///   state. The ONLY unrecoverable case is an empty journal
    ///   (`restore_before_block` panics on empty) → `!is_empty()`.
    ///
    /// A pool whose newest delta is below `block` (idempotent no-op restore)
    /// returns `true` under both predicates.
    #[must_use]
    pub fn has_state_prior_to(&self, pool_id: u64, block: u64) -> bool {
        let Some(entry) = self.pools.get(pool_id) else {
            // Pool not registered → no journal → the reorg can't restore it.
            // Treat as "has state" (no-op) so the caller proceeds to the normal
            // pool-not-found no-op path rather than a fail-stop.
            return true;
        };
        match entry {
            PoolEntry::V2(p) => {
                p.1.journal
                    .earliest_block()
                    .is_some_and(|earliest| earliest < block)
            }
            // No genesis anchor — empty journal is the only too-deep case.
            PoolEntry::V3(p) => !p.1.journal.is_empty(),
            PoolEntry::V4(p) => !p.1.journal.is_empty(),
            // Curve carries a genesis delta (mirror of V2) — a target at/before
            // the genesis block is too-deep: `earliest < block`.
            PoolEntry::Curve(p) => {
                p.1.journal
                    .earliest_block()
                    .is_some_and(|earliest| earliest < block)
            }
            // Balancer weighted carries a genesis delta (mirror of V2/Curve).
            // Same predicate: `earliest < block`.
            PoolEntry::BalancerWeighted(p) => {
                p.1.journal
                    .earliest_block()
                    .is_some_and(|earliest| earliest < block)
            }
            // Balancer stable carries a genesis delta (mirror of
            // V2/Curve/BalancerWeighted). Same predicate: `earliest < block`.
            PoolEntry::BalancerStable(p) => {
                p.1.journal
                    .earliest_block()
                    .is_some_and(|earliest| earliest < block)
            }
            // Aerodrome carries a genesis delta (mirror of V2/Curve/Balancer)
            // ADR-005 Aerodrome slice. Same predicate: `earliest < block`.
            PoolEntry::AerodromeV2(p) => {
                p.1.journal
                    .earliest_block()
                    .is_some_and(|earliest| earliest < block)
            }
        }
    }
}

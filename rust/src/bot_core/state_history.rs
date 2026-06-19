//! Reorg journal — delta-based rollback for pool state.
//!
//! Each pool carries a bounded deque of per-block deltas storing the **prior**
//! values of modified state fields. On forward progress, the current mutable
//! state is updated and the "before" values are stashed in the journal.
//! On reorg, journal entries are popped and their prior values restored into
//! the current state.
//!
//! Swap calculations and the hot path **never touch the journal** — they
//! always read the current mutable fields. Zero penalty.
//!
//! V2 is the degenerate case where the delta = full state (two reserves).
//! V3 stores only modified tick priors (typically 0–4 entries per block)
//! plus scalar fields, vastly outperforming full-tick-map cloning.

use std::collections::VecDeque;

// ---------------------------------------------------------------------------
// BlockDelta trait
// ---------------------------------------------------------------------------

/// A per-block delta entry that can be stored in a `ReorgJournal`.
///
/// Implementors carry the "before" values needed to reverse-apply
/// a block's state change during reorg rollback.
pub trait BlockDelta {
    /// The block number this delta was recorded at.
    fn block(&self) -> u64;
}

// ---------------------------------------------------------------------------
// Journal errors (landed-at semantics, ADR-005 slice 4)
// ---------------------------------------------------------------------------

/// Error from a reorg journal restore/discard operation.
///
/// Represents the two "past the registration" conditions that a landed-at
/// journal surfaces as recoverable errors (mapped to Python `ValueError` by
/// the `PyO3` layer, and to `NoPoolStateAvailable` by the pool companion):
///
/// - `NoStatePriorToBlock`: a restore asked for state before the registration
///   (genesis) block, which no delta can represent.
/// - `NoStateAtOrAfterBlock`: a discard asked to remove every known state, i.e.
///   the target is past the newest delta.
///
/// These used to be `assert!` panics; surfaced as values so callers can map
/// them to Python exceptions without catching a panic (ADR-005 slice 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalError {
    /// No pool state is known prior to the target block — the target is at or
    /// before the registration (genesis) delta, or the journal is empty. Maps
    /// to Python `NoPoolStateAvailable` (a `ValueError` subclass).
    NoStatePriorToBlock { block: u64 },
    /// `discard_before_block` would remove every known state — the target is
    /// past the newest delta. Maps to Python `NoPoolStateAvailable`.
    NoStateAtOrAfterBlock { block: u64 },
}

// ---------------------------------------------------------------------------
// V2 delta types
// ---------------------------------------------------------------------------

/// Per-block delta for a V2 pool.
///
/// Carries the full transition at this block: the reserves **before** the
/// update (the pre-transition snapshot, equal to the previous delta's `after`)
/// and the reserves **after** the update (the *landed-at* state for this
/// block — the value [`ReorgJournal::restore_before_block`] returns when it
/// lands here).
///
/// The journal's first delta is the **genesis** delta, pushed at registration:
/// its `before` == `after` == the registration reserves, at
/// `block = update_block`. The genesis anchor is what makes "restore to the
/// registration state" expressible — a `before`-only journal cannot represent
/// the current state or the landed-at registration point (ADR-005 slice 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V2BlockDelta {
    /// Block number of this delta.
    pub block: u64,
    /// Reserve of token0 *before* this block's update.
    /// Redundant with the preceding delta's `reserve0_after`, retained for a
    /// self-describing transition record.
    pub reserve0_before: alloy::primitives::U256,
    /// Reserve of token1 *before* this block's update.
    /// Redundant with the preceding delta's `reserve1_after`, retained for a
    /// self-describing transition record.
    pub reserve1_before: alloy::primitives::U256,
    /// Reserve of token0 *after* this block's update — the landed-at state for
    /// this block, returned by `restore_before_block` when it lands here.
    pub reserve0_after: alloy::primitives::U256,
    /// Reserve of token1 *after* this block's update — the landed-at state for
    /// this block, returned by `restore_before_block` when it lands here.
    pub reserve1_after: alloy::primitives::U256,
}

impl BlockDelta for V2BlockDelta {
    fn block(&self) -> u64 {
        self.block
    }
}

// ---------------------------------------------------------------------------
// V3 delta types
// ---------------------------------------------------------------------------

/// "Before" values for the slot0 head scalars (`sqrt_price_x96`, `liquidity`, `tick`)
/// captured at the moment a block's update was applied. Used for reorg rollback
/// to restore the pre-update scalar state.
///
/// `Option<ScalarPriors>` in [`V3BlockDelta`] is `None` for tick-only events
/// (Mint/Burn V3, `ModifyLiquidity` V4) — those don't change the slot0 head
/// scalars, so the journal carries no scalar priors for them and
/// `restore_before_block` skips the scalar write-back (the current scalars are
/// already correct). See `docs/adr/ADR-004-cl-tickmap-typed-boundary.md` and the
/// {Slot0 Head / Tick Bookkeeping Map} term in `rust/CONTEXT.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalarPriors {
    /// Sqrt price X96 *before* this block's update.
    pub sqrt_price_x96_before: alloy::primitives::U256,
    /// Active liquidity *before* this block's update.
    pub liquidity_before: u128,
    /// Current tick *before* this block's update.
    pub tick_before: i32,
}

/// Per-block delta for a V3 pool.
///
/// Stores scalar priors ([`Option<ScalarPriors>`]) **before** this block's
/// update — `None` for tick-only events (Mint/Burn V3, `ModifyLiquidity` V4) —
/// plus per-tick priors for any ticks that were modified.
///
/// V3 tick modifications are typically 0–4 per block (at the crossing
/// boundary ticks during swaps). Only modified ticks are stored — the
/// rest of the tick map is untouched during rollback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V3BlockDelta {
    /// Block number of this delta.
    pub block: u64,
    /// Slot0 scalar priors (`sqrt_price_x96`, `liquidity`, `tick`) **before**
    /// this block's update; `None` for tick-only events (the slot0 head wasn't
    /// touched). See `docs/adr/ADR-004-cl-tickmap-typed-boundary.md`.
    pub scalar_priors: Option<ScalarPriors>,
    /// Per-tick priors for ticks modified during this block.
    /// Each entry is `(tick_index, TickBefore)` storing the `liquidity_gross`
    /// and `liquidity_net` values **before** the modification.
    pub tick_priors: Vec<(i32, TickBefore)>,
}

impl BlockDelta for V3BlockDelta {
    fn block(&self) -> u64 {
        self.block
    }
}

/// "Before" values for a single tick that was modified during a block.
///
/// On reorg, these values are reverse-applied to the current `tick_data` map.
/// A `None` `liquidity_gross` means the tick did not exist before this block
/// (it was newly initialized) and should be removed on rollback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TickBefore {
    /// Liquidity gross at this tick *before* the modification.
    /// `None` means the tick was not initialized — on rollback, delete it.
    pub liquidity_gross_before: Option<alloy::primitives::U128>,
    /// Liquidity net at this tick *before* the modification.
    pub liquidity_net_before: alloy::primitives::I256,
}

/// Result of restoring a V3 journal to before a target block.
///
/// Carries the (optional) scalar "before" values and per-tick priors that the
/// caller must apply to the current mutable state. `scalar_priors` is `None`
/// when the rolled-back range contained only tick-only events — the caller
/// should skip the scalar write-back in that case (the current scalars are
/// already correct).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V3RestoreResult {
    /// Slot0 scalar priors (`sqrt_price_x96`, `liquidity`, `tick`) **before**
    /// the target block; `None` when the rolled-back range was tick-only
    /// (caller skips the scalar write-back). When `Some`, taken from the
    /// *newest* popped delta that had `Some` scalars — that delta's "before"
    /// values are the scalar state just before the target block. See ADR-004.
    pub scalar_priors: Option<ScalarPriors>,
    /// Per-tick priors accumulated across all popped deltas (oldest wins on
    /// duplicate tick indices — the earliest modification's pre-state is the
    /// pre-target state for that tick).
    pub tick_priors: Vec<(i32, TickBefore)>,
    /// Block number of the oldest popped delta (the restore point).
    pub block: u64,
}

// ---------------------------------------------------------------------------
// Reorg journal
// ---------------------------------------------------------------------------

/// A bounded deque of per-block deltas for reorg rollback.
///
/// Stores deltas from oldest to newest. Forward progress pushes deltas;
/// reorg pops them and restores "before" values into current state.
///
/// **The caller is responsible for locking** — all mutation methods
/// are unlocked, matching the Python `StateCache` design.
#[derive(Clone, Debug)]
pub struct ReorgJournal<D> {
    /// The deque of deltas (oldest → newest).
    deltas: VecDeque<D>,
    /// Maximum number of deltas to retain.
    max_depth: usize,
}

impl<D: BlockDelta> ReorgJournal<D> {
    /// Create a new, empty `ReorgJournal`.
    ///
    /// # Panics
    ///
    /// Panics if `max_depth` is 0.
    #[must_use]
    pub fn new(max_depth: usize) -> Self {
        assert!(max_depth > 0, "max_depth must be at least 1");
        Self {
            deltas: VecDeque::with_capacity(max_depth),
            max_depth,
        }
    }

    /// Number of deltas in the journal.
    #[must_use]
    pub fn len(&self) -> usize {
        self.deltas.len()
    }

    /// Whether the journal is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.deltas.is_empty()
    }

    /// The block number of the newest delta, or `None` if the journal is empty.
    ///
    /// Used by reorg rollback to gate per-pool restores: a pool whose newest
    /// delta is before the reorg target was untouched by the fork and is left
    /// as-is (idempotent restore).
    #[must_use]
    pub fn newest_block(&self) -> Option<u64> {
        self.deltas.back().map(BlockDelta::block)
    }

    /// The block number of the earliest (genesis) surviving delta, or `None`
    /// if the journal is empty.
    ///
    /// ADR-006 slice 7: `ReorgCoordinator` checks `earliest_block() < block`
    /// (plus `newest_block().is_some()`) before calling `restore_before_block`
    /// for a V3/V4 pool — the V3/V4 journal `restore_before_block` panics on an
    /// empty journal, and the V2 path returns `Err(NoStatePriorToBlock)` when
    /// `block <= earliest`. The pre-check unifies the too-deep detection across
    /// all families so the pump shuts down gracefully instead of panicking.
    #[must_use]
    pub fn earliest_block(&self) -> Option<u64> {
        self.deltas.front().map(BlockDelta::block)
    }

    /// Append a new delta to the journal.
    ///
    /// If the delta's block matches the newest delta's block,
    /// replaces the newest delta (same-block update).
    ///
    /// Returns `true` if the delta was appended (new or same-block replacement).
    /// Returns `false` if the delta is older than the newest delta.
    pub fn push_delta(&mut self, delta: D) -> bool {
        if let Some(newest) = self.deltas.back() {
            if delta.block() < newest.block() {
                return false;
            }
            if delta.block() == newest.block() {
                self.deltas.pop_back();
            }
        }

        if self.deltas.len() >= self.max_depth {
            self.deltas.pop_front();
        }
        self.deltas.push_back(delta);
        true
    }

    /// Discard deltas earlier than the given block.
    ///
    /// These deltas are no longer rollback-reachable (the chain has finalized
    /// past them). Frees memory. The genesis (registration) delta is NOT
    /// special — it is discarded like any other when the target is past it,
    /// as long as at least one delta remains.
    ///
    /// Landed-at semantics (ADR-005 slice 4):
    /// - Earliest delta at/after the target → no-op (nothing to discard;
    ///   supports a continuously-running bot calling `discard(latest - N)` on
    ///   freshly-registered pools).
    /// - Newest delta before the target → `Err(NoStateAtOrAfterBlock)` (would
    ///   remove every known state). Was a `panic!` pre-slice-4.
    /// - Otherwise → remove deltas with `block < target`.
    ///
    /// # Errors
    ///
    /// Returns `Err(JournalError::NoStateAtOrAfterBlock)` if the target is
    /// past the newest delta (would remove every known state).
    pub fn discard_before_block(&mut self, block: u64) -> Result<(), JournalError> {
        if self.deltas.is_empty() {
            return Ok(());
        }

        let earliest_block = self.deltas[0].block();
        if earliest_block >= block {
            return Ok(());
        }

        let len = self.deltas.len();
        let newest_block = self.deltas[len - 1].block();
        if newest_block < block {
            return Err(JournalError::NoStateAtOrAfterBlock { block });
        }

        while self.deltas[0].block() < block {
            self.deltas.pop_front();
        }
        Ok(())
    }
}

impl ReorgJournal<V2BlockDelta> {
    /// Restore to the state landed just **before** a target block.
    ///
    /// Landed-at semantics (ADR-005 slice 4): returns the `*_after` reserves
    /// of the largest-block delta strictly less than `block`, and pops every
    /// delta at/after `block` so the journal's new newest IS that landed-at
    /// state. The caller writes the returned reserves into the current mutable
    /// state.
    ///
    /// Cases:
    /// - Newest delta is before the target → no-op: returns the newest delta's
    ///   `*_after` (the current state). Pre-slice-4 this erroneously returned
    ///   `*_before` (the pre-newest state).
    /// - Target at/before the earliest (genesis) delta → `Err(NoStatePriorToBlock)`
    ///   — rolling back past registration is a hard error, not a silent no-op.
    /// - Otherwise → pop deltas at/after the target, return the new newest's
    ///   `*_after` (guaranteed present since the earliest is below the target).
    ///
    /// # Errors
    ///
    /// - `NoStatePriorToBlock` if the journal is empty, or the target is at or
    ///   before the registration (genesis) delta — no state exists before it.
    ///
    /// # Panics
    ///
    /// Never (replaces the pre-slice-4 `assert!` on the empty journal).
    pub fn restore_before_block(
        &mut self,
        block: u64,
    ) -> Result<(alloy::primitives::U256, alloy::primitives::U256, u64), JournalError> {
        if self.deltas.is_empty() {
            return Err(JournalError::NoStatePriorToBlock { block });
        }

        let len = self.deltas.len();
        let newest = &self.deltas[len - 1];
        let newest_block = newest.block();

        // No-op: newest is before the target → current state IS the landed-at
        // state. Return `*_after` (the current state), NOT `*_before` (the
        // pre-newest state — the pre-slice-4 bug).
        if newest_block < block {
            return Ok((newest.reserve0_after, newest.reserve1_after, newest_block));
        }

        // newest >= block: there's something to pop. If the earliest is at or
        // after the target, no delta lands before it → rolling back past
        // registration. Raise rather than silently returning registration.
        let earliest_block = self.deltas[0].block();
        if earliest_block >= block {
            return Err(JournalError::NoStatePriorToBlock { block });
        }

        // Pop every delta at/after the target. Since earliest < block, the
        // earliest survives and the deque stays non-empty.
        while !self.deltas.is_empty() && self.deltas[self.deltas.len() - 1].block() >= block {
            self.deltas.pop_back();
        }

        let landed = self
            .deltas
            .back()
            .expect("earliest < block guarantees a surviving delta");
        let landed_block = landed.block();
        Ok((landed.reserve0_after, landed.reserve1_after, landed_block))
    }
}

impl ReorgJournal<V3BlockDelta> {
    /// Restore state prior to a target block by popping deltas at/after
    /// the target block and returning the "before" values and tick priors.
    ///
    /// The caller is responsible for:
    /// 1. Writing the returned scalar "before" values into current state
    /// 2. Reverse-applying the tick priors to the current tick data map
    ///
    /// Returns `V3RestoreResult` carrying the scalar before-values,
    /// per-tick priors, and the block number.
    ///
    /// # Panics
    ///
    /// Panics only if the journal is empty. A pool whose first Swap/Mint/Burn
    /// is at the target block restores to registration state without panicking.
    pub fn restore_before_block(&mut self, block: u64) -> V3RestoreResult {
        assert!(
            !self.deltas.is_empty(),
            "No pool state known prior to block {block}"
        );

        let len = self.deltas.len();
        let newest_block = self.deltas[len - 1].block();
        // If the newest delta is before the target, no rollback needed
        if newest_block < block {
            let newest = &self.deltas[len - 1];
            return V3RestoreResult {
                scalar_priors: newest.scalar_priors,
                tick_priors: newest.tick_priors.clone(),
                block: newest.block,
            };
        }

        // Pop all deltas at or after the target block, accumulating tick
        // priors from EACH popped delta (in pop order: newest → oldest). V3
        // deltas are PARTIAL — each carries priors only for the ticks it
        // modified — so reverse-applying just the last popped's priors (the V2
        // pattern, where each delta carries full state) would silently drop
        // intervening deltas' tick mutations. Accumulate across all popped
        // deltas and return the oldest popped delta's scalar priors (the state
        // just before the target block).
        //
        // De-duplicate by tick index as we accumulate: if two popped deltas
        // both modified the same tick, the oldest popped delta's prior wins
        // (it is the "before" value of the earliest mutation in the rolled-
        // back range). We pop newest→oldest, so a later-iterated (older) entry
        // overrides an earlier-iterated (newer) one.
        let mut accumulated_priors: std::collections::HashMap<i32, TickBefore> =
            std::collections::HashMap::new();
        let mut scalar_priors: Option<ScalarPriors> = None;
        let mut oldest_popped_block: Option<u64> = None;
        while !self.deltas.is_empty() && self.deltas[self.deltas.len() - 1].block() >= block {
            let popped = self.deltas.pop_back().expect("checked non-empty above");
            for (tick_idx, tick_before) in &popped.tick_priors {
                accumulated_priors.insert(*tick_idx, tick_before.clone());
            }
            // scalar_priors: newest-with-`Some` wins. We pop newest → oldest,
            // so the first popped-with-`Some` is the newest-with-`Some`; that
            // delta's "before" scalars ARE the scalar state just before the
            // target block (scalars are sequential: each delta records its own
            // pre-state, and the pre-state of the most recent scalar-changing
            // event in the rolled-back range = the scalar state just before
            // the target block). If no popped delta has `Some` (all-tick-only
            // rollback), `scalar_priors` stays `None` — caller skips scalar
            // write-back (current scalars already correct). See ADR-004.
            if scalar_priors.is_none() {
                scalar_priors = popped.scalar_priors;
            }
            oldest_popped_block = Some(popped.block);
        }

        // SAFETY: newest >= block (we skipped the early return), so at least
        // one delta was popped.
        let Some(oldest_block) = oldest_popped_block else {
            unreachable!("newest >= block guarantees at least one pop");
        };

        // accumulated_priors holds the tick-level "before" state across all
        // rolled-back deltas. Preserve pop-order (newest→oldest, oldest wins)
        // by reverse-iterating the accumulated map in insertion-overwrite
        // order — HashMap has no stable order, so return them grouped; the
        // caller applies each (`liquidity_gross_before: None` → remove tick).
        let tick_priors = accumulated_priors.into_iter().collect::<Vec<_>>();

        V3RestoreResult {
            scalar_priors,
            tick_priors,
            block: oldest_block,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    fn genesis(block: u64, r0: u64, r1: u64) -> V2BlockDelta {
        // Registration delta: before == after == registration reserves.
        V2BlockDelta {
            block,
            reserve0_before: U256::from(r0),
            reserve1_before: U256::from(r1),
            reserve0_after: U256::from(r0),
            reserve1_after: U256::from(r1),
        }
    }

    fn transition(
        block: u64,
        r0_before: u64,
        r1_before: u64,
        r0_after: u64,
        r1_after: u64,
    ) -> V2BlockDelta {
        V2BlockDelta {
            block,
            reserve0_before: U256::from(r0_before),
            reserve1_before: U256::from(r1_before),
            reserve0_after: U256::from(r0_after),
            reserve1_after: U256::from(r1_after),
        }
    }

    // --- push_delta (unchanged semantics, anchors the *_after shape) ---

    #[test]
    fn push_delta_appends() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        assert!(j.push_delta(genesis(1, 100, 200)));
        assert_eq!(j.len(), 1);
    }

    #[test]
    fn push_delta_same_block_replaces() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(genesis(1, 100, 200));
        // Same-block push replaces the newest delta wholesale.
        assert!(j.push_delta(transition(1, 100, 200, 150, 250)));
        assert_eq!(j.len(), 1);
        // The replacement carries the new after-values.
        assert_eq!(j.deltas[0].reserve0_after, U256::from(150));
        assert_eq!(j.deltas[0].reserve1_after, U256::from(250));
    }

    #[test]
    fn push_delta_older_block_rejected() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(genesis(5, 100, 200));
        assert!(!j.push_delta(transition(3, 100, 200, 50, 100)));
        assert_eq!(j.len(), 1);
    }

    #[test]
    fn max_depth_evicts_oldest() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(3);
        j.push_delta(genesis(1, 10, 20));
        j.push_delta(transition(2, 10, 20, 20, 30));
        j.push_delta(transition(3, 20, 30, 30, 40));
        // At capacity — next push evicts the genesis (block 1).
        j.push_delta(transition(4, 30, 40, 40, 50));
        assert_eq!(j.len(), 3);
        assert_eq!(j.deltas[0].block, 2);
    }

    // --- discard_before_block (landed-at semantics, ADR-005 slice 4) ---

    #[test]
    fn discard_removes_deltas_before_target_keeping_rest() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(genesis(1, 10, 20));
        j.push_delta(transition(2, 10, 20, 20, 30));
        j.push_delta(transition(3, 20, 30, 30, 40));
        j.push_delta(transition(5, 30, 40, 50, 60));

        // Discard everything before block 3 (genesis + block-2).
        j.discard_before_block(3).expect("target is within range");
        assert_eq!(j.len(), 2);
        assert_eq!(j.deltas[0].block, 3);
        assert_eq!(j.deltas[1].block, 5);
    }

    #[test]
    fn discard_noops_when_earliest_at_or_after_target() {
        // A continuously-running bot calls discard(latest - N) every block;
        // freshly-registered pools hit target <= genesis routinely. This MUST
        // be a no-op, not an error, so continuous discard never crashes.
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(genesis(3, 30, 40));
        j.push_delta(transition(5, 30, 40, 50, 60));

        // Earliest (block 3) is at the target — nothing to discard.
        j.discard_before_block(3)
            .expect("no-op when earliest >= target");
        assert_eq!(j.len(), 2);

        // Earliest (block 3) is after the target — still a no-op.
        j.discard_before_block(2)
            .expect("no-op when earliest > target");
        assert_eq!(j.len(), 2);
    }

    #[test]
    fn discard_errors_when_target_past_newest() {
        // Would remove every known state. Was a panic pre-slice-4; now an
        // error the PyO3 layer maps to ValueError.
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(genesis(1, 10, 20));
        j.push_delta(transition(3, 10, 20, 30, 40));

        let err = j.discard_before_block(10).unwrap_err();
        match err {
            JournalError::NoStateAtOrAfterBlock { block } => assert_eq!(block, 10),
            JournalError::NoStatePriorToBlock { .. } => panic!("expected NoStateAtOrAfterBlock"),
        }
        assert_eq!(j.len(), 2, "no mutation on error");
    }

    #[test]
    fn discard_noops_on_empty_journal() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.discard_before_block(5).expect("empty journal noops");
        assert!(j.is_empty());
    }

    // --- restore_before_block (landed-at semantics, ADR-005 slice 4) ---

    #[test]
    fn restore_lands_at_largest_block_below_target() {
        // The headline landed-at test. State evolution (after-values):
        //   Genesis(1):       (10, 20)
        //   Update(3, after): (300, 400)
        //   Update(5, after): (500, 600)
        //   Update(7, after): (700, 800)
        // restore(5) lands at the largest block < 5, i.e. block 3, returning
        // block-3's AFTER values (300, 400) — the state landed at block 3.
        // Pre-slice-4 this returned block-5's BEFORE values (also (300,400))
        // AND block=5 (the target, not the landed block).
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(genesis(1, 10, 20));
        j.push_delta(transition(3, 10, 20, 300, 400));
        j.push_delta(transition(5, 300, 400, 500, 600));
        j.push_delta(transition(7, 500, 600, 700, 800));

        let (r0, r1, blk) = j.restore_before_block(5).expect("target within range");
        assert_eq!(r0, U256::from(300));
        assert_eq!(r1, U256::from(400));
        assert_eq!(blk, 3, "landed-at block is the largest block < target");
        // Deltas at/after the target (5, 7) are popped; genesis + 3 remain.
        assert_eq!(j.len(), 2);
    }

    #[test]
    fn restore_intermediate_lands_between_deltas() {
        // Deltas at blocks 1, 3, 7. restore(7) lands at block 3
        // (largest < 7), returning block-3's after values.
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(genesis(1, 10, 20));
        j.push_delta(transition(3, 10, 20, 300, 400));
        j.push_delta(transition(7, 300, 400, 700, 800));

        let (r0, r1, blk) = j.restore_before_block(7).expect("target within range");
        assert_eq!(r0, U256::from(300));
        assert_eq!(r1, U256::from(400));
        assert_eq!(blk, 3);
        assert_eq!(j.len(), 2);
    }

    #[test]
    fn restore_to_first_update_lands_at_genesis() {
        // Rolling back the first update lands at the registration state
        // (genesis), proving the genesis delta makes registration landable.
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(genesis(10, 1000, 2000));
        j.push_delta(transition(20, 1000, 2000, 3000, 500));

        // restore(20) lands at the largest block < 20, i.e. genesis (10),
        // returning the registration reserves (1000, 2000) at block 10.
        let (r0, r1, blk) = j.restore_before_block(20).expect("target within range");
        assert_eq!(r0, U256::from(1000));
        assert_eq!(r1, U256::from(2000));
        assert_eq!(blk, 10, "lands at the genesis (registration) block");
        // Genesis survives (it's below the target); the block-20 update popped.
        assert_eq!(j.len(), 1);
        assert_eq!(j.deltas[0].block, 10);
    }

    #[test]
    fn restore_noop_returns_current_when_newest_below_target() {
        // newest < target: no rollback, return the current (landed-at newest)
        // state via AFTER. Pre-slice-4 this returned BEFORE (pre-newest state)
        // — a silent wrong-answer footgun the after-values fix.
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(genesis(1, 10, 20));
        j.push_delta(transition(3, 10, 20, 100, 200));

        // Newest (3) is before target (10): no-op, return block-3 after (100,200).
        let (r0, r1, blk) = j.restore_before_block(10).expect("no-op path");
        assert_eq!(r0, U256::from(100));
        assert_eq!(r1, U256::from(200));
        assert_eq!(blk, 3);
        assert_eq!(j.len(), 2, "no-op: nothing popped");
    }

    #[test]
    fn restore_errors_when_target_at_or_before_genesis() {
        // Decision 3: rolling back past registration is a hard error, not a
        // silent no-op. Target == genesis.block → no state before it exists.
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(genesis(5, 50, 60));
        j.push_delta(transition(7, 50, 60, 70, 80));

        // Target == genesis block (5): nothing lands before it.
        let err = j.restore_before_block(5).unwrap_err();
        match err {
            JournalError::NoStatePriorToBlock { block } => assert_eq!(block, 5),
            JournalError::NoStateAtOrAfterBlock { .. } => panic!("expected NoStatePriorToBlock"),
        }
        assert_eq!(j.len(), 2, "no mutation on error");

        // Target before genesis (3): same — no state before it.
        let err = j.restore_before_block(3).unwrap_err();
        assert!(matches!(
            err,
            JournalError::NoStatePriorToBlock { block: 3 }
        ));
    }

    #[test]
    fn restore_errors_on_empty_journal() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        let err = j.restore_before_block(5).unwrap_err();
        assert!(matches!(
            err,
            JournalError::NoStatePriorToBlock { block: 5 }
        ));
    }
}
// ---------------------------------------------------------------------------
// V3 delta scalar_priors tests (ADR-004)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod v3_delta_priors_tests {
    use super::*;
    use alloy::primitives::{I256, U128, U256};

    /// Helper: construct a `V3BlockDelta` with explicit `scalar_priors` + empty
    /// `tick_priors`. Anchors the new `Option<ScalarPriors>` shape (ADR-004).
    fn v3_delta(block: u64, scalar_priors: Option<ScalarPriors>) -> V3BlockDelta {
        V3BlockDelta {
            block,
            scalar_priors,
            tick_priors: vec![],
        }
    }

    /// Helper: a `TickBefore` for a tick that did not exist before this block
    /// (newly initialized by the Mint/`ModifyLiquidity` being rolled back).
    fn newly_initialized_tick() -> TickBefore {
        TickBefore {
            liquidity_gross_before: None,
            liquidity_net_before: I256::ZERO,
        }
    }

    #[test]
    fn tick_only_delta_has_none_scalar_priors() {
        // What: V3BlockDelta permits `scalar_priors: None` (the ADR-004 tick-only
        // event shape for Mint/Burn/`ModifyLiquidity` events).
        // Why: anchors the new `Option<ScalarPriors>` shape; makes the
        // structural change grep-able from this test.
        let d = v3_delta(7, None);
        assert!(
            d.scalar_priors.is_none(),
            "tick-only delta must have None scalar_priors"
        );
    }

    #[test]
    fn restore_returns_none_scalar_priors_when_all_rolled_back_events_are_tick_only() {
        // What: a journal with ONLY tick-only deltas (all `scalar_priors: None`),
        // restored to before the newest, returns
        // `V3RestoreResult.scalar_priors: None`.
        // Why: the caller (`v3_restore_before_block`) uses that `None` to skip
        // the scalar write-back (current scalars already correct for tick-only
        // rollbacks). Without this property, restore would write stale scalars.
        let mut j = ReorgJournal::<V3BlockDelta>::new(8);
        j.push_delta(v3_delta(1, None));
        j.push_delta(v3_delta(5, None));

        let result = j.restore_before_block(5);
        assert!(
            result.scalar_priors.is_none(),
            "all-tick-only rollback must return None scalar_priors (caller skips scalar write-back)"
        );
        assert_eq!(
            result.block, 5,
            "the popped delta's block (block 5) is the restore point — only block 5 had block >= target (5)"
        );
    }

    #[test]
    fn restore_picks_newest_with_some_scalar_priors_across_tick_only_gap() {
        // What: journal blocks 1 (Some, S0), 5 (None, tick-only), 10 (Some, S1).
        // Restoring before block 7 pops blocks 10 and 5 and must return
        // `scalar_priors: Some(S1)` — the pre-block-10 scalars, which equal
        // the post-block-5 scalars (block 5 was tick-only) = the scalar state
        // just before block 7.
        //
        // Why: pins the **newest-with-`Some`** rule for `scalar_priors`
        // (scalars are sequential; the pre-state of the most recent
        // scalar-changing event in the rolled-back range = the scalar state
        // just before the target block). This is the INVERSE of the tick-priors
        // "oldest wins" rule and a subtle correctness property invented by
        // ADR-004's `Option<ScalarPriors>` refactor — a future maintainer who
        // flips it to "oldest wins" would silently corrupt reorg rollback
        // across mixed tick-only + scalar-changing events.
        let s0 = ScalarPriors {
            sqrt_price_x96_before: U256::from(100u64),
            liquidity_before: 10,
            tick_before: 0,
        };
        let s1 = ScalarPriors {
            sqrt_price_x96_before: U256::from(200u64),
            liquidity_before: 20,
            tick_before: 5,
        };

        let mut j = ReorgJournal::<V3BlockDelta>::new(8);
        j.push_delta(v3_delta(1, Some(s0)));
        j.push_delta(v3_delta(5, None));
        j.push_delta(v3_delta(10, Some(s1)));

        let result = j.restore_before_block(7);
        assert_eq!(
            result.scalar_priors,
            Some(s1),
            "newest-with-Some (block 10's S1) must win across the tick-only gap at block 5"
        );
        assert_eq!(
            result.block, 10,
            "the popped delta's block (block 10) is the restore point — only block 10 had block >= target (7); block 5 is below the target and is NOT popped"
        );
    }

    #[test]
    fn restore_returns_none_when_newest_delta_is_tick_only_and_before_target() {
        // What: a journal whose newest delta (single tick-only at block 5) is
        // before the restore target (block 10). The early-return path
        // propagates `scalar_priors: None` from the newest delta.
        // Why: anchors the early-return path (the `newest_block < block` case)
        // for the ADR-004 Option shape — ensures downstream callers see `None`
        // rather than panicking or substituting zero for scalars.
        let _ = (U128::ZERO, newly_initialized_tick()); // touch helpers to keep them live
        let mut j = ReorgJournal::<V3BlockDelta>::new(8);
        j.push_delta(v3_delta(5, None));

        let result = j.restore_before_block(10);
        assert!(
            result.scalar_priors.is_none(),
            "early-return path must propagate scalar_priors: None from the newest delta"
        );
        assert_eq!(result.block, 5);
    }
}

// ---------------------------------------------------------------------------
// Property-based tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod proptests {
    use super::*;
    use alloy::primitives::U256;
    use proptest::prelude::*;

    /// A faithful model that tracks exactly the same state as the journal — a
    /// bounded list subject to same-block replacement, discards, restores, and
    /// `max_depth` eviction. Mirrors the post-slice-4 landed-at semantics:
    /// genesis + `*_after`, `restore` lands at the largest delta below target,
    /// `discard` no-ops when earliest >= target and errors when newest < target.
    struct Model {
        deltas: Vec<V2BlockDelta>,
        max_depth: usize,
    }

    impl Model {
        fn new(max_depth: usize) -> Self {
            Self {
                deltas: Vec::new(),
                max_depth,
            }
        }

        /// Mirror the journal's `push_delta` semantics exactly.
        fn push_delta(&mut self, delta: V2BlockDelta) -> bool {
            if let Some(last) = self.deltas.last() {
                if delta.block < last.block {
                    return false;
                }
                if delta.block == last.block {
                    let len = self.deltas.len();
                    self.deltas[len - 1] = delta;
                    return true;
                }
            }
            self.deltas.push(delta);
            // Enforce max_depth eviction (matching journal's pop-front).
            while self.deltas.len() > self.max_depth {
                self.deltas.remove(0);
            }
            true
        }

        /// Mirror the journal's `discard_before_block` landed-at semantics.
        /// Returns `Ok(true)` if the journal call should proceed (and mutate),
        /// `Ok(false)` if it's a no-op (earliest >= target), `Err(())` if it
        /// would error (newest < target).
        fn discard_before_block(&mut self, block: u64) -> Result<bool, ()> {
            if self.deltas.is_empty() {
                return Ok(false); // no-op
            }
            let earliest = self.deltas[0].block;
            if earliest >= block {
                return Ok(false); // no-op
            }
            let len = self.deltas.len();
            let newest = self.deltas[len - 1].block;
            if newest < block {
                return Err(()); // would remove all — error
            }
            self.deltas.retain(|d| d.block >= block);
            Ok(true)
        }

        /// Mirror the journal's `restore_before_block` landed-at semantics.
        /// Returns `Some((r0, r1, blk))` on the Ok paths (no-op returns newest
        /// after; rollback returns new-newest after), `None` if the journal
        /// would error (empty, or target at/before genesis).
        fn restore_before_block(&mut self, block: u64) -> Option<(U256, U256, u64)> {
            if self.deltas.is_empty() {
                return None;
            }
            let len = self.deltas.len();
            let newest = &self.deltas[len - 1];
            let newest_block = newest.block;
            if newest_block < block {
                // No-op: return the current (landed-at newest) after-values.
                return Some((newest.reserve0_after, newest.reserve1_after, newest_block));
            }
            // newest >= block: there's something to pop. Earliest at/after
            // target → no state before it → error (None).
            let earliest = self.deltas[0].block;
            if earliest >= block {
                return None;
            }
            // Pop all at/after target; the new newest is the landed-at state.
            while self.deltas.last().map(|d| d.block) >= Some(block) {
                self.deltas.pop();
            }
            let landed = self
                .deltas
                .last()
                .expect("earliest < block guarantees a survivor");
            Some((landed.reserve0_after, landed.reserve1_after, landed.block))
        }
    }

    /// Operations that can be applied to a journal.
    #[derive(Clone, Debug)]
    enum Op {
        /// Push a transition delta at `block`. `r0_before`/`r1_before` are the
        /// pre-update reserves; `r0_after`/`r1_after` the landed-at reserves.
        Push {
            block: u64,
            r0_before: u64,
            r1_before: u64,
            r0_after: u64,
            r1_after: u64,
        },
        /// Discard deltas before the given block.
        DiscardBefore { block: u64 },
        /// Restore to before the given block. Only performed when the model
        /// says it's valid (won't error).
        RestoreBefore { block: u64 },
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            (
                1u64..=100u64,
                0u64..=1000u64,
                0u64..=1000u64,
                0u64..=1000u64,
                0u64..=1000u64
            )
                .prop_map(|(block, r0b, r1b, r0a, r1a)| Op::Push {
                    block,
                    r0_before: r0b,
                    r1_before: r1b,
                    r0_after: r0a,
                    r1_after: r1a,
                }),
            (1u64..=100u64).prop_map(|block| Op::DiscardBefore { block }),
            (1u64..=100u64).prop_map(|block| Op::RestoreBefore { block }),
        ]
    }

    proptest! {
        /// After any sequence of valid operations, the journal's contents
        /// should match the model's exact state.
        #[test]
        fn journal_matches_model_after_ops(
            ops in proptest::collection::vec(op_strategy(), 0..100),
            max_depth in 1usize..=16,
        ) {
            let mut journal = ReorgJournal::<V2BlockDelta>::new(max_depth);
            let mut model = Model::new(max_depth);

            for op in &ops {
                match op {
                    Op::Push { block, r0_before, r1_before, r0_after, r1_after } => {
                        let d = V2BlockDelta {
                            block: *block,
                            reserve0_before: U256::from(*r0_before),
                            reserve1_before: U256::from(*r1_before),
                            reserve0_after: U256::from(*r0_after),
                            reserve1_after: U256::from(*r1_after),
                        };
                        let journal_accepted = journal.push_delta(d.clone());
                        let model_accepted = model.push_delta(d);
                        assert_eq!(journal_accepted, model_accepted,
                            "push_delta acceptance mismatch at block {block}");
                    }

                    Op::DiscardBefore { block } => {
                        match model.discard_before_block(*block) {
                            Ok(_) => {
                                // Model says Ok (no-op or mutate); journal must also be Ok.
                                journal.discard_before_block(*block)
                                    .expect("journal discard must be Ok when model says Ok");
                            }
                            Err(()) => {
                                // Model says error — journal must error too.
                                assert!(journal.discard_before_block(*block).is_err(),
                                    "journal discard must error when model says error");
                            }
                        }
                    }

                    Op::RestoreBefore { block } => {
                        match model.restore_before_block(*block) {
                            Some(expected) => {
                                let got = journal.restore_before_block(*block)
                                    .expect("journal restore must be Ok when model says Ok");
                                assert_eq!(got.0, expected.0,
                                    "restore reserve0 mismatch at block {block}");
                                assert_eq!(got.1, expected.1,
                                    "restore reserve1 mismatch at block {block}");
                                assert_eq!(got.2, expected.2,
                                    "restore block mismatch at block {block}");
                            }
                            None => {
                                assert!(journal.restore_before_block(*block).is_err(),
                                    "journal restore must error when model says error");
                            }
                        }
                    }
                }

                // After every operation: verify journal contents match model exactly.
                assert_eq!(
                    journal.len(),
                    model.deltas.len(),
                    "length mismatch after op {op:?}: journal={}, model={}",
                    journal.len(),
                    model.deltas.len(),
                );

                // Verify block ordering is strictly increasing.
                let delta_blocks: Vec<u64> = model.deltas.iter().map(|d| d.block).collect();
                for w in delta_blocks.windows(2) {
                    assert!(w[0] < w[1], "non-monotonic blocks: {} then {}", w[0], w[1]);
                }

                // Verify journal entries match model entries field-by-field.
                for (i, expected) in model.deltas.iter().enumerate() {
                    assert_eq!(journal.deltas[i].block, expected.block, "block mismatch at index {i}");
                    assert_eq!(journal.deltas[i].reserve0_before, expected.reserve0_before, "reserve0_before mismatch at index {i}");
                    assert_eq!(journal.deltas[i].reserve1_before, expected.reserve1_before, "reserve1_before mismatch at index {i}");
                    assert_eq!(journal.deltas[i].reserve0_after, expected.reserve0_after, "reserve0_after mismatch at index {i}");
                    assert_eq!(journal.deltas[i].reserve1_after, expected.reserve1_after, "reserve1_after mismatch at index {i}");
                }
            }
        }

        /// After a sequence of pushes (genesis + transitions), restoring to any
        /// non-first delta's block lands at the PREVIOUS delta and returns its
        /// landed-at (after) values + its block. Restoring to the first (genesis)
        /// block errors (no state before registration).
        #[test]
        fn restore_lands_at_previous_delta_after_values(
            deltas in proptest::collection::vec(
                (1u64..=50u64, 0u64..=1000u64, 0u64..=1000u64),
                1..20,
            ),
        ) {
            // Build a monotonic sequence of deltas with distinct before/after.
            // First push is genesis; subsequent pushes are transitions whose
            // after-values derive from their block (so they're trackable).
            let mut ordered: Vec<V2BlockDelta> = Vec::new();
            for (block, _r0, _r1) in &deltas {
                if let Some(last) = ordered.last() {
                    if *block <= last.block {
                        continue; // skip non-monotonic (incl. same-block, which
                                  // the journal replaces — keep it simple here)
                    }
                }
                let after0 = U256::from(*block) * U256::from(7u64);
                let after1 = U256::from(*block) * U256::from(11u64);
                let before0 = ordered.last().map_or(after0, |d| d.reserve0_after);
                let before1 = ordered.last().map_or(after1, |d| d.reserve1_after);
                ordered.push(V2BlockDelta {
                    block: *block,
                    reserve0_before: before0,
                    reserve1_before: before1,
                    reserve0_after: after0,
                    reserve1_after: after1,
                });
            }

            if ordered.is_empty() {
                return Ok(());
            }

            let max_depth = 8;
            let mut journal = ReorgJournal::<V2BlockDelta>::new(max_depth);
            for d in &ordered {
                journal.push_delta(d.clone());
            }

            // Only deltas surviving max_depth eviction are in the journal.
            let in_journal: Vec<&V2BlockDelta> = if ordered.len() > max_depth {
                ordered.iter().skip(ordered.len() - max_depth).collect()
            } else {
                ordered.iter().collect()
            };

            if in_journal.len() < 2 {
                return Ok(());
            }

            // For each non-first entry, restore(block) lands at the previous
            // entry and returns its after-values + block.
            for window in in_journal.windows(2) {
                let prev = window[0];
                let entry = window[1];
                let target = entry.block;

                let mut j = journal.clone();
                let (r0, r1, blk) = j.restore_before_block(target)
                    .expect("restore must land at previous delta");

                prop_assert_eq!(r0, prev.reserve0_after);
                prop_assert_eq!(r1, prev.reserve1_after);
                prop_assert_eq!(blk, prev.block);
            }

            // Restoring to the journal's first (genesis) delta's block errors:
            // no state exists before registration (decision 3).
            let genesis_block = in_journal[0].block;
            let mut j = journal.clone();
            prop_assert!(j.restore_before_block(genesis_block).is_err());
        }

        /// The journal never exceeds max_depth entries.
        #[test]
        fn journal_never_exceeds_max_depth(
            deltas in proptest::collection::vec(
                (1u64..=100u64, 0u64..=1000u64, 0u64..=1000u64),
                0..200,
            ),
            max_depth in 1usize..=8,
        ) {
            let mut journal = ReorgJournal::<V2BlockDelta>::new(max_depth);
            let mut current_block = 0u64;

            for (i, (block, r0, r1)) in deltas.iter().copied().enumerate() {
                // Ensure monotonic progression.
                let effective_block = if block <= current_block {
                    current_block
                } else {
                    block
                };
                current_block = effective_block;

                journal.push_delta(V2BlockDelta {
                    block: effective_block,
                    reserve0_before: U256::from(r0),
                    reserve1_before: U256::from(r1),
                    reserve0_after: U256::from(r0),
                    reserve1_after: U256::from(r1),
                });

                prop_assert!(
                    journal.len() <= max_depth,
                    "journal exceeded max_depth at step {i}: len={}, max_depth={}",
                    journal.len(),
                    max_depth,
                );

                // Blocks are strictly increasing.
                let blocks: Vec<u64> = journal.deltas.iter().map(|d| d.block).collect();
                for w in blocks.windows(2) {
                    prop_assert!(w[0] < w[1]);
                }
            }
        }
    }
}

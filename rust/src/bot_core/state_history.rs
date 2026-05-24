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
// V2 delta types
// ---------------------------------------------------------------------------

/// Per-block delta for a V2 pool.
///
/// Stores the reserve values **before** the update at this block.
/// On reorg, these "before" values are restored into the current state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V2BlockDelta {
    /// Block number of this delta.
    pub block: u64,
    /// Reserve of token0 *before* this block's update.
    pub reserve0_before: alloy::primitives::U256,
    /// Reserve of token1 *before* this block's update.
    pub reserve1_before: alloy::primitives::U256,
}

impl BlockDelta for V2BlockDelta {
    fn block(&self) -> u64 {
        self.block
    }
}

// ---------------------------------------------------------------------------
// V3 delta types
// ---------------------------------------------------------------------------

/// Per-block delta for a V3 pool.
///
/// Stores scalar fields (`sqrt_price`, `liquidity`, `tick`) **before** this
/// block's update, plus per-tick priors for any ticks that were modified.
///
/// V3 tick modifications are typically 0–4 per block (at the crossing
/// boundary ticks during swaps). Only modified ticks are stored — the
/// rest of the tick map is untouched during rollback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V3BlockDelta {
    /// Block number of this delta.
    pub block: u64,
    /// Sqrt price X96 *before* this block's update.
    pub sqrt_price_x96_before: alloy::primitives::U256,
    /// Active liquidity *before* this block's update.
    pub liquidity_before: alloy::primitives::U128,
    /// Current tick *before* this block's update.
    pub tick_before: i32,
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
/// Carries the scalar "before" values and per-tick priors that the
/// caller must apply to the current mutable state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V3RestoreResult {
    /// Sqrt price X96 *before* the target block.
    pub sqrt_price_x96_before: alloy::primitives::U256,
    /// Active liquidity *before* the target block.
    pub liquidity_before: alloy::primitives::U128,
    /// Current tick *before* the target block.
    pub tick_before: i32,
    /// Per-tick priors from the last popped delta.
    pub tick_priors: Vec<(i32, TickBefore)>,
    /// Block number of the restored delta.
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
    /// These deltas are no longer rollback-reachable (the chain has
    /// finalized past them). Frees memory.
    ///
    /// # Panics
    ///
    /// Panics if all deltas are before the target block
    /// (no delta is known at or after the block).
    pub fn discard_before_block(&mut self, block: u64) {
        if self.deltas.is_empty() {
            return;
        }

        let earliest_block = self.deltas[0].block();
        if earliest_block >= block {
            return;
        }

        let len = self.deltas.len();
        let newest_block = self.deltas[len - 1].block();
        assert!(
            newest_block >= block,
            "No pool state known at or after block {block}"
        );

        while self.deltas[0].block() < block {
            self.deltas.pop_front();
        }
    }
}

impl ReorgJournal<V2BlockDelta> {
    /// Restore state prior to a target block by popping deltas at/after
    /// the target block and returning the "before" values from the last
    /// popped delta.
    ///
    /// The caller is responsible for applying the returned "before" values
    /// to the current mutable state.
    ///
    /// Returns `(reserve0_before, reserve1_before, block)` from the last
    /// popped delta, which represents the state just before the target block.
    ///
    /// # Panics
    ///
    /// Panics if no delta exists before the target block.
    pub fn restore_before_block(
        &mut self,
        block: u64,
    ) -> (alloy::primitives::U256, alloy::primitives::U256, u64) {
        assert!(
            !self.deltas.is_empty(),
            "No pool state known prior to block {block}"
        );

        let len = self.deltas.len();
        let newest_block = self.deltas[len - 1].block();
        // If the newest delta is before the target, no rollback needed
        if newest_block < block {
            let newest = &self.deltas[len - 1];
            return (newest.reserve0_before, newest.reserve1_before, newest.block);
        }

        let earliest_block = self.deltas[0].block();
        assert!(
            earliest_block < block,
            "No pool state known prior to block {block}"
        );

        // Pop all deltas at or after the target block.
        // The last one popped gives us the "before" values to restore.
        let mut last_popped: Option<V2BlockDelta> = None;
        while self.deltas[self.deltas.len() - 1].block() >= block {
            last_popped = self.deltas.pop_back();
        }

        // SAFETY: We verified earliest < block and newest >= block, so at
        // least one delta was popped. `Option<T>` is always `Some` here.
        let Some(popped) = last_popped else {
            unreachable!("earliest < block <= newest guarantees at least one pop");
        };
        // Now we need the "before" values from *just before* the target block.
        // The deltas remaining in the deque represent blocks before the target.
        // The last popped delta's "before" values ARE the state just before
        // the target block (since they were captured before that delta's update).
        (popped.reserve0_before, popped.reserve1_before, popped.block)
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
    /// Panics if no delta exists before the target block.
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
                sqrt_price_x96_before: newest.sqrt_price_x96_before,
                liquidity_before: newest.liquidity_before,
                tick_before: newest.tick_before,
                tick_priors: newest.tick_priors.clone(),
                block: newest.block,
            };
        }

        let earliest_block = self.deltas[0].block();
        assert!(
            earliest_block < block,
            "No pool state known prior to block {block}"
        );

        // Pop all deltas at or after the target block.
        let mut last_popped: Option<V3BlockDelta> = None;
        while self.deltas[self.deltas.len() - 1].block() >= block {
            last_popped = self.deltas.pop_back();
        }

        // SAFETY: We verified earliest < block and newest >= block, so at
        // least one delta was popped.
        let Some(popped) = last_popped else {
            unreachable!("earliest < block <= newest guarantees at least one pop");
        };

        V3RestoreResult {
            sqrt_price_x96_before: popped.sqrt_price_x96_before,
            liquidity_before: popped.liquidity_before,
            tick_before: popped.tick_before,
            tick_priors: popped.tick_priors,
            block: popped.block,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    fn delta(block: u64, r0_before: u64, r1_before: u64) -> V2BlockDelta {
        V2BlockDelta {
            block,
            reserve0_before: U256::from(r0_before),
            reserve1_before: U256::from(r1_before),
        }
    }

    #[test]
    fn push_delta_appends() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        assert!(j.push_delta(delta(1, 100, 200)));
        assert_eq!(j.len(), 1);
    }

    #[test]
    fn push_delta_same_block_replaces() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(delta(1, 100, 200));
        assert!(j.push_delta(delta(1, 150, 250)));
        assert_eq!(j.len(), 1);
        // The replacement should have the newer "before" values
        assert_eq!(j.deltas[0].reserve0_before, U256::from(150));
    }

    #[test]
    fn push_delta_older_block_rejected() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(delta(5, 100, 200));
        assert!(!j.push_delta(delta(3, 50, 100)));
        assert_eq!(j.len(), 1);
    }

    #[test]
    fn max_depth_evicts_oldest() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(3);
        j.push_delta(delta(1, 10, 20));
        j.push_delta(delta(2, 20, 30));
        j.push_delta(delta(3, 30, 40));
        // At capacity — next push evicts block 1
        j.push_delta(delta(4, 40, 50));
        assert_eq!(j.len(), 3);
        // Oldest is now block 2
        assert_eq!(j.deltas[0].block, 2);
    }

    #[test]
    fn discard_before_block() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(delta(1, 10, 20));
        j.push_delta(delta(2, 20, 30));
        j.push_delta(delta(3, 30, 40));
        j.push_delta(delta(5, 50, 60));

        j.discard_before_block(3);
        assert_eq!(j.len(), 2);
        assert_eq!(j.deltas[0].block, 3);
        assert_eq!(j.deltas[1].block, 5);
    }

    #[test]
    fn discard_before_block_early_return_if_earliest_at_target() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(delta(3, 30, 40));
        j.push_delta(delta(5, 50, 60));

        // Earliest is already at target — no-op
        j.discard_before_block(3);
        assert_eq!(j.len(), 2);
    }

    #[test]
    #[should_panic(expected = "No pool state known at or after block 10")]
    fn discard_before_block_panics_if_all_before() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(delta(1, 10, 20));
        j.push_delta(delta(3, 30, 40));

        j.discard_before_block(10);
    }

    #[test]
    fn restore_before_block_pops_and_returns_priors() {
        // Simulate pool state evolution:
        //   Block 1: reserves were (10, 20) before update → now (100, 200)
        //   Block 3: reserves were (100, 200) before update → now (300, 400)
        //   Block 5: reserves were (300, 400) before update → now (500, 600)
        //   Block 7: reserves were (500, 600) before update → now (700, 800)
        //
        // Restoring before block 5 should pop deltas for 5 and 7,
        // then return "before" values from the block-5 delta: (300, 400)
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(delta(1, 10, 20));
        j.push_delta(delta(3, 100, 200));
        j.push_delta(delta(5, 300, 400));
        j.push_delta(delta(7, 500, 600));

        let (r0, r1, blk) = j.restore_before_block(5);
        assert_eq!(r0, U256::from(300));
        assert_eq!(r1, U256::from(400));
        assert_eq!(blk, 5);
        // Only deltas for blocks 1 and 3 remain
        assert_eq!(j.len(), 2);
    }

    #[test]
    fn restore_before_block_returns_priors_if_newest_before_target() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(delta(1, 10, 20));
        j.push_delta(delta(3, 100, 200));

        // Newest delta is at block 3, which is before target 10
        let (r0, r1, blk) = j.restore_before_block(10);
        assert_eq!(r0, U256::from(100));
        assert_eq!(r1, U256::from(200));
        assert_eq!(blk, 3);
    }

    #[test]
    #[should_panic(expected = "No pool state known prior to block 1")]
    fn restore_before_block_panics_if_earliest_at_target() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(delta(1, 10, 20));

        // The delta at block 1 gives "before" state, but it IS at the target
        j.restore_before_block(1);
    }

    #[test]
    #[should_panic(expected = "No pool state known prior to block 5")]
    fn restore_before_block_panics_if_empty() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.restore_before_block(5);
    }

    #[test]
    fn restore_before_block_intermediate() {
        // Restore to between existing deltas
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(delta(1, 10, 20));
        j.push_delta(delta(3, 100, 200));
        j.push_delta(delta(7, 300, 400));

        // Restore before block 7: pops block 7 delta, returns its "before" values
        let (r0, r1, blk) = j.restore_before_block(7);
        assert_eq!(r0, U256::from(300));
        assert_eq!(r1, U256::from(400));
        assert_eq!(blk, 7);
        assert_eq!(j.len(), 2);
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

    /// A faithful model that tracks exactly the same state as the journal —
    /// a bounded list subject to same-block replacement, discards, restores,
    /// and max_depth eviction. No "windowed" approximation.
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

        /// Mirror the journal's push_delta semantics exactly.
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
            // Enforce max_depth eviction (matching journal's pop-front)
            while self.deltas.len() > self.max_depth {
                self.deltas.remove(0);
            }
            true
        }

        /// Mirror the journal's discard_before_block semantics exactly.
        /// Returns false if the operation would be skipped (empty, would-panic).
        fn discard_before_block(&mut self, block: u64) -> bool {
            if self.deltas.is_empty() {
                return false;
            }
            let earliest = self.deltas[0].block;
            if earliest >= block {
                // All deltas are at/after target — no-op
                return true;
            }
            let len = self.deltas.len();
            let newest = self.deltas[len - 1].block;
            if newest < block {
                // Would panic — all deltas are before target
                return false;
            }
            self.deltas.retain(|d| d.block >= block);
            true
        }

        /// Mirror the journal's restore_before_block semantics exactly.
        /// Returns None if the operation would panic (empty or no prior state).
        fn restore_before_block(&mut self, block: u64) -> Option<(U256, U256, u64)> {
            if self.deltas.is_empty() {
                return None;
            }
            let len = self.deltas.len();
            let newest = self.deltas[len - 1].block;
            if newest < block {
                // No rollback needed — return newest's before values
                let d = &self.deltas[len - 1];
                return Some((d.reserve0_before, d.reserve1_before, d.block));
            }
            let earliest = self.deltas[0].block;
            if earliest >= block {
                // No prior state — would panic
                return None;
            }
            // Pop all at/after target, return last popped's before values
            let mut last_popped: Option<V2BlockDelta> = None;
            while self.deltas.last().map(|d| d.block) >= Some(block) {
                last_popped = self.deltas.pop();
            }
            last_popped.map(|p| (p.reserve0_before, p.reserve1_before, p.block))
        }
    }

    /// Operations that can be applied to a journal.
    #[derive(Clone, Debug)]
    enum Op {
        /// Push a delta at the given block with the given "before" values.
        Push { block: u64, reserve0: u64, reserve1: u64 },
        /// Discard deltas before the given block.
        DiscardBefore { block: u64 },
        /// Restore to before the given block. Only performed when the model
        /// says it's valid (won't panic).
        RestoreBefore { block: u64 },
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            (1u64..=100u64, 0u64..=1000u64, 0u64..=1000u64)
                .prop_map(|(block, r0, r1)| Op::Push { block, reserve0: r0, reserve1: r1 }),
            (1u64..=100u64)
                .prop_map(|block| Op::DiscardBefore { block }),
            (1u64..=100u64)
                .prop_map(|block| Op::RestoreBefore { block }),
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
                    Op::Push { block, reserve0, reserve1 } => {
                        let d = V2BlockDelta {
                            block: *block,
                            reserve0_before: U256::from(*reserve0),
                            reserve1_before: U256::from(*reserve1),
                        };
                        let journal_accepted = journal.push_delta(d.clone());
                        let model_accepted = model.push_delta(d);
                        assert_eq!(journal_accepted, model_accepted,
                            "push_delta acceptance mismatch at block {block}");
                    }

                    Op::DiscardBefore { block } => {
                        let model_ok = model.discard_before_block(*block);
                        if !model_ok {
                            // Would panic or is no-op — skip journal call
                            continue;
                        }
                        journal.discard_before_block(*block);
                    }

                    Op::RestoreBefore { block } => {
                        let model_result = model.restore_before_block(*block);
                        if model_result.is_none() {
                            // Would panic — skip
                            continue;
                        }
                        let journal_result = journal.restore_before_block(*block);

                        let (mr0, mr1, mblk) = model_result.unwrap();
                        assert_eq!(journal_result.0, mr0,
                            "restore reserve0 mismatch at block {block}");
                        assert_eq!(journal_result.1, mr1,
                            "restore reserve1 mismatch at block {block}");
                        assert_eq!(journal_result.2, mblk,
                            "restore block mismatch at block {block}");
                    }
                }

                // After every operation: verify journal contents match model exactly
                assert_eq!(
                    journal.len(),
                    model.deltas.len(),
                    "length mismatch after op {op:?}: journal={}, model={}",
                    journal.len(),
                    model.deltas.len(),
                );

                // Verify block ordering is strictly increasing
                let delta_blocks: Vec<u64> = model.deltas.iter().map(|d| d.block).collect();
                for w in delta_blocks.windows(2) {
                    assert!(w[0] < w[1], "non-monotonic blocks: {} then {}", w[0], w[1]);
                }

                // Verify journal entries match model entries field-by-field
                for (i, expected) in model.deltas.iter().enumerate() {
                    assert_eq!(
                        journal.deltas[i].block,
                        expected.block,
                        "block mismatch at index {i}"
                    );
                    assert_eq!(
                        journal.deltas[i].reserve0_before,
                        expected.reserve0_before,
                        "reserve0 mismatch at index {i}"
                    );
                    assert_eq!(
                        journal.deltas[i].reserve1_before,
                        expected.reserve1_before,
                        "reserve1 mismatch at index {i}"
                    );
                }
            }
        }

        /// After a sequence of pushes, restoring to any block in the journal
        /// always returns the correct "before" values that were originally
        /// pushed for that block.
        #[test]
        fn restore_returns_correct_priors_for_any_block(
            deltas in proptest::collection::vec(
                (1u64..=50u64, 0u64..=1000u64, 0u64..=1000u64),
                1..20,
            ),
        ) {
            // Build a monotonic sequence of deltas (skip gaps, handle
            // same-block by overwriting in a BTreeMap)
            let mut ordered: Vec<V2BlockDelta> = Vec::new();
            for (block, r0, r1) in &deltas {
                if let Some(last) = ordered.last() {
                    if *block < last.block {
                        continue; // Skip out-of-order
                    }
                    if *block == last.block {
                        // Same-block replace
                        let len = ordered.len();
                        ordered[len - 1].reserve0_before = U256::from(*r0);
                        ordered[len - 1].reserve1_before = U256::from(*r1);
                        continue;
                    }
                }
                ordered.push(V2BlockDelta {
                    block: *block,
                    reserve0_before: U256::from(*r0),
                    reserve1_before: U256::from(*r1),
                });
            }

            if ordered.is_empty() {
                return Ok(());
            }

            let max_depth = 8;
            let mut journal = ReorgJournal::<V2BlockDelta>::new(max_depth);

            // Push all deltas into the journal
            for d in &ordered {
                journal.push_delta(d.clone());
            }

            // For each block in the journal, verify restore returns
            // the expected "before" values
            let expected: Vec<&V2BlockDelta> = if ordered.len() > max_depth {
                ordered.iter().skip(ordered.len() - max_depth).collect()
            } else {
                ordered.iter().collect()
            };

            // Pick a target block that's in the journal (not the first entry)
            if expected.len() < 2 {
                return Ok(());
            }

            // Restore before each non-first entry's block
            for target_idx in 1..expected.len() {
                let target_block = expected[target_idx].block;

                // Clone the journal so each restore is independent
                let mut j = journal.clone();
                let (r0, r1, blk) = j.restore_before_block(target_block);

                prop_assert_eq!(r0, expected[target_idx].reserve0_before);
                prop_assert_eq!(r1, expected[target_idx].reserve1_before);
                prop_assert_eq!(blk, target_block);
            }
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
                // Ensure monotonic progression
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
                });

                prop_assert!(
                    journal.len() <= max_depth,
                    "journal exceeded max_depth at step {i}: len={}, max_depth={}",
                    journal.len(),
                    max_depth,
                );

                // Blocks are strictly increasing
                let blocks: Vec<u64> = journal.deltas.iter().map(|d| d.block).collect();
                for w in blocks.windows(2) {
                    prop_assert!(w[0] < w[1]);
                }
            }
        }
    }
}

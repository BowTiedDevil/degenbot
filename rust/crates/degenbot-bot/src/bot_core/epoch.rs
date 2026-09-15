//! `Epoch` + `BlockContext` — THE single block coordinate (epic MROOY7, task T6IYKY).
//!
//! Before this module, "what block is this work about" had several answers
//! living in parallel field soups: the solve anchor ([`super::solve_anchor`]),
//! the cold-start `results_block` seed, the ADR-021 verifier anchor carried in
//! the Published-edge verifier anchor, the pump FSM's recovery anchor (`record_backfill`),
//! the engine's `last_solved_block`, the coordinator's `last_drained_block`,
//! and the bare `(block, BlockMetadata)` parameter pairs threaded through the
//! drain seam. Each answer was locally correct; none shared a coordinate
//! system, so a reorg that rewound work had no way to say "every context
//! minted before the rewind is void".
//!
//! ## `Epoch`
//!
//! One per-block coordinate: `block` (the block) + `seq` (a monotone
//! generation counter that bumps on every REWIND, i.e. every reorg episode).
//! Two epochs with different `seq` are different timelines: a context minted
//! in generation N describes work against a chain view that generation N+1
//! has partially unwound. `ensure_current` FAILS such contexts rather than
//! silently applying them — the codebase's fail-loud posture, not a
//! best-effort stale-tolerance scheme.
//!
//! `Ord` is deliberately the generation-aware cursor order `(seq, block)`:
//! a monotone cursor (the engine's `results_block`, the FSM's recovery
//! anchor) must never appear to regress when a rewind lowers the block while
//! bumping the generation. It is NOT a spatial "later block" comparison —
//! `Epoch::at(501) < Epoch::at(500).rewind()` is TRUE and correct (the
//! rewind is the newer world).
//!
//! ## `BlockContext`
//!
//! The work-item description: the epoch plus the block's execution metadata
//! (`BlockMetadata`). It carries the coordinate only — no `BotState`
//! representation attaches here; the `StateView` mechanism is a separate
//! data-plane decision (epic spike). `StageMachine::context_for` mints contexts
//! at the pump's decision points; the stage-machine task  makes the
//! stages consume them.

use crate::bot_core::BlockMetadata;

/// The one block coordinate: `block` + rewind generation `seq`.
///
/// Construct with [`Epoch::at`] (generation 0 — no rewind has happened) and
/// bump with [`Epoch::rewind`] / [`Epoch::rewind_to`] when a reorg rewinds
/// work. See the module docs for the ordering rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Epoch {
    /// Field order is load-bearing for the derived `Ord`: the cursor order
    /// is `(seq, block)` — a rewind sorts ABOVE any earlier-generation epoch
    /// (see the module docs), never below it.
    seq: u64,
    block: u64,
}

impl Epoch {
    /// The epoch of `block` in the initial (no rewind) generation.
    #[must_use]
    pub const fn at(block: u64) -> Self {
        Self { block, seq: 0 }
    }

    /// `block` in an explicit generation (what `StageMachine::context_for` mints:
    /// the decision block stamped with the FSM's current generation).
    #[must_use]
    pub const fn with_generation(block: u64, seq: u64) -> Self {
        // Literal field order matches the (load-bearing) definition order.
        Self { seq, block }
    }

    /// The block coordinate.
    #[must_use]
    pub const fn block(self) -> u64 {
        self.block
    }

    /// The rewind generation (bumps on every reorg episode).
    #[must_use]
    pub const fn seq(self) -> u64 {
        self.seq
    }

    /// A rewind AT the same block coordinate: the generation bumps, the
    /// block stays. Call this when a reorg episode invalidates previously
    /// minted contexts without moving the cursor.
    #[must_use]
    pub const fn rewind(self) -> Self {
        Self {
            seq: self.seq + 1,
            block: self.block,
        }
    }

    /// A rewind to `block` (possibly lower than the current cursor): the
    /// generation bumps AND the block moves — the reorg landed the head
    /// somewhere new. Contexts from the previous generation are stale.
    #[must_use]
    pub const fn rewind_to(self, block: u64) -> Self {
        Self {
            seq: self.seq + 1,
            block,
        }
    }

    /// Has the generation moved past `self` (or `self` raced ahead of the
    /// current cursor)? A stale epoch is rejected, never silently applied.
    #[must_use]
    pub const fn is_stale(self, current: Self) -> bool {
        self.seq != current.seq || self.block > current.block
    }

    /// Fail-fast validation: `self` must still be current against
    /// `current`. Returns `self` on success; a [`StaleEpoch`] error
    /// otherwise (the caller aborts the work — mirrors the codebase's
    /// fail-loud posture, never a silent apply of a rewound past).
    ///
    /// # Errors
    /// [`StaleEpoch`] when a rewind (generation bump) or a forward race
    /// invalidated the held context.
    pub const fn ensure_current(self, current: Self) -> Result<Self, StaleEpoch> {
        if self.is_stale(current) {
            Err(StaleEpoch {
                held: self,
                current,
            })
        } else {
            Ok(self)
        }
    }
}

impl From<u64> for Epoch {
    /// The initial-generation epoch of `block`. Lets anchor call sites that
    /// have no generation in hand (`record_backfill(through: u64)`-style
    /// callers, fixture writes) land on the unified type unchanged.
    fn from(block: u64) -> Self {
        Self::at(block)
    }
}

/// A context is described in a different generation than the current one
/// (a rewind happened after it was minted) or races the cursor ahead of it.
/// The holder must abandon the work; applying the stale context would replay
/// a rewound chain view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StaleEpoch {
    /// The epoch the caller held.
    pub held: Epoch,
    /// The epoch that is current now.
    pub current: Epoch,
}

impl std::fmt::Display for StaleEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "stale epoch: held block {} @ gen {} is not current (now block {} @ gen {})",
            self.held.block(),
            self.held.seq(),
            self.current.block(),
            self.current.seq()
        )
    }
}

impl std::error::Error for StaleEpoch {}

/// Bare-u64 comparison for the pinned FSM field reads: an `Epoch` equals a
/// raw block number exactly when its BLOCK coordinate matches, regardless of
/// generation. Generation-blind by design — this exists so the `StageMachine`'s
/// pinned recovery-anchor tests read `fsm.recovery_anchor == 104` on an
/// `Epoch` field; generation-sensitive code must use [`Epoch::ensure_current`].
impl PartialEq<u64> for Epoch {
    fn eq(&self, other: &u64) -> bool {
        self.block == *other
    }
}

/// The work-item description the pump hands downstream: the block epoch
/// (with its rewind generation) plus the block's execution metadata.
///
/// Carries the coordinate ONLY — no `BotState` representation is baked in
/// (the `StateView` decision attaches later, in the data-plane task). Every
/// anchor that used to travel as a loose field (the Published-edge anchor
/// verifier anchor, the `(block, metadata)` drain/finalize/solve pairs)
/// travels as ONE context now: "what block is this work about" has exactly
/// one answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockContext {
    epoch: Epoch,
    metadata: BlockMetadata,
}

impl BlockContext {
    /// Mint a context for `epoch` carrying `metadata`.
    #[must_use]
    pub fn new(epoch: impl Into<Epoch>, metadata: BlockMetadata) -> Self {
        Self {
            epoch: epoch.into(),
            metadata,
        }
    }

    /// The (block, generation) coordinate.
    #[must_use]
    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// The block coordinate.
    #[must_use]
    pub const fn block(&self) -> u64 {
        self.epoch.block()
    }

    /// The block metadata (fees/gas/timestamp — the VTWCIG batch contract).
    #[must_use]
    pub const fn metadata(&self) -> &BlockMetadata {
        &self.metadata
    }

    /// Fail-fast validation of the context against the current epoch —
    /// a context minted before a rewind ([`StaleEpoch`]) must be rejected,
    /// never silently applied. See [`Epoch::ensure_current`].
    ///
    /// # Errors
    /// [`StaleEpoch`] when the context no longer describes the current
    /// generation or races ahead of the current cursor.
    pub const fn ensure_current(&self, current: Epoch) -> Result<Epoch, StaleEpoch> {
        self.epoch.ensure_current(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- coordinate construction ---

    #[test]
    fn at_is_the_initial_generation() {
        let e = Epoch::at(42);
        assert_eq!(e.block(), 42);
        assert_eq!(e.seq(), 0);
    }

    #[test]
    fn from_u64_is_at() {
        assert_eq!(Epoch::from(7), Epoch::at(7));
    }

    // --- rewind semantics: seq bumps on Rewind, block may move ---

    #[test]
    fn rewind_bumps_seq_keeps_block() {
        let e = Epoch::at(500).rewind();
        assert_eq!(e.block(), 500);
        assert_eq!(e.seq(), 1);
        // A second rewind bumps again.
        assert_eq!(e.rewind().seq(), 2);
    }

    #[test]
    fn rewind_to_bumps_seq_and_moves_block() {
        // A reorg rewinds the cursor: block moves down, generation moves up.
        let e = Epoch::at(501).rewind_to(500);
        assert_eq!(e.block(), 500);
        assert_eq!(e.seq(), 1);
    }

    // --- generation-aware cursor order (NOT a spatial block order) ---

    #[test]
    fn forward_advance_within_a_generation_is_greater() {
        // Same generation: the later block is the greater cursor.
        assert!(Epoch::at(500) < Epoch::at(501));
    }

    #[test]
    fn a_rewind_is_never_a_cursor_regression() {
        // The dominant failure this order prevents: a monotone cursor updated
        // with `max` would keep the pre-rewind (higher-block) epoch and
        // silently drop the new generation. The rewind must sort ABOVE it.
        assert!(Epoch::at(500).rewind() > Epoch::at(501));
        assert!(Epoch::at(500).rewind() > Epoch::at(499)); // and lower blocks
    }

    // --- staleness: fail fast, never silently apply ---

    #[test]
    #[expect(clippy::unwrap_used)] // the negative-path probe panics on Ok
    fn stale_after_a_rewind_is_detected_and_rejected() {
        let current = Epoch::at(500).rewind(); // a reorg landed
        let held = Epoch::at(500); // a context minted BEFORE the rewind
        assert!(held.is_stale(current));
        let err = held.ensure_current(current).unwrap_err();
        assert_eq!(err.held, held);
        assert_eq!(err.current, current);
        // Same coordinate, different generation — the whole point.
        assert_ne!(err.held, current);
    }

    #[test]
    fn stale_across_coordinates_is_detected() {
        // Forward race: the held context describes work at a block the
        // current cursor has not reached.
        assert!(Epoch::at(600).is_stale(Epoch::at(500)));
    }

    #[test]
    fn current_context_passes_ensure_current() {
        let current = Epoch::at(500);
        assert_eq!(current.ensure_current(current), Ok(current));
    }

    // --- BlockContext: the coordinate + metadata carrier ---

    fn meta(ts: u64) -> BlockMetadata {
        BlockMetadata {
            timestamp: ts,
            base_fee_per_gas: Some(ts),
            gas_used: 1,
            gas_limit: 2,
        }
    }

    #[test]
    fn context_carries_epoch_and_metadata() {
        let ctx = BlockContext::new(Epoch::at(101), meta(101_000));
        assert_eq!(ctx.epoch(), Epoch::at(101));
        assert_eq!(ctx.block(), 101);
        assert_eq!(ctx.metadata().timestamp, 101_000);
    }

    #[test]
    fn context_from_a_bare_block_seeds_generation_zero() {
        let ctx = BlockContext::new(43u64, meta(1));
        assert_eq!(ctx.epoch(), Epoch::at(43));
    }

    #[test]
    fn context_staleness_fails_fast() {
        let ctx = BlockContext::new(Epoch::at(101), meta(1));
        let current = Epoch::at(101).rewind_to(100); // a reorg landed at 100
        assert!(ctx.ensure_current(current).is_err());
    }
}

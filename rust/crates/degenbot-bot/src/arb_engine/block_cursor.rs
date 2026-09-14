//! The engine block cursor (ergo task 6XB6NJ) — the ONE owner of the
//! engine-side block-coordinate residue of the arb engine.
//!
//! Completes ADR-041 §3.5's anchor-soup fold on the engine side (no ADR
//! change — the §3.5 data-plane answer, one block coordinate carried on the
//! epoch context, already landed with epic MROOY7/PLRGIN; what remained was
//! exactly this engine-side residue). The four block coordinates that used
//! to live as free-floating engine fields move into one [`BlockCursor`],
//! and every advance rule lives in one place.
//!
//! ## Folded sites (each was a hand-written advance)
//!
//! - engine struct (`arb_engine/mod.rs`): the four field defs + their
//!   init (`results_block`, `last_processed_block`, `last_solved_block`,
//!   `has_logs_this_block`) → one `cursor: BlockCursor`.
//! - the retired `event_routing.rs::finalize_block` (now
//!   `engine_stages.rs::StageHandlers::on_finalize`): the hand-written
//!   guarded four-field transition → [`BlockCursor::finalize`].
//! - the retired `event_routing.rs::solve_dirty` / `process_updates` tails
//!   (now `engine_stages.rs::run_engine_cycle` / the `mod.rs` `process_updates`
//!   test harness) and `lifecycle.rs::set_last_processed_block`: unconditional
//!   `last_processed_block = Some(n)` writes →
//!   [`BlockCursor::advance_processed`].
//! - `solve_cycle.rs` solve stamps (the empty-fanout early return, the
//!   detached enqueue tail, the in-cycle tail) and
//!   `lifecycle.rs::solve_all_paths`: unconditional `results_block = n`
//!   writes → [`BlockCursor::advance_solved`].
//! - `lifecycle.rs::set_solve_anchor`: the resume-time cold-start
//!   only-if-zero seed → a plain [`BlockCursor::advance_solved`] (monotone
//!   subsumes the only-if-zero semantics).
//! - `lifecycle.rs::set_last_solved_block` (ADR-006 D4 mid-flight inherit)
//!   → monotone [`BlockCursor::advance_solved_boundary`]
//!   (behavior-preserving: the engine starts at 0 and production stamps
//!   are non-decreasing).
//! - `delivery_policy.rs::diff_and_send`: re-derived
//!   `anchored = results_block != 0` → consumes
//!   [`BlockCursor::is_anchored`] handed over by the engine.
//!
//! ## The monotone discipline
//!
//! Every advance is monotone-max: a stamp can move a coordinate forward,
//! never backward. The resume-time cold-start anchor seed is a plain
//! advance — "never regress" by construction, including a late detached
//! stamp.
//!
//! ## The ONE intentional behavior strengthening
//!
//! A late/stale stamp can no longer regress `results_block` (the 6XB6NJ
//! review's Q6 decision; pinned by
//! `tests::late_solve_stamp_cannot_regress_results_anchor`): the solve
//! stamps were unconditional writes, so a stale cycle (a lagging drain
//! entry, a re-fired boundary, a detached straggler) could drag the publish
//! anchor backwards and re-emit a batch at a regressed `solve_block`.
//! [`BlockCursor::advance_solved`] clamps at the max. Everything else here
//! is behavior-preserving.
/// The engine block cursor — one owner of the engine-side block-coordinate
/// residue (6XB6NJ; the module docs carry the fold map, the monotone
/// discipline, and the one intentional strengthening).
//
// The field names are the settled 6XB6NJ interface — the pre-cursor engine
// field names carried over verbatim (CONTEXT.md "Engine block cursor"); the
// shared `_block` postfix is the point, not an accident.
#[expect(clippy::struct_field_names)]
#[derive(Debug, Default)]
pub(crate) struct BlockCursor {
    /// The solve-anchor stamp (KNEUQX): the block the MOST RECENT solve
    /// cycle ran anchored on — the solve-anchor resolution (request block
    /// floored by the pool-state head, see
    /// `crate::bot_core::solve_anchor`), published as every batch's
    /// `solve_block` and stamped on the cycle span as `cycle.solve_block`.
    /// `0` = cold start, no solve yet (the delivery policy's anchored gate
    /// defers candidates while 0).
    results_block: u64,
    /// Last block number processed by the engine (a solve cycle or a
    /// finalize boundary). `None` means no block has been processed yet.
    /// Used by the pump to determine the backfill boundary on startup, and
    /// by the sim-diag snapshot as the engine's last-applied block (O5SKZ6).
    last_processed_block: Option<u64>,
    /// The last block this engine's `finalize_block` guard advanced past
    /// (i.e. the last block whose boundary transition completed). Owned by
    /// the engine since ergo task LEZJAS (the pump's `&mut` out-params
    /// retired). Initialize to `0` so the first header/tombstone
    /// `finalize_block(block > 0)` fires; survives a mid-flight engine
    /// joining the pump (ADR-006 D4). The one-shot finalize guard lives on
    /// this.
    last_solved_block: u64,
    /// Whether any forward log applied since the last [`Self::finalize`].
    /// `true` after `record_logs()` (the pump's forward-log path), cleared
    /// by `finalize`. Owned by the engine since LEZJAS.
    has_logs_this_block: bool,
}
impl BlockCursor {
    /// The guarded combined finalize transition — the engine's
    /// `finalize_block` boundary. The `block > last_solved_block` guard is
    /// load-bearing (PWPPAZ T1): it makes the boundary advance one-shot
    /// even when the tombstone re-fires for an already-finalized block.
    ///
    /// On fire: `last_solved_block = block`, `has_logs_this_block = false`,
    /// `last_processed_block = Some(block)`, and the anchor advances
    /// monotonically (`results_block.max(block)`). Returns whether it
    /// fired so the engine keeps the terminal publish inside the same
    /// guard.
    ///
    /// The processed-cursor write is monotone too: the finalize is
    /// tombstone-dispatched and runs while the successor block's log burst
    /// is being applied (the PWPPAZ interleave), so a solve at N+1 can
    /// already have advanced `last_processed_block` past N and a late
    /// finalize(N) must not drag it back.
    pub(crate) fn finalize(&mut self, block: u64) -> bool {
        if block <= self.last_solved_block {
            return false;
        }
        self.last_solved_block = block;
        self.has_logs_this_block = false;
        self.last_processed_block = Some(self.last_processed_block.unwrap_or(0).max(block));
        // Anchor is monotonic: never regress a real solve's anchor.
        self.results_block = self.results_block.max(block);
        true
    }
    /// Monotone processed-cursor advance (`last_processed_block`): stores
    /// `Some(max(prev, block))` — a late/stale entry can never move the
    /// processed boundary backwards.
    pub(crate) fn advance_processed(&mut self, block: u64) {
        let prev = self.last_processed_block.unwrap_or(0);
        self.last_processed_block = Some(prev.max(block));
    }
    /// The solve-anchor stamp (`results_block`): monotone-max, so a
    /// late/stale stamp can never regress a real solve's anchor (the ONE
    /// intentional 6XB6NJ strengthening — see the module docs).
    ///
    /// This also subsumes the resume-time cold-start seed
    /// (`set_solve_anchor`'s old only-if-zero guard): seeding the settled
    /// resume boundary is a plain advance, and "never regress" holds by
    /// construction — once a real solve has established a (possibly
    /// higher) anchor, no seed can pull it back.
    pub(crate) fn advance_solved(&mut self, block: u64) {
        self.results_block = self.results_block.max(block);
    }
    /// Monotone solved-boundary advance (`last_solved_block`) — the
    /// ADR-006 D4 mid-flight-inherit stamp (a late engine inherits the
    /// pump's current solved block on join). Behavior-preserving: the
    /// engine starts at 0 and the production stamps are non-decreasing.
    pub(crate) fn advance_solved_boundary(&mut self, block: u64) {
        self.last_solved_block = self.last_solved_block.max(block);
    }
    /// Record that at least one forward log applied this block (cleared by
    /// the next [`Self::finalize`]).
    pub(crate) fn record_logs(&mut self) {
        self.has_logs_this_block = true;
    }
    /// Whether a real solve has anchored `results_block` (`!= 0`): the
    /// delivery policy's publish gate (a 0 anchor would sim at block 0 —
    /// the 0x841820 code-less panic) and the sim-diag solve-block fallback
    /// condition. The cursor owns the derivation; no reader re-derives it
    /// from the raw integer.
    #[must_use]
    pub(crate) const fn is_anchored(&self) -> bool {
        self.results_block != 0
    }
    /// The solve-anchor stamp (KNEUQX span tagging + batch `solve_block`).
    #[must_use]
    pub(crate) const fn results_block(&self) -> u64 {
        self.results_block
    }
    /// The processed boundary (`None` = nothing processed yet).
    #[must_use]
    pub(crate) const fn last_processed_block(&self) -> Option<u64> {
        self.last_processed_block
    }
    /// The solved boundary (the finalize one-shot guard's left side).
    #[must_use]
    pub(crate) const fn last_solved_block(&self) -> u64 {
        self.last_solved_block
    }
    /// Whether any forward log applied since the last [`Self::finalize`].
    #[must_use]
    pub(crate) const fn has_logs_this_block(&self) -> bool {
        self.has_logs_this_block
    }
    /// White-box test seam (6XB6NJ): byte-identical replacement for the
    /// tests' direct `engine.results_block = n` writes (the field moved
    /// into the cursor). Plain assignment on purpose — tests force an
    /// arbitrary anchor state; production advances stay monotone.
    #[cfg(test)]
    pub(crate) fn set_results_block_for_test(&mut self, block: u64) {
        self.results_block = block;
    }
}

//! The solve anchor — the block solve / verify / sim run against.
//!
//! One pure rule with one owner (ADR-008 D2): the anchor is the request block
//! floored by the **pool-state head** (the max `update_block` across all
//! pools), and a hop whose price clock runs **ahead of** the anchor is
//! *future* and never legitimate. The pump's `drain_decision`, the engine's
//! solve re-anchor, and the ADR-021 publish verifier all derive from this —
//! never from a raw lagging block. `CONTEXT.md`: "Solve anchor".
//!
//! ## Why the head floor (the backfill-ahead desync class)
//!
//! - **MQIZ5M / IIA (+1-wei)** — solving below the state head consumes pools
//!   whose state reflects a later block; the +1-wei / IIA mispricing class
//!   followed. The `max(pool_state_head)` floor is load-bearing, not optional.
//! - **BO5FBS (QMSTSV)** — the pump promotes `active_block` once before
//!   `on_drain`, so on the pump path the engine's re-anchor is a defensive
//!   no-op; it stays load-bearing for callers that bypass the pump (tests
//!   driving `solve_dirty` directly).
//! - **B2 / 0x99ac8c** — during a backfill/drain desync the pools sit AHEAD
//!   of the lagging drain clock. A hop at the head (`update_block > raw
//!   block`) is *LIVE* state, not a future price; aborting on it killed a
//!   capturable opportunity (the 0x99ac8c false-abort). The future test must
//!   therefore run against the anchor, never the raw block.
//!
//! ## Why the future rule is strict (the future-price class)
//!
//! - **U6RNHH T1 / TVJF6K T2** — even +1 ahead is never legitimate: a
//!   future-price solve reports a misleading downstream IIA. The guard is a
//!   belt-and-suspenders invariant assertion, not a normal-path rejection:
//!   after the head floor, `update_block > anchor` is impossible by definition
//!   (the head is the max across all pools).
//! - **Equal is NOT future** — `update_block == anchor` is a mid-block capture
//!   (an early swap of the anchor block, e.g. 0xE0554a @ 25658682) and is
//!   legitimate.

use crate::bot_core::BotState;

/// The block solve / verify / sim run against: the request block floored by
/// the pool-state head (see the module docs for the desync / IIA history),
/// with the future-hop rule bound to the resolved anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolveAnchor {
    block: u64,
}

impl SolveAnchor {
    /// Resolve the anchor from a request block and a pure state head
    /// (`max(base, state_head)` — the backfill-ahead desync rule, module
    /// docs). The `PumpFSM` path: the FSM is I/O-free and receives the head
    /// as data.
    #[must_use]
    pub fn for_head(base: u64, state_head: u64) -> Self {
        Self {
            block: base.max(state_head),
        }
    }

    /// `for_head` against the live state head (engine / verifier paths).
    #[must_use]
    pub fn resolve(base: u64, core: &BotState) -> Self {
        Self::for_head(base, core.pool_state_head())
    }

    /// The anchored block.
    #[must_use]
    pub fn block(self) -> u64 {
        self.block
    }

    /// The future-hop rule (module docs): strictly ahead of the anchor is
    /// future and never legitimate; a hop *at* the anchor is a mid-block
    /// capture and is NOT future.
    #[must_use]
    pub fn is_future(self, update_block: u64) -> bool {
        update_block > self.block
    }
}

#[expect(clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::SolveAnchor;
    use crate::bot_core::{BotState, RegisterV2PoolParams};
    use alloy::primitives::aliases::U112;
    use alloy::primitives::Address;

    /// A single-pool state whose pool-state head tracks `block`.
    fn core_with_head(block: u64) -> BotState {
        let mut core = BotState::new();
        let addr = Address::from([0x11u8; 20]);
        core.register_v2_pool(&RegisterV2PoolParams {
            address: addr,
            token0: Address::from([0xbb; 20]),
            token1: Address::from([0xcc; 20]),
            ..Default::default()
        })
        .expect("setup: V2 pool registration");
        core.apply_v2_sync(addr, U112::from(1_000), U112::from(1_000), block)
            .expect("setup: forward Sync at block");
        assert_eq!(
            core.pool_state_head(),
            block,
            "setup: head tracks the sync block"
        );
        core
    }

    // --- the rule: anchor = max(base, state head) ---

    #[test]
    fn anchor_floors_lagging_base_at_state_head() {
        assert_eq!(SolveAnchor::for_head(499, 500).block(), 500);
    }

    #[test]
    fn anchor_keeps_base_when_base_leads() {
        assert_eq!(SolveAnchor::for_head(600, 500).block(), 600);
    }

    #[test]
    fn anchor_equal_is_identity() {
        assert_eq!(SolveAnchor::for_head(500, 500).block(), 500);
    }

    #[test]
    fn anchor_empty_state_head_is_zero() {
        assert_eq!(SolveAnchor::for_head(42, 0).block(), 42);
    }

    // --- future rule: strictly ahead; at-anchor is a mid-block capture ---

    #[test]
    fn ahead_is_future_never_legitimate() {
        // Any magnitude ahead is future (U6RNHH T1 / TVJF6K T2) — ported from
        // the former `hop_is_future` / `is_future_price` suites.
        assert!(SolveAnchor::for_head(100, 100).is_future(101));
        assert!(SolveAnchor::for_head(25_677_777, 0).is_future(25_677_789));
    }

    #[test]
    fn at_anchor_is_mid_block_capture_not_future() {
        assert!(!SolveAnchor::for_head(100, 100).is_future(100));
        assert!(!SolveAnchor::for_head(0, 100).is_future(100));
    }

    #[test]
    fn behind_anchor_is_normal_latency_not_future() {
        assert!(!SolveAnchor::for_head(100, 100).is_future(99));
        assert!(!SolveAnchor::for_head(100, 100).is_future(0));
    }

    // --- behavioral pins (engine head-ahead; B2 live state) ---

    #[test]
    fn pin_a_engine_reanchored_solve_blocks_at_head() {
        // A drain `block_number` lagging the state head still solves at the
        // head — mirrors the engine's
        // `future_state_path_is_reanchored_to_pool_state_head`.
        let core = core_with_head(500);
        assert_eq!(SolveAnchor::resolve(499, &core).block(), 500);
    }

    #[test]
    fn pin_b2_hop_at_head_is_live_state_not_future() {
        // B2: during a backfill/drain desync a hop at the head (past the raw
        // lagging block) is LIVE state — capturable, not a future abort.
        let core = core_with_head(500);
        let anchor = SolveAnchor::resolve(499, &core);
        assert_eq!(anchor.block(), 500);
        assert!(
            !anchor.is_future(500),
            "a hop at the anchor is a mid-block capture"
        );
        assert!(
            anchor.is_future(501),
            "a hop past the anchor is genuinely future"
        );
    }
}

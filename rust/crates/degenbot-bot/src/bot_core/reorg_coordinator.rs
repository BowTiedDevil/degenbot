//! `ReorgCoordinator` — optimistic per-event journal rollback (ADR-006 D4,
//! slice 7; ADR-006 Q8-Q9 resolved).
//!
//! Replaces the bulk `engine.handle_reorg(block)` ADR-003 Option α path
//! (which restored **every** pool + cleared all resolved hops + dirtied every
//! path pool on a single `removed: true` log). The new path is per-event,
//! per-pool, mirroring slice 5a's `dispatch_log` shape:
//!
//! - Pump, on a WS log with `removed: true`, calls
//!   `bot.dispatch_reorg_log(&log)` (NOT `sink.on_reorg(block)` — reorg no
//!   longer flows through the `DrainSink`; the engine solves at the next
//!   drain tick via the normal subscriber→`on_drain` path).
//! - `ReorgCoordinator::dispatch_reorg_log` decodes the log to resolve the
//!   target `pool_id` (V2/V3 via `pool_id_by_address`, V4 via
//!   `v4_pool_id_by_key`), RESTORES that pool's state via
//!   `BotState::restore_before_block` (V2 path returns `Result`; V3/V4 via
//!   pre-checked `*_restore_before_block`), releases the write guard, then
//!   `dispatcher.notify(pool_id)` — the **same** notify path slice 4 uses for
//!   forward updates. No separate `on_reorg` method on subscribers.
//!
//! **Optimistic + order-insensitive.** The WS `removed: true` replay ordering
//! is unspecified by any standard. `ReorgJournal::restore_before_block` is
//! idempotent + order-insensitive (verified `state_history.rs:340-375`):
//! newest `< B` → no-op; newest `≥ B` → while-loop pops every
//! `back().block() >= B`. Chronological / reverse-chronological / out-of-order
//! all land at the same state.
//!
//! **`max_depth` graceful shutdown (NOT panic).** A `removed: true` event
//! whose block is at or below the journal's earliest surviving delta →
//! `Err(NoStatePriorToBlock)` (V2) or pre-check failure (V3/V4 — the V3/V4
//! journal `restore_before_block` panics on an empty journal, so this module
//! pre-checks `has_state_prior_to` to avoid the panic and return `Err`
//! uniformly). `dispatch_reorg_log` returns `Result<(), ReorgError>`;
//! the pump sees the error, logs it, sets its shutdown flag, and exits the
//! loop — graceful shutdown, not a task panic. This is **stricter** than the
//! slice-6 bulk path (which silently skipped too-deep pools); silently
//! leaving stale state after a too-deep reorg is a correctness bug.
//!
//! **Lock order (D2):** `ReorgCoordinator` holds `Arc<Bot>`; restore takes
//! the `BotState` write guard (under the `Bot`'s shared `RwLock`), applies +
//! writes the landed-at state, RELEASES, then `notify` (subscribers take only
//! their own lock — same as `dispatch_log`). Never nests a subscriber lock
//! under the state write.

use alloy::rpc::types::Log;
use std::sync::Arc;

use crate::bot_core::Bot;

/// A too-deep reorg — the removed event's block is at or below the journal's
/// earliest surviving delta. There is no known state before it, so the bot
/// cannot safely continue; the pump shuts down gracefully.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ReorgError {
    /// `pool_id`'s journal has no state at or before `block`.
    NoStatePriorToBlock { pool_id: u64, block: u64 },
}

impl ReorgCoordinator {
    /// Decode `log` (reusing the dispatch decoders), resolve its `pool_id`
    /// WITHOUT applying it forward, restore that pool's state to just before
    /// `log`'s block, then notify subscribers.
    ///
    /// Returns `Ok` on success (including the no-op case where the pool's
    /// newest delta is already before the target — idempotent). Returns
    /// `Err(NoStatePriorToBlock)` if the reorg target is at or below the
    /// journal's earliest delta (the pump shuts down gracefully).
    ///
    /// The removed event's CONTENT is unused — only its block number + pool
    /// identity (the journal's stored "before" values are the source of truth).
    #[allow(dead_code)]
    pub fn dispatch_reorg_log(&self, log: &Log) -> Result<(), ReorgError> {
        let bot = &*self.bot;
        // Decode the log to identify the target pool. Reuses the same decoder
        // registry as forward `dispatch_log` — a removed log carries the same
        // topics/data; we just don't `apply` it forward.
        let Some(decoded) = bot.try_decode_log(log) else {
            // Unrecognized / no decoder matches → no-op (same as forward
            // dispatch's unrecognized-path). A reorg for an unknown event
            // family is harmless.
            return Ok(());
        };
        let block = decoded.block_number();
        let Some(pool_id) = bot.resolve_pool_id(&decoded) else {
            // Pool not registered → no-op (parallel to forward dispatch's
            // `apply returns None` path). Subscribers can't be notified about
            // a pool that isn't in the registry.
            return Ok(());
        };

        // Restore under the write guard. Pre-check `has_state_prior_to` for
        // V3/V4 (whose journal `restore_before_block` panics on empty); the
        // V2 path's `Result` would catch too-deep natively, but the pre-check
        // unifies V2/V3/V4 onto one `Err` path before any mutation.
        if !bot.has_state_prior_to(pool_id, block) {
            return Err(ReorgError::NoStatePriorToBlock { pool_id, block });
        }
        bot.restore_pool_before_block(pool_id, block);
        // `restore_pool_before_block` released the write guard internally;
        // notify subscribers (engine dirties + re-solves at the next drain
        // tick; no separate reorg path in the engine).
        bot.notify_pool_state_updated(pool_id);
        Ok(())
    }
}

/// Thin struct wrapping `Arc<Bot>` — gives the ADR-006-named helper a real
/// home + its own test seam. Holds no own state; restoration delegates to
/// `BotState`, decode+notify to `LogDispatcher`.
#[allow(dead_code)]
pub struct ReorgCoordinator {
    bot: Arc<Bot>,
}

#[allow(dead_code)]
impl ReorgCoordinator {
    #[must_use]
    pub fn new(bot: Arc<Bot>) -> Self {
        Self { bot }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot_core::log_dispatcher::PoolStateSubscriber;
    use crate::bot_core::state_history::{ReorgJournal, V2BlockDelta};
    use crate::bot_core::{Bot, RegisterV2PoolParams};
    use alloy::primitives::{Address, Bytes, I256, U128, U256};
    use alloy::rpc::types::Log;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    // Journal-level invariants the coordinator relies on. These are
    // restatements of existing `state_history.rs` semantics at the coordinator
    // layer — the coordinator delegates to `restore_before_block`, so the
    // invariants MUST hold here for any arrival ordering (ADR-006 Q8-Q9).

    fn v2_genesis(block: u64, r0: u64, r1: u64) -> V2BlockDelta {
        V2BlockDelta {
            block,
            reserve0_before: U256::from(r0),
            reserve1_before: U256::from(r1),
            reserve0_after: U256::from(r0),
            reserve1_after: U256::from(r1),
        }
    }
    fn v2_transition(
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

    /// ADR-006 slice 7: duplicate removed event (same pool, same block) is a
    /// no-op — `restore_before_block` is idempotent. The second call finds the
    /// newest delta already below the target and returns the current state
    /// without mutating.
    #[test]
    fn restore_before_block_is_idempotent_on_duplicate() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(v2_genesis(5, 1_000, 2_000));
        j.push_delta(v2_transition(7, 1_000, 2_000, 1_100, 2_200));

        // First restore at block 7 → pops the block-7 delta, lands at genesis.
        let first = j.restore_before_block(7).expect("first restore ok");
        assert_eq!(first, (U256::from(1_000), U256::from(2_000), 5));

        // Second restore at block 7 → newest delta (5) < target (7) → no-op.
        let second = j.restore_before_block(7).expect("second restore ok");
        assert_eq!(second, first, "second restore is a no-op");
    }

    /// ADR-006 slice 7: chronological rollback (D then C, D > C) unwinds two
    /// single blocks, landing at pre-C. Order-insensitive by construction.
    #[test]
    fn chronological_rollback_lands_at_pre_c() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(v2_genesis(5, 1_000, 2_000));
        j.push_delta(v2_transition(6, 1_000, 2_000, 1_500, 2_500));
        j.push_delta(v2_transition(8, 1_500, 2_500, 1_900, 2_900));

        let (r0, _, _) = j.restore_before_block(8).expect("restore before 8");
        assert_eq!(r0, U256::from(1_500), "land at block-6 after-state");
        let (r0, _, _) = j.restore_before_block(6).expect("restore before 6");
        assert_eq!(r0, U256::from(1_000), "land at genesis (pre-C)");
    }

    /// ADR-006 slice 7: reverse-chronological rollback (C then D) — first call
    /// pops both at-or-after-block-6 deltas, second is a no-op; lands at pre-C.
    #[test]
    fn reverse_chronological_rollback_lands_at_pre_c() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(v2_genesis(5, 1_000, 2_000));
        j.push_delta(v2_transition(6, 1_000, 2_000, 1_500, 2_500));
        j.push_delta(v2_transition(8, 1_500, 2_500, 1_900, 2_900));

        // Reverse: target C=6 first — pops both block-6 and block-8 deltas.
        let (r0, _, _) = j.restore_before_block(6).expect("restore before 6");
        assert_eq!(r0, U256::from(1_000), "land at genesis");

        // Then target D=8 — newest is now 5 < 8 → no-op, stays at genesis.
        let (r0_2, _, _) = j.restore_before_block(8).expect("restore before 8 (no-op)");
        assert_eq!(r0_2, U256::from(1_000), "no-op: still at genesis");
    }

    /// ADR-006 slice 7: a too-deep reorg (target at/below the journal's
    /// genesis) returns `Err(NoStatePriorToBlock)` — the coordinator propagates
    /// this so the pump can shut down gracefully (NOT panic).
    #[test]
    fn too_deep_reorg_returns_no_state_prior_error() {
        let mut j = ReorgJournal::<V2BlockDelta>::new(8);
        j.push_delta(v2_genesis(5, 1_000, 2_000));
        let err = j.restore_before_block(5).unwrap_err();
        assert!(matches!(
            err,
            crate::bot_core::state_history::JournalError::NoStatePriorToBlock { block: 5 }
        ));
        let err = j.restore_before_block(3).unwrap_err();
        assert!(matches!(
            err,
            crate::bot_core::state_history::JournalError::NoStatePriorToBlock { block: 3 }
        ));
    }

    // --- Coordinator end-to-end: decode → restore → notify (the real slice-7
    //     wiring). A registered V2 pool + a real Sync log + a removed-flag
    //     variant. Verifies the per-event restore + the subscriber notify.

    /// The V2 `Sync` topic, duplicated here from `optimizers::v2_sync_decoder`
    /// to avoid a cross-module test import (the constant is `pub` there, but
    /// the test module is in `bot_core` and importing from `optimizers`
    /// would reverse the layering edge the slice avoids).
    const V2_SYNC_TOPIC: alloy::primitives::B256 = alloy::primitives::B256::new([
        0x1c, 0x41, 0x1e, 0x9a, 0x96, 0xe0, 0x71, 0x24, 0x1c, 0x2f, 0x21, 0xf7, 0x72, 0x6b, 0x17,
        0xae, 0x89, 0xe3, 0xca, 0xb4, 0xc7, 0x8b, 0xe5, 0x0e, 0x06, 0x2b, 0x03, 0xa9, 0xff, 0xfb,
        0xba, 0xd1,
    ]);

    /// Build a V2 `Sync` log for `pool_address` carrying `(reserve0, reserve1)`,
    /// at `block_number`, with `removed` set as requested.
    fn make_sync_log(
        pool_address: Address,
        reserve0: U256,
        reserve1: U256,
        block_number: u64,
        removed: bool,
    ) -> Log {
        let data = {
            let mut data = Vec::with_capacity(64);
            data.extend_from_slice(&reserve0.to_be_bytes::<32>());
            data.extend_from_slice(&reserve1.to_be_bytes::<32>());
            data
        };
        let inner = alloy::primitives::Log::new_unchecked(
            pool_address,
            vec![V2_SYNC_TOPIC],
            Bytes::from(data),
        );
        Log {
            inner,
            block_hash: None,
            block_number: Some(block_number),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed,
        }
    }

    /// A counting fake subscriber — verifies the reorg notify path mirrors
    /// forward `on_pool_state_updated` (the engine dirties + re-solves at the
    /// next drain tick, with no distinct reorg path).
    struct CountingSubscriber {
        notifies: Mutex<u32>,
    }
    impl PoolStateSubscriber for CountingSubscriber {
        fn on_pool_state_updated(&self, _pool_id: u64) {
            *self.notifies.lock().unwrap() += 1;
        }
    }

    /// Build a `Bot` with a V2 pool registered at `update_block`, a counting
    /// subscriber attached, returning `(bot, pool_id, counting_subscriber)`.
    fn bot_with_v2(
        pool_addr: Address,
        update_block: u64,
    ) -> (Arc<Bot>, u64, Arc<CountingSubscriber>) {
        let bot = Arc::new(Bot::new(1));
        let pool_id = bot.state.write().register_v2_pool(&RegisterV2PoolParams {
            address: pool_addr,
            token0: Address::from([0xa0u8; 20]),
            token1: Address::from([0xa1u8; 20]),
            reserve0: U256::from(1_000),
            reserve1: U256::from(2_000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::from([0xf0u8; 20]),
            update_block,
        });
        let counting = Arc::new(CountingSubscriber {
            notifies: Mutex::new(0),
        });
        let sub: Arc<dyn PoolStateSubscriber> = counting.clone();
        bot.attach_engine(pool_id, Arc::downgrade(&sub));
        (bot, pool_id, counting)
    }

    /// ADR-006 slice 7 (coordinator e2e): a `removed: true` V2 Sync log at
    /// block 7 restores the pool's state to the genesis reserves (block 5),
    /// then fires the SAME `on_pool_state_updated` notify as a forward Sync.
    /// The per-event path lands at the same state as the bulk `handle_reorg`
    /// would have, but for just the targeted pool + via the normal notify.
    #[test]
    fn dispatch_reorg_log_restores_pool_and_notifies() {
        let pool_addr = Address::from([0x11u8; 20]);
        let (bot, pool_id, sub) = bot_with_v2(pool_addr, 5);
        let count = || *sub.notifies.lock().unwrap();

        // Forward Sync at block 7 — misprices the pool (reserves change).
        // This seeds the journal: genesis(5) → transition(7).
        bot.dispatch_log(&make_sync_log(
            pool_addr,
            U256::from(1_500),
            U256::from(2_500),
            7,
            false,
        ));
        assert_eq!(count(), 1, "forward dispatch notified once");
        assert_eq!(
            bot.state.read().v2_snapshot(pool_id),
            Some((U256::from(1_500), U256::from(2_500), 7)),
            "forward Sync applied"
        );

        // Reorg: a removed-flag Sync log at block 7 rolls it back to genesis.
        let coordinator = ReorgCoordinator::new(Arc::clone(&bot));
        coordinator
            .dispatch_reorg_log(&make_sync_log(
                pool_addr,
                U256::from(1_500), // content unused — block + pool identity matter
                U256::from(2_500),
                7,
                true,
            ))
            .expect("restore before 7 succeeds");
        assert_eq!(count(), 2, "reorg dispatched the SAME notify as forward");
        assert_eq!(
            bot.state.read().v2_snapshot(pool_id),
            Some((U256::from(1_000), U256::from(2_000), 5)),
            "reorg rolled back to genesis reserves"
        );
    }

    /// ADR-006 slice 7 (coordinator e2e): a too-deep reorg — the removed log's
    /// block is at the journal's genesis → `Err(NoStatePriorToBlock)` (the
    /// pump will shut down gracefully on this). Verify the pool's state is
    /// left UNCHANGED (no partial restore).
    #[test]
    fn dispatch_reorg_log_too_deep_returns_err_and_leaves_state_unchanged() {
        let pool_addr = Address::from([0x22u8; 20]);
        let (bot, pool_id, _sub) = bot_with_v2(pool_addr, 5);
        // No forward dispatch → journal has only the genesis(5) delta.
        let coordinator = ReorgCoordinator::new(Arc::clone(&bot));

        // Removed log at block 5 (== genesis) → too-deep.
        let err = coordinator
            .dispatch_reorg_log(&make_sync_log(
                pool_addr,
                U256::from(1_500),
                U256::from(2_500),
                5,
                true,
            ))
            .unwrap_err();
        match err {
            ReorgError::NoStatePriorToBlock { pool_id: p, block } => {
                assert_eq!(p, pool_id);
                assert_eq!(block, 5);
            }
        }
        // State untouched (the genesis reserves).
        assert_eq!(
            bot.state.read().v2_snapshot(pool_id),
            Some((U256::from(1_000), U256::from(2_000), 5)),
            "too-deep reorg left state unchanged"
        );
    }

    // ── V3 mirror helpers ───────────────────────────────────────────
    //
    // V3/V4 journals have NO genesis anchor (registration pushes no delta),
    // so the "before" values of the FIRST forward event ARE the registration
    // state. A too-deep reorg is therefore ONLY the empty-journal case —
    // `restore_before_block(block)` panics iff the journal is empty, and
    // handles single-delta-at-target by popping it and returning registration
    // state. The pre-check must reflect that, not the V2 rule.

    /// Build a `Bot` with a V3 pool registered at `update_block` (no tick data,
    /// `Sparse` coverage) + a counting subscriber. Registration scalars:
    /// `sqrt_price` = 1<<96, liquidity = `1_000_000`, tick = 0.
    fn bot_with_v3(
        pool_addr: Address,
        update_block: u64,
    ) -> (Arc<Bot>, u64, Arc<CountingSubscriber>) {
        use crate::bot_core::{PoolTickCoverage, RegisterV3PoolParams};
        let bot = Arc::new(Bot::new(1));
        let pool_id = bot.state.write().register_v3_pool(&RegisterV3PoolParams {
            address: pool_addr,
            token0: Address::from([0xa0u8; 20]),
            token1: Address::from([0xa1u8; 20]),
            fee: 3_000,
            tick_spacing: 60,
            factory: Address::from([0xf0u8; 20]),
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block,
            coverage: PoolTickCoverage::Sparse,
        });
        let counting = Arc::new(CountingSubscriber {
            notifies: Mutex::new(0),
        });
        let sub: Arc<dyn PoolStateSubscriber> = counting.clone();
        bot.attach_engine(pool_id, Arc::downgrade(&sub));
        (bot, pool_id, counting)
    }

    /// Build a V3 `Swap` log. `sender`/`recipient`/`amount0`/`amount1` are
    /// filler — the reorg path only consumes `pool_address` + `block_number`
    /// + the `removed` flag (the journal's stored "before" values are truth).
    #[allow(clippy::too_many_arguments)]
    fn make_v3_swap_log(
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        removed: bool,
    ) -> Log {
        use crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC;
        let sender = Address::from([0xbbu8; 20]);
        let recipient = Address::from([0xccu8; 20]);
        let amount0 = I256::try_from(-1_000_i128).unwrap();
        let amount1 = I256::try_from(4_000_i128).unwrap();
        let mut data = Vec::with_capacity(160);
        data.extend_from_slice(&amount0.to_be_bytes::<32>());
        data.extend_from_slice(&amount1.to_be_bytes::<32>());
        data.extend_from_slice(&sqrt_price_x96.to_be_bytes::<32>());
        let liq_bytes = U128::from(liquidity).to_be_bytes::<16>();
        let mut liq_word = [0u8; 32];
        liq_word[16..32].copy_from_slice(&liq_bytes);
        data.extend_from_slice(&liq_word);
        let tick_i256 = I256::try_from(i128::from(tick)).unwrap();
        data.extend_from_slice(&tick_i256.to_be_bytes::<32>());

        let inner = alloy::primitives::Log::new_unchecked(
            pool_address,
            vec![V3_SWAP_TOPIC, sender.into_word(), recipient.into_word()],
            Bytes::from(data),
        );
        Log {
            inner,
            block_hash: None,
            block_number: Some(block_number),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed,
        }
    }

    /// Regression (perm-V3-V4-V3 reorg crash): a V3 pool whose journal holds
    /// ONLY the forward swap at the reorg target block must restore to
    /// registration state — NOT fail-stop as "too-deep". V3 journals carry no
    /// genesis anchor: the first delta's `scalar_priors` ARE the registration
    /// scalars, so `restore_before_block(B)` pops it and returns registration
    /// state. Pre-fix `has_state_prior_to` rejected `earliest >= block`
    /// uniformly, so the pump shut down on the exact single-delta-at-target
    /// case `restore_before_block` handles correctly — the crash in
    /// `logs/perm-V3-V4-V3-reorg_crash.log` (pool 16, block 25350592).
    #[test]
    fn dispatch_reorg_log_v3_single_delta_at_target_restores_to_registration() {
        let pool_addr = Address::from([0x33u8; 20]);
        let (bot, pool_id, sub) = bot_with_v3(pool_addr, 5);
        let count = || *sub.notifies.lock().unwrap();

        let reg_sqrt = U256::from(1u128) << 96;

        // Forward V3 Swap at block 7 — moves scalars off registration + seeds
        // the journal with its single delta (block 7, scalar_priors = registration).
        let new_sqrt = U256::from(2u128) << 96;
        bot.dispatch_log(&make_v3_swap_log(
            pool_addr, new_sqrt, 2_000_000, 50, 7, false,
        ));
        assert_eq!(count(), 1, "forward swap notified once");
        {
            let guard = bot.state.read();
            let s = guard.get_v3_pool(pool_id).expect("registered");
            assert_eq!(s.sqrt_price_x96, new_sqrt);
            assert_eq!(s.liquidity, 2_000_000);
            assert_eq!(s.tick, 50);
            assert_eq!(
                s.journal.len(),
                1,
                "V3 registration pushes no genesis — one delta after the swap"
            );
        }

        // Reorg: removed-flag V3 Swap log at block 7 (the only journal delta).
        let coordinator = ReorgCoordinator::new(Arc::clone(&bot));
        coordinator
            .dispatch_reorg_log(&make_v3_swap_log(
                pool_addr, new_sqrt, 2_000_000, 50, 7, true,
            ))
            .expect("single-delta-at-target restores, is NOT too-deep");
        assert_eq!(count(), 2, "reorg dispatched the SAME notify as forward");
        {
            let guard = bot.state.read();
            let s = guard.get_v3_pool(pool_id).expect("registered");
            assert_eq!(
                s.sqrt_price_x96, reg_sqrt,
                "scalars rolled back to registration"
            );
            assert_eq!(s.liquidity, 1_000_000);
            assert_eq!(s.tick, 0);
            assert_eq!(
                s.journal.len(),
                0,
                "the block-7 delta was popped — journal now empty"
            );
            // V3 has no genesis anchor, so the restore point is the popped
            // delta's block (7), not the registration block (5). The scalars
            // are the registration state; update_block is a staleness hint,
            // not a correctness input for the solver.
            assert_eq!(s.update_block, 7);
        }
    }

    /// The flip side of the single-delta-at-target regression: a V3 pool that
    /// has received NO forward event has an EMPTY journal, and
    /// `restore_before_block` would panic on it. `has_state_prior_to` MUST
    /// return `false` here (→ `Err(NoStatePriorToBlock)` → graceful shutdown)
    /// even after the V3/V4 predicate relaxation. This pins that the relaxed
    /// predicate didn't drop the empty-journal guard — only an empty V3/V4
    /// journal is too-deep.
    #[test]
    fn dispatch_reorg_log_v3_empty_journal_is_too_deep() {
        let pool_addr = Address::from([0x34u8; 20]);
        let (bot, pool_id, _sub) = bot_with_v3(pool_addr, 5);
        // No forward dispatch → V3 journal is empty (registration pushes no
        // genesis delta, unlike V2).
        assert!(bot
            .state
            .read()
            .get_v3_pool(pool_id)
            .unwrap()
            .journal
            .is_empty());

        let coordinator = ReorgCoordinator::new(Arc::clone(&bot));
        let err = coordinator
            .dispatch_reorg_log(&make_v3_swap_log(
                pool_addr,
                U256::from(2u128) << 96,
                2_000_000,
                50,
                5,
                true,
            ))
            .unwrap_err();
        match err {
            ReorgError::NoStatePriorToBlock { pool_id: p, block } => {
                assert_eq!(p, pool_id);
                assert_eq!(block, 5);
            }
        }
        // State untouched — the registration scalars survive.
        let guard = bot.state.read();
        let s = guard.get_v3_pool(pool_id).expect("registered");
        assert_eq!(s.sqrt_price_x96, U256::from(1u128) << 96);
        assert_eq!(s.liquidity, 1_000_000);
        assert_eq!(s.tick, 0);
        assert!(s.journal.is_empty(), "empty journal left untouched");
    }

    // ── V4 mirror ────────────────────────────────────────────────────
    //
    // V4 uses the same `V3BlockDelta` journal shape as V3 (no genesis anchor).
    // The crash log's permutation (`V3-V4-V3`) means V4 pools are registered
    // alongside V3 — the same single-delta-at-target restore must work, and
    // the same empty-journal guard must hold.

    /// Build a `Bot` with a V4 pool registered under `pool_manager` + `pool_id`
    /// at `update_block` (no tick data, `Sparse` coverage) + a counting
    /// subscriber. Registration scalars: `sqrt_price` = 1<<96, liq = `1_000_000`,
    /// tick = 0.
    fn bot_with_v4(
        pool_manager: Address,
        pool_id: crate::bot_core::v4_swap_decoder::PoolId,
        update_block: u64,
    ) -> (Arc<Bot>, u64, Arc<CountingSubscriber>) {
        use crate::bot_core::{PoolTickCoverage, RegisterV4PoolParams, V4PoolKey};
        let bot = Arc::new(Bot::new(1));
        let pool_id = bot
            .state
            .write()
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager,
                pool_id,
                pool_key: V4PoolKey {
                    currency0: Address::from([0xa0u8; 20]),
                    currency1: Address::from([0xa1u8; 20]),
                    fee: 3_000,
                    tick_spacing: 60,
                    hooks: Address::from([0xf0u8; 20]),
                },
                hook_flags: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block,
                coverage: PoolTickCoverage::Sparse,
            })
            .expect("V4 pool registers");
        let counting = Arc::new(CountingSubscriber {
            notifies: Mutex::new(0),
        });
        let sub: Arc<dyn PoolStateSubscriber> = counting.clone();
        bot.attach_engine(pool_id, Arc::downgrade(&sub));
        (bot, pool_id, counting)
    }

    /// Build a V4 `Swap` log emitted by `pool_manager`. `sender`/`amount0`/
    /// `amount1`/`fee` are filler — the reorg path consumes only
    /// `pool_manager` (the log emitter) + `pool_id` (topic[1]) + `block_number`
    /// + the `removed` flag.
    #[allow(clippy::too_many_arguments)]
    fn make_v4_swap_log(
        pool_manager: Address,
        pool_id: crate::bot_core::v4_swap_decoder::PoolId,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        removed: bool,
    ) -> Log {
        use crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC;
        use alloy::primitives::B256;
        let sender = Address::from([0xbbu8; 20]);
        let amount0 = I256::try_from(-1_000_i128).unwrap();
        let amount1 = I256::try_from(4_000_i128).unwrap();
        let fee: u32 = 3_000;
        let mut data = Vec::with_capacity(192);
        data.extend_from_slice(&amount0.to_be_bytes::<32>());
        data.extend_from_slice(&amount1.to_be_bytes::<32>());
        data.extend_from_slice(&sqrt_price_x96.to_be_bytes::<32>());
        let liq_bytes = U128::from(liquidity).to_be_bytes::<16>();
        let mut liq_word = [0u8; 32];
        liq_word[16..32].copy_from_slice(&liq_bytes);
        data.extend_from_slice(&liq_word);
        let tick_i256 = I256::try_from(i128::from(tick)).unwrap();
        data.extend_from_slice(&tick_i256.to_be_bytes::<32>());
        let mut fee_word = [0u8; 32];
        fee_word[28..32].copy_from_slice(&fee.to_be_bytes());
        data.extend_from_slice(&fee_word);

        let inner = alloy::primitives::Log::new_unchecked(
            pool_manager,
            vec![V4_SWAP_TOPIC, B256::from(pool_id), sender.into_word()],
            Bytes::from(data),
        );
        Log {
            inner,
            block_hash: None,
            block_number: Some(block_number),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed,
        }
    }

    /// V4 mirror of `dispatch_reorg_log_v3_single_delta_at_target_restores_to_registration`:
    /// a V4 pool whose only journal delta is the forward swap at the reorg
    /// target block restores to registration state. Confirms the
    /// `has_state_prior_to` split covers V4 (same `V3BlockDelta` journal shape,
    /// no genesis anchor).
    #[test]
    fn dispatch_reorg_log_v4_single_delta_at_target_restores_to_registration() {
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: crate::bot_core::v4_swap_decoder::PoolId = [0xeeu8; 32];
        let (bot, pool_id, sub) = bot_with_v4(pool_manager, pool_id_bytes, 5);
        let count = || *sub.notifies.lock().unwrap();

        let reg_sqrt = U256::from(1u128) << 96;
        let new_sqrt = U256::from(2u128) << 96;

        // Forward V4 Swap at block 7.
        bot.dispatch_log(&make_v4_swap_log(
            pool_manager,
            pool_id_bytes,
            new_sqrt,
            2_000_000,
            50,
            7,
            false,
        ));
        assert_eq!(count(), 1);
        {
            let guard = bot.state.read();
            let s = guard.get_v4_pool(pool_id).expect("registered");
            assert_eq!(s.sqrt_price_x96, new_sqrt);
            assert_eq!(s.journal.len(), 1, "V4 registration pushes no genesis");
        }

        // Reorg: removed-flag V4 Swap at block 7 (the only journal delta).
        let coordinator = ReorgCoordinator::new(Arc::clone(&bot));
        coordinator
            .dispatch_reorg_log(&make_v4_swap_log(
                pool_manager,
                pool_id_bytes,
                new_sqrt,
                2_000_000,
                50,
                7,
                true,
            ))
            .expect("V4 single-delta-at-target restores, is NOT too-deep");
        assert_eq!(count(), 2, "reorg dispatched the SAME notify as forward");
        {
            let guard = bot.state.read();
            let s = guard.get_v4_pool(pool_id).expect("registered");
            assert_eq!(
                s.sqrt_price_x96, reg_sqrt,
                "V4 scalars rolled back to registration"
            );
            assert_eq!(s.liquidity, 1_000_000);
            assert_eq!(s.tick, 0);
            assert_eq!(s.journal.len(), 0);
        }
    }

    /// V4 mirror of the empty-journal guard: a V4 pool with no forward event
    /// has an empty journal → too-deep (would panic in `restore_before_block`).
    #[test]
    fn dispatch_reorg_log_v4_empty_journal_is_too_deep() {
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: crate::bot_core::v4_swap_decoder::PoolId = [0xefu8; 32];
        let (bot, pool_id, _sub) = bot_with_v4(pool_manager, pool_id_bytes, 5);
        assert!(bot
            .state
            .read()
            .get_v4_pool(pool_id)
            .unwrap()
            .journal
            .is_empty());

        let coordinator = ReorgCoordinator::new(Arc::clone(&bot));
        let err = coordinator
            .dispatch_reorg_log(&make_v4_swap_log(
                pool_manager,
                pool_id_bytes,
                U256::from(2u128) << 96,
                2_000_000,
                50,
                7,
                true,
            ))
            .unwrap_err();
        match err {
            ReorgError::NoStatePriorToBlock { pool_id: p, block } => {
                assert_eq!(p, pool_id);
                assert_eq!(block, 7);
            }
        }
        let guard = bot.state.read();
        let s = guard.get_v4_pool(pool_id).expect("registered");
        assert_eq!(s.sqrt_price_x96, U256::from(1u128) << 96);
        assert!(s.journal.is_empty());
    }
}

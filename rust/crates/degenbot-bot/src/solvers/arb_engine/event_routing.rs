//! Log event routing: apply live and backfill events to sub-engines.

#[cfg(test)]
use alloy::primitives::{aliases::U112, Address};

#[cfg(test)]
use crate::bot_core::{V3SwapUpdate, V4SwapUpdate};
use degenbot_solvers::mixed::MixedPoolRef;

#[cfg(test)]
use super::HashSet;

use super::{ArbitrageEngine, BlockMetadata, HopType};

impl ArbitrageEngine {
    /// Mark `pool_id` dirty, classifying it into the V2/V3/V4 dirty set by
    /// consulting the shared `BotState`'s `PoolEntry` variant (ADR-006 D4).
    ///
    /// This is the subscriber-facing entry point: the `EngineSubscriber`
    /// adapter calls this from `on_pool_state_updated` (after the `BotState`
    /// write guard is released), taking only the engine `Mutex`. If `pool_id`
    /// isn't registered in `core`, the call is a no-op (the event was for a
    /// pool no path references).
    pub fn insert_dirty(&self, pool_id: u64) {
        let core = self.core.read();
        if core.get_v2_pool_state(pool_id).is_some() {
            drop(core);
            self.dirty_sets.insert(pool_id, HopType::V2);
        } else if core.get_v3_pool(pool_id).is_some() {
            drop(core);
            self.dirty_sets.insert(pool_id, HopType::V3);
        } else if core.get_v4_pool(pool_id).is_some() {
            drop(core);
            self.dirty_sets.insert(pool_id, HopType::V4);
        }
        // Unregistered pool_id → no-op (no path references it).
    }

    /// call, but do NOT send a result batch to Python.
    ///
    /// The pump calls this eagerly after each WS log to keep engine state
    /// current. The actual batch send is triggered by the pump's debounce
    /// timer or block boundary logic.
    pub fn solve_dirty(&mut self, block_number: u64, metadata: &BlockMetadata) {
        // Expire stale buffered events in the V3/V4 buffers (ADR-003: both
        // now live on BotState).
        //
        // XC7SWD: these core.write() calls ran uninstrumented and own a
        // ~2.8-3.1s window of every engine mutex hold (solve_duration p95
        // 4.85s vs the rebuild-cycle internal p95 of 0.46s; Jaeger children
        // sum to <0.5s of a 3.1-3.3s solve span).
        //
        // LPEOBI: with the cockpit default (`max_age=None`) the expiry is a
        // provable no-op (expire() early-returns: "If `max_age` is `None`",
        // liquidity_event_buffer.rs) - and each write still bought a ~2.9s
        // writer-queue slot under the block-apply stream (lock WAIT p90
        // 2.76-3.03s, work 0us in 4,298/4,298 samples). Skip lock-free when
        // expiry is not configured.
        if self.event_buffer_expiry_enabled {
            let (v3_lock_wait_us, v3_work_us) =
                self.expire_buffered_telemetry("v3", |core| core.expire_v3_buffered(block_number));
            let (v4_lock_wait_us, v4_work_us) =
                self.expire_buffered_telemetry("v4", |core| core.expire_v4_buffered(block_number));
            tracing::info!(
                target: "degenbot::solver",
                block_number,
                expire_v3_lock_wait_us = v3_lock_wait_us,
                expire_v3_work_us = v3_work_us,
                expire_v4_lock_wait_us = v4_lock_wait_us,
                expire_v4_work_us = v4_work_us,
                "[solve-phase] buffered-event expiry (pre-cycle) complete"
            );
        } else {
            tracing::info!(
                target: "degenbot::solver",
                block_number,
                expiry_enabled = false,
                "[solve-phase] buffered-event expiry skipped (max_age unset)"
            );
        }

        // Snapshot all dirty sets atomically (RAYPAR engine-shard T3).
        let (dirty_v2, dirty_v3, dirty_v4) = self.dirty_sets.take_all();

        // Re-solve only paths containing updated pools (no batch send)
        self.rebuild_and_solve_affected(&dirty_v2, &dirty_v3, &dirty_v4, block_number, metadata);

        // dirty sets are already cleared by std::mem::take
        self.last_processed_block = Some(block_number);
    }

    /// One buffered-event expiry round under its own `degenbot.arb.expire`
    /// span, split into lock-WAIT (time to acquire the core write lock -
    /// contention with the pump apply loop / Python bridge) vs expiry WORK
    /// (the expire pass itself under the held lock). Returns microseconds
    /// for the aggregated pre-cycle event.
    fn expire_buffered_telemetry(
        &mut self,
        kind: &'static str,
        expire: impl FnOnce(&mut crate::bot_core::BotState),
    ) -> (u64, u64) {
        use std::time::Instant;
        let span = tracing::info_span!(
            target: "degenbot::solver",
            "degenbot.arb.expire",
            kind,
            lock_wait_us = tracing::field::Empty,
            expire_work_us = tracing::field::Empty,
        );
        let ctx = span.enter();
        let lock_t0 = Instant::now();
        let mut core = self.core.write();
        let lock_wait_us = u64::try_from(lock_t0.elapsed().as_micros()).unwrap_or(u64::MAX);
        let work_t0 = Instant::now();
        expire(&mut core);
        let expire_work_us = u64::try_from(work_t0.elapsed().as_micros()).unwrap_or(u64::MAX);
        drop(core);
        drop(ctx);
        span.record("lock_wait_us", lock_wait_us);
        span.record("expire_work_us", expire_work_us);
        (lock_wait_us, expire_work_us)
    }

    /// Compute the incremental diff and send a result batch to Python.
    ///
    /// Called by the pump when the debounce timer fires (mid-block) or
    /// when a block boundary is detected. Results must already be
    /// up-to-date (via `solve_dirty`) before calling this.
    pub fn send_result_batch(&mut self, metadata: &BlockMetadata) {
        self.compute_diff_and_send(metadata);
    }

    /// Returns `true` if there are unsolved dirty pool keys from `apply_log`
    /// calls that haven't been followed by `solve_dirty` yet.
    #[must_use]
    pub fn has_dirty_paths(&self) -> bool {
        !self.dirty_sets.is_empty()
    }

    /// Snapshot every registered path's per-hop pool refs for the Option-A
    /// solver-state accuracy gate (`solver_state_tripwire`). The pump extracts
    /// the `BotState` scalar state + diffs it against the chain at the solve
    /// block; engines with non-scalar-diffable paths override the default.
    #[must_use]
    pub fn solver_path_pool_refs(&self) -> Vec<Vec<MixedPoolRef>> {
        self.path_pools
            .values()
            .map(|path| path.pools.clone())
            .collect()
    }

    /// Consume-and-clear the ADR-021 solver-state change set: return the pool
    /// refs for ONLY the paths re-solved since the last call and clear it, so
    /// the verifier diffs just this block's re-solved paths (never the whole
    /// registered set). `&mut self` is satisfied by the engine's owner holding
    /// it behind a `Mutex`; the atomic take-then-clear avoids any race between
    /// a reader and the next solve cycle overwriting the set.
    pub fn take_solver_path_pool_refs_change_set(&mut self) -> Vec<Vec<MixedPoolRef>> {
        let ids = std::mem::take(&mut self.last_solved_path_ids);
        ids.iter()
            .filter_map(|id| self.path_pools.get(id))
            .map(|path| path.pools.clone())
            .collect()
    }

    /// Finalize the current block: if dirty paths accumulated since the last
    /// solve would be left behind by a block advance, solve them and send a
    /// result batch carrying `metadata`. Otherwise, if logs were observed but
    /// touched no registered pools, send an empty block-boundary batch so
    /// Python sees the advance.
    ///
    /// This is the engine-side logic behind `ArbitrageEnginePump::finalize_if_dirty`.
    /// Holding it on the engine (rather than the pump) keeps it next to its
    /// siblings (`solve_dirty`, `send_result_batch`)
    /// and makes the metadata-threading contract unit-testable without a live
    /// WS connection. The pump passes its real `current_metadata` so that any
    /// batch emitted here carries genuine fees/gas/timestamp (previously this
    /// path sent `BlockMetadata::default()`, which would make the Python
    /// consumer compute `base_fee_next = 0` and broadcast underpriced txs.
    ///
    /// The `block > last_solved_block` guard is load-bearing: the pump runs
    /// `solve_dirty` at the top of every loop iteration before awaiting the
    /// next event, so this normally only sends on a genuine block advance.
    ///
    /// `last_solved_block` + `has_logs_this_block` are owned by the engine
    /// since ergo task LEZJAS (the pump's `&mut` out-params retired); a
    /// mid-flight engine joining the pump can inherit the pump's last solved
    /// block via `set_last_solved_block` (ADR-006 D4).
    pub fn finalize_block(&mut self, block: u64, metadata: &BlockMetadata) {
        if block > self.last_solved_block {
            if self.has_dirty_paths() {
                self.solve_dirty(block, metadata);
                self.send_result_batch(metadata);
            } else if self.has_logs_this_block {
                // X35QKN: previously this called `process_block_and_send(&[], ...)`
                // — the parallel log-routing API. `process_block(&[])` over an
                // empty slice is a no-op loop + `solve_dirty` (an empty-dirty-
                // sets no-op apart from the `last_processed_block` stamp), so the
                // empty-block boundary just sends an empty diff batch so Python
                // sees the advance. Inlined here to retire the parallel path.
                self.solve_dirty(block, metadata);
                self.compute_diff_and_send(metadata);
            }
            self.last_solved_block = block;
            self.has_logs_this_block = false;
        }
        // Authoritative per-family apply split (2SDIQW): hotpath labels do
        // not aggregate reliably in impl_type mode, so the atomics summarize
        // per block here. Format: calls:us per family.
        let (apply_calls, apply_us) = crate::bot_core::apply_telemetry::snapshot_reset();
        if apply_calls.iter().any(|&c| c > 0) {
            let mut parts = Vec::with_capacity(5);
            for i in 0..5 {
                if apply_calls[i] > 0 {
                    parts.push(format!(
                        "{}={}:{}us",
                        crate::bot_core::apply_telemetry::FAMILY_NAMES[i],
                        apply_calls[i],
                        apply_us[i] / 1_000
                    ));
                }
            }
            tracing::info!(
                target: "degenbot::state",
                block_number = block,
                apply.block_us = apply_us.iter().sum::<u128>() / 1_000,
                apply.families = %parts.join(","),
                "[apply-telemetry] block family split"
            );
        }
    }

    /// Process pre-decoded updates for testing.
    #[cfg(test)]
    pub fn process_updates(
        &mut self,
        v2_updates: &[(Address, U112, U112)],
        v3_updates: &[V3SwapUpdate],
        block_number: u64,
        metadata: &BlockMetadata,
    ) {
        // Apply V2+V3 updates to BotState and collect affected pool ids (ADR-003)
        let mut v2_affected = HashSet::new();
        let mut v3_affected = HashSet::new();
        {
            let mut core = self.core.write();
            for &(addr, r0, r1) in v2_updates {
                if let Some(pool_id) = core.apply_v2_sync(addr, r0, r1, block_number) {
                    v2_affected.insert(pool_id);
                }
            }
            for update in v3_updates {
                if let Some(pool_id) = core.apply_v3_swap(
                    update.pool_address,
                    update.sqrt_price_x96,
                    update.liquidity,
                    update.tick,
                    block_number,
                    &update.tick_priors,
                ) {
                    v3_affected.insert(pool_id);
                }
            }
        }

        // Re-solve only paths containing updated pools
        self.rebuild_and_solve_affected(
            &v2_affected,
            &v3_affected,
            &HashSet::new(),
            block_number,
            metadata,
        );
        self.last_processed_block = Some(block_number);
    }

    /// Process pre-decoded V4 updates.
    #[cfg(test)]
    pub fn process_v4_updates(
        &mut self,
        v4_updates: &[V4SwapUpdate],
        block_number: u64,
        metadata: &BlockMetadata,
    ) {
        let mut v4_affected = HashSet::new();
        {
            let mut core = self.core.write();
            for update in v4_updates {
                if let Some(pool_id) = core.apply_v4_swap(update, block_number) {
                    v4_affected.insert(pool_id);
                }
            }
        }
        self.rebuild_and_solve_affected(
            &HashSet::new(),
            &HashSet::new(),
            &v4_affected,
            block_number,
            metadata,
        );
    }
}

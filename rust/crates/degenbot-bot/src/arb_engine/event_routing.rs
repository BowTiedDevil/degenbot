//! Log event routing: apply live and backfill events to sub-engines.

#[cfg(test)]
use alloy::primitives::{aliases::U112, Address};
use degenbot_core::diag;

#[cfg(test)]
use crate::bot_core::{V3SwapUpdate, V4SwapUpdate};

#[cfg(test)]
use super::HashSet;
#[cfg(test)]
use crate::arb_engine::tests::test_keys::affected_keys;

use super::{ArbitrageEngine, BlockMetadata};

impl ArbitrageEngine {
    // (NOTE, LXDY4C): the former `insert_dirty` (BotState-bucket
    // classification into the shared dirty sets) is retired — touched pools
    // are recorded into the block's `EpochDelta` by log application
    // (`LogDispatcher::dispatch` / `Bot::record_pool_state_changed`), and
    // the affected-path derivation consumes the delta's taken keys.

    /// The CURRENT cycle's dispatch arm (ADR-045 T5): the latency
    /// histograms' `arm` label, read by the caller that observes the cycle.
    /// Backed by the typed `last_arm` latch, not a string stash; the stage
    /// hook reads the `CycleOutcome` directly for its own span/duration.
    #[must_use]
    pub fn cycle_arm(&self) -> &'static str {
        self.cycle.last_arm.map_or("unset", |arm| arm.label())
    }

    /// KNEUQX: the block the MOST RECENT solve cycle ran anchored on - the
    /// solve-anchor resolution (request block floored by the pool-state head,
    /// see `crate::bot_core::solve_anchor`), stamped into the cycle's
    /// span as `cycle.solve_block`. The span's `block.number`
    /// tag records the ENTRY (drain/finalize event) block per ZZS6CG lineage;
    /// when a late finalize runs at a settle boundary these differ and the
    /// phase children (fanout/resolve/stage) always carry the anchor.
    #[must_use]
    pub fn results_block(&self) -> u64 {
        self.cycle.cursor.results_block()
    }

    pub(crate) fn solve_dirty(
        &mut self,
        block_number: u64,
        metadata: &BlockMetadata,
        affected: &[degenbot_solvers::affected_keys::AffectedKey],
    ) -> super::solve_cycle::CycleOutcome {
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
            diag!(
                domain = solver,
                block_number,
                expire_v3_lock_wait_us = v3_lock_wait_us,
                expire_v3_work_us = v3_work_us,
                expire_v4_lock_wait_us = v4_lock_wait_us,
                expire_v4_work_us = v4_work_us,
                "buffered-event expiry (pre-cycle) complete"
            );
        } else {
            diag!(
                domain = solver,
                block_number,
                expiry_enabled = false,
                "buffered-event expiry skipped (max_age unset)"
            );
        }

        // LXDY4C: the affected keys arrive from the block's EpochDelta
        // (consumed by the stage surface's on_resolve hook); no engine-local
        // dirty-set intake remains.
        // Re-solve only paths containing updated pools (no batch send)
        let outcome = self.rebuild_and_solve_affected(affected, block_number, metadata);

        // 6XB6NJ: monotone advance on the block cursor.
        self.cycle.cursor.advance_processed(block_number);
        outcome
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

    /// Finalize the current block: advance the solved boundary and emit the
    /// terminal block-boundary batch carrying `metadata` so Python observes
    /// the advance with genuine fees/gas/timestamp. Bookkeeping-only — this
    /// method NEVER runs a solve cycle.
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
    /// The `block > last_solved_block` guard is load-bearing: it makes the
    /// boundary advance one-shot even when the tombstone re-fires for an
    /// already-finalized block. 6XB6NJ: the guarded transition lives on the
    /// block cursor (`BlockCursor::finalize`); this method threads the
    /// terminal publish through the same guard.
    ///
    /// The solved boundary + the logs flag are engine-owned since ergo task
    /// LEZJAS (the pump's `&mut` out-params retired; now on the block
    /// cursor); a mid-flight engine joining the pump can inherit the pump's
    /// last solved block via `set_last_solved_block` (ADR-006 D4).
    pub fn finalize_block(&mut self, block: u64, metadata: &BlockMetadata) {
        if self.cycle.cursor.finalize(block) {
            // PWPPAZ T1 (supersedes the two former solve branches and the
            // X35QKN empty-block inlining): the finalize is tombstone-
            // dispatched and executed by the drainer while the SUCCESSOR
            // block's log burst is still being applied, so the old inner
            // `solve_dirty` consumed the successor's first-dirt under the
            // dead block's identity (trace ab13f75f: finalize(83) solved
            // 1,755 paths of block 84's dirt; 98f7cf52 repeats the pattern
            // blocks apart). Unconsumed dirt now waits for the pump's
            // drained-settle gate (plus its T2 early slice) — solve cycles
            // are the settle gate's exclusive job. The boundary is the
            // cursor's guarded four-field advance + the terminal publish.
            self.compute_diff_and_send(metadata);
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
            diag!(domain = solver, block_number = block,
                apply.block_us = apply_us.iter().sum::<u128>() / 1_000,
                apply.families = %parts.join(","),
                "block family split"
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

        // Re-solve only paths containing updated pools (test-only intake)
        self.rebuild_and_solve_affected(
            &affected_keys(&v2_affected, &v3_affected, &HashSet::new()),
            block_number,
            metadata,
        );
        // 6XB6NJ: monotone advance on the block cursor.
        self.cycle.cursor.advance_processed(block_number);
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
            &affected_keys(&HashSet::new(), &HashSet::new(), &v4_affected),
            block_number,
            metadata,
        );
    }
}

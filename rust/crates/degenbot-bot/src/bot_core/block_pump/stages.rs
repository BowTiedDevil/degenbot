use super::{
    ms_to_secs, op_error, op_warn, wall_ms, BlockPump, Epoch, Finalize, Publish, Resolve, Solve,
    StageDecision, StageMachine, BACKFILL_TIMEOUT_SECS,
};

impl BlockPump {
    /// Shared strictly-synchronous solve execution for the drained-settle
    /// gate's quiesce solve and the PWPPAZ T2 early slice (TQ7PD6: no await
    /// inside — the stage hooks run INLINE on this driver task, so their
    /// spans nest under the entered cursor block span naturally).
    ///
    /// Pump-owned ACTIVE BLOCK promotion (QMSTSV/BO5FBS): the solve anchor is
    /// the LOG-DRIVEN settled block (`fsm.latest_observed()`, never a
    /// racing header), floored by the pool-state head so it is never below
    /// the state it solves against (MQIZ5M +1-wei / IIA class; the
    /// backfill-ahead semantics). `drain_decision` owns the exact rule.
    pub(super) fn boundary_drain_dispatch(
        &self,
        fsm: &StageMachine,
        block_span: Option<&tracing::Span>,
        _now_ms: u64, // the retired header→solved stamp consumed it; the epoch race is stamped in drive_publish
    ) {
        let _solve_ctx = block_span.map(tracing::Span::enter);
        let state_head = self
            .bot
            .state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .pool_state_head();
        let StageDecision::Drain { block, metadata } = fsm.drain_decision(state_head) else {
            unreachable!("drain_decision always drains when called");
        };
        self.drive_solve(fsm, fsm.context_for(block, metadata));
    }

    /// The I3 stale-epoch drop (7NFYQW, T6IYKY review Q2 edge) — the
    /// dissolved `DispatchOwner` FIFO drop, moved onto the driver: a work
    /// item whose rewind generation sits BELOW the machine's current one is
    /// reorg-flying. It is dropped LOUDLY here instead of silently consuming
    /// `epoch.block()` into solve/finalize bookkeeping; the fresh
    /// generation's stream re-delivers the block's work. Returns true when
    /// the item was dropped.
    #[expect(
        clippy::unused_self,
        reason = "driver-side hook kept on the pump for seam discoverability; the stage machine owns all state the check reads"
    )]
    pub(super) fn reorg_flying_stale(
        &self,
        fsm: &StageMachine,
        ctx: &crate::bot_core::BlockContext,
    ) -> bool {
        let observed_seq = fsm.rewind_seq();
        let epoch = ctx.epoch();
        if epoch.seq() < observed_seq {
            op_warn!(domain = pump, item_block = epoch.block(),
                item_seq = epoch.seq(),
                observed_seq,
                "reorg-flying stage work: stale epoch dropped instead of consuming epoch.block() (I3)"
            );
            if let Some(p) = crate::instruments::pipeline() {
                p.count_stale_drop();
            }
            return true;
        }
        false
    }

    /// Drive the Solved cycle (Quiesced → Resolved → Solved) for `ctx`:
    /// the stage-table row order the drained-settle gate and the backfill
    /// solve both express. The affected keys are the epoch delta's take
    /// (`on_resolve`), consumed by `on_solve`. Marks the block solved
    /// (LEZJAS) on success. Ignores `StageError`s the engine cannot produce
    /// (its hooks are infallible; a hard failure logs loud, never silently
    /// skips — ADR-021 posture).
    pub(super) fn drive_solve(&self, fsm: &StageMachine, ctx: crate::bot_core::BlockContext) {
        if self.reorg_flying_stale(fsm, &ctx) {
            return;
        }
        let delta = self.bot.active_delta();
        let quiesced = match self.engine.on_streaming_complete(
            &crate::bot_core::stage_handlers::StreamingComplete {
                ctx,
                delta: &delta,
                backfill: None,
            },
        ) {
            Ok(q) => q,
            Err(error) => {
                op_error!(domain = pump, %error, "stage StreamingComplete failed — solve skipped");
                return;
            }
        };
        let paths = match self.engine.on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        }) {
            Ok(p) => p,
            Err(error) => {
                op_error!(domain = pump, %error, "stage Resolve failed — solve skipped");
                return;
            }
        };
        match self.engine.on_solve(&Solve { ctx, paths }) {
            Ok(outcome) => {
                // LEZJAS: the engine owns `last_solved_block`. The cursor
                // fact now crosses the seam ON the outcome (the Solved row
                // knows its anchor epoch), so the driver derives the cursor
                // from the product instead of re-poking the seam with
                // `ctx.epoch()`.
                self.control.set_last_solved_block(outcome.solved);
            }
            Err(error) => {
                op_error!(domain = pump, %error, "stage Solve failed");
            }
        }
    }

    /// Drive the Published row: the delivery-to-Python edge for the
    /// quiesce-gated publish (ADR-008 D2).
    pub(super) fn drive_publish(
        &self,
        fsm: &StageMachine,
        ctx: crate::bot_core::BlockContext,
        gated: &crate::bot_core::GateOutcome,
    ) {
        if self.reorg_flying_stale(fsm, &ctx) {
            return;
        }
        // ADR-041 epoch race: header accept → Published dispatch (succeeds
        // the dissolved `DispatchOwner` drainer's header→solved stamp; the
        // money race ends at this edge — submission/delivery subscribe here).
        // Single-writer: the pump task alone stores `header_ms`.
        let header_ms = self.header_ms.load(std::sync::atomic::Ordering::Relaxed);
        if header_ms != 0 {
            if let Some(p) = crate::instruments::pipeline() {
                p.observe_header_to_publish(ms_to_secs(wall_ms().saturating_sub(header_ms)));
            }
        }
        if let Err(error) = self.engine.on_publish(&Publish {
            ctx,
            gated: gated.clone(),
        }) {
            op_error!(domain = pump, %error, "stage Publish failed — batch not delivered");
        }
    }

    /// Drive the Finalized row: the tombstone boundary catch (VTWCIG
    /// metadata; terminal publish supersedes the pending quiesce publish).
    pub(super) fn drive_finalize(&self, fsm: &StageMachine, ctx: crate::bot_core::BlockContext) {
        if self.reorg_flying_stale(fsm, &ctx) {
            return;
        }
        if let Err(error) = self.engine.on_finalize(&Finalize { ctx }) {
            op_error!(domain = pump, %error, "stage Finalize failed — boundary not stamped");
        }
    }

    /// Handle a 60s timeout by backfilling any missed blocks (eager variant).
    pub(super) async fn handle_timeout_eager(&self, fsm: &mut StageMachine) {
        op_warn!(
            domain = pump,
            backfill_timeout_secs = BACKFILL_TIMEOUT_SECS,
            "BlockPump: no activity — attempting backfill"
        );
        let latest_block = match self.ingestor.latest_block().await {
            Ok(n) => n,
            Err(e) => {
                op_error!(domain = pump, %e, "BlockPump: backfill failed — can't get block number");
                return;
            }
        };

        if latest_block > fsm.current_block() {
            self.backfill_range(fsm.current_block() + 1, latest_block, fsm)
                .await;
            // ADR-028: the cursor + single-writer recovery-anchor advance happen
            // inside the FSM (`on_backfill_range_done`). The driver only reports
            // the engine-side solved boundary.
            fsm.on_backfill_range_done(latest_block);
            // LEZJAS: engine owns `last_solved_block` now — mark the backfilled
            // range solved through the engine seam.
            self.control.set_last_solved_block(Epoch::at(latest_block));
        }
    }
}

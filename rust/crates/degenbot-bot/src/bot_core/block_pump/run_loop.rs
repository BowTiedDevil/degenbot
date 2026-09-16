use super::{
    feed_executor_throttle_sample, op_error, op_info, op_warn, stream, timeout, us_to_secs,
    wall_ms, BlockMetadata, BlockPump, CompletenessDecision, Duration, Epoch, GateOutcome, HashSet,
    Instrument, LogDecision, Ordering, PreSolveGapTrack, StageDecision, StageMachine, StreamExt,
    WsEvent, B256, BACKFILL_TIMEOUT_SECS, RELEVANT_TOPICS,
};
// The `hotpath`-gated cooperative timed-exit block below is the only user of
// `Arc` in this submodule (`Arc::clone(&self.shutdown)`). The pre-split monolith
// had a file-scope `use std::sync::Arc;` that the decomposition did not carry
// across the module boundary; import it under the same feature gate so the
// default build keeps no unused import while the extension feature set resolves.
#[cfg(feature = "hotpath")]
use super::Arc;

impl BlockPump {
    // phasing-trigger POLICY: phase
    // this method only when a single change must touch MORE THAN TWO of its five
    // interleaved concerns in one PR (below the threshold the interleaving in a
    // single-writer loop is measured-and-accepted, not a hazard). Line anchors are
    // HEAD-of-this-commit (0bd2b8909 + this comment block):
    //   (1) hotpath guards + timed-exit pruning (hotpath_guard / timed_exit_tick);
    //   (2) allocator purge control (allocator_ctrl::on_header_observed, header
    //       branch @1136; init_from_env_at_pump_start @527);
    //   (3) WS-completeness cross-check (CompletenessDecision Verify @1591 /
    //       BackfillOwned @1603, behind ws_completeness_enabled);
    //   (4) posture telemetry on the executor seam (feed_executor_throttle_sample
    //       header-cadence feed @1159);
    //   (5) backfill/rewind re-anchoring (fsm.record_backfill @619 + the header
    //       epoch anchors).
    // Re-baseline: production body ~1400 lines (492..1891, up to
    // boundary_drain_dispatch's docs); the original review's "1391+" came from a
    // drifted revision — churn since is additive test mass, so the
    // trailing-average shrink claim stays qualitative.
    // PRESERVE under phasing: Arc<dyn StageHandlers> (the test surface) and the
    // for_test knobs (set_quiesce_for_test / bot_arc_for_test / header-staleness /
    // early-slice / log-silence, ~2400-2485), plus the FSM instance and its
    // mutation points (fsm.set_quiesce_params @611, fsm.record_backfill @619).
    // The future phaser must document the phase-state carrier decision (shared
    // struct vs heavy parameter passing) with its change. Decision record:
    // CONTEXT.md "Block-pump dispatch seam" -> pump-driver phasing (DECIDED).
    /// Processes logs eagerly: each WS log is applied to engine state
    /// immediately and affected paths are solved right away, without
    /// waiting for a block header. Block headers provide metadata
    /// (timestamp, fees) and handle empty-block detection.
    ///
    /// # Panics
    ///
    /// Hard-aborts the process (never unwinds a half-alive pump) on the fatal
    /// failure buckets, on a live-websocket log drop
    /// (`DEGENBOT_WS_COMPLETENESS`), or a dead or
    /// stalled background drainer (a send into a closed channel, or
    /// `NO_PROGRESS_STRIKE_LIMIT` consecutive no-progress pushes). Also shuts
    /// down on a
    /// late-forward log on a tombstoned block (unreliable WS, ADR-008 D3).
    // `clippy::used_underscore_binding` expectation retired — it
    // was only fired by the removed `#[tracing::instrument]` expansion.
    #[expect(clippy::too_many_lines, clippy::cast_possible_truncation)]
    // the former `#[tracing::instrument]` here was a root span that
    // stayed open for the whole bot run. OTel only exports CLOSED spans, so the
    // root never reached Jaeger while every pump-task span referenced it as a
    // missing parent — one giant orphaned trace. Per-epoch `degenbot.epoch`
    // spans (below) are the trace roots now.
    pub async fn run_with_stream(
        &mut self,
        combined: stream::BoxStream<'static, WsEvent>,
        first_observed_block: u64,
    ) {
        // Drained-settle solve gate: peekable so the loop
        // can probe "is another event already buffered?" WITHOUT consuming it.
        let mut combined = combined.peekable();
        // [DIAG] newHeads-stall investigation: track header arrivals so the
        // log shows, in production, whether `BlockHeader` events actually stop
        // arriving (subscription silent) vs. arrive but the arm doesn't fire
        // (pump not polling / bug). Remove once the freeze root cause is
        // confirmed and fixed — the counters/interval now live behind the
        // `PumpTelemetry` seam (`bot_core::pump_telemetry`).

        // hotpath drain-path tracer bullet (`src/profiling.rs`): hold a
        // profiling guard for the whole pump loop iff `DEGENBOT_HOTPATH=1`.
        // No-op (not even constructed) otherwise, and a no-op stub when the
        // `hotpath` Cargo feature is off. Dropping at loop exit writes the
        // report. With HOTPATH_SHUTDOWN_MS set, the cooperative timer below
        // raises the shutdown flag at the window; the guard drops HERE — after
        // the post-loop OTel flush — so the report captures the final state
        // without racing live workers (S53STH: replaces hotpath's own
        // build_with_shutdown thread, whose process::exit aborted tokio
        // workers mid-TLS-teardown).
        let _hotpath_guard = crate::profiling::hotpath_guard("block_pump");
        // hotpath tokio-runtime monitor (hotpath_tokio_* families): must run
        // inside the ambient I/O runtime, which this is. The interesting
        // getters need --cfg tokio_unstable (root .cargo/config.toml); without
        // it the monitor emits only the unstable-free subset. No-op when the
        // hotpath feature is off.
        #[cfg(feature = "hotpath")]
        hotpath::tokio_runtime!(&tokio::runtime::Handle::current());
        // apply any fixed DEGENBOT_MIMALLOC_PURGE_DELAY_MS and
        // arm the block-cadence discovery for the purge-delay control.
        crate::allocator_ctrl::init_from_env_at_pump_start();
        // S53STH cooperative timed exit: a 500ms tick that polls the shutdown
        // flag inside the parked select, so the loop unwinds through its span
        // guards promptly when the hotpath timer raises the flag. The flag is
        // the single source of truth (also checked at the loop head).
        let mut timed_exit_tick = tokio::time::interval(Duration::from_millis(500));
        timed_exit_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        timed_exit_tick.tick().await; // discard the immediate first tick
        #[cfg(feature = "hotpath")]
        if let Some(window) = crate::profiling::timed_exit_window() {
            let flag = Arc::clone(&self.shutdown);
            op_info!(
                domain = pump,
                window_ms = window.as_millis() as u64,
                "timed exit: cooperative pump shutdown armed"
            );
            tokio::spawn(async move {
                tokio::time::sleep(window).await;
                op_info!(
                    domain = pump,
                    "timed exit: HOTPATH_SHUTDOWN_MS window elapsed — raising pump shutdown"
                );
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            });
        }

        let relevant_topic_set: HashSet<B256> = RELEVANT_TOPICS.into_iter().collect();

        // Read the last block processed by the engine (the post-backfill
        // cursor when the snapshot→WS gap was closed inside resume; cold-start
        // otherwise). J3FMDO: the core `BlockPump::backfill_from_snapshot`
        // applies state via `BotState::process_backfill_logs`, which advances
        // neither the solve/finalize hooks' cursor nor the engine's
        // `last_processed_block`. Hence on the post-backfill resume path the
        // engine's `last_processed_block` is still `None` and the branch below
        // re-anchors on `first_observed_block`. (SZJUKL: the dissolved
        // coordinator cursor — `last_drained_block` under `drain_lock` — is
        // gone; work runs inline in this single-writer driver, so the engine
        // cursor IS the drained cursor.)
        let mut current_block: u64 = self.control.last_processed_block().map_or(0, Epoch::block);

        let snapshot_seed = self
            .bot
            .state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .snapshot_seed_block();
        if current_block == 0 && first_observed_block > 0 {
            current_block = first_observed_block;
            // One resume/cold-start line either way (audit: the two identical
            // cold-start lines across branches were collapsed).
            if matches!(snapshot_seed, Some(seed) if seed > 0 && seed < first_observed_block) {
                let seed = snapshot_seed.unwrap_or_default();
                op_info!(
                    domain = pump,
                    first_observed_block,
                    backfill_start = seed + 1,
                    backfill_end = first_observed_block,
                    "BlockPump: resuming from block (backfilled snapshot gap)"
                );
            } else {
                op_info!(
                    domain = pump,
                    first_observed_block,
                    "BlockPump: cold start from block"
                );
            }
        } else {
            op_info!(
                domain = pump,
                current_block,
                "BlockPump: starting from block"
            );
        }

        // Track the last block we've solved for: owned by the engine (the
        // pump's `last_solved_block` local is retired).
        // Seed it to the pump's starting block so the first `finalize_block`
        // guard fires only on a genuine advance (matching the prior local
        // init). A mid-flight-joining engine inherits via `set_last_solved_block`
        // (ADR-006 D4).
        self.control.set_last_solved_block(Epoch::at(current_block));
        // Seed the cold-start solve-results anchor to the settled resume
        // boundary (`current_block` = `first_observed_block` = backfill end):
        // `results_block` is 0 until the first real `on_drain` solve, but
        // registration eagerly solves paths over this backfilled (tip-persisting)
        // state and would otherwise deliver at block 0 or be deferred until the
        // first dirty event. Anchoring it to the settled resume block (a
        // completed, fully-applied block within the backfill window) lets those
        // candidates deliver immediately at a valid, verification-safe solve
        // block — NOT the chain head, which a partially-applied live event could
        // race past the backfill window.
        self.control.set_solve_anchor(Epoch::at(current_block));
        // Whether we're past the first header after resume. The first
        // Epic A1: the pump's decision state now lives in the StageMachine; the
        // driver routes the decision arms through it. `current_block` seeds the FSM.
        let mut fsm = StageMachine::new(current_block, 0);
        // the FSM owns the adaptive quiesce estimator (pure) — the
        // pump hands it the operator-tuned parameter snapshot once and then
        // only feeds settle-point observations and reads the armed window.
        fsm.set_quiesce_params(self.quiesce_params);
        // DFQYM5 single-writer, now FSM-owned: on a resume
        // where the snapshot→WS gap was backfilled (S < W), the backfill owns
        // [S+1, W] inclusive and the live WS owns [W+1, ∞). Seed the FSM's
        // recovery anchor with W so `should_drop_recovered_forward` is the
        // single owner of the boundary drop rule — reorgs stay exempt (they
        // must reach the reorg classifier), and no inline duplicate remains.
        if snapshot_seed.is_some_and(|s| s > 0 && s < first_observed_block) {
            fsm.record_backfill(first_observed_block);
        }
        // header establishes our anchor but shouldn't trigger a solve
        // (backfill already solved up to this point).

        // Current block metadata — updated from headers, used for
        // solve batches when logs close out a block.

        // WS-delivery completeness tracker (see `assert_ws_block_complete`):
        // the set of relevant-topic log indices delivered per block, cross-
        // checked against `eth_getLogs` at the block's tombstone to panic on a
        // live websocket log drop. Default-ON (`DEGENBOT_WS_COMPLETENESS`, via
        // `bot_env_flag_default_on`; disable with `=0`); the map is only
        // populated when the gate is on (so the hot loop adds no work when
        // disabled).
        let ws_completeness_enabled = self.ws_completeness_enabled;

        // `has_logs_this_block` is engine-owned — driven through
        // `self.sink.record_logs_this_block()` (cleared by `finalize_block`).
        // Debounce timer: started when the first dirty log arrives, reset on
        // each new log. When it fires, we send the accumulated result batch
        // to Python. This ensures one dispatch per burst of logs rather than
        // one per individual log.
        // ADR-008 D2: solver-release gate (see the flush in the Err + Ok(None) arms
        // below). `publish_pending` is armed when a forward log applies; the
        // flush fires `on_send` (gated on `consume_quiesced`) at a settle
        // point — a `DEBOUNCE_MS` window with no new event (coalescing a
        // same-block burst into one publish at the tail) OR stream exhaustion.
        // Replaces the wall-clock `DEBOUNCE_MS` send timer: publication is
        // gated on the truth condition (all dispatched logs applied).

        // FSM recovery state: `recovery_anchor` is the highest block an
        // authoritative (eth_getLogs) catch-up has OWNEed — either a live-loop
        // gap/`handle_timeout_eager` backfill, or (at resume) the backfilled
        // snapshot→WS first block. Per the single-writer rule (DFQYM5
        // precedent), the live WS NO LONGER owns any block ≤ `recovery_anchor`:
        // when a stalled WS recovers and flushes buffered forward logs for
        // those blocks, they are duplicates of state we already applied and are
        // dropped (they never reach the `LateForward` benign late-admit
        // drop). Reorg logs (`removed: true`) are NEVER dropped — they always
        // reach the reorg classifier. A forward ABOVE `recovery_anchor` that
        // is stale takes the same benign `LateForward` drop as any other
        // post-tombstone survivor: lateness is counted noise, never
        // a fatal signal — only blocks the pump itself backfilled are silent
        // duplicates by construction).

        // ADR-008 per-block state machine. The clock is the authority for
        // block completeness (the tombstone) and the cursor; the pump loop is
        // a thin async driver translating its decisions into stage-hook
        // calls + backfill + shutdown. A header alone NEVER advances the cursor —
        // only `advance_to_drained` (after the tombstone) does.

        // Per-block metadata, snapshotted from each block's header. A block's
        // tombstone (first log for N+1) may arrive AFTER header N+1 overwrote
        // `current_metadata`, so the result batch that finalizes N must carry
        // N's OWN metadata, retrieved here (VTWCIG).

        // [DIAG] newHeads-stall counters — owned by the `PumpTelemetry` seam
        // (`diag_header_count`/`diag_log_count`/`last_header_at`/stats all live
        // inside it; the driver just calls `on_header`/`on_log`/`maybe_stats`).
        let mut telemetry = crate::bot_core::pump_telemetry::PumpTelemetry::new();
        // Logs-subscription liveness watchdog (the INVERSE of
        // `header_staleness`): anchored at pump start and refreshed on EVERY
        // `WsEvent::Log` (before the topic pre-filter, so an irrelevant log
        // still proves the `eth_subscribe "logs"` arm is alive). When the
        // staleness tick wins and headers are FRESH but this has elapsed past
        // `self.log_silence`, the logs sub is presumed stalled → one warning
        // per silence episode (re-armed when the next log resumes).
        // (the logs-silence clock + re-arm alarm now live in the FSM, fed via
        // `record_log`; the telemetry seam owns the DIAG gap anchor).

        // SZJUKL seam retirement: NO dispatch owner, NO drain FIFO, NO
        // background drainer task. The stage hooks run INLINE at the
        // machine's decision points (below), so the B4GX7C drainer-liveness
        // machinery (`DrainerHealth`/`StallWatch`/closed-channel abort) has
        // no separate task to police and is DELETED. The dissolved
        // `DrainerHealth`'s no-progress obligation maps onto the machine's
        // `WatchdogPhase` — a driver that stops making stall-window progress
        // stops accepting headers, and the header-staleness watchdog
        // (`StageDecision::Recover`) fires exactly as before; the logs-silence
        // watchdog covers the inverse. There is no queue left to go silently
        // dead while the loop advances.

        // JIABO3 Option A — header-staleness watchdog. A `tokio::time::interval`
        // selected against `combined.next()` (below) whose internal `Sleep`
        // elapses independently of stream activity. This catches a silent
        // `newHeads` (dead/stalled WS subscription) even under dense-log
        // pressure, where the in-loop `timeout(.. combined.next())` `Err(_)`
        // no-activity path never elapses because `combined.next()` keeps
        // yielding logs. When the tick wins the select AND headers are
        // genuinely stale (>= `header_staleness`), it runs the SAME
        // `handle_timeout_eager` catch-up the no-activity path uses.
        //
        // Limitation (documented in JIABO3 Option A): this fires only when the
        // pump is parked AT the select. If the pump parks BEFORE the select
        // (GIL re-entry park via `PySubscriberAdapter`, or engine-lock
        // contention inside `on_drain`/`apply_buffer_v3`), the interval can't
        // advance — that residual unbounded risk is Option B's
        // notify-delocalization work, out of scope here.
        let mut staleness_tick = tokio::time::interval(self.watchdog.header_staleness);
        staleness_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        staleness_tick.tick().await; // discard the immediate first tick
                                     // A4: a monotonic ms epoch feeding the FSM's pure watchdog `Tick` input
                                     // (time enters as data; the FSM owns no timer or `Instant`).
        let tick_epoch = tokio::time::Instant::now();
        let now_ms = || tick_epoch.elapsed().as_millis() as u64;

        // the current block's span, replaced by each accepted header.
        let mut block_span: Option<tracing::Span> = None;

        // Pre-solve gap decomposition (GC? tracking epilogue to the pump/`
        // solve-gap investigation): per-block marks so the gap between the
        // `degenbot.epoch` span start (header accepted) and the solve
        // dispatch decomposes into WS delivery (header → first relevant
        // log), burst jitter (first → last log), and the settle wait (last
        // log → settle decision). Reset on every accepted header; recorded
        // onto the block span + the three instruments at the settle point.
        let mut pregap = PreSolveGapTrack {
            header_at: std::time::Instant::now(),
            first_log: None,
            last_log: None,
            logs: 0,
        };

        // per-block intra-block silence-gap tracker: the max gap
        // between consecutive relevant logs feeds the FSM's adaptive
        // trailing-quiesce EWMA at each settle point (timings arrive as
        // data; the FSM owns no clock). Reset at each accepted header.
        let mut last_relevant_log_at: Option<std::time::Instant> = None;
        let mut block_max_gap_us: u64 = 0;

        // `Some` from the first gate iteration
        // that observed unsolved dirt in the current block window; the slice
        // fires once when the age crosses `early_slice_ms`. `slice_done`
        // makes it one-per-window. Reset at each accepted header (new window)
        // and at each settle dispatch. `tokio::time::Instant` (not std) so
        // paused-runtime tests advance the age with virtual time.
        let mut slice_first_dirty: Option<tokio::time::Instant> = None;
        let mut slice_done = false;

        // WAJEQP T-R1 reorg-window telemetry state: the episode span + its
        // per-window counters live ACROSS loop iterations (EnterReorg →
        // CloseReorg). The window span is its own trace root (episodes cross
        // block windows); the `restore` children are emitted by the
        // coordinator with this span as their explicit parent.
        let mut reorg_span: Option<tracing::Span> = None;
        let mut reorg_pools_restored: u64 = 0;
        let mut reorg_idempotent_noops: u64 = 0;
        // the per-stage waterfall seam. The legacy
        // `pump.log_wait` / `pump.apply_stream` children are replaced by the
        // machine's stage cycle rendered as `degenbot.stage.*` spans under
        // the per-epoch root, and the SONJQA force-close law carries over
        // (`force_close_aged` from the timed-exit tick below).
        let mut stage_tel = crate::bot_core::stage_telemetry::StageTelemetry::new();
        // per-block phase attribution - the apply-stream start
        // (first relevant log) vs the settle point, recorded on the throttled
        // diag line so slow-block serialization between the WS log wait and
        // the solve is visible from the console. (The apply-stream span
        // itself was folded into the stage waterfall's streaming interval.)
        let mut apply_started_at: Option<std::time::Instant> = None;
        loop {
            // Span lifecycle: an enter guard must never outlive a
            // single poll. This task runs on a multi-threaded tokio runtime and
            // may migrate between worker threads at any `.await`; a guard
            // entered on one thread and dropped on another leaks the span's
            // entered state in that worker's TLS forever (observed as 23
            // nested pump.block spans; the leaked spans never close, so OTel
            // never exports them and every child span orphanes in Jaeger).
            // No loop-wide enter here: each dispatch site enters the cursor
            // span in a strictly-synchronous scope and the few futures that
            // must carry block context across an await are wrapped with
            // `.instrument(…)` instead.
            //
            // Solve execution moved OUT of the loop head to the drained-settle
            // gate at the bottom of the loop: the solver
            // must not fire while buffered WS events are still unprocessed —
            // the 2026-08-22 stall crash was exactly the loop-head solve
            // racing a still-queued swap log.

            // ADR-008 D2: solver-release gate. `fsm.publish_pending` is set when a forward
            // log applies (block becomes quiesced). The flush below fires
            // the Published-edge `on_publish` (gated on `consume_quiesced`) only at a
            // settle point — a timeout with no new event (coalescing a
            // same-block burst into one publish at the tail) OR stream
            // exhaustion. This replaces the wall-clock `DEBOUNCE_MS` send
            // timer: publication is gated on the truth condition (all
            // dispatched logs applied), not schedule.

            // Check shutdown
            if self.shutdown.load(Ordering::Relaxed) {
                op_info!(domain = pump, "BlockPump: shutting down");
                return;
            }

            // Wait for the next event. Use a shorter settle window when a publish is
            // pending so the quiesce-gated flush fires promptly if no new log
            // arrives (coalescing a same-block burst); otherwise the long
            // inactivity backfill window. A new event arriving before the
            // window elapses cancels the flush (the burst is still in flight).
            let wait_timeout = if fsm.publish_pending() {
                // the settle timers arm the FSM's window (fixed mode
                // = the debounce history; adaptive = the estimator's current
                // W) instead of the raw debounce field.
                Duration::from_millis(fsm.settle_window_ms())
            } else {
                Duration::from_secs(BACKFILL_TIMEOUT_SECS)
            };
            let event = tokio::select! {
                biased;
                // S53STH cooperative timed exit: the hotpath timer raises the
                // shutdown flag; this arm polls it every 500ms so the parked
                // select wakes promptly (worst case otherwise: one full
                // BACKFILL_TIMEOUT_SECS park). The loop-head shutdown check
                // then exits and unwinds all span guards on this task. A tick
                // is free relative to the window (minutes) it serves.
                _ = timed_exit_tick.tick() => {
                    if self.shutdown.load(Ordering::Relaxed) {
                        op_info!(domain = pump, "timed exit: shutdown signaled — unwinding pump loop");
                        break;
                    }
                    // Preserved G3: force-close a stale
                    // held stage interval. The epoch root exports when the
                    // header arm's ENTERED scope exits (entry-refcount
                    // law) - microseconds after header acceptance on an
                    // all-quiet block - so an open stage span dangling until
                    // the next transition would extend a waterfall child far
                    // past a closed parent (trace a1ad51bd, block 25913381:
                    // 12.7s child on a 319us parent). Bound the child with an
                    // explicit stall event.
                    stage_tel.force_close_aged(self.stage_max_age);
                    // Flag not yet raised: re-park. `continue` keeps both arm
                    // paths diverging so the arm types coerce to the event
                    // arm's `Option<WsEvent>`.
                    continue;
                }
                // JIABO3 header-staleness watchdog — see the interval setup
                // above. Firing here does NOT consume the stream event; it runs
                // `handle_timeout_eager` then re-loops (the top-of-loop drain
                // picks up any dirty paths the backfill created). The
                // `timeout(wait_timeout, combined.next())` future is dropped on
                // this arm winning, so the inactivity/debounce countdown
                // restarts — acceptable since `DEBOUNCE_MS << header_staleness`
                // and the no-activity path is now superseded by this watchdog.
                _ = staleness_tick.tick() => {
                    // A4: the watchdog window decision lives in the FSM
                    // (`on_tick`), fed a synthetic `now_ms`; the interval only
                    // drives it. The driver executes the emitted decisions.
                    for decision in fsm.on_tick(
                        now_ms(),
                        self.watchdog.header_staleness.as_millis() as u64,
                        self.watchdog.log_silence.as_millis() as u64,
                    ) {
                        match decision {
                            StageDecision::Recover => {
                                self.handle_timeout_eager(&mut fsm)
                                    .instrument(block_span.clone().unwrap_or_else(tracing::Span::none))
                                    .await;
                            }
                            StageDecision::LogSilence => {
                                // Logs-subscription liveness watchdog (inverse
                                // of header staleness): headers are FRESH (the
                                // Recover branch did not fire) but no
                                // `WsEvent::Log` arrived in `self.log_silence`
                                // the `eth_subscribe "logs"` arm is presumed
                                // stalled/dead while `newHeads` is alive. One
                                // warning per silence episode (re-armed when
                                // the next log resumes the sub).
                                op_warn!(domain = pump, silence_secs = self.watchdog.log_silence.as_secs(),
                                    "logs subscription silent: headers flowing but no log"
                                );
                                self.watchdog.record_silence_alarm();
                            }
                            other => unreachable!(
                                "on_tick only emits Recover|LogSilence, got {other:?}"
                            ),
                        }
                    }
                    continue;
                }
                event = timeout(wait_timeout, combined.next()) => event,
            };

            match event {
                // Settle point — no new event in the window. Flush the
                // quiesce-gated publish, OR (if nothing pending) the 60s
                // inactivity backfill path.
                Err(_) => {
                    // A2: settle-point rules live in the FSM (`on_settle`)
                    // the quiesce-before-publish gate + solver-release gate
                    // (ADR-008 D2) vs the inactivity backfill. The driver only
                    // executes the emitted decisions.
                    //
                    // feed the settled block's observed max silence
                    // gap (when any relevant log arrived) so the estimator
                    // re-arms W for the NEXT settle, publish the current W on
                    // the quiesce-window gauge, and measure the
                    // on_settle-entry-to-decision latency in the hotpath
                    // profiler (open item #1 — the 8.4%-of-blocks
                    // settle-overshoot suspects become visible as a bucket).
                    if pregap.logs > 0 {
                        fsm.observe_settle_gap(block_max_gap_us / 1000, now_ms());
                    }
                    if let Some(p) = crate::instruments::pipeline() {
                        p.observe_quiesce_window(fsm.settle_window_ms());
                    }
                    let settle_decisions =
                        hotpath::measure_block!("pump.settle_decision", fsm.on_settle());
                    for decision in settle_decisions {
                        match decision {
                            StageDecision::Publish { open, metadata } => {
                                // Option-A solver-state accuracy gate:
                                // publish the debounced batch to Python (the
                                // Published edge — delivery/submission/Python
                                // subscribe HERE, SZJUKL), then hand the
                                // quiesced `open` block + its change set to the
                                // latest-wins verifier task. The anchor is
                                // `open`, the LOG-DRIVEN quiesced block, NOT the
                                // racing header.
                                // Pre-solve gap decomposition (Jaeger span
                                // fields + pregap histograms): delivery, burst
                                // jitter, and the settle wait each own a
                                // measured slice of the block-to-solve gap so
                                // the opaque pump.span stretch stops hiding
                                // the WS-delivery and debounce components.
                                {
                                    let now = std::time::Instant::now();
                                    let (header_us, burst_us, settle_us, logs) =
                                        match (pregap.first_log, pregap.last_log) {
                                            (Some(f), Some(last)) => (
                                                Some(
                                                    f.saturating_duration_since(pregap.header_at)
                                                        .as_micros()
                                                        as u64,
                                                ),
                                                Some(last.saturating_duration_since(f).as_micros()
                                                    as u64),
                                                Some(
                                                    now.saturating_duration_since(last).as_micros()
                                                        as u64,
                                                ),
                                                pregap.logs,
                                            ),
                                            _ => (None, None, None, pregap.logs),
                                        };
                                    if let Some(span_ref) = block_span.as_ref() {
                                        span_ref.record("pregap.logs", logs);
                                        if let Some(us) = header_us {
                                            span_ref.record("header_to_first_log_us", us);
                                        }
                                        if let Some(us) = burst_us {
                                            span_ref.record("log_burst_us", us);
                                        }
                                        if let Some(us) = settle_us {
                                            span_ref.record("settle_wait_us", us);
                                        }
                                    }
                                    if let Some(p) = crate::instruments::pipeline() {
                                        if let Some(us) = header_us {
                                            p.observe_header_to_first_log(us_to_secs(us));
                                            // ok: us is small
                                        }
                                        if let Some(us) = burst_us {
                                            p.observe_log_burst(us_to_secs(us));
                                        }
                                        if let Some(us) = settle_us {
                                            p.observe_settle_wait(us_to_secs(us));
                                        }
                                    }
                                }
                                // the publish stage span (parented to
                                // this epoch's root) carries the from/to/
                                // queue-age attrs; the publish-cycle histogram
                                // (first relevant log → publish, per quiesce
                                // cycle) is recorded with it. The solve spans
                                // that follow sit beside it under the same
                                // epoch root.
                                stage_tel.on_publish(
                                    block_span.as_ref().unwrap_or(&tracing::Span::none()),
                                    Epoch::with_generation(open, fsm.rewind_seq()),
                                );
                                // throttled per-block phase
                                // attribution on the console (every 20th block
                                // - the Jaeger span carries all blocks).
                                #[expect(clippy::items_after_statements)]
                                static DIAG_ATTEMPT: std::sync::atomic::AtomicU32 =
                                    std::sync::atomic::AtomicU32::new(0);
                                let nth =
                                    DIAG_ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                if nth.is_multiple_of(20) {
                                    let apply_us = apply_started_at
                                        .map_or(0, |t| t.elapsed().as_micros() as u64);
                                    let (hw, lw) = (pregap.header_at, pregap.first_log);
                                    op_info!(
                                        domain = pump,
                                        block_number = open,
                                        sequence = nth,
                                        logs = pregap.logs,
                                        header_to_first_log_us = lw.map(|t| {
                                            t.saturating_duration_since(hw).as_micros() as u64
                                        }),
                                        apply_stream_us = apply_us,
                                        "per-block phase attribution (throttled)"
                                    );
                                }
                                let _ctx = block_span.as_ref().map(tracing::Span::enter);
                                self.drive_publish(
                                    &fsm,
                                    fsm.context_for(open, metadata),
                                    &GateOutcome::default(),
                                );
                            }
                            StageDecision::Backfill { from, to } => {
                                // No activity for 60s — backfill `[from, to)`.
                                debug_assert!(from == fsm.current_block() + 1 && to.is_none());
                                self.handle_timeout_eager(&mut fsm)
                                    .instrument(
                                        block_span.clone().unwrap_or_else(tracing::Span::none),
                                    )
                                    .await;
                            }
                            other => {
                                unreachable!("on_settle only emits Publish|Backfill, got {other:?}")
                            }
                        }
                    }
                }

                // Got a block header from the combined stream
                Ok(Some(WsEvent::BlockHeader {
                    number,
                    timestamp,
                    base_fee_per_gas,
                    gas_used,
                    gas_limit,
                })) => {
                    // The
                    // per-epoch beat — one entered root span per observed
                    // header, carrying the EPOCH context (block + rewind
                    // generation) every span in the epoch waterfall answers
                    // to. Future solver/submission/stage spans fired within
                    // this arm nest under it for free.
                    let epoch_seq = fsm.rewind_seq();
                    let new_block_span = tracing::info_span!(
                        "degenbot.epoch.run",
                        epoch.block = number,
                        epoch.seq = epoch_seq,
                        block.number = number,
                        // pre-solve gap decomposition, recorded at the settle
                        // point (declared Empty so `record` at settle actually
                        // lands — undeclared fields are silently dropped by
                        // the OTel pass-through)
                        pregap.logs = tracing::field::Empty,
                        header_to_first_log_us = tracing::field::Empty,
                        log_burst_us = tracing::field::Empty,
                        settle_wait_us = tracing::field::Empty,
                        // WAJEQP T-R1 reorg breadcrumbs: when a reorg episode
                        // opens or closes while THIS block window is current,
                        // the block trace is silently interrupted — surface it
                        // here so an operator reading the block trace sees the
                        // interruption without opening the window root trace.
                        reorg.entry_block = tracing::field::Empty,
                        reorg.closed = tracing::field::Empty,
                    );
                    // MQUKB6-T0 / JYCTXI: detached-at-creation so each header
                    // span is its own trace ROOT (the detach + its reasoning
                    // live on the telemetry seam). Children (logs, solves,
                    // dispatch) nest under it via the loop-context below; only
                    // the parent linkage at creation changes.
                    crate::telemetry::make_trace_root(&new_block_span);
                    // this span becomes the loop's per-block context —
                    // subsequent iterations (logs, settle decisions) nest under it
                    // until the next header replaces it.
                    // A span minted while no subscriber is installed carries
                    // NoSubscriber's 0xDEAD sentinel id; keep it out of the loop
                    // context so a subscriber installed mid-run cannot turn the
                    // next `parent:` clone into a Registry::clone_span panic.
                    block_span = crate::telemetry::subscriber_backed_span(&new_block_span);
                    // Pre-solve gap marks: this header is the clock anchor
                    // for the new block's gap decomposition.
                    pregap = PreSolveGapTrack {
                        header_at: std::time::Instant::now(),
                        first_log: None,
                        last_log: None,
                        logs: 0,
                    };
                    // restart the silence-gap tracker for the new
                    // block window.
                    last_relevant_log_at = None;
                    block_max_gap_us = 0;
                    // new block window — re-arm the early slice.
                    slice_first_dirty = None;
                    slice_done = false;
                    // a new epoch root — close any held stage interval
                    // from the prior epoch (an all-quiet prior block never
                    // reached a settle) so nothing dangles into the new one.
                    stage_tel.new_epoch();
                    // the epoch that just closed is fully accounted —
                    // sample its log ledger into the per-block funnel gauges.
                    let ledger = self.bot.dispatcher().snapshot_epoch_logs_and_reset();
                    if let Some(p) = crate::instruments::pipeline() {
                        p.observe_epoch_logs(
                            ledger.seen,
                            ledger.received,
                            ledger.applied,
                            ledger.undecoded + ledger.apply_missed,
                            number,
                        );
                    }
                    // Sync-only header-processing scope: this enter
                    // guard dies before the first await below, so it can never
                    // leak across a task migration. The backfill future below
                    // carries the same span across ITS await via Instrument.
                    {
                        let _ctx = new_block_span.enter();
                        // [DIAG] newHeads-liveness: HEADER count, gap, and 20s stall
                        // warning → one call on the telemetry seam.
                        telemetry.on_header(number);
                        // block-cadence sample for the runtime
                        // mimalloc purge-delay control (hysteresis-limited
                        // option write; no-op unless allocator-ctrl + auto).
                        crate::allocator_ctrl::on_header_observed();
                        // T2: blocks-observed counter + the header→solved anchor.
                        if let Some(p) = crate::instruments::pipeline() {
                            p.count_block();
                            // follow-up: the kernel throttle counters
                            // that identified the >10s solve p95 belong on the
                            // dashboard, one sample per block cadence.
                            if let Some(stats) = degenbot_core::cpu_budget::cgroup_throttle_delta()
                            {
                                p.observe_cgroup_throttled(
                                    stats.nr_throttled,
                                    stats.throttled_usec,
                                );
                                // LW-T5 (Seam E), re-routed by JCI2FW
                                // Part A: the SAME per-block sample feeds
                                // the ONE process fleet posture owner
                                // (`degenbot_workers::posture::process()`
                                // the Executor seam channel is
                                // dissolved). The pump owns the
                                // header-cadence delta
                                // (LAST_HEADER_SAMPLE_MS). Always fed.
                                feed_executor_throttle_sample(
                                    wall_ms(),
                                    stats.nr_throttled,
                                    stats.throttled_usec,
                                );
                            }
                        }
                        // ADR-041: the header→publish epoch-race anchor
                        // (the dissolved DispatchOwner drainer's header→solved
                        // anchor, re-stamped at the Published edge; single-writer
                        // pump task). Wall clock — see `wall_ms`.
                        self.header_ms
                            .store(wall_ms(), std::sync::atomic::Ordering::Relaxed);
                    }
                    // ADR-028: THE header decision lives in the FSM. Feeding
                    // the header (metadata + a wall-clock `now_ms` for the
                    // watchdog anchors) emits, in order, the effects the driver
                    // must execute (Backfill → SetLastSolved → Notify); the FSM
                    // owns every cursor/metadata/anchor transition. The inline
                    // `is_first_header` copy that used to live here is gone —
                    // `on_header` is the single authoritative header handler.
                    let metadata = BlockMetadata {
                        timestamp,
                        base_fee_per_gas,
                        gas_used,
                        gas_limit,
                    };
                    for decision in fsm.on_header(number, metadata, now_ms()) {
                        match decision {
                            StageDecision::Backfill { from, to } => {
                                // Header-gap catch-up over `[from, to]`
                                // (ephemeral, header-driven). The decision's
                                // explicit range is authoritative — the FSM has
                                // already advanced its own cursor past it, so a
                                // `current_block + 1`-derived range would be
                                // wrong here (BQ7ZBC single-writer anchor is set
                                // inside `on_header`).
                                let to = to.unwrap_or_else(|| {
                                    unreachable!("on_header backfill always carries an upper bound")
                                });
                                op_info!(
                                    domain = pump,
                                    from_block = from,
                                    to_block = to,
                                    "BlockPump: gap from block to block — backfilling"
                                );
                                self.backfill_range(from, to, &mut fsm)
                                    .instrument(new_block_span.clone())
                                    .await;
                            }
                            StageDecision::SetLastSolved { block } => {
                                // the backfill/first header solved up
                                // to `block` already — mark it solved so the
                                // first `finalize_block` guard no-ops.
                                let _ctx = new_block_span.enter();
                                self.control.set_last_solved_block(Epoch::at(block));
                            }
                            StageDecision::Notify { block, metadata } => {
                                // Python's block fsm tracks `newHeads` — the
                                // block-clock pipe (delivery-to-Python at the
                                // async boundary; never queued behind solve work).
                                let _ctx = new_block_span.enter();
                                self.control.notify_block(block, &metadata);
                            }
                            other => {
                                unreachable!("on_header only emits Backfill|SetLastSolved|Notify, got {other:?}")
                            }
                        }
                    }
                    // The `PendingSuccessor` / `OpenNew` decisions carry no
                    // pump action beyond the above — the liveness-probe signal
                    // (dead-logs-sub detection) is handled by the timeout path.
                }

                // Got a log event from the combined stream — apply eagerly.
                // Solve happens at the top of the next iteration. Batch send
                // is debounced — the timer starts/resets on each log.
                Ok(Some(WsEvent::Pool(pe))) => {
                    // (5WTYYQ) The ingestion crate emits the structured
                    // PoolEvent { epoch, log_index, payload }; the apply path
                    // consumes the raw payload.
                    let log = pe.payload;
                    // WS-delivery volume signal (pre topic-filter): pairs with
                    // `degenbot.logs.received` (relevant subset) so a WS feed
                    // that stops delivering relevant logs stays distinguishable
                    // from one that stopped delivering logs at all.
                    if let Some(p) = crate::instruments::pipeline() {
                        p.count_ws_log_seen();
                    }
                    // the funnel's `seen` leg — tallied at the event
                    // source (pre topic-filter) exactly like the instrument.
                    self.bot.dispatcher().inc_seen();
                    // Logs-subscription liveness: ANY log (even one the topic
                    // pre-filter drops below) proves the `eth_subscribe
                    // "logs"` arm is delivering. Refresh before the pre-filter
                    // and re-arm the silence alarm so a single warning fires
                    // per silence episode (not per tick) — fed to the FSM.
                    fsm.record_log(now_ms());
                    // Fast-path topic pre-filter: the `logs` WS subscription
                    // is unfiltered (no topic/address filter on the server —
                    // see `stream_select`), so the overwhelming majority of
                    // logs here are irrelevant to any pool we track. Checking
                    // topic0 against `RELEVANT_TOPICS` *before* acquiring
                    // `engine.lock()` (a parking_lot mutex) and running the
                    // decoders skips the lock + decode work for those logs,
                    // keeping the hot path off the contention path. This is
                    // NOT redundant with the topic re-match inside
                    // `apply_log` — that re-check is defensive, so `apply_log`
                    // stays safe to call with unfiltered inputs (e.g. from
                    // backfill or tests). Do not remove this pre-filter: it
                    // is the lock-avoidance fast path.
                    if !relevant_topic_set.contains(log.topics().first().unwrap_or(&B256::ZERO)) {
                        continue;
                    }

                    let log_block = log.block_number.unwrap_or(fsm.current_block());
                    // Pre-solve gap marks: a relevant delivered log. First-log
                    // marks the WS delivery latency phase; last-log feeds the
                    // settle wait at the settle point.
                    {
                        let now = std::time::Instant::now();
                        if pregap.first_log.is_none() {
                            pregap.first_log = Some(now);
                        }
                        pregap.last_log = Some(now);
                        pregap.logs += 1;
                        // consecutive-relevant-log silence deltas —
                        // the exact r.v. the adaptive trailing quiesce must
                        // cover (design §2.1 intra-block gaps).
                        if let Some(prev) = last_relevant_log_at {
                            let gap_us = now.saturating_duration_since(prev).as_micros() as u64;
                            if gap_us > block_max_gap_us {
                                block_max_gap_us = gap_us;
                            }
                        }
                        last_relevant_log_at = Some(now);
                    }
                    // the Streaming stage interval opens at the first
                    // relevant log of the epoch (idempotent within the epoch —
                    // the burst's remaining logs only bump its age); it runs
                    // until the quiesce/tombstone/rewind transition. REMED1 T3
                    // keeps the apply-start anchor for the throttled diag line.
                    if apply_started_at.is_none() {
                        apply_started_at = Some(std::time::Instant::now());
                    }
                    stage_tel.on_first_log(
                        block_span.as_ref().unwrap_or(&tracing::Span::none()),
                        Epoch::with_generation(log_block, fsm.rewind_seq()),
                    );
                    // FSM single-writer recovery discard. After an
                    // authoritative eth_getLogs catch-up (`fsm.recovery_anchor`), a
                    // stalled WS that recovers flushes buffered forward logs for
                    // blocks ≤ the anchor — those are duplicates of state the
                    // backfill already applied and are DROPPED (they never reach
                    // `observe_log`'s `LateForward` class). This mirrors the DFQYM5
                    // resume-boundary rule, generalized to mid-run recovery.
                    // Reorg logs (`removed: true`) are NEVER dropped — they must
                    // reach the reorg classifier to unwind the backfilled range.
                    // A forward ABOVE `recovery_anchor` that is still stale
                    // remains a hard ADR-008 D3 fault (only the pump's own
                    // single-writer range is benign).
                    if fsm.should_drop_recovered_forward(log_block, log.removed) {
                        // WAJEQP T-R1: a recovery-dropped log during an OPEN
                        // reorg window is episode evidence — emit it as a
                        // child span so the window trace shows which replay
                        // events were discarded (outside a window it is
                        // routine resume noise; only the log line remains).
                        if let Some(window) = reorg_span.as_ref() {
                            drop(tracing::info_span!(
                                parent: window.clone(),
                                "degenbot.reorg.dropped_recovery",
                                reorg.block = log_block,
                            ));
                        }
                        if let Some(p) = crate::instruments::pipeline() {
                            p.count_reorg_recovery_dropped();
                        }
                        crate::bot_core::apply_telemetry::trace_ws_log_dispatch(
                            log.address(),
                            log.topics(),
                            log_block,
                            log.log_index,
                            log.transaction_index,
                            log.removed,
                            "DroppedRecovery",
                        );
                        continue;
                    }
                    // WS-completeness tracker: record the delivered relevant
                    // log index for this block so the tombstone can cross-check
                    // it against authoritative on-chain logs (a missing index =
                    // a websocket drop → panic). Only tracked when the gate is
                    // on to keep the default hot loop at zero-cost.
                    if ws_completeness_enabled {
                        if let Some(li) = log.log_index {
                            fsm.record_ws_delivered(log_block, li);
                        }
                    }

                    // ADR-008: route the log via the per-block state machine.
                    // The FSM owns the clock transition + cursor + publish
                    // disarm (ADR-028): `on_log` decides whether this is a
                    // forward dispatch, a tombstone (first removed:false log
                    // for N+1), a reorg signal, or an unreliable-WS late
                    // forward (→ shutdown), and returns the verdict for the
                    // driver to execute the I/O.
                    // the stage row BEFORE this log's transition —
                    // the `stage.from` side of the transition attrs below.
                    let prev_stage = fsm.stage();
                    let log_decision = fsm.on_log(log_block, log.removed);
                    // The reorg classification may have just bumped the
                    // rewind generation (I2). SZJUKL: the dissolved FIFO's
                    // `observe_rewind_seq` mirror is gone — the driver checks
                    // each work item's epoch INLINE at its execution site
                    // (`reorg_flying_stale`), so a stale item cannot slip
                    // through a queue because its check happened pre-bump.
                    // WS delivery trace: log EVERY relevant-topic WS log —
                    // block, log-index, tx-index, topic0, removed, and the fsm
                    // decision — so the delivery order of same-block Mint/Burn
                    // logs is visible against the registration drain+pin that
                    // follows. Always-on DEBUG on `ingest`.
                    crate::bot_core::apply_telemetry::trace_ws_log_dispatch(
                        log.address(),
                        log.topics(),
                        log_block,
                        log.log_index,
                        log.transaction_index,
                        log.removed,
                        match log_decision {
                            LogDecision::EnterReorg(_) => "EnterReorg",
                            LogDecision::ContinueReorg => "ContinueReorg",
                            LogDecision::CloseReorg { .. } => "CloseReorg",
                            LogDecision::TombstonePrevious(_) => "TombstonePrevious",
                            LogDecision::DispatchForward => "DispatchForward",
                            LogDecision::LateForward(_) => "LateForward",
                        },
                    );
                    match log_decision {
                        LogDecision::EnterReorg(reorg_block) => {
                            // Reorg: per-event per-pool restore via the
                            // coordinator (ADR-006 slice 7). A too-deep reorg
                            // → graceful shutdown. The previous block was
                            // tombstoned; this `removed: true` log reopens it.
                            // Visible operator signal so an unwind is no longer
                            // silent — the prior success path logged nothing,
                            // making a duplicate block log ambiguous (reorg
                            // vs. WS duplication).
                            // the Rewind stage opens (from ANY row —
                            // I6), counted for the A/B Rewind-frequency series.
                            stage_tel.on_enter_reorg(
                                block_span.as_ref().unwrap_or(&tracing::Span::none()),
                                Epoch::with_generation(log_block, fsm.rewind_seq()),
                                prev_stage,
                            );
                            op_warn!(domain = pump, reorg_block,
                                "BlockPump: chain reorg detected (removed log) — entering unwind path"
                            );
                            // WAJEQP T-R1: open the episode span — its OWN
                            // trace root (the episode crosses block windows;
                            // parenting it under the current block span would
                            // misattribute the unwind to the delivering block,
                            // the same disease the solve-span reparenting
                            // cured). Depth is the rollback distance at entry.
                            let depth_blocks = fsm.current_block().saturating_sub(reorg_block);
                            let window = tracing::info_span!(
                                "degenbot.reorg.window",
                                reorg.block = reorg_block,
                                reorg.log_block = log_block,
                                reorg.depth_blocks = depth_blocks,
                                reorg.pools_restored = tracing::field::Empty,
                                reorg.idempotent_noops = tracing::field::Empty,
                                reorg.new_head = tracing::field::Empty,
                                reorg.outcome = tracing::field::Empty,
                            );
                            crate::telemetry::make_trace_root(&window);
                            // WAJEQP T-R1 metrics: episode count + entry depth.
                            if let Some(p) = crate::instruments::pipeline() {
                                p.count_reorg_window();
                                p.observe_reorg_depth(depth_blocks);
                            }
                            if let Some(bs) = block_span.as_ref() {
                                bs.record("reorg.entry_block", reorg_block);
                            }
                            reorg_span = crate::telemetry::subscriber_backed_span(&window);
                            reorg_pools_restored = 0;
                            reorg_idempotent_noops = 0;
                            let outcome = {
                                let _entered = reorg_span.as_ref().map(tracing::Span::enter);
                                self.reorg_coordinator
                                    .dispatch_reorg_log(&log, reorg_span.as_ref())
                            };
                            match outcome {
                                Ok(crate::bot_core::reorg_coordinator::ReorgOutcome::Restored) => {
                                    reorg_pools_restored += 1;
                                    if let Some(p) = crate::instruments::pipeline() {
                                        p.count_reorg_unwound_pool();
                                    }
                                }
                                Ok(
                                    crate::bot_core::reorg_coordinator::ReorgOutcome::IdempotentNoop,
                                ) => {
                                    reorg_idempotent_noops += 1;
                                }
                                Err(err) => {
                                    // Too-deep: record the fault on the window
                                    // span FIRST so the last trace before the
                                    // graceful shutdown names the cause, then
                                    // unwind (span guards drop on return).
                                    if let Some(window) = reorg_span.as_ref() {
                                        window.record("reorg.outcome", "too_deep_shutdown");
                                    }
                                    op_error!(domain = pump, ?err, "BlockPump: too-deep reorg — shutting down");
                                    self.shutdown.store(true, Ordering::Relaxed);
                                    return;
                                }
                            }
                            // Cancel any pending publish: results accumulated
                            // from pre-reorg state are invalid (the FSM disarmed
                            // the publish in `on_log`).
                            continue;
                        }
                        LogDecision::ContinueReorg => {
                            // Subsequent removed: true log in the same window —
                            // restore another pool at `log_block`. Trailing the
                            // first event lets the operator correlate successive
                            // unwinds in the same reorg.
                            op_warn!(
                                domain = pump,
                                log_block,
                                "BlockPump: reorg continues — restoring pool for removed log"
                            );
                            let outcome = {
                                let _entered = reorg_span.as_ref().map(tracing::Span::enter);
                                self.reorg_coordinator
                                    .dispatch_reorg_log(&log, reorg_span.as_ref())
                            };
                            match outcome {
                                Ok(crate::bot_core::reorg_coordinator::ReorgOutcome::Restored) => {
                                    reorg_pools_restored += 1;
                                    if let Some(p) = crate::instruments::pipeline() {
                                        p.count_reorg_unwound_pool();
                                    }
                                }
                                Ok(
                                    crate::bot_core::reorg_coordinator::ReorgOutcome::IdempotentNoop,
                                ) => {
                                    reorg_idempotent_noops += 1;
                                }
                                Err(err) => {
                                    if let Some(window) = reorg_span.as_ref() {
                                        window.record("reorg.outcome", "too_deep_shutdown");
                                    }
                                    op_error!(domain = pump, ?err, "BlockPump: too-deep reorg — shutting down");
                                    self.shutdown.store(true, Ordering::Relaxed);
                                    return;
                                }
                            }
                            continue;
                        }
                        LogDecision::CloseReorg { new_head } => {
                            // Reorg window closed — the coordinator restored
                            // unwound pools per-event; this forward log's block
                            // is the new head. Resume forward tracking from it.
                            op_info!(
                                domain = pump,
                                new_head,
                                "BlockPump: reorg window closed — resuming forward tracking"
                            );
                            // WAJEQP T-R1: close the episode span with its
                            // counters + outcome, and leave a breadcrumb field
                            // on the current block window's span.
                            if let Some(window) = reorg_span.take() {
                                window.record("reorg.pools_restored", reorg_pools_restored);
                                window.record("reorg.idempotent_noops", reorg_idempotent_noops);
                                window.record("reorg.new_head", new_head);
                                window.record("reorg.outcome", "closed");
                                // Explicit drop: the window ends HERE, not at
                                // the next reassignment of the local.
                                drop(window);
                            }
                            if let Some(bs) = block_span.as_ref() {
                                bs.record("reorg.closed", new_head);
                            }
                            // the Rewind interval closes (its duration
                            // histogram records) and the fresh epoch's cycle
                            // restarts at Streaming.
                            stage_tel.on_close_reorg(
                                block_span.as_ref().unwrap_or(&tracing::Span::none()),
                                Epoch::with_generation(new_head, fsm.rewind_seq()),
                            );
                            reorg_pools_restored = 0;
                            reorg_idempotent_noops = 0;
                            // Fall through to dispatch this forward log (the FSM
                            // moved the cursor to `new_head` in `on_log`).
                        }
                        LogDecision::TombstonePrevious(prev) => {
                            // 3M5PO5 correction: this tombstone verdict is the
                            // pump's single writer of the delivery cutoff — `BotState`
                            // owns the value and the driver mirrors the verdict on
                            // execution (the same decision-execution pattern as the
                            // `set_last_solved_block` steps).
                            self.bot
                                .state_arc()
                                .write_at(crate::bot_core::state_lock::LockSite::Pump)
                                .advance_pump_complete_cutoff(prev);
                            // First removed:false log for N+1 → tombstone N.
                            // Finalize N with N's OWN metadata (snapshotted
                            // when N's header arrived), not fsm.current_metadata
                            // which may now hold N+1's — VTWCIG. The terminal
                            // publish (finalize_block) supersedes any pending
                            // quiesce publish for the open block.
                            //
                            // the tombstone is the ADR-008 D1 signal
                            // that block `prev` is FULLY delivered — every log
                            // for `prev` has been buffered. Mark the V3/V4
                            // pump-buffer completeness marker so the
                            // registration drain+pin cannot capture a
                            // half-delivered `prev` (the rolling-start race
                            // where a later same-block log lands after the pin).
                            // 3M5PO5: no explicit `mark_pump_blocks_complete`
                            // here — the fsm's own `tombstone(prev)` (inside
                            // `on_log`) already advanced the shared cutoff
                            // the registration drain reads.
                            // LOUD WS-completeness check: block `prev` is now
                            // confirmed complete (tombstoned by the first log of
                            // N+1). The FSM owns the whole accountability policy
                            // (single-writer ownership included): `Verify` means
                            // the live WS is answerable for this block — fetch
                            // `eth_getLogs` and abort on a real drop;
                            // `BackfillOwned` means an authoritative catch-up
                            // delivered it, so the cross-check is vacuous.
                            if ws_completeness_enabled {
                                match fsm.completeness_decision(prev) {
                                    CompletenessDecision::Verify {
                                        block,
                                        delivered_log_indices,
                                    } => {
                                        self.assert_ws_block_complete(block, delivered_log_indices)
                                            .instrument(
                                                block_span
                                                    .clone()
                                                    .unwrap_or_else(tracing::Span::none),
                                            )
                                            .await;
                                    }
                                    CompletenessDecision::BackfillOwned => {}
                                }
                            }
                            let prev_meta = fsm
                                .block_metadata_for(prev)
                                .unwrap_or(fsm.current_metadata());
                            let _ctx = block_span.as_ref().map(tracing::Span::enter);
                            // the tombstone is the Finalize row of the
                            // EPOCH `prev` (the machine's coordinate, stamped
                            // with the current rewind generation).
                            stage_tel.on_tombstone(
                                block_span.as_ref().unwrap_or(&tracing::Span::none()),
                                Epoch::with_generation(prev, fsm.rewind_seq()),
                            );
                            self.drive_finalize(&fsm, fsm.context_for(prev, prev_meta));
                        }
                        LogDecision::DispatchForward => {}
                        LogDecision::LateForward(b) => {
                            // No-landmine ruling: a removed:false log on
                            // a tombstoned block (NOT a reorg) is delivery-
                            // jitter LATENESS, not a structural fault. Blocks ≤
                            // the authoritative `fsm.recovery_anchor` are
                            // already dropped by the single-writer recovery
                            // discard before they reach this
                            // classifier — so what lands here is a
                            // post-tombstone survivor ABOVE the anchor. It is
                            // dropped UN-applied: I4 forbids pool-state writes
                            // outside the block's Streaming window, and the
                            // tombstone already fixed the delivery cutoff (I7 —
                            // no move, either way). Count it in the benign
                            // late-admit metric family and emit ONE deduped
                            // `late_log` policy event whose message names the
                            // benign path explicitly, so a spate of tight-
                            // settle-window drops can never masquerade as a
                            // structural bug. The completeness verify at the
                            // tombstone/Published edge stays the loud safety
                            // net for genuinely dropped WS logs.
                            if let Some(p) = crate::instruments::pipeline() {
                                p.count_late_log_admitted();
                            }
                            // feed the estimator's sliding-hour
                            // ledger — sustained budget overruns hold the
                            // adaptive window at the ceiling (backstop contract).
                            fsm.record_late_admit(now_ms());
                            // The bool (first sighting vs cooldown-suppressed)
                            // is informational; the counted home is the
                            // late_log.admitted counter above.
                            let _ = crate::telemetry::record_exception_keyed(
                                crate::telemetry::error_kind::LATE_LOG,
                                "ws",
                                b,
                                format_args!(
                                    "late forward log for tombstoned block {b}; dropped un-applied via the benign late-admit path (delivery jitter past the D1 tombstone edge); not a structural fault"
                                ),
                            );
                            crate::bot_core::apply_telemetry::trace_ws_log_dispatch(
                                log.address(),
                                log.topics(),
                                log_block,
                                log.log_index,
                                log.transaction_index,
                                log.removed,
                                "LateAdmitDropped",
                            );
                            // Skip the apply below: the late log never writes
                            // state (I4) — back to the top of the loop.
                            continue;
                        }
                    }

                    // Apply the log immediately to engine state (no solve yet).
                    // ADR-006 D4: routes through `Bot::dispatch_log` (decode →
                    // apply to BotState → record the EpochDelta byproduct) —
                    // NOT `engine.apply_log`. The FSM's `on_log_applied`
                    // records the clock's received/applied edges and arms the
                    // quiesce-gated publish (ADR-008 D2).
                    // One fact — a forward log applied to engine state — feeds
                    // two consumers (T4 pairing pin): the FSM
                    // quiesce arm (`on_log_applied` -> publish_pending) and
                    // the engine's `has_logs_this_block` (finalize
                    // bookkeeping). Coordinated here, once; do not
                    // split or drop either write.
                    // the DispatchForward arm never entered the
                    // block span (unlike Finalize et al.), so the
                    // `LogDispatcher::dispatch` instrument span ran with an
                    // empty thread-local and forked a single-span Jaeger ROOT
                    // per applied log (~25 roots/s — 1488 of 1500 newest
                    // traces in the live probe). Enter the block span so the
                    // apply chain nests under its block's trace.
                    let _block_ctx = block_span.as_ref().map(tracing::Span::enter);
                    self.bot.dispatch_log(&log);
                    fsm.on_log_applied(log_block);
                    // the apply completed — the epoch's Streaming
                    // interval closes and the Quiesced (StreamingComplete)
                    // point span fires with the burst's log count.
                    stage_tel.on_quiesced(
                        block_span.as_ref().unwrap_or(&tracing::Span::none()),
                        Epoch::with_generation(log_block, fsm.rewind_seq()),
                        pregap.logs,
                    );
                    telemetry.note_apply();

                    // engine owns `has_logs_this_block` now — routed
                    // through the sink so the next `finalize_block` sees it.
                    self.control.record_logs_this_block();

                    // [DIAG] count logs + emit periodic stats so we can see,
                    // during a freeze, that the pump IS polling logs while
                    // headers are gone. This is the liveness signal the loop
                    // otherwise lacks — owned by the `PumpTelemetry` seam.
                    telemetry.on_log();
                    let pool_state_head = self
                        .bot
                        .state_arc()
                        .read_at(crate::bot_core::state_lock::LockSite::Pump)
                        .pool_state_head();
                    telemetry.maybe_stats(fsm.current_block(), pool_state_head);
                }

                Ok(None) => {
                    // ADR-008 D2: stream exhausted — final settle point. Flush
                    // any pending quiesce-gated publish before returning. The
                    // settle rule is the FSM's `on_stream_end`; the driver only
                    // executes the emitted Publish (I/O) and stops.
                    for decision in fsm.on_stream_end() {
                        match decision {
                            StageDecision::Publish { open, metadata } => {
                                let _ctx = block_span.as_ref().map(tracing::Span::enter);
                                // the final settle's publish carries
                                // the same publish stage span as the timed
                                // settle path.
                                stage_tel.on_publish(
                                    block_span.as_ref().unwrap_or(&tracing::Span::none()),
                                    Epoch::with_generation(open, fsm.rewind_seq()),
                                );
                                self.drive_publish(
                                    &fsm,
                                    fsm.context_for(open, metadata),
                                    &GateOutcome::default(),
                                );
                            }
                            StageDecision::Stop => {}
                            other => {
                                unreachable!("on_stream_end only emits Publish|Stop, got {other:?}")
                            }
                        }
                    }
                    // Incident 2026-08-20 (WS-silent class): the pump is DEAD -
                    // the WS subscription dropped and no reconnect exists.
                    // Loud error + sink notification (drops the engine delivery
                    // channels) so the Python consumer's block stream ENDS and
                    // the settlement bot aborts loudly instead of idling
                    // forever (the "deadlock" operators observed).
                    op_error!(domain = pump, "BlockPump: WS subscription streams ended - pump is STOPPED. The bot will no longer process blocks (no reconnect). Check the WS endpoint / restart."
                    );
                    self.control.on_pump_ended();
                    return;
                }
            }

            // DRAINED-SETTLE SOLVE GATE: the solve fires
            // only once the combined stream is drained — no event is
            // immediately buffered. "Freshest available state" therefore
            // means "everything the WS has delivered so far has been applied",
            // not "whatever happened to fit before the top of the loop". The
            // peek below does NOT consume the next event, so a buffered event
            // simply re-arms the drain loop and the solve happens exactly once
            // at the end of the burst.
            //
            // MBNASQ: the original `poll_fn` was a single non-yielding poll —
            // it checked the stream's internal channel once without giving the
            // tokio runtime a chance to schedule the WS socket reader task. If
            // the WS delivered logs in multiple frames with brief gaps (5-70ms
            // between frames), the poll found the channel empty and the solve
            // fired prematurely. The next frame then triggered ANOTHER solve,
            // producing 2-3 serial solves per block whose total wall-time was
            // the sum. Replaced with a 50ms timed `peek()` await: if the WS
            // has another event ready within 50ms, this resolves `Ok` and the
            // solve is skipped (the loop processes the new event + re-checks).
            // If no event arrives in 50ms, the stream is genuinely quiet and
            // the solve fires — coalescing all logs in the burst into one
            // solve. 50ms is well within the 12s block interval (same
            // `DEBOUNCE_MS` as the publish gate).
            let dirty_now = self.control.has_dirty_paths();
            // designed first-slice trigger: remember when the
            // window's unsolved dirt was first observed. While the burst
            // outlives `early_slice_ms`, ONE bounded early Drain fires
            // mid-burst (the timed peek below is shortened to the slice
            // deadline, so the dispatch happens at first-dirty + ~25ms
            // without waiting for quiesce — the latency the retired finalize
            // steal was accidentally providing). The slice consumes the dirty
            // sets (`take_all` semantics) and re-derives its anchor per
            // cycle, so a following tail solve only re-solves NEWLY dirtied
            // pools; one slice per block window keeps MBNASQ's unbounded
            // serial solves from returning. `0` = disabled → the wait below
            // is always the bare debounce window (exact pre-T2 behavior).
            if dirty_now && slice_first_dirty.is_none() && !slice_done {
                slice_first_dirty = Some(tokio::time::Instant::now());
            }
            let slice_pending =
                self.early_slice_ms > 0 && slice_first_dirty.is_some() && !slice_done;
            // The timed peek waits only as long as the EARLIEST of the settle
            // debounce and the slice deadline — the gate self-wakes at the
            // deadline instead of waiting for the next event.
            let peek_wait = match (slice_pending, slice_first_dirty) {
                (true, Some(first)) => {
                    let age = first.elapsed();
                    let target = Duration::from_millis(self.early_slice_ms);
                    if age >= target {
                        // Deadline passed: the slice dispatch decision is
                        // purely a function of the age below; the timed peek
                        // resolves immediately (zero wait).
                        Duration::ZERO.min(Duration::from_millis(fsm.settle_window_ms()))
                    } else {
                        target
                            .saturating_sub(age)
                            .min(Duration::from_millis(fsm.settle_window_ms()))
                    }
                }
                _ => Duration::from_millis(fsm.settle_window_ms()),
            };
            let has_buffered = if dirty_now {
                // Only await when there's work to solve — otherwise skip
                // straight to the select (no dirty paths = nothing to do).
                // `peek()` resolves immediately when an event is buffered or
                // the stream has ended (Ready(None)); it returns Pending (and
                // yields to the runtime so the WS task can deliver) only when
                // the stream is alive but momentarily empty. The timeout
                // fires only in that latter case — coalescing burst gaps without
                // adding latency to streams with ready events.
                use std::pin::Pin;
                match tokio::time::timeout(peek_wait, Pin::new(&mut combined).peek()).await {
                    // event buffered — skip solve
                    Ok(Some(_)) => true,
                    // stream ended OR the wait elapsed — dispatch solve
                    _ => false,
                }
            } else {
                false
            };
            if !has_buffered && dirty_now {
                // Strictly-synchronous solve dispatch: enter the cursor
                // block span just long enough for dispatch() to capture it
                // as the drainer parent (no await inside).
                let slice_due = slice_pending
                    && slice_first_dirty.is_some_and(|first| {
                        first.elapsed() >= Duration::from_millis(self.early_slice_ms)
                    });
                self.boundary_drain_dispatch(&fsm, block_span.as_ref(), now_ms());
                if slice_due {
                    // The bounded slice took its one shot this window.
                    slice_done = true;
                    slice_first_dirty = None;
                } else {
                    // Settled-quiet solve: the window's tail is done, and the
                    // slice budget resets with the next header.
                    slice_done = false;
                    slice_first_dirty = None;
                }
            }
        }
        // the loop has unwound — every span guard (pump iteration,
        // drainer parent, solve) has popped through its scope on THIS task
        // before this point. Flush + shut down telemetry BEFORE the hotpath
        // guard drops at scope end (its Drop writes the report), so the report
        // and the exporter see the complete, final state. This is the exit
        // ordering that replaces hotpath's old process::exit() race.
        #[cfg(feature = "otel")]
        {
            if let Some(handle) = crate::otel::global_handle() {
                let _ = handle.flush();
                let _ = handle.shutdown();
            }
            crate::metrics::shutdown_global_metrics();
        }
    }
}

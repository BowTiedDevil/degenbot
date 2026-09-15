use super::{
    op_error, op_info, op_warn, stance, stream, Arc, AtomicBool, BlockPump, Bot, Duration,
    PumpControl, QuiesceParams, StageHandlers, StreamExt, SubscribeState, Watchdog, WsEvent,
    WsIngestor, DEFAULT_BACKFILL_CHUNK_SIZE,
};

impl BlockPump {
    /// Subscribe phase: open WS connections and observe until first complete block.
    ///
    /// Returns a `SubscribeState` containing the first observed block number
    /// and the live WS stream. Python should:
    /// 1. Run backfill up to `subscribe_state.first_block`
    /// 2. Call `resume(subscribe_state)` to begin normal processing
    ///
    /// During this phase, no events are buffered. The backfill is the sole
    /// authority for blocks S+1..W (inclusive). The subscribe phase only
    /// observes until
    /// both a newHeads notification and a log for the same block arrive,
    /// confirming the logs subscription is live and caught up.
    #[expect(clippy::missing_errors_doc)]
    pub async fn subscribe(
        rpc_url: &str,
        bot: Arc<Bot>,
        engine: Arc<dyn StageHandlers>,
        control: Arc<dyn PumpControl>,
        reorg_coordinator: Arc<crate::bot_core::reorg_coordinator::ReorgCoordinator>,
        shutdown: Arc<AtomicBool>,
    ) -> Result<(Self, SubscribeState), String> {
        // (5WTYYQ) Transport connect + subscribe + MJXP5Z handshake all live in
        // degenbot-ingestion; the driver receives the fused, re-injected
        // `IngestEvent` stream + keeps the handle for gap-backfill fetching.
        let ingestor = WsIngestor::connect(rpc_url).await?;

        let pump = Self {
            bot,
            engine,
            control,
            reorg_coordinator,
            ingestor,
            shutdown: Arc::clone(&shutdown),
            watchdog: Watchdog::new(),
            stage_max_age: Duration::from_secs(
                crate::bot_core::stage_telemetry::STAGE_MAX_AGE_SECS,
            ),
            ws_completeness_enabled: stance::config().pump.ws_completeness,
            header_ms: std::sync::atomic::AtomicU64::new(0),
            early_slice_ms: stance::config().pump.early_slice_ms,
            quiesce_params: QuiesceParams::from_schema(
                stance::config(),
                stance::config().pump.pump_debounce_ms,
            ),
        };
        // KAHU5W: the dispatcher-side strict decode-miss fault follows the
        // pump's completeness stance (respecting any per-pump opt-out).
        pump.bot
            .dispatcher()
            .set_strict_decode_fault(pump.ws_completeness_enabled);

        // MJXP5Z (Alternative B): single-stream handshake - NO resubscribe.
        // The ingestion handshake hands the SAME merged stream onward,
        // re-injecting any logs consumed during header-only polling. One WS,
        // one handoff.
        let boundary = pump
            .ingestor
            .subscribe_with_handshake(Arc::clone(&shutdown))
            .await?;
        Ok((
            pump,
            SubscribeState {
                first_block: boundary.first_block,
                first_timestamp: boundary.first_timestamp,
                combined_stream: Some(boundary.stream),
            },
        ))
    }

    /// Resume the pump from a subscribe state — auto-backfilling the
    /// snapshot→WS gap (J3FMDO) before the live loop begins.
    ///
    /// When the core `BotState` carries a snapshot seed `S` (set by
    /// `Bot::load_snapshot_from_db` or `load_*_from_py`) strictly less than
    /// the first observed WS block `W`, this method first awaits
    /// [`backfill_from_snapshot`](Self::backfill_from_snapshot) with the
    /// pump's own provider — applying `S+1..W` (inclusive) log state under
    /// `BotState::process_backfill_logs` with zero result batches. The Python
    /// `engine_registry.start()` no longer calls the pyo3
    /// `backfill_from_snapshot`; one Python `resume()` invocation drives both.
    ///
    /// When `S` is `None` (cold start) or `S >= W` (snapshot already at/after
    /// the live head), the backfill step is skipped — the live loop anchors
    /// on `W` directly.
    ///
    /// # Panics
    ///
    /// Panics if `subscribe_state.combined_stream` is `None` (i.e., `subscribe()`
    /// was not called first).
    pub async fn resume_from_subscribe(&mut self, subscribe_state: SubscribeState) {
        #[expect(clippy::expect_used)] // invariant-guarded (documented)
        let combined = subscribe_state
            .combined_stream
            .expect("resume() called without WS stream — did you call subscribe() first?");
        let first_block = subscribe_state.first_block;
        let (backfill_res, combined) = self.backfill_with_drain(first_block, combined).await;
        if let Err(e) = backfill_res {
            op_error!(domain = pump, first_block,
                %e,
                "BlockPump: auto-backfill failed — starting live loop from gap (not closed)"
            );
        }
        self.run_with_stream(combined, first_block).await;
    }

    /// DFQYM5/WS-DROP: run the snapshot→WS gap backfill while concurrently
    /// draining `combined`, returning `(backfill_result, combined')` where
    /// `combined'` re-injects every event drained during the backfill ahead
    /// of the still-owned live tail, preserving arrival order (MJXP5Z).
    ///
    /// Why the drain is not optional: the alloy `logs` subscription buffers
    /// into a small broadcast channel (default capacity 16) that DROPS the
    /// OLDEST messages for a lagging receiver. A backfill that awaits without
    /// polling `combined` therefore loses the freshly-mined live blocks' logs
    /// permanently — the first live block then shows most of its logs missing
    /// and immediately trips the WS-completeness abort (observed live:
    /// `eth_getLogs=44 logs, WS delivered=0` at block 25800995). Both
    /// consumers of the synchronous backfill — the core
    /// [`resume_from_subscribe`](Self::resume_from_subscribe) AND the pyo3
    /// `PumpState::resume` (which must `block_on` the backfill before
    /// returning so Python's `build_paths` cannot race the per-pool buffer,
    /// J3FMDO) — MUST go through this helper so the drain discipline has a
    /// single owner.
    pub async fn backfill_with_drain(
        &self,
        first_block: u64,
        combined: stream::BoxStream<'static, WsEvent>,
    ) -> (Result<u64, String>, stream::BoxStream<'static, WsEvent>) {
        let mut combined = combined;
        let (backfill_res, drained) = self
            .drain_stream_during_backfill(first_block, &mut combined)
            .await;
        let combined = if drained.is_empty() {
            combined
        } else {
            stream::iter(drained).chain(combined).boxed()
        };
        (backfill_res, combined)
    }

    /// Concurrently drain the live WS stream while the blocking snapshot→WS
    /// gap backfill runs, returning `(backfill_result, drained_events)`.
    ///
    /// Rationale/member-fn boundary: isolating the `&self`-borrowing backfill
    /// future inside this method lets its borrow end on return, so the caller
    /// can then re-borrow `&mut self` for the live loop (see caller). See
    /// [`resume_from_subscribe`](Self::resume_from_subscribe) for the
    /// broadcast-overflow root cause this drains around.
    async fn drain_stream_during_backfill(
        &self,
        first_block: u64,
        combined: &mut stream::BoxStream<'static, WsEvent>,
    ) -> (Result<u64, String>, Vec<WsEvent>) {
        let mut drained: Vec<WsEvent> = Vec::new();
        let backfill = self.backfill_to_ws_block(first_block);
        tokio::pin!(backfill);
        loop {
            tokio::select! {
                biased;
                res = &mut backfill => return (res, drained),
                ev = combined.next() => {
                    if let Some(ev) = ev {
                        drained.push(ev);
                    } else {
                        op_warn!(domain = pump, "BlockPump: WS stream ended during backfill (no re-inject gap)"
                        );
                        return (Ok(0), drained);
                    }
                },
            }
        }
    }

    /// Close the snapshot→WS gap by buffering `eth_getLogs(S+1..W)` (inclusive)
    /// into the
    /// core `BotState`'s per-pool backfill buffer (no solve, no `on_send`).
    ///
    /// This is the SYNCHRONOUSLY-awaitable half of `resume_from_subscribe` —
    /// `PumpState::resume` `block_on`s it BEFORE spawning the live loop so
    /// Python's `build_paths` (which drains the per-pool backfill buffer via
    /// `apply_backfill_buffer_v3`) cannot race the backfill. Pre-fix the
    /// backfill ran inside the spawned `resume_from_subscribe` task and
    /// `resume` returned immediately, so an active pool's burn was not yet
    /// buffered when `build_paths` drained → `VerificationMismatchError` at
    /// post-drain verify (2026-07-12 settlement-arbitrage crash).
    ///
    /// No-op when `S` is unset (cold start), `S >= W` (catch-up snapshot), or
    /// `S == 0`. Errors log + return (the live loop still starts from `W`).
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` if a chunk's `eth_getLogs` call fails (message
    /// includes the offending block range + provider error).
    pub async fn backfill_to_ws_block(&self, ws_block: u64) -> Result<u64, String> {
        let s = self
            .bot
            .state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .snapshot_seed_block();
        let Some(seed) = s else { return Ok(0) };
        if seed == 0 || ws_block == 0 || seed >= ws_block {
            return Ok(0);
        }
        op_info!(
            domain = pump,
            seed,
            ws_block,
            "BlockPump: auto-backfill from snapshot block to WS block before resume"
        );
        self.backfill_from_snapshot(ws_block, DEFAULT_BACKFILL_CHUNK_SIZE)
            .await
    }
}

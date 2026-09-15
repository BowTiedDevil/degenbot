use super::{
    op_error, op_info, op_warn, BlockMetadata, BlockPump, HashMap, Log, LogDecision, Ordering,
    StageMachine, RELEVANT_TOPICS,
};

impl BlockPump {
    /// Backfill a range of blocks via `eth_getLogs`, applying each backfilled
    /// log through the SAME per-block state machine as a live WS log (ADR-008
    /// D4, single branch). The provider I/O (`get_logs`) and engine I/O
    /// (`dispatch_log`, `drive_finalize`, `drive_solve`) stay here on the driver;
    /// every FSM-state transition is routed through `StageMachine` methods
    /// (`on_log`, `on_log_applied`) — no fields are threaded out of the capsule.
    pub(super) async fn backfill_range(
        &self,
        from_block: u64,
        to_block: u64,
        fsm: &mut StageMachine,
    ) {
        if from_block > to_block {
            return;
        }

        op_info!(
            domain = pump,
            from_block,
            to_block,
            "BlockPump: backfilling blocks"
        );
        // T2: one counter per executed backfill range.
        if let Some(p) = crate::instruments::pipeline() {
            p.count_backfill();
        }

        // (5WTYYQ) The transport fetch is degenbot-ingestion's; the
        // apply/solve loop below stays on the driver (its FSM + dispatch).
        let logs = match self.ingestor.fetch_logs(from_block, to_block).await {
            Ok(logs) => logs,
            Err(e) => {
                op_error!(domain = pump, %e, "BlockPump: backfill eth_getLogs failed");
                return;
            }
        };

        // Group logs by block number for sequential processing
        let mut logs_by_block: HashMap<u64, Vec<Log>> = HashMap::new();
        for log in &logs {
            if let Some(block_num) = log.block_number {
                logs_by_block
                    .entry(block_num)
                    .or_default()
                    .push(log.clone());
            }
        }

        let mut any_processed = false;
        for block in from_block..=to_block {
            if self.shutdown.load(Ordering::Relaxed) {
                op_info!(domain = pump, "BlockPump: shutting down during backfill");
                return;
            }

            let block_logs = logs_by_block.remove(&block).unwrap_or_default();
            for log in &block_logs {
                match fsm.on_log(block, log.removed) {
                    LogDecision::TombstonePrevious(prev) => {
                        let prev_meta = fsm.block_metadata_for(prev).unwrap_or_default();
                        self.drive_finalize(fsm, fsm.context_for(prev, prev_meta));
                        self.bot.dispatch_log(log);
                        fsm.on_log_applied(block);
                    }
                    LogDecision::DispatchForward => {
                        self.bot.dispatch_log(log);
                        fsm.on_log_applied(block);
                    }
                    // Backfilled logs come from an authoritative eth_getLogs
                    // against the canonical chain. Reorg/late-forward signals
                    // are not expected here; if one surfaces, skip applying
                    // this log (the canonical chain doesn't contain it) and let
                    // the live stream reconcile.
                    LogDecision::EnterReorg(_)
                    | LogDecision::ContinueReorg
                    | LogDecision::CloseReorg { .. }
                    | LogDecision::LateForward(_) => {
                        op_warn!(
                            domain = pump,
                            block,
                            "BlockPump: backfill saw unexpected decision; skipping log"
                        );
                    }
                }
            }
            if !block_logs.is_empty() {
                // The backfill solve: the Solved cycle at the block's default
                // metadata — NO Published row (no `on_publish`): the
                // backfill applies state without dispatching result batches
                // (the `Backfilled` phase invariant, FD7NFG).
                self.drive_solve(fsm, fsm.context_for(block, BlockMetadata::default()));
                any_processed = true;
            }
        }

        if any_processed {
            op_info!(
                domain = pump,
                from_block,
                to_block,
                "BlockPump: backfill complete for blocks"
            );
        } else {
            op_info!(
                domain = pump,
                from_block,
                to_block,
                "BlockPump: backfill found no relevant events"
            );
        }
    }

    /// LOUD assertion of the core WS-delivery invariant (ADR-008 D1): when
    /// block `block` is tombstoned, EVERY relevant-topic log that exists
    /// on-chain@block must have been delivered by the live WS subscription.
    ///
    /// The pump's correctness model assumes the websocket delivers every log;
    /// a silently dropped log (observed while driving the bot — a single `Mint`
    /// missing from an otherwise-delivered block) produces a pin/verify
    /// mismatch later and, worse, silently stale solve state. This check
    /// cross-references the delivered relevant-topic log-index set against the
    /// authoritative `eth_getLogs` for the block and PANICS if any on-chain
    /// relevant log is missing — a catastrophic websocket delivery failure that
    /// must NOT be masked or silently corrected.
    ///
    /// Gated on `DEGENBOT_WS_COMPLETENESS` (default ON via
    /// `bot_env_flag_default_on`; disable with `=0`; deterministically OFF in
    /// the test constructor). When disabled it is a no-op. On an
    /// `eth_getLogs` transport error (not a mismatch) it logs loudly and
    /// returns — the check cannot run, but the bot is not taken down by a
    /// transient RPC failure.
    ///
    /// # Panics
    ///
    /// Panics if `eth_getLogs` reveals a relevant-topic log for `block` that
    /// the live websocket did not deliver — a catastrophic WS delivery drop
    /// that must fail loudly rather than silently stale the engine state.
    // future=true: poll-level attribution for the per-block getLogs
    // completeness call (WS-gap verification) — poll time here is network
    // wait, useful against the header->published latency race.
    #[hotpath::measure(future = true)]
    pub async fn assert_ws_block_complete(
        &self,
        block: u64,
        delivered_log_indices: std::collections::HashSet<u64>,
    ) {
        // (5WTYYQ) The eth_getLogs transport call is the ingestion crate's.
        let logs = match self.ingestor.fetch_logs(block, block).await {
            Ok(logs) => logs,
            Err(e) => {
                op_error!(domain = pump, block,
                    %e,
                    "BlockPump: WS-completeness eth_getLogs failed (not a mismatch; "
                );
                return;
            }
        };
        // Filter the fetched logs CLIENT-SIDE by exact topic0 ∈ RELEVANT_TOPICS
        // before collecting log_index. `build_backfill_filter`'s server-side
        // topic[0] OR-list over-matches on some nodes (returns a superset —
        // observed: a block with 35 exact-topic relevant logs came back as 43),
        // inflating the "missing" set and creating FALSE drop positives. The
        // WS-delivered side is exact, so the on-chain side must be exact too
        // for an apples-to-apples comparison.
        let onchain: std::collections::HashSet<u64> = logs
            .iter()
            .filter(|l| matches!(l.topic0(), Some(t0) if RELEVANT_TOPICS.contains(t0)))
            .filter_map(|l| l.log_index)
            .collect();
        let missing: Vec<u64> = onchain
            .iter()
            .filter(|li| !delivered_log_indices.contains(li))
            .copied()
            .collect();
        if !missing.is_empty() {
            // LOUD immediate failure: a websocket legitimately failed to
            // deliver events — the very failure mode surfaced loudly rather
            // than masked or silently corrected. Log the full message to
            // stderr/a tracing sink, then ABORT the process so the bot dies
            // HARD and immediately. A contained worker-thread panic would
            // leave the bot half-alive (silent-ish), which is itself a failure
            // mode; `std::process::abort` guarantees termination.
            op_error!(domain = pump, "LIVE WEBSOCKET LOG DROP at block {block}: {} relevant on-chain log(s) missing from WS delivery: log_index {:?}. eth_getLogs={} logs, WS delivered={} logs. The websocket/pump delivery path dropped a relevant event — ABORT (DFQYM5/WS-DROP). Investigate the subscription/reconnect path; do NOT silence this.",
                missing.len(),
                missing,
                onchain.len(),
                delivered_log_indices.len(),
            );
            crate::telemetry::record_exception(
                crate::telemetry::error_kind::WS_COMPLETENESS,
                format_args!(
                    "live WS log drop at block {block}: {} of {} relevant logs missing (log_index {missing:?})",
                    missing.len(),
                    onchain.len()
                ),
            );
            crate::telemetry::flush_before_exit();
            #[expect(clippy::print_stderr)] // invariant-failure diagnostic before abort
            {
                eprintln!(
                    "ABORT: live websocket log drop at block {block} ({} of {} relevant logs missing); eth_getLogs vs WS divergence — see the untraced log for the log_index list.",
                    missing.len(),
                    onchain.len(),
                );
            }
            std::process::abort();
        }
        let extra: Vec<u64> = delivered_log_indices
            .iter()
            .filter(|li| !onchain.contains(li))
            .copied()
            .collect();
        if !extra.is_empty() {
            op_warn!(domain = pump, block,
                extras = ?extra,
                "BlockPump: WS delivered relevant logs not present in eth_getLogs"
            );
        }
    }

    /// Backfill the snapshot→WS gap `S+1..W` (inclusive) using the NO-SOLVE path
    /// (FD7NFG, epic P73ER6). Reads `S` from `BotState::snapshot_seed_block`
    /// (set by `Bot::load_snapshot_from_db`) and `W` from the `ws_block` param
    /// (the block the WS subscription landed on — `SubscribeState::first_block`,
    /// passed by the pyo3 caller or J3FMDO's auto-backfill before `resume`).
    /// Fetches logs via the pump's own `AlloyProvider` (no `rpc_url` from
    /// Python) in `chunk_size` chunks via `build_backfill_filter`, applying
    /// each chunk via `BotState::process_backfill_logs` (the relocated engine
    /// loop). No solve cycle / no batches — the `Backfilled` phase invariant
    /// is "state advanced, no dispatch".
    ///
    /// Returns the count of blocks backfilled (`W - (S+1) + 1 = W-S`), or
    /// `Ok(0)` for a no-op (cold start / S≥W). The post-backfill boundary is
    /// `W`; the pump's resume anchors on `first_observed_block = W` regardless
    /// (the WS anchor, NOT `last_processed_block`), so this method does NOT stamp
    /// the sink's cursor.
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` on a `get_logs` RPC failure.
    pub async fn backfill_from_snapshot(
        &self,
        ws_block: u64,
        chunk_size: u64,
    ) -> Result<u64, String> {
        let w = ws_block;
        let s = {
            let arc = self.bot.state_arc();
            let state = arc.read_at(crate::bot_core::state_lock::LockSite::Pump);
            state.snapshot_seed_block()
        };
        let Some(s) = s else {
            op_info!(
                domain = pump,
                "BlockPump::backfill_from_snapshot: no snapshot loaded, cold-start path"
            );
            return Ok(0);
        };
        if s == 0 {
            op_warn!(
                domain = pump,
                "BlockPump::backfill_from_snapshot: snapshot block S=0, skipping"
            );
            return Ok(0);
        }
        if s >= w {
            op_info!(
                domain = pump,
                s,
                ws_block = w,
                "BlockPump::backfill_from_snapshot: snapshot >= WS block, nothing to backfill"
            );
            return Ok(0);
        }
        let from_block = s + 1;
        // Include `w` (the resume boundary block) so the backfill covers
        // [S+1, W] INCLUSIVE (DFQYM5). Block W is a delivery hole if excluded:
        // the snapshot→WS gap backfill stops at W-1, and the fresh WS `logs`
        // subscription streams ONLY logs mined after it engages — block W's
        // pre-existing logs are never delivered by the WS (observed: 6 of 35
        // at the boundary block). Fetching W deterministically via eth_getLogs
        // closes the hole; the pump drops the sparse WS partial-W-dup logs in
        // `run_with_stream` (see the `log_block <= W` guard).
        let to_block = w;
        let total_blocks = to_block - from_block + 1;
        op_info!(
            domain = pump,
            from_block,
            to_block,
            total_blocks,
            chunk_size,
            "BlockPump::backfill_from_snapshot: fetching events"
        );
        let mut total_logs = 0usize;
        let mut chunk_start = from_block;
        while chunk_start <= to_block {
            let chunk_end = (chunk_start + chunk_size - 1).min(to_block);
            op_info!(
                domain = pump,
                chunk_start,
                chunk_end,
                "BlockPump::backfill_from_snapshot: fetching chunk"
            );
            let t0 = std::time::Instant::now();
            // (5WTYYQ) The eth_getLogs chunk fetch is the ingestion crate's;
            // the apply loop stays on the driver.
            let logs = self
                .ingestor
                .fetch_logs(chunk_start, chunk_end)
                .await
                .map_err(|e| {
                    format!("eth_getLogs failed for blocks {chunk_start}-{chunk_end}: {e}")
                })?;
            let n = logs.len();
            let fetch_ms = t0.elapsed().as_millis();
            op_info!(domain = pump, chunk_start,
                chunk_end,
                log_count = n,
                fetch_ms = %fetch_ms,
                "BlockPump::backfill_from_snapshot: chunk fetched logs"
            );
            total_logs += n;
            // Hold the write guard across the chunk so the apply + buffer-expire
            // (which advance `last_processed_block`) stay atomic per chunk.
            self.bot
                .state_arc()
                .write_at(crate::bot_core::state_lock::LockSite::Pump)
                .process_backfill_logs(self.bot.dispatcher(), &logs, chunk_end);
            op_info!(
                domain = pump,
                chunk_start,
                chunk_end,
                log_count = n,
                "BlockPump::backfill_from_snapshot: chunk logs applied"
            );
            chunk_start = chunk_end + 1;
        }
        op_info!(
            domain = pump,
            total_logs,
            total_blocks,
            "BlockPump::backfill_from_snapshot: complete"
        );
        Ok(total_blocks)
    }
}

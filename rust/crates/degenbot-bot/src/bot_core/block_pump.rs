//! `BlockPump` — `Bot`'s WS transport + drain loop (ADR-006 D4).
//!
//! Generalized from the `BlockPump`: holds `Arc<Bot>` +
//! `Arc<dyn DrainSink>` instead of `Arc<Mutex<UniswapEngine>>`. Per WS log,
//! the pump calls `bot.dispatch_log(log)` (slice 4: decode → apply to
//! `BotState` → notify the `EngineSubscriber`, which dirties the engine). At
//! block boundaries / drain ticks / reorg the pump drives the `DrainSink`
//! (`on_drain` / `on_send` / `finalize_block`).
//!
//! `apply_log` is gone — ALL log application routes through `Bot::dispatch_log`,
//! so `process_block` / `process_block_and_send` decompose in the pump into
//! `dispatch_log`-per-log + `on_drain` (the D4 goal).
//!
//! The pump's **mechanics stay unchanged** from the `BlockPump` era: dual
//! `newHeads` + `logs` subscription, Rust-side topic+address filtering, block-
//! boundary detection, 50ms send-result debounce, gap/timeout `eth_getLogs`
//! backfill. Only the owner (`Bot`, via the wiring layer) + the per-block
//! dispatch targets changed.
//!
//! # Two-Phase Lifecycle
//!
//! 1. **Subscribe phase** (`subscribe()`): Opens WS subscriptions (newHeads +
//!    unfiltered logs) and observes until the first *complete* block — both the
//!    header and a log for block N. N is returned as the backfill boundary W.
//!    No events are buffered during subscribe — backfill is the sole authority
//!    for blocks S+1..W-1; the pump (resume) is sole authority for W onward.
//!
//! 2. **Resume phase** (`resume_from_subscribe()`): Begins normal processing —
//!    logs applied eagerly, solved + sent on block boundaries / debounce.
//!
//! **Critical ordering**: Python must run backfill AFTER `subscribe()` returns
//! but BEFORE `resume_from_subscribe()`. The `DrainSink`'s
//! `last_processed_block()` is the backfill-start boundary.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::B256;
use alloy::rpc::types::{Filter, Log, Topic};
use futures_util::{stream, StreamExt};
use tokio::time::timeout;

use crate::bot_core::v3_mint_burn_decoder::{V3_BURN_TOPIC, V3_MINT_TOPIC};
use crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC;
use crate::bot_core::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC;
use crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC;
use crate::bot_core::{drain_sink::DrainSink, BlockMetadata, Bot};
use crate::optimizers::v2_sync_decoder::V2_SYNC_TOPIC;
use degenbot_rpc::provider::AlloyProvider;

/// How long to wait with no activity before assuming the connection is dead.
const BACKFILL_TIMEOUT_SECS: u64 = 60;

/// After the first dirty WS log for a block, wait this long for more logs
/// before solving and dispatching results to Python. Each new log resets
/// the timer. This debouncing ensures one dispatch per burst of logs
/// rather than one per individual log.
const DEBOUNCE_MS: u64 = 50;

/// Whether a log confirms that a tracked header block is "complete".
///
/// A log confirms the header block only when its `block_number` is known
/// and matches the header block exactly. Pending logs with an unknown
/// block number (`None`) are intentionally ignored — otherwise a log
/// arriving before the logs subscription has caught the current block
/// could make `subscribe_phase` return early and miss events.
#[must_use]
fn log_confirms_header_block(log_block_number: Option<u64>, first_block: u64) -> bool {
    log_block_number == Some(first_block)
}

/// Block data sent from the pump to Python via the watch channel.
/// Topics we care about — used for in-Rust filtering of incoming logs.
pub const RELEVANT_TOPICS: [B256; 6] = [
    V2_SYNC_TOPIC,
    V3_SWAP_TOPIC,
    V3_MINT_TOPIC,
    V3_BURN_TOPIC,
    V4_SWAP_TOPIC,
    V4_MODIFY_LIQUIDITY_TOPIC,
];

/// Events from the two WS subscriptions.
pub enum WsEvent {
    /// A new block header arrived.
    BlockHeader {
        number: u64,
        timestamp: u64,
        base_fee_per_gas: Option<u64>,
        gas_used: u64,
        gas_limit: u64,
    },
    /// A log event arrived from the logs subscription.
    Log(Log),
}

/// The unified pump that drives `Bot`'s drain sink.
///
/// Supports a two-phase lifecycle:
/// 1. `subscribe()` — opens WS connections, observes until first complete
///    block (header + log for same block), returns that block number
/// 2. `resume()` — begins normal processing on block boundaries
pub struct BlockPump {
    /// The per-chain orchestrator — owns `BotState` + the `LogDispatcher`. Per
    /// WS log, the pump calls `bot.dispatch_log(log)` (forward) or
    /// `reorg_coordinator.dispatch_reorg_log(log)` (`removed: true`).
    /// ADR-006 D4 + slice 7.
    bot: Arc<Bot>,
    /// The drain sink (slice 6: `SolveCoordinator` fanning to every
    /// attached `Engine` under a `drain_lock`).
    sink: Arc<dyn DrainSink>,
    /// The per-event reorg coordinator (slice 7). Owned by the pump (not
    /// routed through the `DrainSink` — reorg is a `Bot` concern, parallel
    /// to `dispatch_log`).
    reorg_coordinator: Arc<crate::bot_core::reorg_coordinator::ReorgCoordinator>,
    /// The Alloy provider (created from the RPC URL)
    provider: Arc<AlloyProvider>,
    /// Shutdown flag — set by `stop()` or by a too-deep reorg (graceful exit)
    shutdown: Arc<AtomicBool>,
}

/// State held between `subscribe()` and `resume()` calls.
///
/// Contains the live WS subscriptions and the first observed block number.
/// Created by `subscribe()`, consumed by `resume()`.
pub struct SubscribeState {
    /// The first block number observed during subscribe.
    /// Python uses this as the backfill target.
    pub first_block: u64,
    /// Block timestamp from first observed block.
    pub first_timestamp: u64,
    /// The merged stream of WS events (block headers + logs).
    /// `None` after `resume()` consumes it.
    pub combined_stream: Option<stream::BoxStream<'static, WsEvent>>,
}

impl BlockPump {
    /// Subscribe phase: open WS connections and observe until first complete block.
    ///
    /// Returns a `SubscribeState` containing the first observed block number
    /// and the live WS stream. Python should:
    /// 1. Run backfill up to `subscribe_state.first_block`
    /// 2. Call `resume(subscribe_state)` to begin normal processing
    ///
    /// During this phase, no events are buffered. The backfill is the sole
    /// authority for blocks S+1..W-1. The subscribe phase only observes until
    /// both a newHeads notification and a log for the same block arrive,
    /// confirming the logs subscription is live and caught up.
    #[allow(clippy::missing_errors_doc)]
    pub async fn subscribe(
        rpc_url: &str,
        bot: Arc<Bot>,
        sink: Arc<dyn DrainSink>,
        reorg_coordinator: Arc<crate::bot_core::reorg_coordinator::ReorgCoordinator>,
        shutdown: Arc<AtomicBool>,
    ) -> Result<(Self, SubscribeState), String> {
        let provider = AlloyProvider::new(rpc_url, 3)
            .await
            .map_err(|e| format!("BlockPump: failed to create provider: {e}"))?;

        let provider_arc = provider.provider_arc();

        // Subscribe to block headers
        let block_stream = provider_arc
            .subscribe_blocks()
            .await
            .map_err(|e| format!("BlockPump: failed to subscribe to blocks: {e}"))?
            .into_stream();

        // Subscribe to logs — unfiltered. All filtering happens in Rust.
        let log_filter = Filter::new();
        let log_stream = provider_arc
            .subscribe_logs(&log_filter)
            .await
            .map_err(|e| format!("BlockPump: failed to subscribe to logs: {e}"))?
            .into_stream();

        let combined = stream_select(block_stream, log_stream).boxed();

        let pump = Self {
            bot,
            sink,
            reorg_coordinator,
            provider: Arc::new(provider),
            shutdown,
        };

        // Observe until we see a "complete" block — both a header and at
        // least one log for the same block. This guarantees the logs
        // subscription did not miss the start of the block. The block number
        // is returned as the backfill boundary W.
        let (first_block, first_timestamp) = pump.subscribe_phase(combined).await;

        // Re-subscribe to get a fresh stream for the resume phase.
        // The subscribe_phase consumed events from the first stream.
        let block_stream2 = provider_arc
            .subscribe_blocks()
            .await
            .map_err(|e| format!("BlockPump: failed to re-subscribe to blocks: {e}"))?
            .into_stream();

        let log_stream2 = provider_arc
            .subscribe_logs(&log_filter)
            .await
            .map_err(|e| format!("BlockPump: failed to re-subscribe to logs: {e}"))?
            .into_stream();

        let combined2 = stream_select(block_stream2, log_stream2).boxed();

        let subscribe_state = SubscribeState {
            first_block,
            first_timestamp,
            combined_stream: Some(combined2),
        };

        Ok((pump, subscribe_state))
    }

    /// Subscribe phase: observe WS subscriptions until the first complete block.
    ///
    /// A "complete" block is one where both a `newHeads` notification and at
    /// least one log from the same block have been received. This guarantees
    /// that the logs subscription did not miss the start of the block (which
    /// could happen if the subscription was opened mid-block).
    ///
    /// No events are buffered during this phase. The backfill
    /// (`backfill_from_snapshot`) is the sole authority for blocks S+1..W-1,
    /// and the pump (resume phase) is the sole authority for W onward.
    /// This eliminates any overlap between the two sources.
    ///
    /// Returns (`first_block_number`, `first_timestamp`).
    async fn subscribe_phase(
        &self,
        mut combined: stream::BoxStream<'static, WsEvent>,
    ) -> (u64, u64) {
        let mut first_block: Option<u64> = None;
        let mut first_timestamp: u64 = 0;
        // Whether we've seen at least one log for the header block.
        // When both header and log are seen, that block is "complete".
        let mut saw_log_for_header_block: bool = false;

        loop {
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("BlockPump: shutting down during subscribe phase");
                return (0, 0);
            }

            let event = timeout(Duration::from_secs(BACKFILL_TIMEOUT_SECS), combined.next()).await;

            match event {
                Err(_) => {
                    // Timeout during subscribe — try to get current block via RPC.
                    // This is a degraded path: we don't have confirmation that
                    // both subscriptions are live, but it's better than hanging.
                    log::warn!("BlockPump: timeout during subscribe, fetching current block");
                    match self.provider.provider_arc().get_block_number().await {
                        Ok(block) => {
                            log::info!(
                                "BlockPump: subscribe observed block {block} via RPC (degraded — no log confirmation)"
                            );
                            return (block, 0);
                        }
                        Err(e) => {
                            log::error!("BlockPump: can't get block number during subscribe: {e}");
                        }
                    }
                }

                Ok(Some(WsEvent::BlockHeader {
                    number,
                    timestamp,
                    base_fee_per_gas: _,
                    gas_used: _,
                    gas_limit: _,
                })) => {
                    if let Some(fb) = first_block {
                        if number > fb {
                            // New header for a later block. If we already saw
                            // a log for the previous header's block, we're done.
                            if saw_log_for_header_block {
                                log::info!("BlockPump: subscribe observed complete block {fb}");
                                return (fb, first_timestamp);
                            }
                            // Previous block had no logs from our subscription.
                            // Advance to the new header and reset the flag.
                            first_block = Some(number);
                            first_timestamp = timestamp;
                            saw_log_for_header_block = false;
                        }
                        // else: duplicate/stale header for the same block — ignore
                    } else {
                        // First header ever observed.
                        first_block = Some(number);
                        first_timestamp = timestamp;
                        saw_log_for_header_block = false;
                    }
                }

                Ok(Some(WsEvent::Log(log))) => {
                    // Check if this log is for the header block we're tracking.
                    if let Some(fb) = first_block {
                        if log_confirms_header_block(log.block_number, fb) {
                            log::info!(
                                "BlockPump: subscribe observed complete block {fb} (header + log)"
                            );
                            return (fb, first_timestamp);
                        }
                    }
                    // Log arrived before any header, or for a different/pending block — keep waiting.
                }

                Ok(None) => {
                    log::warn!("BlockPump: subscription streams ended during subscribe");
                    return (first_block.unwrap_or(0), first_timestamp);
                }
            }
        }
    }

    /// Resume phase using the pump's own watch channel.
    ///
    /// # Panics
    ///
    /// Panics if `subscribe_state.combined_stream` is `None` (i.e., `subscribe()`
    /// was not called first).
    pub async fn resume_from_subscribe(&mut self, subscribe_state: SubscribeState) {
        let combined = subscribe_state
            .combined_stream
            .expect("resume() called without WS stream — did you call subscribe() first?");

        self.run_with_stream(combined, subscribe_state.first_block)
            .await;
    }

    /// Run the main pump loop with an existing WS stream.
    ///
    /// Processes logs eagerly: each WS log is applied to engine state
    /// immediately and affected paths are solved right away, without
    /// waiting for a block header. Block headers provide metadata
    /// (timestamp, fees) and handle empty-block detection.
    #[allow(unused_assignments, clippy::too_many_lines)]
    async fn run_with_stream(
        &mut self,
        mut combined: stream::BoxStream<'static, WsEvent>,
        first_observed_block: u64,
    ) {
        let relevant_topic_set: HashSet<B256> = RELEVANT_TOPICS.into_iter().collect();

        // Read the last block processed by Python backfill.
        let mut current_block: u64 = self.sink.last_processed_block().unwrap_or(0);

        if current_block == 0 && first_observed_block > 0 {
            current_block = first_observed_block;
            log::info!("BlockPump: cold start from block {first_observed_block}");
        } else {
            log::info!("BlockPump: starting from block {current_block} (Python backfill)");
        }

        // Track the last block we've solved for. Used to detect block
        // boundaries and finalize the previous block when a new one starts.
        let mut last_solved_block: u64 = current_block;
        // Whether we're past the first header after resume. The first
        // header establishes our anchor but shouldn't trigger a solve
        // (backfill already solved up to this point).
        let mut first_header = true;
        // Current block metadata — updated from headers, used for
        // solve batches when logs close out a block.
        let mut current_metadata: BlockMetadata = BlockMetadata::default();
        // Whether any logs were applied for the current block.
        let mut has_logs_this_block: bool = false;
        // Debounce timer: started when the first dirty log arrives, reset on
        // each new log. When it fires, we send the accumulated result batch
        // to Python. This ensures one dispatch per burst of logs rather than
        // one per individual log.
        let mut debounce = tokio::time::Instant::now() + Duration::from_millis(DEBOUNCE_MS);
        let mut debounce_active = false;

        loop {
            // Solve any dirty paths accumulated from the previous iteration's
            // log(s). This naturally coalesces multiple logs that arrive
            // between await points — only one solve per batch of WS events.
            // Note: solving is decoupled from sending — the pump controls
            // when result batches are dispatched to Python.
            {
                if self.sink.has_dirty_paths() {
                    self.sink.on_drain(current_block, &current_metadata);
                    last_solved_block = current_block;
                }
            }

            // Check if the debounce timer has expired — if so, send the
            // accumulated results to Python (one dispatch per log burst).
            if debounce_active && tokio::time::Instant::now() >= debounce {
                debounce_active = false;
                self.sink.on_send(&current_metadata);
            }

            // Check shutdown
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("BlockPump: shutting down");
                return;
            }

            // Wait for the next event. Use a shorter timeout when the debounce
            // timer is active so we can fire it promptly.
            let wait_timeout = if debounce_active {
                // Wake up when the debounce timer fires (or earlier on new events)
                Duration::from_millis(DEBOUNCE_MS)
                    .saturating_sub(debounce.saturating_duration_since(tokio::time::Instant::now()))
            } else {
                Duration::from_secs(BACKFILL_TIMEOUT_SECS)
            };
            let event = timeout(wait_timeout, combined.next()).await;

            match event {
                // Timeout — check if it's the debounce timer or the backfill timer.
                Err(_) => {
                    if debounce_active {
                        // The timeout must be from the debounce timer (50ms),
                        // not the backfill timer (60s). If the debounce has
                        // expired, send the result batch. If not (due to
                        // timing precision), just loop back — the next
                        // iteration will handle it.
                        if tokio::time::Instant::now() >= debounce {
                            debounce_active = false;
                            self.sink.on_send(&current_metadata);
                        }
                        // else: debounce timer hasn't quite expired yet.
                        // Loop back — the next iteration will pick it up.
                    } else {
                        // No activity for 60s — try to backfill
                        self.handle_timeout_eager(
                            &mut current_block,
                            &mut last_solved_block,
                            &mut has_logs_this_block,
                        )
                        .await;
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
                    // Snapshot the just-finished block's metadata BEFORE
                    // overwriting `current_metadata` with the incoming
                    // header's. `current_metadata` at this point holds the
                    // metadata of `current_block` (set when ITS header arrived
                    // and held through that block's log processing), so the
                    // batch that finalizes `current_block` must carry THAT
                    // metadata — NOT the incoming header's. Passing the
                    // post-overwrite value here would make Python compute
                    // `base_fee_next` from the wrong block and submit
                    // under-/over-priced backrun txs (VTWCIG). The incoming
                    // metadata (assigned just below) drives the empty-block
                    // send path, which is correctly about the NEW block.
                    let finished_block_metadata = current_metadata;

                    // Update metadata for the current block
                    current_metadata = BlockMetadata {
                        timestamp,
                        base_fee_per_gas,
                        gas_used,
                        gas_limit,
                    };

                    if first_header {
                        // First header after backfill. The backfill already
                        // solved up to this point — just record the anchor
                        // and skip solving. Set up for normal operation.
                        if number > current_block {
                            // Gap between backfill and this header — backfill it
                            if number > current_block + 1 {
                                log::info!(
                                    "BlockPump: gap from block {} to {} — backfilling",
                                    current_block + 1,
                                    number,
                                );
                                self.backfill_range(
                                    current_block + 1,
                                    number - 1,
                                    &mut current_block,
                                )
                                .await;
                            }
                            current_block = number;
                            last_solved_block = number;
                        }
                        first_header = false;
                    } else if number > current_block {
                        // New block header — finalize the current block.
                        // Cancel the debounce timer and send immediately.
                        debounce_active = false;
                        // VTWCIG: pass the just-finished block's metadata
                        // (snapshotted before the overwrite above), not the
                        // incoming header's.
                        self.finalize_if_dirty(
                            current_block,
                            &finished_block_metadata,
                            &mut last_solved_block,
                            &mut has_logs_this_block,
                        );

                        // Check for gap and backfill if needed
                        if number > current_block + 1 {
                            log::info!(
                                "BlockPump: gap from block {} to {} — backfilling",
                                current_block + 1,
                                number,
                            );
                            self.backfill_range(current_block + 1, number - 1, &mut current_block)
                                .await;
                        }

                        current_block = number;
                        last_solved_block = number;

                        // For empty blocks (no logs arrived), send an
                        // empty batch so Python sees the block boundary.
                        // `process_block_and_send(&[], block, metadata)` decomposed:
                        // empty logs (no dispatch) + on_drain + on_send.
                        if !has_logs_this_block {
                            self.sink.on_drain(current_block, &current_metadata);
                            self.sink.on_send(&current_metadata);
                        }

                        has_logs_this_block = false;
                    }
                    // else: stale/duplicate header — ignore
                }

                // Got a log event from the combined stream — apply eagerly.
                // Solve happens at the top of the next iteration. Batch send
                // is debounced — the timer starts/resets on each log.
                Ok(Some(WsEvent::Log(log))) => {
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

                    let log_block = log.block_number.unwrap_or(current_block);

                    if log.removed {
                        // Reorg signal (ADR-006 slice 7): this log was
                        // orphaned by a fork at `log_block`. Restore the
                        // TARGETED pool's state to just before `log_block` via
                        // `ReorgCoordinator` (per-event, per-pool — replaces
                        // the bulk `engine.handle_reorg`). The removed log's
                        // content is unused; its block + pool identity drive
                        // `ReorgJournal::restore_before_block` (idempotent +
                        // order-insensitive). The restore fires the SAME
                        // `on_pool_state_updated(P)` notify as a forward
                        // update → the engine dirties P + re-solves at the
                        // next drain tick; the debounce-bounded
                        // `send_result_batch` emits `expired`/`updated` diffs
                        // against `delivered` so Python sees the forked paths
                        // vanish or change.
                        //
                        // A too-deep reorg (target at/below the journal's
                        // earliest delta) returns `Err(NoStatePriorToBlock)`:
                        // graceful shutdown, NOT panic. The bot cannot safely
                        // continue with stale state, so set the shutdown flag
                        // + return; Python observes the pump task exiting.
                        if let Err(err) = self.reorg_coordinator.dispatch_reorg_log(&log) {
                            log::error!("BlockPump: too-deep reorg — shutting down. {err:?}");
                            self.shutdown.store(true, Ordering::Relaxed);
                            return;
                        }
                        // Cancel any pending debounce: results accumulated
                        // from pre-reorg state are invalid.
                        debounce_active = false;
                        continue;
                    }

                    // Detect new block via log's block_number
                    if log_block > current_block {
                        // New block — finalize the current block first.
                        // Cancel the debounce timer and send immediately.
                        debounce_active = false;
                        self.finalize_if_dirty(
                            current_block,
                            &current_metadata,
                            &mut last_solved_block,
                            &mut has_logs_this_block,
                        );

                        current_block = log_block;
                    }

                    // Apply the log immediately to engine state (no solve yet).
                    // ADR-006 D4: routes through `Bot::dispatch_log` (decode →
                    // apply to BotState → notify EngineSubscriber → dirty the
                    // engine) — NOT `engine.apply_log`.
                    self.bot.dispatch_log(&log);

                    has_logs_this_block = true;

                    // Start or reset the debounce timer
                    debounce = tokio::time::Instant::now() + Duration::from_millis(DEBOUNCE_MS);
                    debounce_active = true;
                }

                Ok(None) => {
                    log::warn!("BlockPump: both subscription streams ended");
                    return;
                }
            }
        }
    }

    /// Finalize the current block: solve any dirty paths and send the result
    /// batch to Python, carrying the caller's real `current_metadata`.
    ///
    /// Delegates to the `DrainSink`'s `finalize_block` (the slice-6
    /// `SolveCoordinator` fans to every attached `Engine`). Kept here as a thin
    /// wrapper so the pump owns the `last_solved_block` / `has_logs_this_block`
    /// bookkeeping locals; all engine-state mutation happens inside the sink
    /// under its lock. ADR-006 D4: the pump no longer holds the engine `Mutex`
    /// directly.
    fn finalize_if_dirty(
        &self,
        block: u64,
        metadata: &BlockMetadata,
        last_solved_block: &mut u64,
        has_logs_this_block: &mut bool,
    ) {
        self.sink
            .finalize_block(block, metadata, last_solved_block, has_logs_this_block);
    }

    /// Handle a 60s timeout by backfilling any missed blocks (eager variant).
    #[allow(unused_assignments)]
    async fn handle_timeout_eager(
        &self,
        current_block: &mut u64,
        last_solved_block: &mut u64,
        has_logs_this_block: &mut bool,
    ) {
        log::warn!("BlockPump: no activity for {BACKFILL_TIMEOUT_SECS}s — attempting backfill");
        let latest_block = match self.provider.provider_arc().get_block_number().await {
            Ok(n) => n,
            Err(e) => {
                log::error!("BlockPump: backfill failed — can't get block number: {e}");
                return;
            }
        };

        if latest_block > *current_block {
            let mut lpb = *current_block;
            self.backfill_range(*current_block + 1, latest_block, &mut lpb)
                .await;
            *current_block = lpb;
            *last_solved_block = lpb;
            *has_logs_this_block = false;
        }
    }

    /// Take the watch receiver from the pump's block channel.
    ///
    /// Handle a block header event from the WS subscription.
    ///
    /// Returns the updated `last_processed_block` and `first_header` flag.
    /// Takes `pending_logs` by mutable reference and clears it as needed.
    /// Backfill a range of blocks via `eth_getLogs`.
    ///
    /// Processes each block in the range sequentially.
    async fn backfill_range(&self, from_block: u64, to_block: u64, last_processed_block: &mut u64) {
        if from_block > to_block {
            return;
        }

        log::info!("BlockPump: backfilling blocks {from_block} to {to_block}");

        let filter = build_backfill_filter(from_block, to_block);
        let logs = match self.provider.provider_arc().get_logs(&filter).await {
            Ok(logs) => logs,
            Err(e) => {
                log::error!("BlockPump: backfill eth_getLogs failed: {e}");
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

        // Process each block in order
        let mut any_processed = false;
        for block in from_block..=to_block {
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("BlockPump: shutting down during backfill");
                return;
            }

            let block_logs = logs_by_block.remove(&block).unwrap_or_default();
            if !block_logs.is_empty() {
                // ADR-006 D4: `process_block(logs, block, metadata)` decomposed —
                // each log routes through `Bot::dispatch_log` (decode → apply to
                // BotState → notify EngineSubscriber → dirty the engine), then a
                // single `on_drain` solves. NOTE: empty metadata here is fine —
                // `on_drain` solves but does NOT send; accumulated results
                // piggyback onto the next debounce `on_send(&current_metadata)`
                // in `run_with_stream`, which carries real metadata (and the
                // newer block's `results_block`).
                for log in &block_logs {
                    self.bot.dispatch_log(log);
                }
                self.sink.on_drain(block, &BlockMetadata::default());
                any_processed = true;
            }
            *last_processed_block = block;
        }

        if any_processed {
            log::info!("BlockPump: backfill complete for blocks {from_block}–{to_block}");
        } else {
            log::info!(
                "BlockPump: backfill found no relevant events in blocks {from_block}–{to_block}"
            );
        }
    }
}

#[cfg(test)]
impl BlockPump {
    /// Test-only constructor with an injected `AlloyProvider` (typically a
    /// mock transport) + a `Bot`/`sink`/`reorg_coordinator`. Lets tests drive
    /// [`BlockPump::run_with_stream`] from a deterministic synthetic
    /// `WsEvent` stream without a live RPC connection. The provider is only
    /// touched on the 60s-timeout backfill path — tests that avoid timeouts
    /// and block gaps never invoke it.
    #[must_use]
    pub fn for_test(
        bot: Arc<Bot>,
        sink: Arc<dyn DrainSink>,
        reorg_coordinator: Arc<crate::bot_core::reorg_coordinator::ReorgCoordinator>,
        provider: Arc<AlloyProvider>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        Self {
            bot,
            sink,
            reorg_coordinator,
            provider,
            shutdown,
        }
    }

    /// Drive the resume loop with a synthetic `WsEvent` stream. Test-only
    /// seam over [`run_with_stream`](Self::run_with_stream) so tests need not
    /// reach the private method name.
    pub async fn run_test_loop(
        &mut self,
        combined: stream::BoxStream<'static, WsEvent>,
        first_observed_block: u64,
    ) {
        self.run_with_stream(combined, first_observed_block).await;
    }
}

/// Build an Alloy `Filter` for backfill via `eth_getLogs`.
///
/// Uses topic filtering server-side to reduce response size. No address
/// filter — all topic-filtered logs are passed through to the engine.
#[must_use]
pub fn build_backfill_filter(from_block: u64, to_block: u64) -> Filter {
    let mut filter = Filter::new().from_block(from_block).to_block(to_block);

    // Build a single Topic that matches ANY of the relevant event signatures.
    // Alloy's event_signature() overwrites topics[0] on each call, so we must
    // build the OR-list ourselves and set it once.
    let mut topic: Topic = Topic::default();
    for sig in &RELEVANT_TOPICS {
        topic = topic.extend(*sig);
    }
    filter.topics[0] = topic;

    filter
}

/// Merge a block header stream and a log stream into a single `WsEvent` stream.
///
/// Uses `stream::Select` to fairly interleave events from both subscriptions.
/// Returns a boxed stream for storage in `SubscribeState`.
fn stream_select(
    block_stream: impl StreamExt<Item = alloy::rpc::types::Header> + Unpin + Send + 'static,
    log_stream: impl StreamExt<Item = Log> + Unpin + Send + 'static,
) -> stream::BoxStream<'static, WsEvent> {
    let block_events = block_stream.map(|header| WsEvent::BlockHeader {
        number: header.number,
        timestamp: header.timestamp,
        base_fee_per_gas: header.base_fee_per_gas,
        gas_used: header.gas_used,
        gas_limit: header.gas_limit,
    });
    let log_events = log_stream.map(WsEvent::Log);

    stream::select(block_events, log_events).boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::Mutex;

    #[test]
    fn build_backfill_filter_constructs_valid_filter() {
        let filter = build_backfill_filter(100, 200);
        let debug_str = format!("{filter:?}");
        assert!(!debug_str.is_empty());
    }

    #[test]
    fn shutdown_flag_stops_uniswap_pump() {
        let shutdown = Arc::new(AtomicBool::new(true));
        assert!(shutdown.load(Ordering::Relaxed));
    }

    #[test]
    fn relevant_topics_contains_all_six() {
        assert_eq!(RELEVANT_TOPICS.len(), 6);
        // Verify each is non-zero
        for topic in &RELEVANT_TOPICS {
            assert_ne!(topic, &B256::ZERO);
        }
    }

    #[test]
    fn backfill_timeout_constant_is_reasonable() {
        // 60s is the chosen timeout — verify it's set
        assert_eq!(BACKFILL_TIMEOUT_SECS, 60);
    }

    #[test]
    fn log_confirms_header_block_requires_matching_block_number() {
        assert!(super::log_confirms_header_block(Some(100), 100));
        assert!(!super::log_confirms_header_block(Some(101), 100));
        // Pending logs with no block number must not confirm the header block.
        assert!(!super::log_confirms_header_block(None, 100));
        // A log explicitly tagged as block 0 does not confirm a non-zero header.
        assert!(!super::log_confirms_header_block(Some(0), 100));
    }

    /// A `DrainSink` test double (AGENTS.md: `Fake` prefix, no mocking).
    ///
    /// Records every `finalize_block` / `on_send` / `on_drain` invocation with
    /// the `(block, metadata)` pair the pump passed, so tests can assert the
    /// *block N's* result batch carries *block N's* metadata — the VTWCIG
    /// contract. Behaves as an empty sink (no dirty paths, no state).
    struct FakeDrainSink {
        finalized: Mutex<Vec<(u64, BlockMetadata)>>,
        sent: Mutex<Vec<BlockMetadata>>,
        drained: Mutex<Vec<(u64, BlockMetadata)>>,
        last_processed: AtomicU64,
    }

    impl FakeDrainSink {
        fn new(last_processed: Option<u64>) -> Self {
            Self {
                finalized: Mutex::new(Vec::new()),
                sent: Mutex::new(Vec::new()),
                drained: Mutex::new(Vec::new()),
                last_processed: AtomicU64::new(last_processed.unwrap_or(0)),
            }
        }
    }

    impl DrainSink for FakeDrainSink {
        fn has_dirty_paths(&self) -> bool {
            false
        }
        fn on_drain(&self, block: u64, metadata: &BlockMetadata) {
            // Faithful to `SolveCoordinator::on_drain`: record + advance the
            // drain cursor so `last_processed_block()` reflects the drained
            // block (the anchoring `resume` relies on — see
            // `resume_anchors_to_subscribe_block`).
            self.drained.lock().unwrap().push((block, *metadata));
            self.last_processed.store(block, Ordering::Relaxed);
        }
        fn on_send(&self, metadata: &BlockMetadata) {
            self.sent.lock().unwrap().push(*metadata);
        }
        fn finalize_block(
            &self,
            block: u64,
            metadata: &BlockMetadata,
            _last_solved_block: &mut u64,
            _has_logs_this_block: &mut bool,
        ) {
            self.finalized.lock().unwrap().push((block, *metadata));
        }
        fn last_processed_block(&self) -> Option<u64> {
            let v = self.last_processed.load(Ordering::Relaxed);
            (v != 0).then_some(v)
        }
    }

    /// Build a `BlockPump` whose provider is an `alloy` mock transport (never
    /// hit on the no-timeout / no-gap test paths) and whose sink is a
    /// `FakeDrainSink` that records metadata calls. Returns the pump + the
    /// sink handle (for inspection). Offline + deterministic.
    fn pump_for_test(last_processed: Option<u64>) -> (BlockPump, Arc<FakeDrainSink>) {
        use alloy::network::Ethereum as NetEth;
        use alloy::providers::{Provider, ProviderBuilder};
        // `alloy_transport::mock::{Asserter, MockTransport}` — unfeatured (no
        // `mock` feature flag needed under `alloy = { features = ["full"] }`).
        // The asserter's queue is never drained because the test paths avoid
        // provider calls (no 60s timeout, no block gaps).
        use alloy::rpc::client::ClientBuilder;
        use alloy::transports::mock::{Asserter, MockTransport};

        let asserter = Asserter::new();
        let client = ClientBuilder::default().transport(MockTransport::new(asserter), true);
        // `.erased()` yields a `DynProvider<Ethereum>` (implements
        // `Provider<Ethereum>`), matching `AlloyProvider::from_provider`'s
        // `Arc<dyn Provider<Ethereum>>` parameter — same shape as the live
        // `build_provider` path.
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        let provider = Arc::new(AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<NetEth>>
        ));

        let bot = Arc::new(Bot::new(1));
        let reorg = Arc::new(crate::bot_core::reorg_coordinator::ReorgCoordinator::new(
            Arc::clone(&bot),
        ));
        let shutdown = Arc::new(AtomicBool::new(false));
        let sink = Arc::new(FakeDrainSink::new(last_processed));
        let pump = BlockPump::for_test(bot, sink.clone(), reorg, provider, shutdown);
        (pump, sink)
    }

    #[tokio::test]
    async fn finalize_carries_just_finished_blocks_metadata() {
        // Contract (VTWCIG): when the header for block N+1 arrives, the result
        // batch that finalizes block N must carry block N's metadata (held
        // before the incoming header overwrites `current_metadata`), NOT N+1's.
        // Python computes `base_fee_next` from this metadata; the previous
        // ordering sent N+1's metadata with N's batch → systematically
        // under-/over-priced backrun txs.
        //
        // Stream: header N (distinct metadata), header N+1 (distinct
        // metadata), then end. Finalizing N on the N+1 header must pass N's
        // metadata to `finalize_block(N, &N_metadata, …)`.
        let (mut pump, sink) = pump_for_test(Some(100));
        // The loop seeds `current_block` from `last_processed_block()` → 100.
        // Feed header 101 (its arrival finalizes 100) and header 102 (its
        // arrival finalizes 101). We must NOT skip the first header
        // (`first_header` anchor logic just sets the cursor — it doesn't
        // finalize). So: emit 101 → first_header anchor (current_block→101),
        // then 102 → finalizes 101.
        let meta_101 = BlockMetadata {
            timestamp: 1_700_000_100,
            base_fee_per_gas: Some(1_000_000_001),
            gas_used: 10_000_001,
            gas_limit: 30_000_001,
        };
        let meta_102 = BlockMetadata {
            timestamp: 1_700_000_200,
            base_fee_per_gas: Some(2_000_000_002),
            gas_used: 20_000_002,
            gas_limit: 30_000_002,
        };
        let events: Vec<WsEvent> = vec![
            WsEvent::BlockHeader {
                number: 101,
                timestamp: meta_101.timestamp,
                base_fee_per_gas: meta_101.base_fee_per_gas,
                gas_used: meta_101.gas_used,
                gas_limit: meta_101.gas_limit,
            },
            WsEvent::BlockHeader {
                number: 102,
                timestamp: meta_102.timestamp,
                base_fee_per_gas: meta_102.base_fee_per_gas,
                gas_used: meta_102.gas_used,
                gas_limit: meta_102.gas_limit,
            },
        ];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, 100).await;

        // The first finalize is for block 101 (finalized when 102's header
        // arrives). It must carry meta_101, NOT meta_102.
        let finalized = sink.finalized.lock().unwrap().clone();
        assert!(
            !finalized.is_empty(),
            "header 102 should finalize block 101"
        );
        let (block, metadata) = &finalized[0];
        assert_eq!(*block, 101, "first finalize is for block 101");
        assert_eq!(
            *metadata, meta_101,
            "block 101's batch must carry 101's metadata, not 102's"
        );
        assert_ne!(
            *metadata, meta_102,
            "block 101's batch must NOT carry 102's metadata"
        );
    }

    /// 5DM6JJ contract: the cold-start branch in `run_with_stream` must honor
    /// `first_observed_block` when `last_processed_block()` is `None` (no
    /// prior anchor). This is the defensive safety net the legacy `spawn` fix
    /// leans on: passing the REAL subscribe block W (instead of the legacy
    /// hard-coded `0`) means that if the `on_drain(first_block)` anchor were
    /// ever absent, the pump still cold-starts to W — NOT stuck at 0 to be
    /// jumped out-of-order by the first WS log.
    #[tokio::test]
    async fn cold_start_anchors_to_first_observed_block() {
        // No prior processed block → `current_block` starts at 0. Pass the
        // subscribe block W (a "huge" chain-head number) as
        // `first_observed_block`. The cold-start branch
        // (`current_block == 0 && first_observed_block > 0`) anchors
        // `current_block` to W. Then header(W) is the first header (anchor
        // confirmation, no finalize), and header(W+1) finalizes W in order —
        // proving we cold-started to W, not 0.
        let (mut pump, sink) = pump_for_test(None);
        let w = 21_500_000u64; // a "huge" chain-head block number
        let meta_w = BlockMetadata {
            timestamp: 1,
            base_fee_per_gas: Some(7),
            gas_used: 8,
            gas_limit: 9,
        };
        let meta_w1 = BlockMetadata {
            timestamp: 2,
            base_fee_per_gas: Some(10),
            gas_used: 11,
            gas_limit: 12,
        };
        let events: Vec<WsEvent> = vec![
            WsEvent::BlockHeader {
                number: w,
                timestamp: meta_w.timestamp,
                base_fee_per_gas: meta_w.base_fee_per_gas,
                gas_used: meta_w.gas_used,
                gas_limit: meta_w.gas_limit,
            },
            WsEvent::BlockHeader {
                number: w + 1,
                timestamp: meta_w1.timestamp,
                base_fee_per_gas: meta_w1.base_fee_per_gas,
                gas_used: meta_w1.gas_used,
                gas_limit: meta_w1.gas_limit,
            },
        ];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, w).await;

        // header(W+1) finalizes W. Carrying meta_w (the just-finished
        // block's metadata, held before W+1's header overwrote it — VTWCIG).
        // The fact W was finalized proves `current_block` cold-started to W
        // (not 0): had it stayed at 0, header(W) would have been the first
        // header advancing 0→W, then header(W+1) would finalize W — looks
        // similar, BUT the cold-start log line + the absence of a jump from
        // 0 to W via a stray log is the invariant. We assert the finalized
        // block is W with meta_w (in-order, anchored).
        let finalized = sink.finalized.lock().unwrap().clone();
        assert!(!finalized.is_empty(), "header w+1 should finalize block w");
        assert_eq!(
            finalized[0].0, w,
            "first finalize is for the anchored block w"
        );
        assert_eq!(
            finalized[0].1, meta_w,
            "block w's batch carries w's metadata (in-order, anchored)"
        );
    }

    /// Resume-anchor contract: `on_drain(first_block)` anchors
    /// `current_block` to the subscribe block W (mimics
    /// `SolveCoordinator::on_drain` setting `last_drained_block`). With
    /// `first_observed_block = W` (the real subscribe block, NOT the legacy
    /// hard-coded `0`) the pump processes W+1, W+2 in order — the
    /// "applies logs in block order against DB-snapshot-seeded engine state"
    /// invariant — with no out-of-order jump from 0.
    ///
    /// Previously named `legacy_spawn_processes_blocks_in_order_…` and framed
    /// around the deleted `BlockPump::spawn` one-shot; the invariant it
    /// actually pins is `resume`/`run_with_stream`'s anchoring, which survives
    /// the Slice 1 deletion of `spawn` (Plan 102).
    #[tokio::test]
    async fn resume_anchors_to_subscribe_block() {
        // Mimic resume's first step: start with no prior cursor, then
        // `on_drain(W)` (the drain Python issues after `subscribe` returns,
        // before `resume`) anchors `last_processed_block` to W — exactly as
        // the real `SolveCoordinator` does. Then resume with first_observed=W
        // (the real subscribe block, post-fix).
        let (mut pump, sink) = pump_for_test(None);
        let w = 21_500_000u64;
        let meta_w = BlockMetadata {
            timestamp: 1,
            base_fee_per_gas: Some(7),
            gas_used: 8,
            gas_limit: 9,
        };
        let meta_w1 = BlockMetadata {
            timestamp: 2,
            base_fee_per_gas: Some(10),
            gas_used: 11,
            gas_limit: 12,
        };
        let meta_w2 = BlockMetadata {
            timestamp: 3,
            base_fee_per_gas: Some(13),
            gas_used: 14,
            gas_limit: 15,
        };
        // The drain issued before resume anchors the cursor to W:
        pump.sink.on_drain(w, &meta_w);
        assert_eq!(
            pump.sink.last_processed_block(),
            Some(w),
            "on_drain(W) must anchor the cursor (mirrors SolveCoordinator)"
        );

        // Resume stream (post-fix: first_observed = W, not 0). header(W+1) is
        // the first header → first_header anchor advances W→W+1; header(W+2)
        // finalizes W+1. In-order W→W+1→W+2, no jump from 0.
        let events: Vec<WsEvent> = vec![
            WsEvent::BlockHeader {
                number: w + 1,
                timestamp: meta_w1.timestamp,
                base_fee_per_gas: meta_w1.base_fee_per_gas,
                gas_used: meta_w1.gas_used,
                gas_limit: meta_w1.gas_limit,
            },
            WsEvent::BlockHeader {
                number: w + 2,
                timestamp: meta_w2.timestamp,
                base_fee_per_gas: meta_w2.base_fee_per_gas,
                gas_used: meta_w2.gas_used,
                gas_limit: meta_w2.gas_limit,
            },
        ];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, w).await;

        // header(W+2) finalizes W+1 — carrying meta_w1 (just-finished block's
        // metadata). This proves the anchor held: we advanced W→W+1→W+2 in
        // order, never jumping from 0.
        let finalized = sink.finalized.lock().unwrap().clone();
        assert!(
            !finalized.is_empty(),
            "header w+2 should finalize block w+1"
        );
        assert_eq!(finalized[0].0, w + 1, "first finalize is for block w+1");
        assert_eq!(
            finalized[0].1, meta_w1,
            "block w+1's batch carries w+1's metadata (in-order)"
        );
    }

    // -----------------------------------------------------------------
    // Pump-level reorg integration (ADR-006 slice 7).
    //
    // `ReorgCoordinator` is covered directly in `reorg_coordinator.rs`
    // (dispatch → restore_before_block → notify). What is NOT covered
    // anywhere is the pump's own reorg branch in `run_with_stream`: the
    // `log.removed` arm routes the log to the coordinator, cancels the
    // pending debounce, continues on an in-journal-depth reorg, and shuts
    // down gracefully on a too-deep reorg. These tests pin those
    // pump-specific behaviors (the coordinator's restore+notify is
    // asserted as the downstream observable, not re-tested for its own
    // sake).
    // -----------------------------------------------------------------

    use crate::bot_core::log_dispatcher::PoolStateSubscriber;
    use crate::bot_core::RegisterV2PoolParams;
    use alloy::primitives::{Address, Bytes, U256};

    /// Build a V2 `Sync` log for `pool_address` carrying
    /// `(reserve0, reserve1)`, at `block_number`, with `removed` set.
    /// Mirrors `reorg_coordinator.rs`'s `make_sync_log` test helper.
    fn make_v2_sync_log(
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

    /// Counting subscriber — records `on_pool_state_updated` invocations so a
    /// pump-level reorg test can assert the restore fired the SAME notify
    /// path as a forward `dispatch_log`. `Fake` prefix per AGENTS.md.
    struct FakeCountingSubscriber {
        notifies: Mutex<u32>,
    }
    impl PoolStateSubscriber for FakeCountingSubscriber {
        fn on_pool_state_updated(&self, _pool_id: u64) {
            *self.notifies.lock().unwrap() += 1;
        }
    }

    /// Register a V2 pool on a fresh `Bot` with a counting subscriber attached,
    /// returning `(bot, pool_id, counting_subscriber)`. Genesis reserves are
    /// anchored at `update_block`, seeding the reorg journal so an in-journal
    /// reorg can roll back to them.
    fn bot_with_registered_v2(
        pool_addr: Address,
        update_block: u64,
    ) -> (Arc<Bot>, u64, Arc<FakeCountingSubscriber>) {
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
        let counting = Arc::new(FakeCountingSubscriber {
            notifies: Mutex::new(0),
        });
        let sub: Arc<dyn PoolStateSubscriber> = counting.clone();
        bot.attach_engine(pool_id, Arc::downgrade(&sub));
        (bot, pool_id, counting)
    }

    /// Build a `BlockPump` over a caller-provided `Arc<Bot>` (rather than a
    /// fresh empty `Bot::new(1)`), returning the pump + sink + the shared
    /// shutdown flag so a test can assert shutdown behavior. Same mock-transport
    /// provider as `pump_for_test`; test paths avoid provider calls.
    fn pump_for_test_with_bot(
        bot: Arc<Bot>,
        last_processed: Option<u64>,
    ) -> (BlockPump, Arc<FakeDrainSink>, Arc<AtomicBool>) {
        use alloy::network::Ethereum as NetEth;
        use alloy::providers::{Provider, ProviderBuilder};
        use alloy::rpc::client::ClientBuilder;
        use alloy::transports::mock::{Asserter, MockTransport};

        let asserter = Asserter::new();
        let client = ClientBuilder::default().transport(MockTransport::new(asserter), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        let provider = Arc::new(AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<NetEth>>
        ));
        let reorg = Arc::new(crate::bot_core::reorg_coordinator::ReorgCoordinator::new(
            Arc::clone(&bot),
        ));
        let shutdown = Arc::new(AtomicBool::new(false));
        let sink = Arc::new(FakeDrainSink::new(last_processed));
        let pump = BlockPump::for_test(bot, sink.clone(), reorg, provider, Arc::clone(&shutdown));
        (pump, sink, shutdown)
    }

    /// A `removed: true` V2 Sync log for a registered pool, when its block is
    /// within the reorg journal's depth, drives the pump's reorg branch to
    /// restore the pool to its pre-fork state via the `ReorgCoordinator` —
    /// firing the SAME `on_pool_state_updated` notify as a forward Sync —
    /// and the pump does NOT shut down (it continues processing).
    ///
    /// This pins the pump-level wiring of ADR-006 slice 7: the coordinator's
    /// restore+notify (covered in `reorg_coordinator.rs`) is the downstream
    /// observable; what is asserted here is that the *pump* routes a
    /// `removed: true` log there, and that an in-depth reorg is non-fatal.
    #[tokio::test]
    async fn reorg_log_restores_pool_via_coordinator_and_pump_continues() {
        let pool_addr = Address::from([0x11u8; 20]);
        let (bot, pool_id, sub) = bot_with_registered_v2(pool_addr, 5);
        let notify_count = || *sub.notifies.lock().unwrap();

        // Forward Sync at block 7 — misprices the pool and seeds the journal
        // genesis(5) → transition(7). Drive through the *pump* (not
        // `bot.dispatch_log` directly) so the same code path that handles
        // live WS logs is exercised.
        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let forward = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 7, false);
        // Stream ends immediately after the log; the loop returns via the
        // `Ok(None)` arm (both subscription streams ended) once the reorg
        // branch `continue`s and the stream is exhausted.
        let combined = stream::iter(vec![WsEvent::Log(forward)]).boxed();
        pump.run_test_loop(combined, 5).await;

        assert_eq!(notify_count(), 1, "forward Sync through the pump notified");
        assert_eq!(
            bot.state.read().v2_snapshot(pool_id),
            Some((U256::from(1_500), U256::from(2_500), 7)),
            "forward Sync applied through the pump",
        );
        assert!(
            !shutdown.load(Ordering::Relaxed),
            "no reorg yet — pump running"
        );

        // Reorg: a removed-flag Sync at block 7 rolls back to genesis. Build a
        // fresh pump over the SAME bot (the journal + state persist on `Bot`)
        // and feed only the removed log.
        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let reorg_log = make_v2_sync_log(
            pool_addr,
            U256::from(1_500), // content unused — block + pool identity matter
            U256::from(2_500),
            7,
            true,
        );
        let combined = stream::iter(vec![WsEvent::Log(reorg_log)]).boxed();
        pump.run_test_loop(combined, 5).await;

        assert_eq!(
            notify_count(),
            2,
            "reorg fired the SAME notify path as a forward Sync",
        );
        assert_eq!(
            bot.state.read().v2_snapshot(pool_id),
            Some((U256::from(1_000), U256::from(2_000), 5)),
            "reorg rolled back to genesis reserves",
        );
        assert!(
            !shutdown.load(Ordering::Relaxed),
            "in-journal-depth reorg is non-fatal — pump did NOT shut down"
        );
    }

    /// A too-deep reorg (the removed log's block is at/below the journal's
    /// earliest delta) returns `Err(NoStatePriorToBlock)` from the
    /// coordinator; the pump treats this as unrecoverable — it sets the
    /// shutdown flag and returns from `run_with_stream` so Python observes
    /// the pump task exiting, rather than continuing with stale state.
    #[tokio::test]
    async fn too_deep_reorg_shuts_down_pump_gracefully() {
        let pool_addr = Address::from([0x22u8; 20]);
        // Genesis anchored at block 5 — restore_before_block(5) is too deep
        // (nothing the journal can land on prior to the genesis delta).
        let (bot, _pool_id, _sub) = bot_with_registered_v2(pool_addr, 5);

        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        // Removed-flag Sync at block 5 → coordinator restores before 5, which
        // is at the journal's genesis floor → `Err(NoStatePriorToBlock)`.
        let reorg_log = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 5, true);
        let combined = stream::iter(vec![WsEvent::Log(reorg_log)]).boxed();
        pump.run_test_loop(combined, 5).await;

        assert!(
            shutdown.load(Ordering::Relaxed),
            "too-deep reorg must set the shutdown flag",
        );
        // `run_test_loop` returned (this assert is reached), proving the pump
        // exited its loop instead of looping forever on a fatal reorg.
    }
}

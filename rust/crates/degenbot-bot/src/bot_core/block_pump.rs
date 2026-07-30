//! `BlockPump` — `Bot`'s WS transport + drain loop (ADR-006 D4).
//!
//! Generalized from the `BlockPump`: holds `Arc<Bot>` +
//! `Arc<dyn DrainSink>` instead of `Arc<Mutex<ArbitrageEngine>>`. Per WS log,
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
//! **Critical ordering**: backfill must run AFTER `subscribe()` returns but
//! BEFORE `resume_from_subscribe()`. The `DrainSink`'s
//! `last_processed_block()` is the backfill-start boundary. (Pre-epic-P73ER6
//! Python orchestrated this manually; the epic relocates backfill into the
//! core, driven automatically by `resume`.)

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::B256;
use alloy::rpc::types::{Filter, Log, Topic};
use futures_util::{stream, StreamExt};
use tokio::time::timeout;

use crate::bot_core::{drain_sink::DrainSink, BlockMetadata, Bot};
use crate::bot_core::{BlockClock, HeaderDecision, LogDecision};
use degenbot_decoders::v2_sync_decoder::V2_SYNC_TOPIC;
use degenbot_decoders::v3_mint_burn_decoder::{V3_BURN_TOPIC, V3_MINT_TOPIC};
use degenbot_decoders::v3_swap_decoder::V3_SWAP_TOPIC;
use degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC;
use degenbot_decoders::v4_swap_decoder::V4_SWAP_TOPIC;
use degenbot_rpc::provider::AlloyProvider;

/// How long to wait with no activity before assuming the connection is dead.
const BACKFILL_TIMEOUT_SECS: u64 = 60;

/// After the first dirty WS log for a block, wait this long for more logs
/// before solving and dispatching results to Python. Each new log resets
/// the timer. This debouncing ensures one dispatch per burst of logs
/// rather than one per individual log.
const DEBOUNCE_MS: u64 = 50;

/// If no block header arrives within this window, poll `eth_blockNumber`
/// and backfill the gap — independent of log activity.
///
/// The `newHeads` WS subscription can die silently while `logs` keeps
/// flowing. `stream::select` masks a dead block stream as long as logs
/// arrive every `< BACKFILL_TIMEOUT_SECS` (the combined-stream-silence
/// backfill path never fires). Because only headers advance `current_block`,
/// every result batch would then be stamped with a frozen block while
/// prices keep updating from the live log applies — looking like the bot is
/// running but making no block progress. This watchdog independently
/// detects header staleness (capped `wait_timeout` wakes the loop by the
/// deadline even under dense log pressure) and runs the same
/// `handle_timeout_eager` catch-up the no-activity path uses.
const HEADER_STALENESS_SECS: u64 = 30;

/// Default backfill chunk size (blocks per `eth_getLogs` request) for the
/// snapshot→WS gap closed automatically inside `resume_from_subscribe`
/// (J3FMDO). Mirrors the `pyo3` `backfill_from_snapshot` default (`chunk_size` = 2000):
/// the per-chunk response size stays under `eth_getLogs` payload caps.
const DEFAULT_BACKFILL_CHUNK_SIZE: u64 = 2000;

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
    /// If no header arrives within this window, poll `eth_blockNumber` and
    /// backfill regardless of log activity (dead-`newHeads` recovery — see
    /// `HEADER_STALENESS_SECS`). Overridable in tests via
    /// `set_header_staleness_for_test`.
    #[allow(dead_code)] // wired in a follow-up commit (header-staleness watchdog)
    header_staleness: Duration,
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
            header_staleness: Duration::from_secs(HEADER_STALENESS_SECS),
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

    /// Resume the pump from a subscribe state — auto-backfilling the
    /// snapshot→WS gap (J3FMDO) before the live loop begins.
    ///
    /// When the core `BotState` carries a snapshot seed `S` (set by
    /// `Bot::load_snapshot_from_db` or `load_*_from_py`) strictly less than
    /// the first observed WS block `W`, this method first awaits
    /// [`backfill_from_snapshot`](Self::backfill_from_snapshot) with the
    /// pump's own provider — applying `S+1..W-1` log state under
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
        let combined = subscribe_state
            .combined_stream
            .expect("resume() called without WS stream — did you call subscribe() first?");
        let first_block = subscribe_state.first_block;
        // J3FMDO: auto-backfill the snapshot→WS gap before the live loop. The
        // backfill only buffers state into BotState (no solve, no on_send);
        // result batches therefore do not flow pre-resume.
        if let Err(e) = self.backfill_to_ws_block(first_block).await {
            log::error!(
                "BlockPump: auto-backfill failed — starting live loop from {first_block} (gap not closed): {e}"
            );
        }
        self.run_with_stream(combined, first_block).await;
    }

    /// Close the snapshot→WS gap by buffering `eth_getLogs(S+1..W-1)` into the
    /// core `BotState`'s per-pool backfill buffer (no solve, no `on_send`).
    ///
    /// This is the SYNCHRONOUSLY-awaitable half of `resume_from_subscribe` —
    /// `PumpState::resume` `block_on`s it BEFORE spawning the live loop so
    /// Python's `build_paths` (which drains the per-pool backfill buffer via
    /// `apply_backfill_buffer_v3`) cannot race the backfill. Pre-fix the
    /// backfill ran inside the spawned `resume_from_subscribe` task and
    /// `resume` returned immediately, so an active pool's burn was not yet
    /// buffered when `build_paths` drained → `VerificationMismatchError` at
    /// post-drain verify (2026-07-12 backrun crash).
    ///
    /// No-op when `S` is unset (cold start), `S >= W` (catch-up snapshot), or
    /// `S == 0`. Errors log + return (the live loop still starts from `W`).
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` if a chunk's `eth_getLogs` call fails (message
    /// includes the offending block range + provider error).
    pub async fn backfill_to_ws_block(&self, ws_block: u64) -> Result<u64, String> {
        let s = self.bot.state_arc().read().snapshot_seed_block();
        let Some(seed) = s else { return Ok(0) };
        if seed == 0 || ws_block == 0 || seed >= ws_block {
            return Ok(0);
        }
        log::info!(
            "BlockPump: auto-backfill from snapshot block {seed} to WS block {ws_block} before resume"
        );
        self.backfill_from_snapshot(ws_block, DEFAULT_BACKFILL_CHUNK_SIZE)
            .await
    }

    /// Run the main pump loop with an existing WS stream.
    ///
    /// Processes logs eagerly: each WS log is applied to engine state
    /// immediately and affected paths are solved right away, without
    /// waiting for a block header. Block headers provide metadata
    /// (timestamp, fees) and handle empty-block detection.
    #[allow(unused_assignments, clippy::too_many_lines)]
    pub async fn run_with_stream(
        &mut self,
        mut combined: stream::BoxStream<'static, WsEvent>,
        first_observed_block: u64,
    ) {
        // [DIAG] newHeads-stall investigation: track header arrivals so the
        // log shows, in production, whether `BlockHeader` events actually stop
        // arriving (subscription silent) vs. arrive but the arm doesn't fire
        // (pump not polling / bug). Remove once the freeze root cause is
        // confirmed and fixed.
        const DIAG_STATS_INTERVAL: Duration = Duration::from_secs(10);

        // hotpath drain-path tracer bullet (`src/profiling.rs`): hold a
        // profiling guard for the whole pump loop iff `DEGENBOT_HOTPATH=1`.
        // No-op (not even constructed) otherwise, and a no-op stub when the
        // `hotpath` Cargo feature is off. Dropping at loop exit writes the
        // report; for a long-running bot use `HOTPATH_SHUTDOWN_MS`.
        let _hotpath_guard = crate::profiling::hotpath_guard("block_pump");

        let relevant_topic_set: HashSet<B256> = RELEVANT_TOPICS.into_iter().collect();

        // Read the last block processed by the engine (the post-backfill
        // cursor when the snapshot→WS gap was closed inside resume; cold-start
        // otherwise). J3FMDO: the core `BlockPump::backfill_from_snapshot`
        // applies state via `BotState::process_backfill_logs`, which advances
        // neither the sink's drain cursor (only `on_drain`/`finalize_block`
        // do) nor the engine's `last_processed_block`. Hence on the
        // post-backfill resume path the sink's `last_processed_block` is still
        // `None` and the branch below re-anchors on `first_observed_block`.
        let mut current_block: u64 = self.sink.last_processed_block().unwrap_or(0);

        let snapshot_seed = self.bot.state_arc().read().snapshot_seed_block();
        if current_block == 0 && first_observed_block > 0 {
            current_block = first_observed_block;
            if let Some(seed) = snapshot_seed {
                if seed > 0 && seed < first_observed_block {
                    log::info!(
                        "BlockPump: resuming from block {first_observed_block} (backfilled snapshot gap {start}–{end})",
                        start = seed + 1,
                        end = first_observed_block - 1
                    );
                } else {
                    log::info!("BlockPump: cold start from block {first_observed_block}");
                }
            } else {
                log::info!("BlockPump: cold start from block {first_observed_block}");
            }
        } else {
            log::info!("BlockPump: starting from block {current_block}");
        }

        // Track the last block we've solved for: owned by the engine since
        // ergo task LEZJAS (the pump's `last_solved_block` local retired).
        // Seed it to the pump's starting block so the first `finalize_block`
        // guard fires only on a genuine advance (matching the prior local
        // init). A mid-flight-joining engine inherits via `set_last_solved_block`
        // (ADR-006 D4).
        self.sink.set_last_solved_block(current_block);
        // Whether we're past the first header after resume. The first
        // header establishes our anchor but shouldn't trigger a solve
        // (backfill already solved up to this point).
        let mut first_header = true;
        // Current block metadata — updated from headers, used for
        // solve batches when logs close out a block.
        let mut current_metadata: BlockMetadata = BlockMetadata::default();
        // `has_logs_this_block` is engine-owned since LEZJAS — driven through
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
        let mut publish_pending = false;

        // ADR-008 per-block state machine. The clock is the authority for
        // block completeness (the tombstone) and the cursor; the pump loop is
        // a thin async driver translating its decisions into sink calls +
        // backfill + shutdown. A header alone NEVER advances the cursor —
        // only `advance_to_drained` (after the tombstone) does.
        let mut clock = BlockClock::new();
        // Per-block metadata, snapshotted from each block's header. A block's
        // tombstone (first log for N+1) may arrive AFTER header N+1 overwrote
        // `current_metadata`, so the result batch that finalizes N must carry
        // N's OWN metadata, retrieved here (VTWCIG).
        let mut block_metadata: HashMap<u64, BlockMetadata> = HashMap::new();

        // [DIAG]
        let mut diag_header_count: u64 = 0;
        let mut diag_log_count: u64 = 0;
        let mut diag_last_header_at = tokio::time::Instant::now();
        let mut diag_last_stats = tokio::time::Instant::now();

        loop {
            // Solve any dirty paths accumulated from the previous iteration's
            // log(s). This naturally coalesces multiple logs that arrive
            // between await points — only one solve per batch of WS events.
            // Note: solving is decoupled from sending — the pump controls
            // when result batches are dispatched to Python.
            {
                if self.sink.has_dirty_paths() {
                    self.sink.on_drain(current_block, &current_metadata);
                    // LEZJAS: engine owns `last_solved_block` now — mark this
                    // block solved so the next `finalize_block` guard no-ops.
                    self.sink.set_last_solved_block(current_block);
                }
            }

            // ADR-008 D2: solver-release gate. `publish_pending` is set when a forward
            // log applies (block becomes quiesced). The flush below fires
            // `on_send` (gated on `consume_quiesced`) only at a settle point —
            // a timeout with no new event (coalescing a same-block burst into
            // one publish at the tail) OR stream exhaustion. This replaces the
            // wall-clock `DEBOUNCE_MS` send timer: publication is gated on the
            // truth condition (all dispatched logs applied), not schedule.

            // Check shutdown
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("BlockPump: shutting down");
                return;
            }

            // Wait for the next event. Use a shorter settle window when a publish is
            // pending so the quiesce-gated flush fires promptly if no new log
            // arrives (coalescing a same-block burst); otherwise the long
            // inactivity backfill window. A new event arriving before the
            // window elapses cancels the flush (the burst is still in flight).
            let wait_timeout = if publish_pending {
                Duration::from_millis(DEBOUNCE_MS)
            } else {
                Duration::from_secs(BACKFILL_TIMEOUT_SECS)
            };
            let event = timeout(wait_timeout, combined.next()).await;

            match event {
                // Settle point — no new event in the window. Flush the
                // quiesce-gated publish, OR (if nothing pending) the 60s
                // inactivity backfill path.
                Err(_) => {
                    if publish_pending {
                        if let Some(open) = clock.latest_observed() {
                            if clock.consume_quiesced(open) {
                                self.sink.on_send(&current_metadata);
                            }
                        }
                        publish_pending = false;
                    } else {
                        // No activity for 60s — try to backfill
                        self.handle_timeout_eager(
                            &mut current_block,
                            &mut clock,
                            &mut block_metadata,
                            &mut publish_pending,
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
                    // [DIAG]
                    diag_header_count += 1;
                    let diag_gap = if diag_header_count == 1 {
                        0.0
                    } else {
                        diag_last_header_at.elapsed().as_secs_f64()
                    };
                    log::info!(
                        "BlockPump: [DIAG] HEADER block={number} (#{diag_header_count}) gap={diag_gap:.1}s"
                    );
                    if diag_header_count > 1
                        && diag_last_header_at.elapsed() > Duration::from_secs(20)
                    {
                        log::warn!(
                            "BlockPump: [DIAG] *** HEADER STALL: headers were silent {diag_gap:.1}s before block {number}"
                        );
                    }
                    diag_last_header_at = tokio::time::Instant::now();
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
                    // ADR-008: a header alone NEVER finalizes/drain. The
                    // clock records the block's metadata (so a later
                    // tombstone-driven finalize carries the CORRECT block's
                    // metadata — the tombstone log for N+1 may arrive AFTER
                    // header N+1, at which point `current_metadata` would
                    // already hold N+1's). `notify_block` still fires so
                    // Python's block clock tracks `newHeads`.
                    current_metadata = BlockMetadata {
                        timestamp,
                        base_fee_per_gas,
                        gas_used,
                        gas_limit,
                    };
                    let header_decision = clock.observe_header(number);
                    if matches!(header_decision, HeaderDecision::Stale) {
                        // duplicate/stale header — ignore
                        continue;
                    }
                    // Snapshot this block's metadata so its (deferred)
                    // tombstone-finalize carries the correct block's metadata.
                    block_metadata.insert(number, current_metadata);

                    let is_first_header = first_header;
                    if is_first_header {
                        // First header after backfill. The backfill already
                        // solved up to this point — just record the anchor and
                        // skip solving. Set up for normal operation.
                        if number > current_block {
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
                                    &mut clock,
                                    &mut block_metadata,
                                    &mut publish_pending,
                                )
                                .await;
                            }
                            current_block = number;
                            // LEZJAS: the backfill path solved up to `number`
                            // already; mark it solved on the engine so the next
                            // `finalize_block` guard no-ops for this block.
                            self.sink.set_last_solved_block(number);
                            self.sink.notify_block(current_block, &current_metadata);
                        }
                        first_header = false;
                    } else if number > current_block {
                        // New block header, but the previous block is NOT
                        // finalized here — only the tombstone (first log for
                        // N+1) closes it (ADR-008 D1). Gap backfill still runs.
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
                                &mut clock,
                                &mut block_metadata,
                                &mut publish_pending,
                            )
                            .await;
                        }

                        current_block = number;
                        self.sink.notify_block(current_block, &current_metadata);
                    }
                    // The `PendingSuccessor` / `OpenNew` decisions carry no
                    // pump action beyond the above — the liveness-probe signal
                    // (dead-logs-sub detection) is handled by the timeout path.
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

                    // ADR-008: route the log via the per-block state machine.
                    // The clock decides whether this is a forward dispatch, a
                    // tombstone (first removed:false log for N+1), a reorg
                    // signal, or an unreliable-WS late forward (→ shutdown).
                    match clock.observe_log(log_block, log.removed) {
                        LogDecision::EnterReorg(reorg_block) => {
                            // Reorg: per-event per-pool restore via the
                            // coordinator (ADR-006 slice 7). A too-deep reorg
                            // → graceful shutdown. The previous block was
                            // tombstoned; this `removed: true` log reopens it.
                            // Visible operator signal so an unwind is no longer
                            // silent — the prior success path logged nothing,
                            // making a duplicate block log ambiguous (reorg
                            // vs. WS duplication).
                            log::warn!(
                                "BlockPump: chain reorg detected at block \
                                 {reorg_block} (removed log) — entering unwind path"
                            );
                            if let Err(err) = self.reorg_coordinator.dispatch_reorg_log(&log) {
                                log::error!("BlockPump: too-deep reorg — shutting down. {err:?}");
                                self.shutdown.store(true, Ordering::Relaxed);
                                return;
                            }
                            // Cancel any pending publish: results accumulated
                            // from pre-reorg state are invalid.
                            publish_pending = false;
                            continue;
                        }
                        LogDecision::ContinueReorg => {
                            // Subsequent removed: true log in the same window —
                            // restore another pool at `log_block`. Trailing the
                            // first event lets the operator correlate successive
                            // unwinds in the same reorg.
                            log::warn!(
                                "BlockPump: reorg continues — restoring pool for \
                                 removed log at block {log_block}"
                            );
                            if let Err(err) = self.reorg_coordinator.dispatch_reorg_log(&log) {
                                log::error!("BlockPump: too-deep reorg — shutting down. {err:?}");
                                self.shutdown.store(true, Ordering::Relaxed);
                                return;
                            }
                            publish_pending = false;
                            continue;
                        }
                        LogDecision::CloseReorg { new_head } => {
                            // Reorg window closed — the coordinator restored
                            // unwound pools per-event; this forward log's block
                            // is the new head. Resume forward tracking from it.
                            log::info!(
                                "BlockPump: reorg window closed — resuming \
                                 forward tracking from block {new_head}"
                            );
                            current_block = new_head;
                            publish_pending = false;
                            // Fall through to dispatch this forward log.
                        }
                        LogDecision::TombstonePrevious(prev) => {
                            // First removed:false log for N+1 → tombstone N.
                            // Finalize N with N's OWN metadata (snapshotted
                            // when N's header arrived), not current_metadata
                            // which may now hold N+1's — VTWCIG. The terminal
                            // publish (finalize_block) supersedes any pending
                            // quiesce publish for the open block.
                            //
                            // YLYJM2: the tombstone is the ADR-008 D1 signal
                            // that block `prev` is FULLY delivered — every log
                            // for `prev` has been buffered. Mark the V3/V4
                            // pump-buffer completeness marker so the
                            // registration drain+pin cannot capture a
                            // half-delivered `prev` (the rolling-start race
                            // where a later same-block log lands after the pin).
                            publish_pending = false;
                            self.bot.mark_pump_blocks_complete(prev);
                            let prev_meta = block_metadata
                                .get(&prev)
                                .copied()
                                .unwrap_or(current_metadata);
                            self.finalize_if_dirty(prev, &prev_meta);
                            clock.advance_to_drained(prev);
                            if log_block > current_block {
                                current_block = log_block;
                            }
                        }
                        LogDecision::DispatchForward => {
                            if log_block > current_block {
                                current_block = log_block;
                            }
                        }
                        LogDecision::PanicLateForward(b) => {
                            // A removed:false log on a tombstoned block, NOT
                            // in a reorg → unreliable WS (out-of-order /
                            // duplicated forward events). Unrecoverable for
                            // correctness — shut down (ADR-008 D3).
                            log::error!(
                                "BlockPump: ADR-008 D3 late forward log on \
                                 tombstoned block {b} — unreliable WS, shutting down"
                            );
                            self.shutdown.store(true, Ordering::Relaxed);
                            return;
                        }
                    }

                    // Apply the log immediately to engine state (no solve yet).
                    // ADR-006 D4: routes through `Bot::dispatch_log` (decode →
                    // apply to BotState → notify EngineSubscriber → dirty the
                    // engine) — NOT `engine.apply_log`.
                    self.bot.dispatch_log(&log);
                    clock.log_received(log_block);
                    clock.log_applied(log_block);

                    // LEZJAS: engine owns `has_logs_this_block` now — routed
                    // through the sink so the next `finalize_block` sees it.
                    self.sink.record_logs_this_block();

                    // ADR-008 D2: arm the quiesce-gated publish. The flush
                    // fires at the next settle point (timeout or stream end)
                    // if `consume_quiesced` is true — once per quiesce cycle.
                    publish_pending = true;

                    // [DIAG] count logs + emit periodic stats so we can see,
                    // during a freeze, that the pump IS polling logs while
                    // headers are gone. This is the liveness signal the loop
                    // otherwise lacks.
                    diag_log_count += 1;
                    if diag_last_stats.elapsed() >= DIAG_STATS_INTERVAL {
                        let diag_since_header = diag_last_header_at.elapsed().as_secs();
                        log::info!(
                            "BlockPump: [DIAG] stats headers={diag_header_count} logs={diag_log_count} last_header_ago={diag_since_header}s current_block={current_block}"
                        );
                        diag_last_stats = tokio::time::Instant::now();
                    }
                }

                Ok(None) => {
                    // ADR-008 D2: stream exhausted — final settle point. Flush
                    // any pending quiesce-gated publish before returning.
                    if publish_pending {
                        if let Some(open) = clock.latest_observed() {
                            if clock.consume_quiesced(open) {
                                self.sink.on_send(&current_metadata);
                            }
                        }
                        publish_pending = false;
                    }
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
    /// `SolveCoordinator` fans to every attached `Engine`). The
    /// `last_solved_block` / `has_logs_this_block` bookkeeping is owned by
    /// the engine since ergo task LEZJAS (the pump's out-params retired);
    /// all engine-state mutation happens inside the sink under its lock.
    /// ADR-006 D4: the pump no longer holds the engine `Mutex` directly.
    fn finalize_if_dirty(&self, block: u64, metadata: &BlockMetadata) {
        self.sink.finalize_block(block, metadata);
    }

    /// Handle a 60s timeout by backfilling any missed blocks (eager variant).
    async fn handle_timeout_eager(
        &self,
        current_block: &mut u64,
        clock: &mut BlockClock,
        block_metadata: &mut HashMap<u64, BlockMetadata>,
        publish_pending: &mut bool,
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
            self.backfill_range(
                *current_block + 1,
                latest_block,
                &mut lpb,
                clock,
                block_metadata,
                publish_pending,
            )
            .await;
            *current_block = lpb;
            // LEZJAS: engine owns `last_solved_block` + `has_logs_this_block`
            // now — mark the backfilled range solved + clear the logs flag
            // through the sink (mirrors the retired pump-local writes).
            self.sink.set_last_solved_block(lpb);
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
    async fn backfill_range(
        &self,
        from_block: u64,
        to_block: u64,
        last_processed_block: &mut u64,
        clock: &mut BlockClock,
        block_metadata: &mut HashMap<u64, BlockMetadata>,
        publish_pending: &mut bool,
    ) {
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

        // ADR-008 D4: backfilled logs flow through the SAME state machine as
        // live WS logs (single branch — no distinguished `Backfilled` edge).
        // Each log routes via `clock.observe_log`; a tombstoned predecessor is
        // finalized + drained through the clock, and the backfilled block is
        // solved via `on_drain` (results piggyback onto the next debounce
        // `on_send` carrying real metadata).
        let mut any_processed = false;
        for block in from_block..=to_block {
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("BlockPump: shutting down during backfill");
                return;
            }

            let block_logs = logs_by_block.remove(&block).unwrap_or_default();
            for log in &block_logs {
                match clock.observe_log(block, log.removed) {
                    LogDecision::TombstonePrevious(prev) => {
                        let prev_meta = block_metadata.get(&prev).copied().unwrap_or_default();
                        self.sink.finalize_block(prev, &prev_meta);
                        clock.advance_to_drained(prev);
                        self.bot.dispatch_log(log);
                        clock.log_received(block);
                        clock.log_applied(block);
                    }
                    LogDecision::DispatchForward => {
                        self.bot.dispatch_log(log);
                        clock.log_received(block);
                        clock.log_applied(block);
                    }
                    // Backfilled logs come from an authoritative eth_getLogs
                    // against the canonical chain. Reorg/late-forward signals
                    // are not expected here; if one surfaces, skip applying
                    // this log (the canonical chain doesn't contain it) and let
                    // the live stream reconcile.
                    LogDecision::EnterReorg(_)
                    | LogDecision::ContinueReorg
                    | LogDecision::CloseReorg { .. }
                    | LogDecision::PanicLateForward(_) => {
                        log::warn!(
                            "BlockPump: backfill saw unexpected decision for block {block}; skipping log"
                        );
                    }
                }
            }
            if !block_logs.is_empty() {
                self.sink.on_drain(block, &BlockMetadata::default());
                any_processed = true;
                // ADR-008 D2: arm the quiesce-gated publish so backfilled
                // solved results flush at the next settle point (the live
                // loop's on_send), carrying real metadata.
                *publish_pending = true;
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

    /// Backfill the snapshot→WS gap `S+1..W-1` using the NO-SOLVE path
    /// (FD7NFG, epic P73ER6). Reads `S` from `BotState::snapshot_seed_block`
    /// (set by `Bot::load_snapshot_from_db`) and `W` from the `ws_block` param
    /// (the block the WS subscription landed on — `SubscribeState::first_block`,
    /// passed by the pyo3 caller or J3FMDO's auto-backfill before `resume`).
    /// Fetches logs via the pump's own `AlloyProvider` (no `rpc_url` from
    /// Python) in `chunk_size` chunks via `build_backfill_filter`, applying
    /// each chunk via `BotState::process_backfill_logs` (the relocated engine
    /// loop). No `solve_dirty` / no batches — the `Backfilled` phase invariant
    /// is "state advanced, no dispatch".
    ///
    /// Returns the count of blocks backfilled (`W-1 - (S+1) + 1 = W-1-S`), or
    /// `Ok(0)` for a no-op (cold start / S≥W). The post-backfill boundary is
    /// `W-1`; the pump's resume anchors on `first_observed_block = W` regardless
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
            let state = arc.read();
            state.snapshot_seed_block()
        };
        let Some(s) = s else {
            log::info!(
                "BlockPump::backfill_from_snapshot: no snapshot loaded (S=None), cold-start path"
            );
            return Ok(0);
        };
        if s == 0 {
            log::warn!("BlockPump::backfill_from_snapshot: snapshot block S=0, skipping");
            return Ok(0);
        }
        if s >= w {
            log::info!(
                "BlockPump::backfill_from_snapshot: snapshot at {s} ≥ WS block {w}, nothing to backfill"
            );
            return Ok(0);
        }
        let from_block = s + 1;
        let to_block = w - 1;
        let total_blocks = to_block - from_block + 1;
        log::info!(
            "BlockPump::backfill_from_snapshot: fetching events {from_block}–{to_block} ({total_blocks} blocks, chunk_size={chunk_size})"
        );
        let provider = self.provider.provider_arc();
        let mut total_logs = 0usize;
        let mut chunk_start = from_block;
        while chunk_start <= to_block {
            let chunk_end = (chunk_start + chunk_size - 1).min(to_block);
            let filter = build_backfill_filter(chunk_start, chunk_end);
            log::info!(
                "BlockPump::backfill_from_snapshot: fetching chunk {chunk_start}-{chunk_end}"
            );
            let t0 = std::time::Instant::now();
            let logs = provider.get_logs(&filter).await.map_err(|e| {
                format!("eth_getLogs failed for blocks {chunk_start}-{chunk_end}: {e}")
            })?;
            let n = logs.len();
            let fetch_ms = t0.elapsed().as_millis();
            log::info!(
                "BlockPump::backfill_from_snapshot: chunk {chunk_start}-{chunk_end} fetched {n} logs in {fetch_ms}ms"
            );
            total_logs += n;
            // Hold the write guard across the chunk so the apply + buffer-expire
            // (which advance `last_processed_block`) stay atomic per chunk.
            self.bot
                .state_arc()
                .write()
                .process_backfill_logs(&logs, chunk_end);
            log::info!(
                "BlockPump::backfill_from_snapshot: blocks {chunk_start}-{chunk_end}: {n} logs applied"
            );
            chunk_start = chunk_end + 1;
        }
        log::info!(
            "BlockPump::backfill_from_snapshot: complete — {total_logs} logs across {total_blocks} blocks"
        );
        Ok(total_blocks)
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
            header_staleness: Duration::from_secs(HEADER_STALENESS_SECS),
        }
    }

    /// Test-only access to the shared `Bot` arc (FD7NFG tests inject
    /// `snapshot_seed_block` to drive the `S≥W` / `S=0` no-op branches).
    #[must_use]
    pub fn bot_arc_for_test(&self) -> Arc<Bot> {
        Arc::clone(&self.bot)
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
        notified: Mutex<Vec<(u64, BlockMetadata)>>,
        last_processed: AtomicU64,
    }

    impl FakeDrainSink {
        fn new(last_processed: Option<u64>) -> Self {
            Self {
                finalized: Mutex::new(Vec::new()),
                sent: Mutex::new(Vec::new()),
                drained: Mutex::new(Vec::new()),
                notified: Mutex::new(Vec::new()),
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
        fn finalize_block(&self, block: u64, metadata: &BlockMetadata) {
            self.finalized.lock().unwrap().push((block, *metadata));
        }
        fn set_last_solved_block(&self, _block: u64) {}
        fn record_logs_this_block(&self) {}
        fn last_processed_block(&self) -> Option<u64> {
            let v = self.last_processed.load(Ordering::Relaxed);
            (v != 0).then_some(v)
        }
        fn notify_block(&self, block: u64, metadata: &BlockMetadata) {
            // Record the forwarded newHeads tick so task 22Y7AB can assert the
            // pump emits one BlockNotification per accepted header.
            self.notified.lock().unwrap().push((block, *metadata));
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
        // Contract (VTWCIG, ADR-008): block N is finalized when the FIRST
        // `removed: false` LOG for N+1 arrives (the tombstone — NOT a header).
        // The result batch that finalizes N must carry N's OWN metadata, even
        // though header N+1 (with distinct metadata) arrived earlier and
        // overwrote `current_metadata`. Python computes `base_fee_next` from
        // this metadata; carrying N+1's would systematically mis-price backruns.
        //
        // Stream: header 101, header 102 (overwrites current_metadata to
        // meta_102), then a forward log for block 102 (tombstones 101). The
        // finalize(101) must carry meta_101, NOT meta_102.
        let (mut pump, sink) = pump_for_test(Some(100));
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
        // header(101): first_header anchor → current_block 101.
        // header(102): new block, current_metadata overwritten to meta_102,
        //   but NO finalize on header (ADR-008).
        // log(102, removed=false): tombstones 101 → finalize(101, meta_101).
        let tombstone_log = make_v2_sync_log(
            Address::from([0xfcu8; 20]),
            U256::from(1),
            U256::from(2),
            102,
            false,
        );
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
            WsEvent::Log(tombstone_log),
        ];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, 100).await;

        let finalized = sink.finalized.lock().unwrap().clone();
        assert!(
            !finalized.is_empty(),
            "log 102 should tombstone+finalize 101"
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

    /// RED→GREEN tracer (epic 6W35AI, 22Y7AB): the pump forwards a
    /// `BlockNotification` for every `newHeads` header it accepts (one per
    /// header, carrying the header's number + metadata), via
    /// `DrainSink::notify_block` — independent of solve/debounce state. This
    /// is the seam that lets Python derive its block clock from `newHeads`
    /// instead of the stale `ResultBatch::solve_block`.
    #[tokio::test]
    async fn notify_block_fires_once_per_accepted_header() {
        let (mut pump, sink) = pump_for_test(Some(100));
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

        let notified = sink.notified.lock().unwrap().clone();
        assert_eq!(
            notified.len(),
            2,
            "exactly one notify_block per accepted header"
        );
        assert_eq!(notified[0].0, 101);
        assert_eq!(notified[0].1, meta_101);
        assert_eq!(notified[1].0, 102);
        assert_eq!(notified[1].1, meta_102);
    }

    /// 5DM6JJ contract: the cold-start branch in `run_with_stream` must honor
    /// `first_observed_block` when `last_processed_block()` is `None` (no
    /// prior anchor). This is the defensive safety net the legacy `spawn` fix
    /// leans on: passing the REAL subscribe block W (instead of the legacy
    /// hard-coded `0`) means that if the `on_drain(first_block)` anchor were
    /// ever absent, the pump still cold-starts to W — NOT stuck at 0 to be
    /// jumped out-of-order by the first WS log. Under ADR-008 the tombstone is
    /// a real log for W+1 (not a header).
    #[tokio::test]
    async fn cold_start_anchors_to_first_observed_block() {
        // No prior processed block → `current_block` starts at 0. Pass the
        // subscribe block W as `first_observed_block`. The cold-start branch
        // anchors `current_block` to W. header(W) is the first header
        // (anchor, no finalize); a forward log for W+1 tombstones W →
        // finalize(W) carrying meta_w. Proves we cold-started to W, not 0.
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
        let tombstone_log = make_v2_sync_log(
            Address::from([0xfcu8; 20]),
            U256::from(1),
            U256::from(2),
            w + 1,
            false,
        );
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
            WsEvent::Log(tombstone_log),
        ];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, w).await;

        let finalized = sink.finalized.lock().unwrap().clone();
        assert!(!finalized.is_empty(), "log w+1 should tombstone+finalize w");
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

        // Resume stream (post-fix: first_observed = W, not 0). header(W+1)
        // is the first header → first_header anchor advances W→W+1; then a
        // forward log for W+2 tombstones W+1 → finalize(W+1, meta_w1).
        let tombstone_log = make_v2_sync_log(
            Address::from([0xfcu8; 20]),
            U256::from(1),
            U256::from(2),
            w + 2,
            false,
        );
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
            WsEvent::Log(tombstone_log),
        ];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, w).await;

        // log(W+2) tombstones W+1 — carrying meta_w1 (W+1's own metadata,
        // snapshotted when header W+1 arrived). Proves the anchor held: we
        // advanced W→W+1→W+2 in order, never jumping from 0.
        let finalized = sink.finalized.lock().unwrap().clone();
        assert!(
            !finalized.is_empty(),
            "log w+2 should tombstone+finalize w+1"
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
    use alloy::primitives::{aliases::U112, Address, Bytes, U256};

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
        // Test helper: emits a raw V2 `Sync(uint112,uint112)` log as 64
        // bytes of ABI data (two 32-byte left-padded words). The decoder
        // narrows to `U112` on decode — this helper keeps the `U256` ABI
        // word shape so the bytes match on-chain log data.
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

    /// Build a V3 `Burn` log with `block_number` set (for backfill tests).
    /// data = abi.encode(uint128 amount, uint256 amount0, uint256 amount1).
    fn make_v3_burn_log_with_block(
        pool_address: Address,
        tick_lower: i32,
        tick_upper: i32,
        amount: u128,
        block_number: u64,
    ) -> Log {
        use alloy::primitives::{I256, U128};
        let tick_to_topic = |tick: i32| {
            let i = I256::try_from(i128::from(tick)).unwrap_or(I256::ZERO);
            alloy::primitives::B256::from(i.to_be_bytes::<32>())
        };
        let mut amount_word = [0u8; 32];
        amount_word[16..32].copy_from_slice(&U128::from(amount).to_be_bytes::<16>());
        let mut data = Vec::with_capacity(96);
        data.extend_from_slice(&amount_word);
        data.extend_from_slice(&alloy::primitives::U256::ZERO.to_be_bytes::<32>());
        data.extend_from_slice(&alloy::primitives::U256::ZERO.to_be_bytes::<32>());
        let owner = alloy::primitives::Address::from([0xccu8; 20]);
        let inner = alloy::primitives::Log::new_unchecked(
            pool_address,
            vec![
                V3_BURN_TOPIC,
                owner.into_word(),
                tick_to_topic(tick_lower),
                tick_to_topic(tick_upper),
            ],
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
            removed: false,
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
        let pool_id = bot
            .state_arc()
            .write()
            .register_v2_pool(&RegisterV2PoolParams {
                address: pool_addr,
                token0: Address::from([0xa0u8; 20]),
                token1: Address::from([0xa1u8; 20]),
                reserve0: U112::from(1_000),
                reserve1: U112::from(2_000),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                factory: Address::from([0xf0u8; 20]),
                update_block,
                variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                stable_swap: false,
                fee_denominator: None,
                ..Default::default()
            })
            .expect("test setup: V2 registration");
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
            bot.state_arc().read().v2_snapshot(pool_id),
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
            bot.state_arc().read().v2_snapshot(pool_id),
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

    /// ADR-008 D3 pump-level: a contiguous `removed: true` chunk (delivered
    /// in REVERSE log-index order — nodes may emit reorg events unordered)
    /// enters + continues the reorg path, restoring the pool per-event via the
    /// coordinator; the first `removed: false` event after entry closes the
    /// window, its block becomes the new head, and the pump CONTINUES (no
    /// shutdown). The forward log at the new head re-applies against the
    /// restored state.
    #[tokio::test]
    async fn reorg_contiguous_chunk_closes_on_first_forward_and_continues() {
        let pool_addr = Address::from([0x33u8; 20]);
        // Genesis anchored at block 5: reserves (1000, 2000).
        let (bot, pool_id, sub) = bot_with_registered_v2(pool_addr, 5);
        let notify_count = || *sub.notifies.lock().unwrap();
        let snapshot = || bot.state_arc().read().v2_snapshot(pool_id);

        // Drive 5 -> 7 (forward sync at 7) -> tombstone 7 via a forward sync
        // at 8 (advance_to_drained(7) follows the tombstone).
        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let s7 = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 7, false);
        let s8 = make_v2_sync_log(pool_addr, U256::from(1_600), U256::from(2_600), 8, false);
        let combined = stream::iter(vec![WsEvent::Log(s7), WsEvent::Log(s8)]).boxed();
        pump.run_test_loop(combined, 5).await;
        assert_eq!(notify_count(), 2, "two forward syncs applied");
        assert_eq!(snapshot(), Some((U256::from(1_600), U256::from(2_600), 8)));
        assert!(!shutdown.load(Ordering::Relaxed));

        // Reorg over blocks 7 and 8: removed logs arrive in REVERSE order
        // (8 then 7), then the first removed:false at block 9 closes it.
        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let r8 = make_v2_sync_log(pool_addr, U256::from(9), U256::from(9), 8, true);
        let r7 = make_v2_sync_log(pool_addr, U256::from(9), U256::from(9), 7, true);
        let s9 = make_v2_sync_log(pool_addr, U256::from(1_700), U256::from(2_700), 9, false);
        let combined =
            stream::iter(vec![WsEvent::Log(r8), WsEvent::Log(r7), WsEvent::Log(s9)]).boxed();
        pump.run_test_loop(combined, 5).await;

        // The reorg unwound 7 and 8 (restore to genesis), then the forward
        // sync at 9 re-applied -> reserves reflect block 9's values.
        assert_eq!(snapshot(), Some((U256::from(1_700), U256::from(2_700), 9)));
        assert!(
            !shutdown.load(Ordering::Relaxed),
            "reorg path closed cleanly — pump did NOT shut down"
        );
    }

    /// ADR-008 D3 pump-level: a `removed: false` log on a tombstoned block
    /// (NOT a reorg) means the WS delivered a forward event out-of-order /
    /// duplicated — unreliable. The pump must shut down rather than silently
    /// re-apply. Cursor never silently regresses.
    #[tokio::test]
    async fn late_forward_log_on_tombstoned_block_shuts_down_pump() {
        let pool_addr = Address::from([0x44u8; 20]);
        let (bot, _pool_id, _sub) = bot_with_registered_v2(pool_addr, 5);

        // Single pump session: forward sync(7) opens block 7; forward sync(8)
        // tombstones 7 (open block becomes 8); THEN a forward (removed:false)
        // sync at block 7 arrives late — block 7 is tombstoned and the open
        // block is 8 -> late forward -> unreliable WS -> shutdown.
        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let s7 = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 7, false);
        let s8 = make_v2_sync_log(pool_addr, U256::from(1_600), U256::from(2_600), 8, false);
        let late = make_v2_sync_log(pool_addr, U256::from(9_999), U256::from(9_999), 7, false);
        let combined =
            stream::iter(vec![WsEvent::Log(s7), WsEvent::Log(s8), WsEvent::Log(late)]).boxed();
        pump.run_test_loop(combined, 5).await;

        assert!(
            shutdown.load(Ordering::Relaxed),
            "late removed:false on a tombstoned block must shut the pump down (ADR-008 D3)"
        );
    }

    // -----------------------------------------------------------------
    // ADR-008 D2: `LogsQuiesced` solver-release gate.
    //
    // The pump must publish (`on_send`) only when the open block is
    // quiesced (all dispatched logs fully applied), and coalesce a burst of
    // same-block logs into ONE publish at the burst tail (not once per log).
    // Re-arm on straggler is covered at the clock level by
    // `consume_quiesced_publishes_once_per_cycle_and_re_arms_on_straggler`.
    // -----------------------------------------------------------------

    /// 3 same-block logs in a tight burst → exactly ONE `on_send`, fired at
    /// the burst tail (after the 3rd log applies + the stream settles), NOT
    /// 3× (one per log) and NOT zero. RED against the wall-clock timer: with
    /// `stream::iter` (no delay between events) the 50ms `DEBOUNCE_MS` timer
    /// never fires before the stream ends, so `on_send` is never called.
    #[tokio::test]
    async fn burst_of_logs_publishes_once_at_tail_via_quiesce_gate() {
        let (mut pump, sink) = pump_for_test(Some(100));
        let pool_addr = Address::from([0x55u8; 20]);
        let mk = |r0, r1| {
            WsEvent::Log(make_v2_sync_log(
                pool_addr,
                U256::from(r0),
                U256::from(r1),
                101,
                false,
            ))
        };
        // 3 same-block sync logs, then stream exhaustion. Under the wall-clock
        // debounce the 50ms timer never fires before Ok(None) returns → 0
        // sends. Under the quiesce gate, after the 3rd log applies the settle
        // probe (timeout(ZERO) on the exhausted stream) flushes on_send once.
        let combined =
            stream::iter(vec![mk(1_500, 2_500), mk(1_600, 2_600), mk(1_700, 2_700)]).boxed();
        pump.run_test_loop(combined, 100).await;

        let sent = sink.sent.lock().unwrap().clone();
        assert_eq!(
            sent.len(),
            1,
            "a 3-log burst publishes exactly once at the tail via the quiesce \
             gate (got {} sends)",
            sent.len()
        );
    }
    /// FD7NFG: `backfill_from_snapshot` no-op when no snapshot loaded (cold
    /// start — `snapshot_seed_block = None`). Default fresh `Bot` has S=None.
    #[tokio::test]
    async fn backfill_from_snapshot_cold_start_is_noop() {
        let (pump, _sink) = pump_for_test(None);
        // Fresh Bot: snapshot_seed_block is None → no-op, no provider call.
        let n = pump.backfill_from_snapshot(100, 10).await.unwrap();
        assert_eq!(n, 0, "cold start (S=None) → no blocks backfilled");
    }

    /// FD7NFG: `backfill_from_snapshot` no-op when `S >= W` (snapshot at/after
    /// the WS block — nothing to backfill).
    #[tokio::test]
    async fn backfill_from_snapshot_s_ge_w_is_noop() {
        let (pump, _sink) = pump_for_test(None);
        // Inject S = W (snapshot caught up to the WS block).
        {
            let bot = pump.bot_arc_for_test();
            bot.state_arc().write().set_snapshot_seed_block(Some(100));
        }
        let n = pump.backfill_from_snapshot(100, 10).await.unwrap();
        assert_eq!(n, 0, "S >= W → nothing to backfill");
    }

    /// FD7NFG: `backfill_from_snapshot` no-op when `S = 0` (degenerate
    /// snapshot block — guarded to avoid a `from_block=1` unbounded fetch).
    #[tokio::test]
    async fn backfill_from_snapshot_s_zero_is_noop() {
        let (pump, _sink) = pump_for_test(None);
        {
            let bot = pump.bot_arc_for_test();
            bot.state_arc().write().set_snapshot_seed_block(Some(0));
        }
        let n = pump.backfill_from_snapshot(100, 10).await.unwrap();
        assert_eq!(n, 0, "S = 0 → skip (degenerate)");
    }

    /// JUCFCB/J3FMDO helper: build a `pump_for_test_with_bot` variant that
    /// also returns the `Asserter` so a test can push `eth_getLogs`
    /// responses and observe whether the auto-backfill path drains them.
    fn pump_for_test_with_asserter(
        bot: Arc<Bot>,
        last_processed: Option<u64>,
    ) -> (
        BlockPump,
        Arc<FakeDrainSink>,
        Arc<AtomicBool>,
        alloy::transports::mock::Asserter,
    ) {
        use alloy::network::Ethereum as NetEth;
        use alloy::providers::{Provider, ProviderBuilder};
        use alloy::rpc::client::ClientBuilder;
        use alloy::transports::mock::{Asserter, MockTransport};

        let asserter = Asserter::new();
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
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
        (pump, sink, shutdown, asserter)
    }

    /// J3FMDO: `resume_from_subscribe` auto-backfills the snapshot→WS gap
    /// (S < W) before the live loop begins — proving the core path closes the
    /// gap with zero Python orchestration. The Asserter queue drains by exactly
    /// one `eth_getLogs` response (S+1..W-1 fits in a single default-size chunk).
    #[tokio::test]
    async fn auto_backfill_runs_inside_resume_when_s_lt_w() {
        let bot = Arc::new(Bot::new(1));
        bot.state_arc().write().set_snapshot_seed_block(Some(85));
        let (mut pump, _sink, _shutdown, asserter) = pump_for_test_with_asserter(bot, None);

        // The single eth_getLogs chunk (blocks 86..99, ≤ DEFAULT_BACKFILL_CHUNK_SIZE)
        // returns an empty log array — the pump's provider drains this response.
        asserter.push_success(&Vec::<Log>::new());

        let combined = stream::iter(Vec::<WsEvent>::new()).boxed();
        let state = SubscribeState {
            first_block: 100,
            first_timestamp: 0,
            combined_stream: Some(combined),
        };
        pump.resume_from_subscribe(state).await;

        assert_eq!(
            asserter.read_q().len(),
            0,
            "auto-backfill inside resume popped exactly one eth_getLogs response; queue must be empty"
        );
    }

    /// J3FMDO race regression: `backfill_to_ws_block` is the
    /// synchronously-awaitable backfill that `PumpState::resume` `block_on`s
    /// BEFORE spawning the live loop. Pre-fix the backfill ran INSIDE the
    /// spawned `resume_from_subscribe` task, so `PumpState::resume` returned
    /// immediately and Python's `build_paths` drained an EMPTY backfill buffer
    /// (the burn for an active pool was not yet buffered) → the post-drain
    /// verify mismatched on-chain and crashed the backrun bot with
    /// `VerificationMismatchError`. This pins the contract: after
    /// `backfill_to_ws_block` returns, the V3 backfill buffer is populated —
    /// the event did NOT require the live loop to run first.
    #[tokio::test]
    async fn backfill_to_ws_block_populates_buffer_before_return() {
        let pool_addr = alloy::primitives::Address::from([0xc2u8; 20]);
        let bot = Arc::new(Bot::new(1));
        bot.state_arc().write().set_snapshot_seed_block(Some(85));
        let (pump, _sink, _shutdown, asserter) =
            pump_for_test_with_asserter(Arc::clone(&bot), None);

        // A V3 Burn log at block 90 (in the backfill range 86..99).
        asserter.push_success(&vec![make_v3_burn_log_with_block(
            pool_addr, -100, 100, 500, 90,
        )]);

        // backfill_to_ws_block must fully buffer the burn BEFORE returning.
        pump.backfill_to_ws_block(100)
            .await
            .expect("backfill_to_ws_block completes against the mock");

        // The burn was buffered (not applied — pool unregistered → buffer
        // branch). Pre-fix: this method did not exist and `resume` returned
        // before the spawned task buffered → count 0 → race.
        assert_eq!(
            bot.state_arc().read().buffered_v3_event_count(&pool_addr),
            1,
            "backfill_to_ws_block must buffer the V3 burn before returning (race regression)"
        );
    }

    /// J3FMDO: `resume_from_subscribe` skips the auto-backfill entirely when no
    /// snapshot seed is present (`S = None`, cold start). The Asserter queue is
    /// left untouched (the pump never calls `eth_getLogs`) and the live loop
    /// anchors on `first_observed_block` directly. An empty queue under a live
    /// `eth_getLogs` request would error; we assert the queue stays empty AND
    /// the resume returns without a provider error.
    #[tokio::test]
    async fn auto_backfill_skipped_when_s_none_in_resume() {
        let bot = Arc::new(Bot::new(1));
        // Fresh Bot: snapshot_seed_block is None — no gap to backfill.
        let (mut pump, _sink, _shutdown, asserter) = pump_for_test_with_asserter(bot, None);

        let combined = stream::iter(Vec::<WsEvent>::new()).boxed();
        let state = SubscribeState {
            first_block: 100,
            first_timestamp: 0,
            combined_stream: Some(combined),
        };
        pump.resume_from_subscribe(state).await;

        assert_eq!(
            asserter.read_q().len(),
            0,
            "cold-start resume never calls eth_getLogs (auto-backfill gated on S<W)"
        );
    }

    /// J3FMDO: `resume_from_subscribe` skips the auto-backfill when the
    /// snapshot is already at/after the WS block (`S >= W` — catch-up snapshot
    /// with no gap to backfill).
    #[tokio::test]
    async fn auto_backfill_skipped_when_s_ge_w_in_resume() {
        let bot = Arc::new(Bot::new(1));
        bot.state_arc().write().set_snapshot_seed_block(Some(100));
        let (mut pump, _sink, _shutdown, asserter) = pump_for_test_with_asserter(bot, None);

        let combined = stream::iter(Vec::<WsEvent>::new()).boxed();
        let state = SubscribeState {
            first_block: 100,
            first_timestamp: 0,
            combined_stream: Some(combined),
        };
        pump.resume_from_subscribe(state).await;

        assert_eq!(
            asserter.read_q().len(),
            0,
            "S ≥ W → no auto-backfill, no eth_getLogs call"
        );
    }

    /// Diagnostic for the 2026-07-12 WS `eth_getLogs` hang.
    ///
    /// Root cause (confirmed here with tracing + a concurrent
    /// `get_block_number` probe): tungstenite correctly returns
    /// `Error::Capacity(MessageTooLong)` for a response larger than the
    /// default `max_frame_size` (16 MiB) / `max_message_size` (64 MiB), but
    /// `alloy-pubsub`'s `WsBackend` converts that to
    /// `TransportErrorKind::backend_gone()` (a *retryable* error) at the
    /// backend→service boundary — losing the Capacity specificity. The pubsub
    /// service then enters an INFINITE reconnect→redispatch loop: `reconnect()`
    /// succeeds on the first attempt (the WS handshake is fine; only the
    /// response is too big), `max_retries` is never consumed, and the pending
    /// in-flight `eth_getLogs` is re-dispatched each cycle. The caller's
    /// `get_logs` future never resolves; small concurrent calls keep working.
    ///
    /// Three variants:
    /// A — default tungstenite caps: demonstrates the infinite cycle (HUNG,
    ///     `get_block_number` probe still succeeding concurrently);
    /// B — raised caps via raw `WsConnect::with_config`: WS handles it;
    /// C — production `AlloyProvider::new` path (= the `build_provider` fix):
    ///     regression sentinel.
    ///
    /// Run with:
    /// `cargo test -p degenbot-bot --manifest-path rust/Cargo.toml \
    ///   -- --ignored --nocapture ws_getlogs_large_filter_diagnostic`
    ///
    /// Requires `DEGENBOT_RPC_WS_CHAINID_1` (a mainnet WS endpoint).
    #[tokio::test]
    #[ignore = "requires a live mainnet WS endpoint (DEGENBOT_RPC_WS_CHAINID_1)"]
    #[allow(clippy::too_many_lines)]
    async fn ws_getlogs_large_filter_diagnostic() {
        use alloy::network::Ethereum;
        use alloy::providers::{Provider, ProviderBuilder, WebSocketConfig, WsConnect};
        use std::time::Duration;
        use tokio::time::timeout;
        use tracing_subscriber::util::SubscriberInitExt;
        type Erased = std::sync::Arc<dyn Provider<Ethereum>>;

        let Ok(ws_url) = std::env::var("DEGENBOT_RPC_WS_CHAINID_1") else {
            eprintln!("skip: DEGENBOT_RPC_WS_CHAINID_1 not set");
            return;
        };

        // Fetch a recent block number (small call — works over default WS).
        let anchor_provider: Erased = {
            let mid = ProviderBuilder::default()
                .connect_ws(WsConnect::new(ws_url.clone()))
                .await
                .expect("ws connect (anchor)")
                .erased();
            Arc::new(mid)
        };
        let latest = anchor_provider
            .get_block_number()
            .await
            .expect("block number");
        // Leave a few blocks of margin so the range is settled.
        let to = latest - 5;
        let from = to - 1_999;
        let filter = build_backfill_filter(from, to);
        eprintln!("filter range {from}–{to} (latest={latest})");

        // --- Variant A: DEFAULT tungstenite config (max_message_size=64MiB) ---
        // Install a tracing subscriber so alloy's reconnect-cycle `error!`/
        // `warn!` logs surface (without one they're silently dropped — which is
        // why the earlier run showed "no error surfaced"). Also poll
        // `get_block_number` concurrently: if it keeps succeeding while
        // `get_logs` is pending, the WS service is alive and silently
        // reconnecting (proving the oversized-response cycle), NOT truly
        // stalled in tungstenite.
        let _guard = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "alloy_pubsub=debug,alloy_transport_ws=debug,tungstenite=info",
            ))
            .with_test_writer()
            .set_default();
        let p: Erased = {
            let mid = ProviderBuilder::default()
                .connect_ws(WsConnect::new(ws_url.clone()))
                .await
                .expect("ws connect (A)")
                .erased();
            Arc::new(mid)
        };
        // Concurrent block-number probe on the SAME provider.
        let probe_p = Arc::clone(&p);
        let probe = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            tick.tick().await; // skip immediate
            for i in 1..=15_u32 {
                tick.tick().await;
                match probe_p.get_block_number().await {
                    Ok(n) => eprintln!("A probe #{i}: get_block_number OK = {n}"),
                    Err(e) => eprintln!("A probe #{i}: get_block_number ERR = {e}"),
                }
            }
        });
        let t0 = std::time::Instant::now();
        let res = timeout(Duration::from_secs(30), p.get_logs(&filter)).await;
        let elapsed = t0.elapsed();
        match res {
            Ok(Ok(logs)) => eprintln!(
                "A DEFAULT   : OK  {} logs in {:.2}s (under the 64MiB cap this run)",
                logs.len(),
                elapsed.as_secs_f64()
            ),
            Ok(Err(e)) => eprintln!(
                "A DEFAULT   : ERR after {:.2}s — `{e}`",
                elapsed.as_secs_f64()
            ),
            Err(_) => {
                eprintln!("A DEFAULT   : HUNG (30s timeout, no error surfaced to the caller)");
            }
        }
        // Let the probe finish printing so we see the concurrent-call verdict.
        let _ = timeout(Duration::from_secs(35), probe).await;

        // --- Variant B: RAISED config (no size cap) ---
        let cfg = WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None);
        let p: Erased = {
            let mid = ProviderBuilder::default()
                .connect_ws(WsConnect::new(ws_url.clone()).with_config(cfg))
                .await
                .expect("ws connect (B)")
                .erased();
            Arc::new(mid)
        };
        let t0 = std::time::Instant::now();
        let res = timeout(Duration::from_mins(1), p.get_logs(&filter)).await;
        let elapsed = t0.elapsed();
        match res {
            Ok(Ok(logs)) => eprintln!(
                "B RAISED    : OK  {} logs in {:.2}s",
                logs.len(),
                elapsed.as_secs_f64()
            ),
            Ok(Err(e)) => eprintln!(
                "B RAISED    : ERR after {:.2}s — `{e}`",
                elapsed.as_secs_f64()
            ),
            Err(_) => eprintln!("B RAISED    : HUNG (60s timeout)"),
        }

        // --- Variant C: production path (`AlloyProvider::new` → ---
        // `build_provider`), which now raises the tungstenite caps in
        // `degenbot_rpc::provider::build_provider`. This is the regression
        // sentinel: if a future change drops the raised-config in
        // `build_provider`, this variant hangs and the test suite surfaces it.
        let alloy_provider = degenbot_rpc::provider::AlloyProvider::new(&ws_url, 3)
            .await
            .expect("AlloyProvider::new");
        let p = alloy_provider.provider_arc();
        let t0 = std::time::Instant::now();
        let res = timeout(Duration::from_mins(1), p.get_logs(&filter)).await;
        let elapsed = t0.elapsed();
        match res {
            Ok(Ok(logs)) => eprintln!(
                "C PRODUCTION: OK  {} logs in {:.2}s",
                logs.len(),
                elapsed.as_secs_f64()
            ),
            Ok(Err(e)) => eprintln!(
                "C PRODUCTION: ERR after {:.2}s — `{e}`",
                elapsed.as_secs_f64()
            ),
            Err(_) => {
                eprintln!("C PRODUCTION: HUNG (60s timeout) — `build_provider` config regression");
            }
        }
    }
}

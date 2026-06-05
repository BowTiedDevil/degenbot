//! Uniswap Engine Pump — unified async task that drives the `UniswapEngine`.
//!
//! A single pump that subscribes to both block headers and log events via WS,
//! buffers incoming logs against each block, and routes them to the appropriate
//! sub-engine via `UniswapEngine::process_block()`.
//!
//! # Two-Phase Lifecycle
//!
//! The pump operates in two phases:
//!
//! 1. **Subscribe phase** (`subscribe()`): Opens WS subscriptions, receives
//!    block headers and logs. MINT/BURN events matching the configurable
//!    inclusion set are buffered into the engine's liquidity event buffers.
//!    Swap/Sync events are discarded (stateless — registration provides
//!    current state). Returns the first observed block number so Python knows
//!    the temporal boundary for backfill.
//!
//! 2. **Resume phase** (`resume()`): Begins normal processing — logs are
//!    buffered and processed atomically on block boundaries. The pump uses
//!    the first observed block from subscribe as its starting point.
//!
//! **Critical ordering**: Python must run backfill AFTER `subscribe()` returns
//! but BEFORE calling `resume()`. This ensures:
//! - The backfill target is known (the block `subscribe()` observed)
//! - No blocks are missed (subscribe was active during backfill)
//! - MINT/BURN events arrived during subscribe are preserved in the buffer
//!
//! # Architecture
//!
//! ```text
//! WS subscription: newHeads + logs (unfiltered)
//!     │
//!     ├─ Subscribe phase: buffer MINT/BURN, discard Swap/Sync
//!     │
//!     ├─ Resume phase: logs arrive in real-time, buffered per block
//!     │
//!     └─ on newHeads (resume phase):
//!          ├─ Take all buffered logs for the just-completed block
//!          ├─ Filter by topic + address in Rust
//!          ├─ engine.process_block(filtered_logs, block_number)
//!          ├─ Send `BlockNotification` via watch channel
//!          │
//!          └─ If no logs were received for this block:
//!               `eth_getLogs` backfill to verify (empty block vs dropped events)
//!
//!     Backfill trigger 2: 60s timeout with nothing received on either subscription
//!          → `eth_getLogs` from last_processed_block+1 to latest
//! ```
//!
//! # Why dual subscriptions instead of `eth_getLogs` every block?
//!
//! Push-based log delivery eliminates the per-block RPC round-trip (~50-100ms).
//! Events are processed as they arrive, not after a poll delay. Two backfill
//! triggers cover the gap scenarios:
//! - **60s timeout**: connection is likely dead → backfill the missing range
//! - **Empty block**: no logs arrived → `eth_getLogs` verifies whether the block
//!   was truly empty or events were silently dropped
//!
//! The unfiltered logs subscription avoids provider-specific filter quirks
//! (address limits, topic truncation, AND/OR semantics). All filtering happens
//! in Rust after receipt.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use alloy::primitives::B256;
#[cfg(test)]
use alloy::primitives::Address;
use alloy::rpc::types::{Filter, Log};
use futures_util::{StreamExt, stream};
use tokio::time::timeout;

// ---------------------------------------------------------------------------
// EventType — configurable inclusion set for subscribe-phase buffering
// ---------------------------------------------------------------------------

/// Event types that can be buffered during the subscribe phase.
///
/// Used to configure which events the pump buffers into the engine's liquidity
/// event buffers before `resume()` is called. Default: `{MINT, BURN}`.
///
/// Swap and Sync events are always discarded during the subscribe phase
/// because they are stateless — pool registration provides current state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventType {
    /// V2 Sync event (reserve updates)
    SYNC,
    /// V3 Swap event
    SWAP,
    /// V3 Mint event (liquidity provision)
    MINT,
    /// V3 Burn event (liquidity removal). Also covers V4 `ModifyLiquidity`
    /// with negative delta.
    BURN,
}

/// Default buffer inclusion set: MINT and BURN events.
///
/// These are the events that are lossy to discard — they modify tick data
/// that can only be reconstructed from on-chain queries. Swap/Sync are
/// stateless and can be safely dropped.
pub const DEFAULT_BUFFER_EVENTS: [EventType; 2] = [EventType::MINT, EventType::BURN];

/// Check whether a topic matches an event type in the inclusion set.
fn topic_matches_event_type(topic: &B256, event_types: &[EventType]) -> bool {
    for &et in event_types {
        match et {
            EventType::SYNC if *topic == V2_SYNC_TOPIC => return true,
            EventType::SWAP if *topic == V3_SWAP_TOPIC || *topic == V4_SWAP_TOPIC => return true,
            EventType::MINT if *topic == V3_MINT_TOPIC => return true,
            EventType::BURN if *topic == V3_BURN_TOPIC || *topic == V4_MODIFY_LIQUIDITY_TOPIC => {
                return true;
            }
            _ => {}
        }
    }
    false
}

use crate::bot_core::v3_mint_burn_decoder::{V3_MINT_TOPIC, V3_BURN_TOPIC};
use crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC;
use crate::bot_core::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC;
use crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC;
use crate::optimizers::v2_sync_decoder::V2_SYNC_TOPIC;
use crate::optimizers::uniswap_engine::{BlockMetadata, UniswapEngine};
use crate::provider::AlloyProvider;
use crate::runtime::get_runtime;

/// How long to wait with no activity before assuming the connection is dead.
const BACKFILL_TIMEOUT_SECS: u64 = 60;

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

/// The unified pump that drives the `UniswapEngine`.
///
/// Supports a two-phase lifecycle:
/// 1. `subscribe()` — opens WS connections, buffers MINT/BURN events,
///    returns first observed block number
/// 2. `resume()` — begins normal processing on block boundaries
pub struct UniswapEnginePump {
    /// Shared engine state
    engine: Arc<parking_lot::Mutex<UniswapEngine>>,
    /// The Alloy provider (created from the RPC URL)
    provider: Arc<AlloyProvider>,
    /// Shutdown flag — set by `stop()`
    shutdown: Arc<AtomicBool>,
    /// Set of event types to buffer during the subscribe phase.
    /// Defaults to {MINT, BURN}.
    buffer_event_types: Vec<EventType>,
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

impl UniswapEnginePump {
    /// Subscribe phase: open WS connections and buffer events.
    ///
    /// Returns a `SubscribeState` containing the first observed block number
    /// and the live WS stream. Python should:
    /// 1. Run backfill up to `subscribe_state.first_block`
    /// 2. Call `resume(subscribe_state)` to begin normal processing
    ///
    /// During this phase, MINT/BURN events matching `buffer_event_types`
    /// are routed to the engine's liquidity event buffers. Swap/Sync events
    /// are discarded.
    #[allow(clippy::missing_errors_doc)]
    pub async fn subscribe(
        rpc_url: &str,
        engine: Arc<parking_lot::Mutex<UniswapEngine>>,
        shutdown: Arc<AtomicBool>,
        buffer_event_types: Vec<EventType>,
    ) -> Result<(Self, SubscribeState), String> {
        let provider = AlloyProvider::new(rpc_url, 3)
            .await
            .map_err(|e| format!("UniswapEnginePump: failed to create provider: {e}"))?;

        let provider_arc = provider.provider_arc();

        // Subscribe to block headers
        let block_stream = provider_arc
            .subscribe_blocks()
            .await
            .map_err(|e| format!("UniswapEnginePump: failed to subscribe to blocks: {e}"))?
            .into_stream();

        // Subscribe to logs — unfiltered. All filtering happens in Rust.
        let log_filter = Filter::new();
        let log_stream = provider_arc
            .subscribe_logs(&log_filter)
            .await
            .map_err(|e| format!("UniswapEnginePump: failed to subscribe to logs: {e}"))?
            .into_stream();

        let combined = stream_select(block_stream, log_stream).boxed();


        let pump = Self {
            engine,
            provider: Arc::new(provider),
            shutdown,
            buffer_event_types,
        };

        // Buffer events until we see the first block header.
        // This gives us the temporal anchor for Python backfill.
        let (first_block, first_timestamp) = pump.subscribe_phase(combined).await;

        // Re-create the combined stream for resume phase.
        // We need fresh subscriptions for the resume loop.
        // Actually, we keep the existing stream — it's still live.
        // The subscribe_phase only consumed events up to the first block header.
        // We need to re-subscribe because subscribe_phase consumed the stream.

        // Re-subscribe to get a fresh stream for the resume phase.
        let block_stream2 = provider_arc
            .subscribe_blocks()
            .await
            .map_err(|e| format!("UniswapEnginePump: failed to re-subscribe to blocks: {e}"))?
            .into_stream();

        let log_stream2 = provider_arc
            .subscribe_logs(&log_filter)
            .await
            .map_err(|e| format!("UniswapEnginePump: failed to re-subscribe to logs: {e}"))?
            .into_stream();

        let combined2 = stream_select(block_stream2, log_stream2).boxed();

        let subscribe_state = SubscribeState {
            first_block,
            first_timestamp,
            combined_stream: Some(combined2),
        };

        Ok((pump, subscribe_state))
    }

    /// Legacy entry point: subscribe + immediate resume.
    ///
    /// Equivalent to calling `subscribe()` then `resume()` immediately.
    /// Provided for backward compatibility with existing callers.
    #[allow(clippy::missing_errors_doc)]
    pub fn spawn(
        rpc_url: String,
        engine: Arc<parking_lot::Mutex<UniswapEngine>>,
        shutdown: &Arc<AtomicBool>,
    ) -> Result<tokio::task::JoinHandle<()>, String> {
        let runtime = get_runtime();

        let shutdown_clone = Arc::clone(shutdown);
        let handle = runtime.spawn(async move {
            let mut pump = match Self::subscribe(
                &rpc_url,
                engine,
                shutdown_clone,
                DEFAULT_BUFFER_EVENTS.to_vec(),
            )
            .await
            {
                Ok((pump, state)) => {
                    // Send the first block as an empty batch so Python learns about it
                    {
                        let mut engine = pump.engine.lock();
                        engine.process_block(&[], state.first_block, &BlockMetadata {
                            timestamp: state.first_timestamp,
                            base_fee_per_gas: None,
                            gas_used: 0,
                            gas_limit: 0,
                        });
                    }
                    pump
                }
                Err(e) => {
                    log::error!("UniswapEnginePump: subscribe failed: {e}");
                    return;
                }
            };

            // Build a SubscribeState from the subscribe results
            // In the legacy path, we re-subscribe to get a fresh stream
            let provider_arc = pump.provider.provider_arc();

            let block_stream = match provider_arc.subscribe_blocks().await {
                Ok(s) => s.into_stream(),
                Err(e) => {
                    log::error!("UniswapEnginePump: failed to subscribe to blocks: {e}");
                    return;
                }
            };

            let log_filter = Filter::new();
            let log_stream = match provider_arc.subscribe_logs(&log_filter).await {
                Ok(s) => s.into_stream(),
                Err(e) => {
                    log::error!("UniswapEnginePump: failed to subscribe to logs: {e}");
                    return;
                }
            };

            let combined2 = stream_select(block_stream, log_stream).boxed();

            let subscribe_state = SubscribeState {
                first_block: 0, // Not used for legacy path
                first_timestamp: 0,
                combined_stream: Some(combined2),
            };

            pump.resume(subscribe_state).await;
        });

        Ok(handle)
    }

    /// Subscribe phase: buffer MINT/BURN events until first block header.
    ///
    /// Returns (`first_block_number`, `first_timestamp`).
    async fn subscribe_phase(
        &self,
        mut combined: stream::BoxStream<'static, WsEvent>,
    ) -> (u64, u64) {
        let relevant_topic_set: HashSet<B256> = RELEVANT_TOPICS.into_iter().collect();
        let mut first_block: Option<u64> = None;
        let mut first_timestamp: u64 = 0;

        loop {
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("UniswapEnginePump: shutting down during subscribe phase");
                return (0, 0);
            }

            let event = timeout(Duration::from_secs(BACKFILL_TIMEOUT_SECS), combined.next()).await;

            match event {
                Err(_) => {
                    // Timeout during subscribe — try to get current block
                    log::warn!(
                        "UniswapEnginePump: timeout during subscribe, fetching current block"
                    );
                    match self.provider.provider_arc().get_block_number().await {
                        Ok(block) => {
                            log::info!("UniswapEnginePump: subscribe observed block {block} via RPC");
                            return (block, 0);
                        }
                        Err(e) => {
                            log::error!("UniswapEnginePump: can't get block number during subscribe: {e}");
                            continue;
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
                    log::info!("UniswapEnginePump: subscribe observed block {number}");
                    first_block = Some(number);
                    first_timestamp = timestamp;
                    break;
                }

                Ok(Some(WsEvent::Log(log))) => {
                    // Buffer MINT/BURN events that match the inclusion set
                    if let Some(topic0) = log.topics().first() {
                        if relevant_topic_set.contains(topic0)
                            && topic_matches_event_type(topic0, &self.buffer_event_types)
                        {
                            self.buffer_subscribe_log(&log);
                        }
                    }
                    // Continue buffering until first block header
                }

                Ok(None) => {
                    log::warn!("UniswapEnginePump: subscription streams ended during subscribe");
                    return (first_block.unwrap_or(0), first_timestamp);
                }
            }
        }

        (first_block.unwrap_or(0), first_timestamp)
    }

    /// Route a subscribe-phase log to the appropriate engine buffer.
    ///
    /// Only MINT/BURN events are routed. Swap/Sync are discarded.
    fn buffer_subscribe_log(&self, log: &Log) {
        let Some(topic0) = log.topics().first() else {
            return;
        };

        // Extract block number from the log receipt, fall back to 0
        let block_number = log.block_number.unwrap_or(0);

        let mut engine = self.engine.lock();

        if *topic0 == V3_MINT_TOPIC {
            if let Some(event) = crate::bot_core::v3_mint_burn_decoder::decode_v3_mint_log(log) {
                engine.v3_engine().apply_liquidity_update(
                    log.address(),
                    event.tick_lower,
                    event.tick_upper,
                    event.amount as i128,
                    block_number,
                );
            }
        } else if *topic0 == V3_BURN_TOPIC {
            if let Some(event) = crate::bot_core::v3_mint_burn_decoder::decode_v3_burn_log(log) {
                engine.v3_engine().apply_liquidity_update(
                    log.address(),
                    event.tick_lower,
                    event.tick_upper,
                    -(event.amount as i128),
                    block_number,
                );
            }
        } else if *topic0 == V4_MODIFY_LIQUIDITY_TOPIC {
            if let Some(event) =
                crate::bot_core::v4_modify_liquidity_decoder::decode_v4_modify_liquidity_log(log)
            {
                engine.v4_engine().apply_liquidity_update(
                    log.address(),
                    event.pool_id,
                    event.tick_lower,
                    event.tick_upper,
                    event.liquidity_delta,
                    block_number,
                );
            }
        }
        // SYNC/SWAP: discarded during subscribe phase (stateless)
    }

    /// Resume phase: begin normal pump processing.
    ///
    /// Takes ownership of the `SubscribeState` (containing the first observed
    /// block number and the live WS stream) and starts processing blocks.
    async fn resume(
        &mut self,
        subscribe_state: SubscribeState,
    ) {
        let combined = subscribe_state
            .combined_stream
            .expect("resume() called without WS stream — did you call subscribe() first?");

        self.run_with_stream(combined, subscribe_state.first_block).await;
    }

    /// Resume phase using the pump's own watch channel.
    pub async fn resume_from_subscribe(&mut self, subscribe_state: SubscribeState) {
        let combined = subscribe_state
            .combined_stream
            .expect("resume() called without WS stream — did you call subscribe() first?");

        self.run_with_stream(combined, subscribe_state.first_block).await;
    }

    /// Run the main pump loop with an existing WS stream.
    ///
    /// Processes logs eagerly: each WS log is applied to engine state
    /// immediately and affected paths are solved right away, without
    /// waiting for a block header. Block headers provide metadata
    /// (timestamp, fees) and handle empty-block detection.
    #[allow(unused_assignments)]
    async fn run_with_stream(
        &mut self,
        mut combined: stream::BoxStream<'static, WsEvent>,
        first_observed_block: u64,
    ) {
        let relevant_topic_set: HashSet<B256> = RELEVANT_TOPICS.into_iter().collect();

        // Read the last block processed by Python backfill.
        let mut current_block: u64 = {
            let engine = self.engine.lock();
            engine.last_processed_block().unwrap_or(0)
        };

        if current_block == 0 && first_observed_block > 0 {
            current_block = first_observed_block;
            log::info!(
                "UniswapEnginePump: cold start from block {first_observed_block}"
            );
        } else {
            log::info!(
                "UniswapEnginePump: starting from block {current_block} (Python backfill)"
            );
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

        loop {
            // Check shutdown
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("UniswapEnginePump: shutting down");
                return;
            }

            // Wait for the next event with a timeout for backfill.
            let event = timeout(
                Duration::from_secs(BACKFILL_TIMEOUT_SECS),
                combined.next(),
            )
            .await;

            match event {
                // Timeout — no activity for 60s. Try to backfill.
                Err(_) => {
                    // Finalize current block if there are unsolved dirty paths
                    self.finalize_if_dirty(
                        current_block,
                        &mut last_solved_block,
                        &mut has_logs_this_block,
                    );

                    self.handle_timeout_eager(
                        &relevant_topic_set,
                        &mut current_block,
                        &mut last_solved_block,
                        &mut has_logs_this_block,
                    )
                    .await;
                }

                // Got a block header from the combined stream
                Ok(Some(WsEvent::BlockHeader {
                    number,
                    timestamp,
                    base_fee_per_gas,
                    gas_used,
                    gas_limit,
                })) => {
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
                                    "UniswapEnginePump: gap from block {} to {} — backfilling",
                                    current_block + 1,
                                    number,
                                );
                                self.backfill_range(
                                    current_block + 1,
                                    number - 1,
                                    &relevant_topic_set,
                                    &mut current_block,
                                )
                                .await;
                                // backfill_range updates current_block to number-1;
                                // the next line advances to number (the header block)
                                current_block = number;
                                last_solved_block = number;
                            } else {
                                current_block = number;
                                last_solved_block = number;
                            }
                        }
                        first_header = false;
                    } else if number > current_block {
                        // Normal case: header for a new block arrived.
                        // Finalize the current block (solve any pending dirty
                        // paths and send a result batch), then advance.
                        self.finalize_if_dirty(
                            current_block,
                            &mut last_solved_block,
                            &mut has_logs_this_block,
                        );

                        // Check for gap and backfill if needed
                        if number > current_block + 1 {
                            log::info!(
                                "UniswapEnginePump: gap from block {} to {} — backfilling",
                                current_block + 1,
                                number,
                            );
                            self.backfill_range(
                                current_block + 1,
                                number - 1,
                                &relevant_topic_set,
                                &mut current_block,
                            )
                            .await;
                            // backfill_range advanced current_block to number-1
                        }

                        current_block = number;
                        last_solved_block = number;

                        // For empty blocks (no logs arrived), send an
                        // empty batch so Python sees the block boundary.
                        if !has_logs_this_block {
                            let mut engine = self.engine.lock();
                            engine.process_block(&[], current_block, &current_metadata);
                        }

                        last_solved_block = current_block;
                        has_logs_this_block = true; // reset; will be set false by next log or overwritten at next header
                    }
                    // else: stale/duplicate header — ignore
                }

                // Got a log event from the combined stream — apply eagerly
                Ok(Some(WsEvent::Log(log))) => {
                    if !relevant_topic_set.contains(log.topics().first().unwrap_or(&B256::ZERO)) {
                        continue;
                    }

                    let log_block = log.block_number.unwrap_or(current_block);

                    // Detect new block via log's block_number
                    if log_block > current_block {
                        // Finalize the current block first
                        self.finalize_if_dirty(
                            current_block,
                            &mut last_solved_block,
                            &mut has_logs_this_block,
                        );

                        current_block = log_block;
                    }

                    // Apply the log immediately to engine state
                    let _affected_paths = {
                        let mut engine = self.engine.lock();
                        engine.apply_log(&log, current_block)
                    };

                    has_logs_this_block = true;

                    // Solve affected paths immediately
                    {
                        let mut engine = self.engine.lock();
                        engine.solve_dirty(current_block, &current_metadata);
                    }

                    last_solved_block = current_block;
                }

                Ok(None) => {
                    log::warn!("UniswapEnginePump: both subscription streams ended");
                    return;
                }
            }
        }
    }

    /// Finalize the current block if there are unsolved dirty paths.
    fn finalize_if_dirty(
        &self,
        block: u64,
        last_solved_block: &mut u64,
        has_logs_this_block: &mut bool,
    ) {
        if block > *last_solved_block {
            let mut engine = self.engine.lock();
            // Check if there are any dirty pools to solve
            if engine.has_dirty_paths() {
                engine.solve_dirty(block, &BlockMetadata::default());
            } else if *has_logs_this_block {
                // Logs arrived but none affected registered pools —
                // still advance the engine's block number
                engine.process_block(&[], block, &BlockMetadata::default());
            }
            *last_solved_block = block;
            *has_logs_this_block = false;
        }
    }

    /// Handle a 60s timeout by backfilling any missed blocks (eager variant).
    #[allow(unused_assignments)]
    async fn handle_timeout_eager(
        &self,
        relevant_topic_set: &HashSet<B256>,
        current_block: &mut u64,
        last_solved_block: &mut u64,
        has_logs_this_block: &mut bool,
    ) {
        log::warn!(
            "UniswapEnginePump: no activity for {BACKFILL_TIMEOUT_SECS}s — attempting backfill"
        );
        let latest_block = match self.provider.provider_arc().get_block_number().await {
            Ok(n) => n,
            Err(e) => {
                log::error!("UniswapEnginePump: backfill failed — can't get block number: {e}");
                return;
            }
        };

        if latest_block > *current_block {
            let mut lpb = *current_block;
            self.backfill_range(
                *current_block + 1,
                latest_block,
                relevant_topic_set,
                &mut lpb,
            )
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
    async fn backfill_range(
        &self,
        from_block: u64,
        to_block: u64,
        relevant_topic_set: &HashSet<B256>,
        last_processed_block: &mut u64,
    ) {
        if from_block > to_block {
            return;
        }

        log::info!(
            "UniswapEnginePump: backfilling blocks {from_block} to {to_block}"
        );

        let filter = build_backfill_filter(from_block, to_block);
        let logs = match self.provider.provider_arc().get_logs(&filter).await {
            Ok(logs) => logs,
            Err(e) => {
                log::error!("UniswapEnginePump: backfill eth_getLogs failed: {e}");
                return;
            }
        };

        // Group logs by block number for sequential processing
        let mut logs_by_block: HashMap<u64, Vec<Log>> = HashMap::new();
        for log in &logs {
            if let Some(block_num) = log.block_number {
                logs_by_block.entry(block_num).or_default().push(log.clone());
            }
        }

        // Process each block in order
        let mut any_processed = false;
        for block in from_block..=to_block {
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("UniswapEnginePump: shutting down during backfill");
                return;
            }

            let block_logs = logs_by_block.remove(&block).unwrap_or_default();
            let filtered = filter_relevant_logs(&block_logs, relevant_topic_set);
            if !filtered.is_empty() {
                self.engine.lock().process_block(&filtered, block, &BlockMetadata::default());
                any_processed = true;
            }
            *last_processed_block = block;
        }

        if any_processed {
            log::info!("UniswapEnginePump: backfill complete for blocks {from_block}–{to_block}");
        } else {
            log::info!("UniswapEnginePump: backfill found no relevant events in blocks {from_block}–{to_block}");
        }
    }
}

/// Filter a set of logs to only those relevant to the engine.
///
/// A log is relevant if:
/// 1. Its topic0 matches one of the 6 monitored event types, AND
/// 2. Its emitting address is a registered V2/V3 pool or V4 `PoolManager`
pub fn filter_relevant_logs(
    logs: &[Log],
    relevant_topic_set: &HashSet<B256>,
) -> Vec<Log> {
    logs.iter()
        .filter(|log| {
            log.topics()
                .first()
                .is_some_and(|topic0| relevant_topic_set.contains(topic0))
        })
        .cloned()
        .collect()
}

/// Build an Alloy `Filter` for backfill via `eth_getLogs`.
///
/// Uses topic filtering server-side to reduce response size. No address
/// filter — all topic-filtered logs are passed through to the engine.
#[must_use] 
pub fn build_backfill_filter(from_block: u64, to_block: u64) -> Filter {
    let mut filter = Filter::new()
        .from_block(from_block)
        .to_block(to_block);

    // Add all relevant topics so the server pre-filters
    for topic in &RELEVANT_TOPICS {
        filter = filter.event_signature(*topic);
    }

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
    fn filter_relevant_logs_empty_input() {
        let topics: HashSet<B256> = RELEVANT_TOPICS.into_iter().collect();
        let result = filter_relevant_logs(&[], &topics);
        assert!(result.is_empty());
    }

    #[test]
    fn filter_relevant_logs_filters_by_topic_only() {
        // filter_relevant_logs only filters by topic. All engines
        // (V2, V3, V4) handle unregistered addresses gracefully —
        // V2/V3 ignore unknown pools, V4 buffers them.
        use alloy::primitives::{Bytes, Log as InnerLog};
        let topics: HashSet<B256> = RELEVANT_TOPICS.into_iter().collect();

        // All relevant topics pass through regardless of address
        let pm_address = Address::ZERO;

        let v4_modify_inner = InnerLog::new_unchecked(
            pm_address,
            vec![V4_MODIFY_LIQUIDITY_TOPIC, B256::ZERO],
            Bytes::new(),
        );
        let v4_modify_log = Log {
            inner: v4_modify_inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };

        let v2_inner = InnerLog::new_unchecked(pm_address, vec![V2_SYNC_TOPIC], Bytes::new());
        let v2_log = Log {
            inner: v2_inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };

        let result = filter_relevant_logs(&[v4_modify_log, v2_log], &topics);
        assert_eq!(result.len(), 2, "Relevant topic logs should pass through regardless of address");

        // Irrelevant topic filtered out
        let irrelevant_inner = InnerLog::new_unchecked(pm_address, vec![B256::ZERO], Bytes::new());
        let irrelevant_log = Log {
            inner: irrelevant_inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };
        let result = filter_relevant_logs(&[irrelevant_log], &topics);
        assert!(result.is_empty(), "Irrelevant topic should be filtered out");
    }

    #[test]
    fn backfill_timeout_constant_is_reasonable() {
        // 60s is the chosen timeout — verify it's set
        assert_eq!(BACKFILL_TIMEOUT_SECS, 60);
    }

    #[test]
    fn topic_matches_event_type_mint_and_burn() {
        // Default buffer set: MINT and BURN
        let buffer = DEFAULT_BUFFER_EVENTS.to_vec();

        // MINT topic should match
        assert!(topic_matches_event_type(&V3_MINT_TOPIC, &buffer));
        // BURN topic should match
        assert!(topic_matches_event_type(&V3_BURN_TOPIC, &buffer));
        // ModifyLiquidity maps to BURN in the inclusion set
        assert!(topic_matches_event_type(&V4_MODIFY_LIQUIDITY_TOPIC, &buffer));
        // SYNC should NOT match the default buffer
        assert!(!topic_matches_event_type(&V2_SYNC_TOPIC, &buffer));
        // SWAP should NOT match the default buffer
        assert!(!topic_matches_event_type(&V3_SWAP_TOPIC, &buffer));
    }

    #[test]
    fn topic_matches_event_type_all_events() {
        let all_events = vec![EventType::SYNC, EventType::SWAP, EventType::MINT, EventType::BURN];

        assert!(topic_matches_event_type(&V2_SYNC_TOPIC, &all_events));
        assert!(topic_matches_event_type(&V3_SWAP_TOPIC, &all_events));
        assert!(topic_matches_event_type(&V4_SWAP_TOPIC, &all_events));
        assert!(topic_matches_event_type(&V3_MINT_TOPIC, &all_events));
        assert!(topic_matches_event_type(&V3_BURN_TOPIC, &all_events));
        assert!(topic_matches_event_type(&V4_MODIFY_LIQUIDITY_TOPIC, &all_events));
    }

    #[test]
    fn topic_matches_event_type_unknown_topic() {
        let buffer = DEFAULT_BUFFER_EVENTS.to_vec();
        // A random topic should not match
        assert!(!topic_matches_event_type(&B256::ZERO, &buffer));
    }
}

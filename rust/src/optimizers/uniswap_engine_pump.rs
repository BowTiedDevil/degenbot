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

use alloy::primitives::{Address, B256};
use alloy::rpc::types::{Filter, Log};
use futures_util::{StreamExt, stream};
use tokio::sync::watch;
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
use crate::optimizers::uniswap_engine::UniswapEngine;
use crate::provider::AlloyProvider;
use crate::runtime::get_runtime;

/// How long to wait with no activity before assuming the connection is dead.
const BACKFILL_TIMEOUT_SECS: u64 = 60;

/// Block data sent from the pump to Python via the watch channel.
///
/// Python reads this on each new block to compute base fees and schedule
/// dispatch — no separate WS subscription needed.
#[derive(Clone, Debug)]
pub struct BlockNotification {
    /// The block number
    pub block_number: u64,
    /// Block timestamp
    pub timestamp: u64,
    /// Base fee per gas (None for pre-EIP-1559 blocks)
    pub base_fee_per_gas: Option<u64>,
    /// Gas used in this block
    pub gas_used: u64,
    /// Gas limit of this block
    pub gas_limit: u64,
    /// Whether a backfill was performed for this block
    pub backfilled: bool,
}

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
    /// Watch sender — publishes `BlockNotification` after each processed block
    block_tx: watch::Sender<BlockNotification>,
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

        let (block_tx, _block_rx) = watch::channel(BlockNotification {
            block_number: 0,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
            backfilled: false,
        });

        let pump = Self {
            engine,
            provider: Arc::new(provider),
            shutdown,
            block_tx,
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
    ) -> Result<(tokio::task::JoinHandle<()>, watch::Receiver<BlockNotification>), String> {
        let runtime = get_runtime();

        let (block_tx, block_rx) = watch::channel(BlockNotification {
            block_number: 0,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
            backfilled: false,
        });

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
                    // Send the first block as a notification
                    let _ = block_tx.send(BlockNotification {
                        block_number: state.first_block,
                        timestamp: state.first_timestamp,
                        base_fee_per_gas: None,
                        gas_used: 0,
                        gas_limit: 0,
                        backfilled: false,
                    });
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

            pump.resume(subscribe_state, block_tx).await;
        });

        Ok((handle, block_rx))
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
    ///
    /// The `block_tx` is used instead of `self.block_tx` to allow the legacy
    /// `spawn()` path to pass a separately-created sender.
    async fn resume(
        &mut self,
        subscribe_state: SubscribeState,
        block_tx: watch::Sender<BlockNotification>,
    ) {
        let combined = subscribe_state
            .combined_stream
            .expect("resume() called without WS stream — did you call subscribe() first?");

        // Use the block_tx from parameter (allows legacy spawn to inject its own)
        self.block_tx = block_tx;

        self.run_with_stream(combined, subscribe_state.first_block).await;
    }

    /// Resume phase using the pump's own watch channel.
    ///
    /// Called after `subscribe()` when the pump already has its own `block_tx`.
    pub async fn resume_from_subscribe(&mut self, subscribe_state: SubscribeState) {
        let combined = subscribe_state
            .combined_stream
            .expect("resume() called without WS stream — did you call subscribe() first?");

        self.run_with_stream(combined, subscribe_state.first_block).await;
    }

    /// Run the main pump loop with an existing WS stream.
    ///
    /// This is the core event loop shared between `resume()` and legacy `spawn()`.
    async fn run_with_stream(
        &mut self,
        mut combined: stream::BoxStream<'static, WsEvent>,
        first_observed_block: u64,
    ) {
        let relevant_topic_set: HashSet<B256> = RELEVANT_TOPICS.into_iter().collect();

        // Read the last block processed by Python backfill.
        let mut last_processed_block: Option<u64> = {
            let engine = self.engine.lock();
            engine.last_processed_block()
        };

        // If Python already backfilled to first_observed_block, use that.
        // If not, the first_observed_block becomes our starting point.
        if last_processed_block.is_none() && first_observed_block > 0 {
            // Cold start — no Python backfill was done.
            // Record this block as the starting point and skip processing.
            last_processed_block = Some(first_observed_block);
            log::info!(
                "UniswapEnginePump: cold start from block {first_observed_block}"
            );
        } else if let Some(lpb) = last_processed_block {
            log::info!(
                "UniswapEnginePump: starting from block {lpb} (Python backfill)"
            );
        }

        // Buffer for logs received since the last newHeads
        let mut pending_logs: Vec<Log> = Vec::new();
        // Whether this is the first block header we receive. Used to skip
        // re-processing the backfill block (already done by Python).
        let mut first_header = true;

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
                    // Take a fresh snapshot of registered addresses for filtering
                    let relevant_addrs = {
                        let engine = self.engine.lock();
                        Self::collect_relevant_addrs(&engine)
                    };
                    self.handle_timeout(
                        &relevant_addrs,
                        &relevant_topic_set,
                        &mut last_processed_block,
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
                    // Take a fresh snapshot of registered addresses for this block
                    let relevant_addrs = {
                        let engine = self.engine.lock();
                        Self::collect_relevant_addrs(&engine)
                    };
                    let (new_lpb, new_first) = self
                        .handle_block_header(
                            number,
                            timestamp,
                            base_fee_per_gas,
                            gas_used,
                            gas_limit,
                            &mut pending_logs,
                            &relevant_addrs,
                            &relevant_topic_set,
                            last_processed_block,
                            first_header,
                        )
                        .await;
                    last_processed_block = new_lpb;
                    first_header = new_first;
                }

                // Got a log event from the combined stream
                Ok(Some(WsEvent::Log(log))) => {
                    pending_logs.push(log);
                }

                Ok(None) => {
                    log::warn!("UniswapEnginePump: both subscription streams ended");
                    return;
                }
            }
        }
    }

    /// Collect the set of registered pool/PoolManager addresses for filtering.
    fn collect_relevant_addrs(engine: &parking_lot::MutexGuard<'_, UniswapEngine>) -> HashSet<Address> {
        let v2 = engine.v2_registered_addresses();
        let v3 = engine.v3_registered_addresses();
        let v4 = engine.v4_registered_pool_managers();

        v2.iter().chain(v3.iter()).chain(v4.iter()).copied().collect()
    }

    /// Take the watch receiver from the pump's block channel.
    ///
    /// This is used by `PyUniswapArbEngine.resume()` to obtain the receiver
    /// for block notifications. The pump retains the sender.
    pub fn take_block_rx(&mut self) -> watch::Receiver<BlockNotification> {
        // Subscribe to the existing channel to get a new receiver.
        // The existing channel might have been created during subscribe().
        self.block_tx.subscribe()
    }

    /// Handle a block header event from the WS subscription.
    ///
    /// Returns the updated `last_processed_block` and `first_header` flag.
    /// Takes `pending_logs` by mutable reference and clears it as needed.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    async fn handle_block_header(
        &self,
        number: u64,
        timestamp: u64,
        base_fee_per_gas: Option<u64>,
        gas_used: u64,
        gas_limit: u64,
        pending_logs: &mut Vec<Log>,
        relevant_addrs: &HashSet<Address>,
        relevant_topic_set: &HashSet<B256>,
        last_processed_block: Option<u64>,
        first_header: bool,
    ) -> (Option<u64>, bool) {
        match last_processed_block {
            None => {
                // Cold start — no Python backfill was done.
                // Record this block as the starting point and skip processing.
                // The next block header will trigger normal processing.
                pending_logs.clear();
                let _ = self.block_tx.send(BlockNotification {
                    block_number: number,
                    timestamp,
                    base_fee_per_gas,
                    gas_used,
                    gas_limit,
                    backfilled: false,
                });
                (Some(number), false)
            }
            Some(prior_block) if first_header => {
                // First header after Python backfill. prior_block
                // was already processed by Python — we must NOT
                // call process_completed_block on it, because:
                //   - pending_logs is empty (WS subscription
                //     started after prior_block completed)
                //   - process_completed_block would detect empty
                //     pending_logs and call backfill_single_block,
                //     which fetches the SAME events Python already
                //     applied via eth_getLogs → double-processing.
                // Instead: backfill any gap between prior_block and
                // this header, then set last_processed_block to
                // this block header for normal operation.
                if number > prior_block + 1 {
                    log::info!(
                        "UniswapEnginePump: gap from block {} to {} — backfilling",
                        prior_block + 1,
                        number,
                    );
                    let mut lpb = prior_block;
                    self.backfill_range(
                        prior_block + 1,
                        number - 1,
                        relevant_addrs,
                        relevant_topic_set,
                        &mut lpb,
                    )
                    .await;
                }
                // Discard any WS logs — they arrived between the
                // backfill and the first header, so they're for
                // blocks that backfill_range just processed via
                // eth_getLogs (or for the current incomplete block
                // which will be collected in the next cycle).
                pending_logs.clear();
                let _ = self.block_tx.send(BlockNotification {
                    block_number: number,
                    timestamp,
                    base_fee_per_gas,
                    gas_used,
                    gas_limit,
                    backfilled: false,
                });
                (Some(number), false)
            }
            Some(prior_block) if number == prior_block + 1 => {
                // Normal case: the new header is exactly one block
                // ahead. Process the prior block with buffered WS logs.
                self.process_completed_block(
                    prior_block,
                    pending_logs,
                    relevant_addrs,
                    relevant_topic_set,
                )
                .await;
                pending_logs.clear();
                let _ = self.block_tx.send(BlockNotification {
                    block_number: number,
                    timestamp,
                    base_fee_per_gas,
                    gas_used,
                    gas_limit,
                    backfilled: false,
                });
                (Some(number), first_header)
            }
            Some(prior_block) if number > prior_block + 1 => {
                // Gap detected during steady-state operation
                // (e.g., after a 60s timeout recovery).
                // Backfill the missing blocks, then process the
                // prior block with WS logs.
                log::info!(
                    "UniswapEnginePump: gap from block {} to {} — backfilling",
                    prior_block + 1,
                    number,
                );
                let mut lpb = prior_block;
                self.backfill_range(
                    prior_block + 1,
                    number - 1,
                    relevant_addrs,
                    relevant_topic_set,
                    &mut lpb,
                )
                .await;
                // Process the prior block with WS logs
                self.process_completed_block(
                    prior_block,
                    pending_logs,
                    relevant_addrs,
                    relevant_topic_set,
                )
                .await;
                pending_logs.clear();
                let _ = self.block_tx.send(BlockNotification {
                    block_number: number,
                    timestamp,
                    base_fee_per_gas,
                    gas_used,
                    gas_limit,
                    backfilled: false,
                });
                (Some(number), first_header)
            }
            Some(prior_block) => {
                // number <= prior_block: stale or duplicate header.
                // Ignore it — we're already past this block.
                log::debug!(
                    "UniswapEnginePump: ignoring stale header {number} (current: {prior_block})"
                );
                (last_processed_block, first_header)
            }
        }
    }

    /// Process the completed block using buffered logs, with backfill on empty.
    ///
    /// Returns `true` if logs were processed from the WS buffer, `false` if
    /// a backfill was attempted (or the block was truly empty).
    async fn process_completed_block(
        &self,
        block_to_process: u64,
        pending_logs: &[Log],
        relevant_addrs: &HashSet<Address>,
        relevant_topic_set: &HashSet<B256>,
    ) -> bool {
        let logs_received = !pending_logs.is_empty();

        // Filter the buffered logs to relevant events only
        let filtered = filter_relevant_logs(pending_logs, relevant_addrs, relevant_topic_set);

        if logs_received && !filtered.is_empty() {
            // Process buffered logs for the completed block
            self.engine.lock().process_block(&filtered, block_to_process);
            log::debug!(
                "UniswapEnginePump: processed block {block_to_process} ({} filtered logs)",
                filtered.len()
            );
            true
        } else if logs_received && filtered.is_empty() {
            // Got logs but none were relevant — still need to advance the engine's
            // block number so staleness checks work correctly
            self.engine.lock().process_block(&[], block_to_process);
            log::debug!(
                "UniswapEnginePump: processed block {block_to_process} (no relevant logs)"
            );
            true
        } else {
            // No logs received since last newHeads — backfill to verify
            log::debug!(
                "UniswapEnginePump: block {block_to_process} — no logs received, verifying via eth_getLogs"
            );
            let backfilled = self
                .backfill_single_block(block_to_process, relevant_addrs, relevant_topic_set)
                .await;
            if !backfilled {
                // Block truly had no relevant events — advance engine block number
                self.engine.lock().process_block(&[], block_to_process);
            }
            false
        }
    }

    /// Handle a 60s timeout by backfilling any missed blocks.
    async fn handle_timeout(
        &self,
        relevant_addrs: &HashSet<Address>,
        relevant_topic_set: &HashSet<B256>,
        last_processed_block: &mut Option<u64>,
    ) {
        log::warn!(
            "UniswapEnginePump: no activity for {BACKFILL_TIMEOUT_SECS}s — attempting backfill"
        );
        let current_block = match self.provider.provider_arc().get_block_number().await {
            Ok(n) => n,
            Err(e) => {
                log::error!("UniswapEnginePump: backfill failed — can't get block number: {e}");
                return; // The timeout will re-trigger next iteration
            }
        };

        if let Some(lpb) = *last_processed_block {
            if current_block > lpb {
                let mut lpb_mut = lpb;
                self.backfill_range(
                    lpb + 1,
                    current_block,
                    relevant_addrs,
                    relevant_topic_set,
                    &mut lpb_mut,
                )
                .await;
                *last_processed_block = Some(lpb_mut);
            }
        }
    }

    /// Backfill a single block via `eth_getLogs`.
    ///
    /// Returns `true` if logs were found and processed, `false` if the block
    /// genuinely had no relevant events.
    async fn backfill_single_block(
        &self,
        block_number: u64,
        relevant_addrs: &HashSet<Address>,
        relevant_topic_set: &HashSet<B256>,
    ) -> bool {
        let filter = build_backfill_filter(block_number, block_number);
        let logs = match self.provider.provider_arc().get_logs(&filter).await {
            Ok(logs) => logs,
            Err(e) => {
                log::warn!(
                    "UniswapEnginePump: backfill eth_getLogs failed for block {block_number}: {e}"
                );
                return false;
            }
        };

        let filtered = filter_relevant_logs(&logs, relevant_addrs, relevant_topic_set);
        if filtered.is_empty() {
            log::debug!("UniswapEnginePump: backfill confirmed block {block_number} has no relevant events");
            return false;
        }

        self.engine.lock().process_block(&filtered, block_number);
        log::info!(
            "UniswapEnginePump: backfilled block {block_number} with {} events",
            filtered.len()
        );
        true
    }

    /// Backfill a range of blocks via `eth_getLogs`.
    ///
    /// Processes each block in the range sequentially.
    async fn backfill_range(
        &self,
        from_block: u64,
        to_block: u64,
        relevant_addrs: &HashSet<Address>,
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
            let filtered = filter_relevant_logs(&block_logs, relevant_addrs, relevant_topic_set);
            if !filtered.is_empty() {
                self.engine.lock().process_block(&filtered, block);
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
    relevant_addrs: &HashSet<Address>,
    relevant_topic_set: &HashSet<B256>,
) -> Vec<Log> {
    logs.iter()
        .filter(|log| {
            relevant_addrs.contains(&log.address())
                && log.topics().first().is_some_and(|topic0| relevant_topic_set.contains(topic0))
        })
        .cloned()
        .collect()
}

/// Build an Alloy `Filter` for backfill via `eth_getLogs`.
///
/// Uses topic filtering server-side to reduce response size. No address
/// filter — the Rust-side `filter_relevant_logs` handles that.
fn build_backfill_filter(from_block: u64, to_block: u64) -> Filter {
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
        let addrs = HashSet::new();
        let topics: HashSet<B256> = RELEVANT_TOPICS.into_iter().collect();
        let result = filter_relevant_logs(&[], &addrs, &topics);
        assert!(result.is_empty());
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

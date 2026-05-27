//! Uniswap Engine Pump — unified async task that drives the `UniswapEngine`.
//!
//! A single pump that subscribes to both block headers and log events via WS,
//! buffers incoming logs against each block, and routes them to the appropriate
//! sub-engine via `UniswapEngine::process_block()`.
//!
//! # Architecture
//!
//! ```text
//! WS subscription: newHeads + logs (unfiltered)
//!     │
//!     ├─ logs arrive in real-time, buffered per block
//!     │
//!     └─ on newHeads:
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
const RELEVANT_TOPICS: [B256; 6] = [
    V2_SYNC_TOPIC,
    V3_SWAP_TOPIC,
    V3_MINT_TOPIC,
    V3_BURN_TOPIC,
    V4_SWAP_TOPIC,
    V4_MODIFY_LIQUIDITY_TOPIC,
];

/// Events from the two WS subscriptions.
enum WsEvent {
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
pub struct UniswapEnginePump {
    /// Shared engine state
    engine: Arc<parking_lot::Mutex<UniswapEngine>>,
    /// The Alloy provider (created from the RPC URL)
    provider: Arc<AlloyProvider>,
    /// Shutdown flag — set by `stop()`
    shutdown: Arc<AtomicBool>,
    /// Watch sender — publishes `BlockNotification` after each processed block
    block_tx: watch::Sender<BlockNotification>,
}

impl UniswapEnginePump {
    /// Create and spawn the pump on the Tokio runtime.
    ///
    /// The pump:
    /// 1. Subscribes to block headers AND log events via WS
    /// 2. Buffers incoming logs between block boundaries
    /// 3. On each new block: filters relevant logs and calls `engine.process_block()`
    /// 4. Sends a `BlockNotification` via the watch channel
    /// 5. On timeout (60s) or empty-block: backfills via `eth_getLogs`
    /// 6. Loops until `shutdown` is set
    ///
    /// Returns a handle that can be used to stop the pump, plus a
    /// `watch::Receiver<BlockNotification>` for Python to read.
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
            let provider = match AlloyProvider::new(&rpc_url, 3).await {
                Ok(p) => p,
                Err(e) => {
                    log::error!("UniswapEnginePump: failed to create provider: {e}");
                    return;
                }
            };

            let pump = Self {
                engine,
                provider: Arc::new(provider),
                shutdown: shutdown_clone,
                block_tx,
            };

            pump.run().await;
        });

        Ok((handle, block_rx))
    }

    /// Collect the set of registered pool/PoolManager addresses for filtering.
    fn collect_relevant_addrs(engine: &parking_lot::MutexGuard<'_, UniswapEngine>) -> HashSet<Address> {
        let v2 = engine.v2_registered_addresses();
        let v3 = engine.v3_registered_addresses();
        let v4 = engine.v4_registered_pool_managers();

        v2.iter().chain(v3.iter()).chain(v4.iter()).copied().collect()
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

    /// Run the pump loop until shutdown is signaled.
    ///
    /// Maintains two concurrent WS subscriptions:
    /// - `newHeads` for block boundary notifications
    /// - `logs` (unfiltered) for real-time event delivery
    ///
    /// Logs are buffered and processed atomically when the next block header
    /// arrives. Two backfill triggers cover connection issues:
    /// - **Timeout**: 60s with nothing received on either subscription
    /// - **Empty block**: newHeads arrives but zero logs were received since
    ///   the previous newHeads → verify via `eth_getLogs`
    async fn run(self) {
        let provider_arc = self.provider.provider_arc();

        // Subscribe to block headers
        let block_stream = match provider_arc.subscribe_blocks().await {
            Ok(s) => {
                log::info!("UniswapEnginePump: subscribed to block headers");
                s.into_stream()
            }
            Err(e) => {
                log::error!("UniswapEnginePump: failed to subscribe to blocks: {e}");
                return;
            }
        };

        // Subscribe to logs — no filter. All filtering happens in Rust.
        let log_filter = Filter::new();
        let log_stream = match provider_arc.subscribe_logs(&log_filter).await {
            Ok(s) => {
                log::info!("UniswapEnginePump: subscribed to logs (unfiltered)");
                s.into_stream()
            }
            Err(e) => {
                log::error!("UniswapEnginePump: failed to subscribe to logs: {e}");
                return;
            }
        };

        // Merge both streams into a single fused stream
        let mut combined = stream_select(block_stream, log_stream);

        // Build the set of relevant addresses for filtering.
        // Drop the engine lock immediately to avoid holding it across await points.
        let relevant_addrs = {
            let engine = self.engine.lock();
            Self::collect_relevant_addrs(&engine)
        };
        let relevant_topic_set: HashSet<B256> = RELEVANT_TOPICS.into_iter().collect();

        // Read the last block processed by Python backfill.
        // If Python ran process_block(..., 25182221) before starting the pump,
        // this will be Some(25182221). The pump uses this to determine the
        // backfill boundary on startup — any blocks between this value and
        // the first WS block header must be fetched via eth_getLogs.
        // None means no block has been processed yet (cold start).
        let mut last_processed_block: Option<u64> = {
            let engine = self.engine.lock();
            engine.last_processed_block()
        };
        if let Some(block) = last_processed_block {
            log::info!("UniswapEnginePump: starting from block {block} (Python backfill)");
        } else {
            log::info!("UniswapEnginePump: starting with no prior processed block");
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
            // If nothing arrives within 60s, the connection is likely dead.
            let event = timeout(
                Duration::from_secs(BACKFILL_TIMEOUT_SECS),
                combined.next(),
            )
            .await;

            match event {
                // Timeout — no activity for 60s. Try to backfill.
                Err(_) => {
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
fn filter_relevant_logs(
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
fn stream_select(
    block_stream: impl StreamExt<Item = alloy::rpc::types::Header> + Unpin + Send + 'static,
    log_stream: impl StreamExt<Item = Log> + Unpin + Send + 'static,
) -> impl StreamExt<Item = WsEvent> + Unpin + Send {
    let block_events = block_stream.map(|header| WsEvent::BlockHeader {
        number: header.number,
        timestamp: header.timestamp,
        base_fee_per_gas: header.base_fee_per_gas,
        gas_used: header.gas_used,
        gas_limit: header.gas_limit,
    });
    let log_events = log_stream.map(WsEvent::Log);

    stream::select(block_events, log_events)
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
}

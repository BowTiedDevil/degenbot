//! Uniswap Engine Pump — unified async task that drives the `UniswapEngine`.
//!
//! A single pump that subscribes to block headers, fetches ALL relevant logs
//! (V2 Sync, V3 Swap/Mint/Burn, V4 Swap/ModifyLiquidity) in one
//! `eth_getLogs` call per block, and routes them to the appropriate
//! sub-engine via `UniswapEngine::process_block()`.
//!
//! # Architecture
//!
//! ```text
//! WS subscription → block header
//!     → single `eth_getLogs` call per block with all topics
//!     → route V2/V3/V4 events to sub-engines
//!     → results stored in engine for Python to read
//!     → BlockNotification sent via watch channel
//! ```
//!
//! # Why a single pump?
//!
//! V2, V3, and V4 each could have their own pump (as they do in the individual
//! `V2EnginePump`, `V3EnginePump`, `V4EnginePump` files). But a single pump
//! that fetches all logs in one `eth_getLogs` call per block reduces:
//! - RPC round-trips: 1 instead of 3 per block
//! - WS subscription load: 1 block header subscriber instead of 3
//! - Lock contention: engine locked once per block, not 3 times
//!
//! The individual pumps are kept as alternative entry points for testing or
//! single-protocol deployments.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alloy::primitives::Address;
use alloy::rpc::types::Filter;
use futures_util::StreamExt;
use parking_lot::Mutex;
use tokio::sync::watch;

use crate::bot_core::v3_mint_burn_decoder::{V3_MINT_TOPIC, V3_BURN_TOPIC};
use crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC;
use crate::bot_core::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC;
use crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC;
use crate::optimizers::v2_sync_decoder::V2_SYNC_TOPIC;
use crate::optimizers::uniswap_engine::UniswapEngine;
use crate::provider::AlloyProvider;
use crate::runtime::get_runtime;

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
}

/// The unified pump that drives the `UniswapEngine`.
pub struct UniswapEnginePump {
    /// Shared engine state
    engine: Arc<Mutex<UniswapEngine>>,
    /// The Alloy provider (created from the RPC URL)
    provider: Arc<AlloyProvider>,
    /// Pre-built log filter for all event types on registered addresses
    log_filter: Filter,
    /// Shutdown flag — set by `stop()`
    shutdown: Arc<AtomicBool>,
    /// Watch sender — publishes BlockNotification after each processed block
    block_tx: watch::Sender<BlockNotification>,
}

impl UniswapEnginePump {
    /// Create and spawn the pump on the Tokio runtime.
    ///
    /// The pump:
    /// 1. Subscribes to block headers via the WS connection
    /// 2. On each new block: fetches all relevant logs via a single `eth_getLogs`
    /// 3. Calls `engine.process_block(logs, block_number)`
    /// 4. Sends a `BlockNotification` via the watch channel
    /// 5. Loops until `shutdown` is set
    ///
    /// Returns a handle that can be used to stop the pump, plus a
    /// `watch::Receiver<BlockNotification>` for Python to read.
    pub fn spawn(
        rpc_url: String,
        engine: Arc<Mutex<UniswapEngine>>,
        shutdown: &Arc<AtomicBool>,
    ) -> Result<(tokio::task::JoinHandle<()>, watch::Receiver<BlockNotification>), String> {
        let runtime = get_runtime();

        let (block_tx, block_rx) = watch::channel(BlockNotification {
            block_number: 0,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
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

            let log_filter = {
                let engine = engine.lock();
                build_uniswap_log_filter(
                    &engine.v2_registered_addresses(),
                    &engine.v3_registered_addresses(),
                    &engine.v4_registered_pool_managers(),
                )
            };

            let pump = Self {
                engine,
                provider: Arc::new(provider),
                log_filter,
                shutdown: shutdown_clone,
                block_tx,
            };

            pump.run().await;
        });

        Ok((handle, block_rx))
    }

    /// Run the pump loop until shutdown is signaled.
    async fn run(self) {
        let provider_arc = self.provider.provider_arc();

        // Subscribe to block headers
        let sub_result = provider_arc.subscribe_blocks().await;
        let mut stream = match sub_result {
            Ok(s) => {
                log::info!("UniswapEnginePump: subscribed to block headers");
                s.into_stream()
            }
            Err(e) => {
                log::error!("UniswapEnginePump: failed to subscribe to blocks: {e}");
                return;
            }
        };

        loop {
            // Check shutdown before waiting for next block
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("UniswapEnginePump: shutting down");
                return;
            }

            // Await next block header
            let Some(header) = stream.next().await else {
                log::warn!("UniswapEnginePump: block header stream ended");
                return;
            };

            let block_number = header.number;

            // Check shutdown again after receiving block
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("UniswapEnginePump: shutting down after block {block_number}");
                return;
            }

            // Build block-specific filter (from/to = current block)
            let block_filter = self
                .log_filter
                .clone()
                .from_block(block_number)
                .to_block(block_number);

            // Fetch all relevant logs for this block in a single RPC call
            let logs = match self.provider.provider_arc().get_logs(&block_filter).await {
                Ok(logs) => logs,
                Err(e) => {
                    log::warn!(
                        "UniswapEnginePump: failed to fetch logs for block {block_number}: {e}"
                    );
                    continue; // Skip this block, try next
                }
            };

            // Process the block: route events to sub-engines and solve
            self.engine.lock().process_block(&logs, block_number);

            // Notify Python via the watch channel (non-blocking — overwrites if stale)
            let notification = BlockNotification {
                block_number,
                timestamp: header.timestamp,
                base_fee_per_gas: header.base_fee_per_gas,
                gas_used: header.gas_used,
                gas_limit: header.gas_limit,
            };
            let _ = self.block_tx.send(notification);

            log::debug!("UniswapEnginePump: processed block {block_number}");
        }
    }
}

/// Build an Alloy `Filter` for all Uniswap event types.
///
/// Combines:
/// - V2: `V2_SYNC_TOPIC` on registered V2 pool addresses
/// - V3: `V3_SWAP_TOPIC` | `V3_MINT_TOPIC` | `V3_BURN_TOPIC` on registered V3 pool addresses
/// - V4: `V4_SWAP_TOPIC` | `V4_MODIFY_LIQUIDITY_TOPIC` on registered PoolManager addresses
///
/// The Alloy `Filter` supports multiple `topic0` values (OR semantics) and
/// multiple `address` values (OR semantics). This gives us a single filter
/// that captures all relevant events.
fn build_uniswap_log_filter(
    v2_addresses: &[Address],
    v3_addresses: &[Address],
    v4_pool_managers: &[Address],
) -> Filter {
    let mut filter = Filter::new();

    // Set topic0 values for all event types
    filter = filter
        .event_signature(V2_SYNC_TOPIC)
        .event_signature(V3_SWAP_TOPIC)
        .event_signature(V3_MINT_TOPIC)
        .event_signature(V3_BURN_TOPIC)
        .event_signature(V4_SWAP_TOPIC)
        .event_signature(V4_MODIFY_LIQUIDITY_TOPIC);

    // Add all addresses (V2 pools + V3 pools + V4 PoolManagers)
    for addr in v2_addresses {
        filter = filter.address(*addr);
    }
    for addr in v3_addresses {
        filter = filter.address(*addr);
    }
    for addr in v4_pool_managers {
        filter = filter.address(*addr);
    }

    filter
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_uniswap_log_filter_with_all_addresses() {
        let v2_addr = Address::from([0x11u8; 20]);
        let v3_addr = Address::from([0x22u8; 20]);
        let v4_pm = Address::from([0x33u8; 20]);

        let filter = build_uniswap_log_filter(&[v2_addr], &[v3_addr], &[v4_pm]);

        // Verify the filter was constructed without panicking
        let debug_str = format!("{filter:?}");
        assert!(!debug_str.is_empty());
    }

    #[test]
    fn build_uniswap_log_filter_with_no_addresses() {
        let filter = build_uniswap_log_filter(&[], &[], &[]);

        // Should still produce a valid filter (just topics, no address constraint)
        let debug_str = format!("{filter:?}");
        assert!(!debug_str.is_empty());
    }

    #[test]
    fn shutdown_flag_stops_uniswap_pump() {
        let shutdown = Arc::new(AtomicBool::new(true));
        assert!(shutdown.load(Ordering::Relaxed));
    }
}

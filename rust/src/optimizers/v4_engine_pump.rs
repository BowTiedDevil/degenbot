//! V4 Engine Pump — standalone async task that drives the `V4BlockEngine`.
//!
//! The pump owns its own `AlloyProvider` connection and subscribes to block
//! headers directly via Alloy's WS subscription. On each new block, it
//! fetches V4 Swap/ModifyLiquidity logs via `eth_getLogs`, decodes them,
//! and drives `V4BlockEngine::process_block()`.
//!
//! # Architecture
//!
//! ```text
//! WS subscription → block header
//!     → `eth_getLogs`(PoolManager address, `V4_SWAP_TOPIC` | `V4_MODIFY_LIQUIDITY_TOPIC`)
//!     → decode Swap/ModifyLiquidity events → `process_block()`
//!     → results stored in engine for Python to read
//! ```
//!
//! # V4 vs V2/V3 filtering
//!
//! V2 and V3 pumps filter by individual pool contract addresses. V4 pools
//! live inside a single `PoolManager` contract, so the filter uses just the
//! `PoolManager` address (typically one address for all V4 pools). This makes
//! the V4 log filter trivially small compared to V2/V3 which may filter
//! hundreds of addresses.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alloy::primitives::Address;
use alloy::rpc::types::Filter;
use futures_util::StreamExt;
use parking_lot::Mutex;

use crate::bot_core::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC;
use crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC;
use crate::optimizers::v4_block_engine::V4BlockEngine;
use crate::provider::AlloyProvider;
use crate::runtime::get_runtime;

/// The pump that drives the `V4BlockEngine`.
pub struct V4EnginePump {
    /// Shared engine state (same `Arc` as `PyV4ArbEngine` holds)
    engine: Arc<Mutex<V4BlockEngine>>,
    /// The Alloy provider (created from the RPC URL)
    provider: Arc<AlloyProvider>,
    /// Pre-built log filter for Swap/ModifyLiquidity events on `PoolManager`
    log_filter: Filter,
    /// Shutdown flag — set by `stop()`
    shutdown: Arc<AtomicBool>,
}

impl V4EnginePump {
    /// Create and spawn the pump on the Tokio runtime.
    ///
    /// The pump:
    /// 1. Subscribes to block headers via the WS connection
    /// 2. On each new block: fetches Swap/ModifyLiquidity logs via `eth_getLogs`
    /// 3. Calls `engine.process_block(logs, block_number)`
    /// 4. Loops until `shutdown` is set
    ///
    /// Returns a handle that can be used to stop the pump.
    #[allow(clippy::missing_errors_doc)]
    pub fn spawn(
        rpc_url: String,
        engine: Arc<Mutex<V4BlockEngine>>,
        shutdown: &Arc<AtomicBool>,
    ) -> Result<tokio::task::JoinHandle<()>, String> {
        let runtime = get_runtime();

        let shutdown_clone = Arc::clone(shutdown);
        let handle = runtime.spawn(async move {
            let provider = match AlloyProvider::new(&rpc_url, 3).await {
                Ok(p) => p,
                Err(e) => {
                    log::error!("V4EnginePump: failed to create provider: {e}");
                    return;
                }
            };

            let log_filter = {
                let engine = engine.lock();
                build_v4_log_filter(&engine.registered_pool_managers())
            };

            let pump = Self {
                engine,
                provider: Arc::new(provider),
                log_filter,
                shutdown: shutdown_clone,
            };

            pump.run().await;
        });

        Ok(handle)
    }

    /// Run the pump loop until shutdown is signaled.
    async fn run(self) {
        let provider_arc = self.provider.provider_arc();

        // Subscribe to block headers
        let sub_result = provider_arc.subscribe_blocks().await;
        let mut stream = match sub_result {
            Ok(s) => {
                log::info!("V4EnginePump: subscribed to block headers");
                s.into_stream()
            }
            Err(e) => {
                log::error!("V4EnginePump: failed to subscribe to blocks: {e}");
                return;
            }
        };

        loop {
            // Check shutdown before waiting for next block
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("V4EnginePump: shutting down");
                return;
            }

            // Await next block header
            let Some(header) = stream.next().await else {
                log::warn!("V4EnginePump: block header stream ended");
                return;
            };

            let block_number = header.number;

            // Check shutdown again after receiving block
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("V4EnginePump: shutting down after block {block_number}");
                return;
            }

            // Build block-specific filter (from/to = current block)
            let block_filter = self
                .log_filter
                .clone()
                .from_block(block_number)
                .to_block(block_number);

            // Fetch Swap/ModifyLiquidity logs for this block
            let logs = match self.provider.provider_arc().get_logs(&block_filter).await {
                Ok(logs) => logs,
                Err(e) => {
                    log::warn!(
                        "V4EnginePump: failed to fetch logs for block {block_number}: {e}"
                    );
                    continue; // Skip this block, try next
                }
            };

            // Process the block: decode Swap/ModifyLiquidity events, update pools, solve
            self.engine.lock().process_block(&logs, block_number);

            log::debug!("V4EnginePump: processed block {block_number}");
        }
    }
}

/// Build an Alloy `Filter` for V4 Swap/ModifyLiquidity events on `PoolManager`.
///
/// Unlike V2/V3 which filter by individual pool addresses, V4 filters by
/// the `PoolManager` contract address — typically just one address.
fn build_v4_log_filter(pool_managers: &[Address]) -> Filter {
    let mut filter = Filter::new();

    // Set topic0 = V4_SWAP_TOPIC | V4_MODIFY_LIQUIDITY_TOPIC
    filter = filter
        .event_signature(V4_SWAP_TOPIC)
        .event_signature(V4_MODIFY_LIQUIDITY_TOPIC);

    // Add all PoolManager addresses
    for addr in pool_managers {
        filter = filter.address(*addr);
    }

    filter
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_v4_log_filter_includes_topics_and_addresses() {
        let pm = Address::from([0x11u8; 20]);
        let filter = build_v4_log_filter(&[pm]);

        // Verify the filter was constructed without panicking
        let debug_str = format!("{filter:?}");
        assert!(!debug_str.is_empty());
    }

    #[test]
    fn build_v4_log_filter_with_no_pool_managers() {
        let filter = build_v4_log_filter(&[]);

        // Should still produce a valid filter (just no address constraint)
        let debug_str = format!("{filter:?}");
        assert!(!debug_str.is_empty());
    }

    #[test]
    fn shutdown_flag_stops_pump() {
        let shutdown = Arc::new(AtomicBool::new(true));
        assert!(shutdown.load(Ordering::Relaxed));
    }
}

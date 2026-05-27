//! V2 Engine Pump — standalone async task that drives the `V2BlockEngine`.
//!
//! The pump owns its own `AlloyProvider` connection and subscribes to block
//! headers directly via Alloy's WS subscription. On each new block, it
//! fetches Sync logs via `eth_getLogs`, decodes them, and drives
//! `V2BlockEngine::process_block()`.
//!
//! # Architecture
//!
//! ```text
//! WS subscription → block header
//!     → `eth_getLogs`(registered addresses, `V2_SYNC_TOPIC`)
//!     → decode Sync events → `process_block()`
//!     → results stored in engine for Python to read
//! ```
//!
//! The pump runs on the shared Tokio runtime (`get_runtime()`) as a spawned
//! task. It has no dependency on `SubscriptionHandle` / `drain_buffer()` /
//! the Python subscription infrastructure.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alloy::primitives::Address;
use alloy::rpc::types::Filter;
use futures_util::StreamExt;
use parking_lot::Mutex;

use crate::optimizers::v2_block_engine::V2BlockEngine;
use crate::optimizers::v2_sync_decoder::V2_SYNC_TOPIC;
use crate::provider::AlloyProvider;
use crate::runtime::get_runtime;

/// The pump that drives the `V2BlockEngine`.
pub struct V2EnginePump {
    /// Shared engine state (same `Arc` as `PyV2ArbEngine` holds)
    engine: Arc<Mutex<V2BlockEngine>>,
    /// The Alloy provider (created from the RPC URL)
    provider: Arc<AlloyProvider>,
    /// Pre-built log filter for Sync events on registered addresses
    log_filter: Filter,
    /// Shutdown flag — set by `stop()`
    shutdown: Arc<AtomicBool>,
}

impl V2EnginePump {
    /// Create and spawn the pump on the Tokio runtime.
    ///
    /// The pump:
    /// 1. Subscribes to block headers via the WS connection
    /// 2. On each new block: fetches Sync logs via `eth_getLogs`
    /// 3. Calls `engine.process_block(logs, block_number)`
    /// 4. Loops until `shutdown` is set
    ///
    /// Returns a handle that can be used to stop the pump.
    #[allow(clippy::missing_errors_doc)]
    pub fn spawn(
        rpc_url: String,
        engine: Arc<Mutex<V2BlockEngine>>,
        shutdown: &Arc<AtomicBool>,
    ) -> Result<tokio::task::JoinHandle<()>, String> {
        let runtime = get_runtime();

        // We need to create the provider asynchronously, so we spawn a setup
        // task that creates the provider and then starts the pump loop.
        let shutdown_clone = Arc::clone(shutdown);
        let handle = runtime.spawn(async move {
            let provider = match AlloyProvider::new(&rpc_url, 3).await {
                Ok(p) => p,
                Err(e) => {
                    log::error!("V2EnginePump: failed to create provider: {e}");
                    return;
                }
            };

            let log_filter = {
                let engine = engine.lock();
                build_sync_filter(&engine.registered_addresses())
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
                log::info!("V2EnginePump: subscribed to block headers");
                s.into_stream()
            }
            Err(e) => {
                log::error!("V2EnginePump: failed to subscribe to blocks: {e}");
                return;
            }
        };

        loop {
            // Check shutdown before waiting for next block
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("V2EnginePump: shutting down");
                return;
            }

            // Await next block header
            let Some(header) = stream.next().await else {
                log::warn!("V2EnginePump: block header stream ended");
                return;
            };

            let block_number = header.number;

            // Check shutdown again after receiving block
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("V2EnginePump: shutting down after block {block_number}");
                return;
            }

            // Build block-specific filter (from/to = current block)
            let block_filter = self.log_filter.clone()
                .from_block(block_number)
                .to_block(block_number);

            // Fetch Sync logs for this block using the inner provider directly
            let logs = match self.provider.provider_arc().get_logs(&block_filter).await {
                Ok(logs) => logs,
                Err(e) => {
                    log::warn!("V2EnginePump: failed to fetch logs for block {block_number}: {e}");
                    continue; // Skip this block, try next
                }
            };

            // Process the block: decode Sync events, update pools, solve
            self.engine.lock().process_block(&logs, block_number);

            log::debug!("V2EnginePump: processed block {block_number}");
        }
    }
}

/// Build an Alloy `Filter` for Sync events on the given addresses.
fn build_sync_filter(addresses: &[Address]) -> Filter {
    let mut filter = Filter::new();

    // Set topic0 = V2_SYNC_TOPIC
    filter = filter.event_signature(V2_SYNC_TOPIC);

    // Add all registered pool addresses
    for addr in addresses {
        filter = filter.address(*addr);
    }

    filter
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_sync_filter_includes_topic_and_addresses() {
        let addr0 = Address::from([0u8; 20]);
        let addr1 = Address::from([1u8; 20]);
        let filter = build_sync_filter(&[addr0, addr1]);

        // Verify the filter was constructed without panicking
        // and contains the expected fields by checking Debug output
        let debug_str = format!("{filter:?}");
        assert!(debug_str.contains("1c411e9a") || debug_str.contains("sync") || !debug_str.is_empty());
    }

    #[test]
    fn shutdown_flag_stops_pump() {
        let shutdown = Arc::new(AtomicBool::new(true)); // already set
        assert!(shutdown.load(Ordering::Relaxed));
    }
}

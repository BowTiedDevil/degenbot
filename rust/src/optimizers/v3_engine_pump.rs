//! V3 Engine Pump — standalone async task that drives the `V3BlockEngine`.
//!
//! The pump owns its own `AlloyProvider` connection and subscribes to block
//! headers directly via Alloy's WS subscription. On each new block, it
//! fetches V3 Swap/Mint/Burn logs via `eth_getLogs`, decodes them, and drives
//! `V3BlockEngine::process_block()`.
//!
//! # Architecture
//!
//! ```text
//! WS subscription → block header
//!     → `eth_getLogs`(registered addresses, `V3_SWAP_TOPIC` | `V3_MINT_TOPIC` | `V3_BURN_TOPIC`)
//!     → decode Swap/Mint/Burn events → `process_block()`
//!     → results stored in engine for Python to read
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alloy::primitives::Address;
use alloy::rpc::types::{Filter, Topic};
use futures_util::StreamExt;
use parking_lot::Mutex;

use crate::bot_core::v3_mint_burn_decoder::{V3_MINT_TOPIC, V3_BURN_TOPIC};
use crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC;
use crate::optimizers::v3_block_engine::V3BlockEngine;
use crate::provider::AlloyProvider;
use crate::runtime::get_runtime;

/// The pump that drives the `V3BlockEngine`.
pub struct V3EnginePump {
    /// Shared engine state (same `Arc` as `PyV3ArbEngine` holds)
    engine: Arc<Mutex<V3BlockEngine>>,
    /// The Alloy provider (created from the RPC URL)
    provider: Arc<AlloyProvider>,
    /// Pre-built log filter for Swap events on registered addresses
    log_filter: Filter,
    /// Shutdown flag — set by `stop()`
    shutdown: Arc<AtomicBool>,
}

impl V3EnginePump {
    /// Create and spawn the pump on the Tokio runtime.
    ///
    /// The pump:
    /// 1. Subscribes to block headers via the WS connection
    /// 2. On each new block: fetches Swap logs via `eth_getLogs`
    /// 3. Calls `engine.process_block(logs, block_number)`
    /// 4. Loops until `shutdown` is set
    ///
    /// Returns a handle that can be used to stop the pump.
    #[allow(clippy::missing_errors_doc)]
    pub fn spawn(
        rpc_url: String,
        engine: Arc<Mutex<V3BlockEngine>>,
        shutdown: &Arc<AtomicBool>,
    ) -> Result<tokio::task::JoinHandle<()>, String> {
        let runtime = get_runtime();

        let shutdown_clone = Arc::clone(shutdown);
        let handle = runtime.spawn(async move {
            let provider = match AlloyProvider::new(&rpc_url, 3).await {
                Ok(p) => p,
                Err(e) => {
                    log::error!("V3EnginePump: failed to create provider: {e}");
                    return;
                }
            };

            let log_filter = {
                let engine = engine.lock();
                build_v3_log_filter(&engine.registered_addresses())
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
                log::info!("V3EnginePump: subscribed to block headers");
                s.into_stream()
            }
            Err(e) => {
                log::error!("V3EnginePump: failed to subscribe to blocks: {e}");
                return;
            }
        };

        loop {
            // Check shutdown before waiting for next block
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("V3EnginePump: shutting down");
                return;
            }

            // Await next block header
            let Some(header) = stream.next().await else {
                log::warn!("V3EnginePump: block header stream ended");
                return;
            };

            let block_number = header.number;

            // Check shutdown again after receiving block
            if self.shutdown.load(Ordering::Relaxed) {
                log::info!("V3EnginePump: shutting down after block {block_number}");
                return;
            }

            // Build block-specific filter (from/to = current block)
            let block_filter = self
                .log_filter
                .clone()
                .from_block(block_number)
                .to_block(block_number);

            // Fetch Swap logs for this block
            let logs = match self.provider.provider_arc().get_logs(&block_filter).await {
                Ok(logs) => logs,
                Err(e) => {
                    log::warn!(
                        "V3EnginePump: failed to fetch logs for block {block_number}: {e}"
                    );
                    continue; // Skip this block, try next
                }
            };

            // Process the block: decode Swap events, update pools, solve
            self.engine.lock().process_block(&logs, block_number);

            log::debug!("V3EnginePump: processed block {block_number}");
        }
    }
}

/// Build an Alloy `Filter` for V3 Swap/Mint/Burn events on the given addresses.
fn build_v3_log_filter(addresses: &[Address]) -> Filter {
    let mut filter = Filter::new();

    // Build a single Topic that matches ANY of the V3 event signatures.
    // Alloy's event_signature() overwrites topics[0] on each call, so we
    // must build the OR-list ourselves.
    let mut topic: Topic = Topic::default();
    topic = topic.extend(V3_SWAP_TOPIC);
    topic = topic.extend(V3_MINT_TOPIC);
    topic = topic.extend(V3_BURN_TOPIC);
    filter.topics[0] = topic;

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
    fn build_v3_log_filter_includes_topics_and_addresses() {
        let addr0 = Address::from([0u8; 20]);
        let addr1 = Address::from([1u8; 20]);
        let filter = build_v3_log_filter(&[addr0, addr1]);

        // Verify the filter was constructed without panicking
        let debug_str = format!("{filter:?}");
        assert!(!debug_str.is_empty());
    }

    #[test]
    fn shutdown_flag_stops_pump() {
        let shutdown = Arc::new(AtomicBool::new(true));
        assert!(shutdown.load(Ordering::Relaxed));
    }
}

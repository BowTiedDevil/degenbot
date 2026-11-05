//! Driver-side executor wiring — parity-ledger rows 15 + 16 (Gap G4; the
//! executor cut-over).
//!
//! The per-batch dispatch choreography is CORE-owned
//! (`degenbot::batch_executor::BatchExecutor`): the typed pre-sim policy
//! (the `AssemblyVerdict` skips), the bounded sim fan-out, and the ordered
//! submit lane all run inside the executor. This module keeps only the
//! driver boundary:
//!
//! - the path-resolver adapters ([`MapResolver`] for maps,
//!   [`StagesResolver`] for the live engine registry);
//! - the receipt probe the submit monitors poll ([`ProviderReceiptProbe`]);
//! - the fee helpers that stay driver reach proofs (row 17):
//!   [`priority_fee`] / [`next_base_fee`] / the fee-history fetchers.
//!
//! The payload-served set (BATCH-LOCAL, derived from the batch's payload
//! rows) and the suppression registry (CROSS-BLOCK failure feedback) remain
//! DISTINCT policy inputs: the served set is re-derived per [`BatchWork`],
//! the registry is injected once into the `ExecutorConfig` and owned across
//! blocks — they are never merged into one knob.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use alloy::primitives::{B256, U256};
use degenbot::arbitrage::{compute_priority_fee, BlockPriorityFees};
use degenbot::batch_executor::PathResolver;
use degenbot::bot::arb_engine::EngineStages;
use degenbot::cmd_executor::composers::PathInfo;
use degenbot::rpc::provider::AlloyProvider;
use degenbot::submission::{Dispatcher, ReceiptProbe, SubmissionError, SubmissionResult};

/// A map-backed [`PathResolver`] (offline drivers + tests).
#[derive(Debug, Default, Clone)]
pub struct MapResolver(pub HashMap<u64, PathInfo>);

impl PathResolver for MapResolver {
    fn resolve(&self, path_id: u64) -> Option<PathInfo> {
        self.0.get(&path_id).cloned()
    }
}

/// The live resolver: the engine registry projection through
/// [`EngineStages::path_info_for`] — the same registry the result batch's
/// path ids name.
#[derive(Clone)]
pub struct StagesResolver {
    /// The engine stage seam (the registry's one home).
    pub stages: Arc<EngineStages>,
}

impl PathResolver for StagesResolver {
    fn resolve(&self, path_id: u64) -> Option<PathInfo> {
        self.stages
            .path_info_for(path_id)
            .and_then(std::result::Result::ok)
    }
}

/// The receipt probe the executor's submit monitors poll: one
/// `eth_getTransactionReceipt` read per probe through the session provider.
#[derive(Clone)]
pub struct ProviderReceiptProbe {
    /// The typed RPC provider (the broadcast provider's read twin).
    pub provider: Arc<AlloyProvider>,
}

impl ReceiptProbe for ProviderReceiptProbe {
    fn receipt_found(
        &self,
        tx_hash: B256,
    ) -> Pin<Box<dyn Future<Output = SubmissionResult<bool>> + Send + '_>> {
        let provider = Arc::clone(&self.provider);
        Box::pin(async move {
            let receipt = provider
                .get_transaction_receipt(&tx_hash.to_string())
                .await
                .map_err(|e| SubmissionError::MonitorProbe(format!("{e}")))?;
            Ok(receipt.is_some())
        })
    }
}

/// Fee determination: the market-aware priority fee (`compute_priority_fee`)
/// and the next-block base fee (`degenbot_core::eip_1559`).
///
/// `block_priority_fees` is the p10/p50 percentile pair the driver fetched
/// via `eth_feeHistory` (row 17).
#[must_use]
pub fn priority_fee(
    gross_profit: U256,
    gas_used: u64,
    base_fee_next: u128,
    solve_block: u64,
    current_block: u64,
    block_priority_fees: Option<&BlockPriorityFees>,
) -> u128 {
    compute_priority_fee(
        gross_profit,
        gas_used,
        base_fee_next,
        solve_block,
        current_block,
        block_priority_fees,
    )
}

/// Compute the next-block base fee from the parent block header.
#[must_use]
pub fn next_base_fee(parent_base_fee: u128, parent_gas_used: u128, parent_gas_limit: u128) -> u128 {
    degenbot::eip_1559::next_base_fee(
        parent_base_fee,
        parent_gas_used,
        parent_gas_limit,
        None,
        degenbot::eip_1559::DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR,
        degenbot::eip_1559::DEFAULT_ELASTICITY_MULTIPLIER,
    )
}

/// The category of the failing call index in the 7-call bundle
/// (`[3 pre-balance] [execute] [3 post-balance]` → the Python `fail_index`
/// attribution).
#[must_use]
pub const fn fail_index_category(fail_index: Option<usize>) -> &'static str {
    match fail_index {
        Some(0..=2) => "pre-balance",
        Some(3) => "execute",
        Some(4..=6) => "post-balance",
        _ => "orchestration",
    }
}

/// Classify raw revert return-data into the canonical label
/// (`degenbot::decoders::revert::classify_revert`, the shared core taxonomy).
#[must_use]
pub fn classify_revert(revert_data: &[u8]) -> String {
    degenbot::decoders::revert::classify_revert(revert_data)
}

/// Fetch the p10/p50 priority-fee percentiles through the umbrella RPC leaf
/// (`degenbot::rpc::fetch_priority_fee_percentiles` →
/// `AlloyProvider::eth_fee_history`) — the row-17 standalone-RPC reach proof.
///
/// # Errors
///
/// Returns the provider error detail when `eth_feeHistory` fails or the
/// response lacks a reward sample.
pub async fn fetch_priority_fees(
    provider: &AlloyProvider,
    newest_block: u64,
) -> Result<BlockPriorityFees, String> {
    degenbot::rpc::fetch_priority_fee_percentiles(
        provider,
        alloy::rpc::types::BlockNumberOrTag::Number(newest_block),
    )
    .await
    .map_err(|e| e.to_string())
}

/// Record the p10/p50 percentile pair on the shared dispatcher through the
/// umbrella submission leaf (`degenbot::submission::fetch_fee_history`).
/// Returns `true` when the sample was recorded (RPC failure / empty rewards
/// are advisory — the previous samples remain valid).
pub async fn record_fee_history(
    provider: &AlloyProvider,
    dispatcher: &Arc<Mutex<Dispatcher>>,
    last_block: u64,
    reward_percentiles: &[f64],
) -> bool {
    degenbot::submission::fetch_fee_history(provider, dispatcher, 1, last_block, reward_percentiles)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_resolver_answers_from_the_map() {
        let mut map = HashMap::new();
        map.insert(
            7_u64,
            degenbot::cmd_executor::composers::PathInfo::new(Vec::new()),
        );
        let resolver = MapResolver(map);
        assert!(resolver.resolve(7).is_some());
        assert!(resolver.resolve(8).is_none());
    }

    #[test]
    fn priority_fee_is_clamped_to_percentile_bounds() {
        let fees = BlockPriorityFees {
            block: 100,
            p10: U256::from(1_000_000_000_u64),
            p50: U256::from(2_000_000_000_u64),
        };
        let fee = priority_fee(
            U256::from(1_000_000_000_000_000_u64),
            100_000,
            1_000_000_000,
            100,
            100,
            Some(&fees),
        );
        assert_eq!(fee, 2_000_000_001);
    }

    #[test]
    fn priority_fee_without_history_uses_target() {
        let fee = priority_fee(
            U256::from(1_000_000_000_000_000_u64),
            100_000,
            1_000_000_000,
            100,
            100,
            None,
        );
        // target = (1e15/1.25 - 1e5*1e9)/1e5 = 7_000_000_000; age 0 → unchanged.
        assert_eq!(fee, 7_000_000_000);
    }

    #[test]
    fn next_base_fee_rises_above_target() {
        let fee = next_base_fee(1_000_000_000, 20_000_000, 30_000_000);
        // target=15e6, delta_gas=5e6, delta = 1e9*5e6/15e6/8 = 41_666_666
        assert_eq!(fee, 1_041_666_666);
    }

    #[test]
    fn revert_taxonomy_labels_match_python_fixtures() {
        assert_eq!(
            classify_revert(&[0x52, 0x12, 0xcb, 0xa1]),
            "CurrencyNotSettled"
        );
        assert_eq!(classify_revert(&[]), "empty");
        assert_eq!(classify_revert(&[0x01, 0x02]), "short:0102");
        assert_eq!(
            classify_revert(&[0xde, 0xad, 0xbe, 0xef]),
            "unknown:0xdeadbeef"
        );
    }

    #[test]
    fn fail_index_maps_the_7_call_bundle() {
        assert_eq!(fail_index_category(Some(0)), "pre-balance");
        assert_eq!(fail_index_category(Some(3)), "execute");
        assert_eq!(fail_index_category(Some(6)), "post-balance");
        assert_eq!(fail_index_category(None), "orchestration");
    }
}

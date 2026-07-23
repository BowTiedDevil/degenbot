//! Per-block priority-fee market oracle: `eth_feeHistory` leaf + the typed
//! p10/p50 parse.
//!
//! A generic market-data RPC primitive (not simulation-specific). A sandwich /
//! liquidation / backrun searcher wanting the same market oracle reaches it
//! through this crate, not a backrun-shaped simulation crate. Moved here from
//! the backrun dispatch leaf by ADR-019 D5 — the fee leaf is market data, so
//! it belongs with the rest of the typed RPC surface (`AlloyProvider`,
//! `EthBlock`, the block fetchers).
//!
//! # Parity (§4.2)
//!
//! Ports the `fee_history(block_count=1, newest_block, reward_percentiles)`
//! and `dict(zip(FEE_PERCENTILES, reward[-1]))` block (L2842–L2851). The
//! wire-shape matches the Python web3.py `fee_history` round-trip; the typed
//! `eth_fee_history` surface (the §ZUZANP leaf) carries retry-with-backoff
//! and error classification.

// Solidity/RPC identifiers (eth_feeHistory, reward, oldest_block, etc.) are
// ubiquitous here.
#![allow(clippy::doc_markdown)]

use alloy::eips::BlockNumberOrTag;
use alloy::primitives::U256;
use alloy::rpc::types::eth::FeeHistory;
use degenbot_core::errors::{ProviderError, ProviderResult};

use crate::provider::AlloyProvider;

/// The fee-history percentiles the Python oracle polls
/// (`FEE_PERCENTILES = (10, 50)` — L146). Indexed 0/1 in the
/// [`FeeHistory::reward`] vector per the same order.
pub const FEE_PERCENTILES: [f64; 2] = [10.0, 50.0];

/// The p10 / p50 percentile indices in [`FEE_PERCENTILES`] /
/// [`FeeHistory::reward`].
pub const P10_INDEX: usize = 0;
pub const P50_INDEX: usize = 1;

/// A per-block percentile fee summary the `_compute_priority_fee` consumer reads.
///
/// Mirrors the Python `dispatcher.block_priority_fees[block]` dict
/// (`dict(zip(FEE_PERCENTILES, reward[-1]))` — L2851): p10 and p50 priority-fee
/// samples for a single block, keyed by percentile.
///
/// The strategy-side consumer (`compute_priority_fee`) lives in
/// `degenbot-simulation` (the `sim::evm` submodule), which imports this type
/// from here — the fee struct is market data, produced by the RPC leaf below.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockPriorityFees {
    /// The block number these fees describe.
    pub block: u64,
    /// The p10 priority-fee sample (wei).
    pub p10: U256,
    /// The p50 priority-fee sample (wei).
    pub p50: U256,
}

/// Dispatch an `eth_feeHistory` for the latest block + poll the p10/p50
/// percentiles, returning the typed per-block summary the
/// `_compute_priority_fee` consumer reads.
///
/// Ports the `fee_history(block_count=1, newest_block, reward_percentiles)`
/// and `dict(zip(FEE_PERCENTILES, reward[-1]))` block (L2842–L2851).
///
/// # Errors
///
/// Returns [`ProviderError`] on RPC failure or if the response lacks a
/// reward sample for the latest block.
pub async fn fetch_priority_fee_percentiles(
    provider: &AlloyProvider,
    newest_block: BlockNumberOrTag,
) -> ProviderResult<BlockPriorityFees> {
    let history: FeeHistory = provider
        .eth_fee_history(1, newest_block, &FEE_PERCENTILES)
        .await?;
    parse_block_priority_fees(&history, newest_block)
}

/// Parse a `FeeHistory` into the per-block p10/p50 summary.
fn parse_block_priority_fees(
    history: &FeeHistory,
    _newest_block: BlockNumberOrTag,
) -> ProviderResult<BlockPriorityFees> {
    // `oldest_block` is the block number of the first history entry; with
    // `block_count=1`, that IS the newest block requested (the only entry).
    // It is non-optional in `FeeHistory` (`u64`).
    let block_num = history.oldest_block;
    let reward_vec = history
        .reward
        .as_ref()
        .and_then(|r| r.last())
        .ok_or_else(|| ProviderError::RpcError {
            code: -32000,
            message: "eth_feeHistory returned no reward percentiles".into(),
        })?;
    if reward_vec.len() < FEE_PERCENTILES.len() {
        return Err(ProviderError::RpcError {
            code: -32000,
            message: format!(
                "eth_feeHistory returned {} reward percentiles, expected {}",
                reward_vec.len(),
                FEE_PERCENTILES.len()
            ),
        });
    }
    Ok(BlockPriorityFees {
        block: block_num,
        p10: U256::from(reward_vec[P10_INDEX]),
        p50: U256::from(reward_vec[P50_INDEX]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::eips::BlockNumberOrTag;
    use alloy::primitives::U256;
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::client::ClientBuilder;
    use alloy::rpc::types::eth::FeeHistory;
    use alloy::transports::mock::{Asserter, MockTransport};
    use std::sync::Arc;

    /// Build a provider backed by a `MockTransport` with one canned response.
    fn mock_provider(asserter: &Asserter) -> AlloyProvider {
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
        )
    }

    #[tokio::test]
    async fn fetch_priority_fee_percentiles_round_trips() {
        let asserter = Asserter::new();
        let history = FeeHistory {
            oldest_block: 100,
            reward: Some(vec![vec![1_000_000_000u128, 2_500_000_000u128]]),
            ..FeeHistory::default()
        };
        asserter.push_success(&history);
        let provider = mock_provider(&asserter);

        let fees = fetch_priority_fee_percentiles(&provider, BlockNumberOrTag::Latest)
            .await
            .unwrap();
        assert_eq!(fees.block, 100);
        assert_eq!(fees.p10, U256::from(1_000_000_000u64));
        assert_eq!(fees.p50, U256::from(2_500_000_000u64));
    }

    #[test]
    fn fee_percentiles_match_python_oracle() {
        // FEE_PERCENTILES = (10.0, 50.0) — L146. Compare element-wise (clippy
        // forbids exact f64 array equality).
        assert_eq!(FEE_PERCENTILES.len(), 2);
        assert!((FEE_PERCENTILES[0] - 10.0).abs() < f64::EPSILON);
        assert!((FEE_PERCENTILES[1] - 50.0).abs() < f64::EPSILON);
    }
}

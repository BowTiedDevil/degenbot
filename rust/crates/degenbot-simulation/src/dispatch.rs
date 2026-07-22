//! The typed RPC dispatch leaf for `eth_simulateV1` / `eth_createAccessList` /
//! `eth_feeHistory` (B3/B4/B5).
//!
//! These functions route through the TYPED `degenbot_rpc::AlloyProvider`
//! surfaces (`eth_simulate_v1` / `eth_create_access_list` / `eth_fee_history`,
//! the §ZUZANP leaf — `d26b8248`), which carry retry-with-backoff + error
//! classification built in. This leaf owns NO business logic: it only shapes
//! the RPC request (the [`build_simulate_payload`] 7-call vector) and parses
//! the response into typed, easy-to-consume structs.
//!
//! # Parity (§4.2)
//!
//! The wire-shape of the `eth_simulateV1` JSON params + the response parse
//! match the Python web3.py `simulate_v1` / `create_access_list` / `fee_history`
//! round-trip. The typed `ZUZANP` surfaces already have §4.2 JSON↔struct
//! round-trip parity tested against canonical execution-apis fixtures; the
//! parity here pins the 7-call payload assembly + the response parse surface.

// Solidity/RPC identifiers (eth_simulateV1, eth_createAccessList, eth_feeHistory,
// returnData, gasUsed, status, etc.) are ubiquitous here.
#![allow(clippy::doc_markdown)]

use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::primitives::{Bytes, U256};
use alloy::rpc::types::eth::simulate::{SimulateError, SimulatedBlock};
use alloy::rpc::types::eth::{AccessListResult, FeeHistory, TransactionRequest};
use degenbot_core::errors::{ProviderError, ProviderResult};
use degenbot_rpc::provider::{AlloyProvider, EthBlock as RpcEthBlock};

use crate::payload::{build_simulate_payload, SimulationParams};
// `BlockPriorityFees` lives in `degenbot-evm` (shared with `simulate_in_process`);
// re-exported from this crate's root.
use crate::BlockPriorityFees;

/// The block identifier the Python oracle simulates against — `"pending"`.
///
/// Ports `block_identifier="pending"` (L2048). The simulate + access-list
/// calls both default to pending so the simulation reflects the mempool-aware
/// pending state (the same block the eventual submission would land in).
pub const SIMULATE_BLOCK: BlockNumberOrTag = BlockNumberOrTag::Pending;

/// The typed [`BlockId`] the dispatch sends on the wire for `eth_simulateV1` +
/// `eth_createAccessList` (the second params entry). `SIMULATE_BLOCK` is the
/// `Pending` tag (L2048 parity); this const forwards it through the
/// `BlockId::Number(..)` shape alloy's `simulate().block_id(..)` takes, so the
/// `"pending"` string survives to the JSON-RPC wire — the previously-used
/// `SIMULATE_BLOCK.as_number().unwrap_or(0)` flattened `Pending` → `0` (the
/// genesis block), which the RPC rejects for non-trivial simulations
/// (simulate-v1 aggregates gas-usage against the parent block's gas limit;
/// genesis's accounting rejects every non-trivial simulate).
pub const SIMULATE_BLOCK_ID: BlockId = BlockId::Number(SIMULATE_BLOCK);

/// The fee-history percentiles the Python oracle polls
/// (`FEE_PERCENTILES = (10, 50)` — L146). Indexed 0/1 in the
/// [`FeeHistory::reward`] vector per the same order.
pub const FEE_PERCENTILES: [f64; 2] = [10.0, 50.0];

/// The p10 / p50 percentile indices in [`FEE_PERCENTILES`] /
/// [`FeeHistory::reward`].
pub const P10_INDEX: usize = 0;
pub const P50_INDEX: usize = 1;

/// The parsed result of a single simulated call within a 7-call vector.
///
/// A typed, ownable projection of the Alloy [`SimCallResult`] that surfaces
/// exactly the fields the orchestration consumes (`returnData`, `gasUsed`,
/// `status`, the revert `error.data`). Mirrors the field-by-field reads the
/// Python oracle makes on `sim[0]["calls"][i]` (L2060–L2110).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulatedCall {
    /// The raw bytes returned by the call (normalized: empty `0x` → `Bytes::new()`).
    pub return_data: Bytes,
    /// Gas used by the call.
    pub gas_used: u64,
    /// Success/failure status (`true` = success).
    pub status: bool,
    /// The revert `error.data` (the ABI-encoded revert reason) when the call
    /// failed AND `returnData` was empty (web3.py falls back to `error.data`).
    /// `None` when the call succeeded or `returnData` carried the reason.
    pub revert_data: Option<Bytes>,
}

/// The parsed result of a 7-call simulation vector.
///
/// Carries the 7 parsed [`SimulatedCall`]s + the index of the first failing
/// call (the Python oracle walks `calls` to find `c["status"] != 1` for revert
/// logging — L2060–L2064). `first_failure` is `None` when all 7 succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulationResult {
    /// The 7 parsed calls (`[0..3]` pre-balance, `[3]` execute, `[4..7]` post).
    pub calls: Vec<SimulatedCall>,
    /// The first failing call's index, if any.
    pub first_failure: Option<usize>,
}

/// Dispatch an `eth_simulateV1` over the typed RPC surface.
///
/// Sends the 7-call [`build_simulate_payload`] vector + `stateOverrides`
/// against the `"pending"` block, then parses the response into a typed
/// [`SimulationResult`]. Routes through the typed `AlloyProvider::eth_simulate_v1`
/// (the §ZUZANP leaf — retry-with-backoff + error classification built in).
///
/// # Errors
///
/// Returns [`ProviderError`] if the RPC call fails or the response carries
/// fewer than the expected 7 simulated calls.
pub async fn simulate_v1(
    provider: &AlloyProvider,
    params: &SimulationParams,
    state_overrides: alloy::rpc::types::eth::state::StateOverride,
) -> ProviderResult<SimulationResult> {
    let payload = build_simulate_payload(params, state_overrides);
    let blocks = provider
        .eth_simulate_v1(&payload, SIMULATE_BLOCK_ID)
        .await?;
    let Some(block) = blocks.into_iter().next() else {
        return Err(ProviderError::RpcError {
            code: -32000,
            message: "eth_simulateV1 returned no results".into(),
        });
    };
    parse_simulation_result(block)
}

/// Dispatch an `eth_createAccessList` over the typed RPC surface.
///
/// Computes the EIP-2930 access list for the `execute()` call so the
/// simulation's `gas_used` reflects the warm-slot savings. Routes through the
/// typed `AlloyProvider::eth_create_access_list` against the `"pending"` block.
///
/// # Errors
///
/// Returns [`ProviderError`] on RPC failure (the Python oracle swallows this
/// and re-simulates without the access list — that orchestration lives
/// upstream; this leaf surfaces the typed error).
pub async fn create_access_list(
    provider: &AlloyProvider,
    request: &TransactionRequest,
) -> ProviderResult<AccessListResult> {
    provider
        .eth_create_access_list(request, SIMULATE_BLOCK_ID)
        .await
}

/// Dispatch an `eth_feeHistory` for the latest block + poll the p10/p50
/// percentiles, returning the typed per-block summary the
/// `_compute_priority_fee` consumer reads.
///
/// Ports the `fee_history(block_count=1, newest_block, reward_percentiles)`
/// + `dict(zip(FEE_PERCENTILES, reward[-1]))` block (L2842–L2851).
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

/// Parse the `SimulatedBlock` response into the typed [`SimulationResult`].
///
/// Mirrors the per-call status walk + the revert-data normalization the Python
/// oracle does (L2060–L2110): `returnData` empty → fall back to `error.data`;
/// `returnData: "0x"` → empty bytes.
fn parse_simulation_result(block: SimulatedBlock<RpcEthBlock>) -> ProviderResult<SimulationResult> {
    let calls = block.calls;
    if calls.len() != crate::payload::SIM_CALL_COUNT {
        return Err(ProviderError::RpcError {
            code: -32000,
            message: format!(
                "eth_simulateV1 returned {} calls, expected {}",
                calls.len(),
                crate::payload::SIM_CALL_COUNT
            ),
        });
    }
    let mut parsed: Vec<SimulatedCall> = Vec::with_capacity(calls.len());
    let mut first_failure = None;
    for (idx, call) in calls.into_iter().enumerate() {
        let status = call.status;
        let return_data = normalize_return_data(call.return_data.clone());
        let revert_data = if !status && return_data.is_empty() {
            call.error.as_ref().and_then(error_data_bytes)
        } else {
            None
        };
        if !status && first_failure.is_none() {
            first_failure = Some(idx);
        }
        parsed.push(SimulatedCall {
            return_data,
            gas_used: call.gas_used,
            status,
            revert_data,
        });
    }
    Ok(SimulationResult {
        calls: parsed,
        first_failure,
    })
}

/// Normalize `returnData` the way the Python oracle does (L2073–L2090):
/// empty `0x` / `""` / zero-length → `Bytes::new()`. Non-empty bytes pass through.
fn normalize_return_data(data: Bytes) -> Bytes {
    if data.is_empty() {
        Bytes::new()
    } else {
        data
    }
}

/// Extract the revert `error.data` bytes from a [`SimulateError`], if present
/// and non-empty. Mirrors L2096–L2107 (fall back to `error["data"]`).
///
/// `SimulateError.data` is `Option<Bytes>` (Alloy decodes the `0x…` hex
/// string to bytes already), so this is a non-empty check.
fn error_data_bytes(err: &SimulateError) -> Option<Bytes> {
    let data = err.data.as_ref()?;
    if data.is_empty() {
        None
    } else {
        Some(data.clone())
    }
}

/// Decode a `0x…` hex string to `Bytes` (mirrors `bytes.fromhex(s.removeprefix("0x"))`).
/// Kept for the parity tests that build `Bytes` from hex literals without a
/// `FromHex` import.
#[cfg(test)]
fn hex_decode(s: &str) -> Option<Bytes> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    alloy::hex::decode(s).ok().map(Bytes::from)
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
    #![allow(clippy::too_many_lines)]

    use super::*;
    use crate::{build_simulation_state_overrides, SIM_CALL_COUNT};
    use alloy::eips::BlockNumberOrTag;
    use alloy::primitives::{address, Address, Bytes, U256};
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::client::ClientBuilder;
    use alloy::rpc::types::eth::simulate::{SimCallResult, SimulateError, SimulatedBlock};
    use alloy::rpc::types::eth::state::StateOverride;
    use alloy::rpc::types::eth::FeeHistory;
    use alloy::transports::mock::{Asserter, MockTransport};
    use degenbot_executor::WarmupSlots;
    use std::sync::Arc;

    const OWNER: Address = address!("9c56a29c7231974c269e24f9fb3c29203039089e");
    const EXECUTOR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
    const MULTICALL3: Address = address!("c411372f0b8ae58585e33b78aea9e0596da9a6f1");
    const RUNTIME_BYTECODE: &[u8] = &0xDEAD_BEEF_u32.to_be_bytes();

    fn warmup() -> WarmupSlots {
        degenbot_executor::compute_simulation_warmup_slots(EXECUTOR, WETH, PM)
    }

    fn overrides() -> StateOverride {
        build_simulation_state_overrides(
            OWNER,
            true,
            Some(EXECUTOR),
            Bytes::from_static(RUNTIME_BYTECODE),
            warmup(),
            WETH,
            PM,
        )
    }

    fn sim_params() -> SimulationParams {
        SimulationParams {
            executor_owner: OWNER,
            executor_address: EXECUTOR,
            weth_address: WETH,
            pool_manager_address: PM,
            multicall3_address: MULTICALL3,
            execute_calldata: Bytes::from_static(&[0xab, 0x58, 0x98, 0xe8]),
            access_list: None,
        }
    }

    /// Build a provider backed by a `MockTransport` with one canned response.
    fn mock_provider(asserter: &Asserter) -> AlloyProvider {
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
        )
    }

    // ---------------------------------------------------------------------
    // §4.2 wire-shape parity: the response parse + the dispatch wire round-trip.
    // The typed `ZUZANP` surfaces have JSON↔struct parity pinned separately;
    // here we pin the parse of a 7-call simulated block into the typed
    // [`SimulationResult`] + a successful dispatch round-trip.
    // ---------------------------------------------------------------------

    fn seven_ok_calls() -> Vec<SimCallResult> {
        (0..crate::payload::SIM_CALL_COUNT)
            .map(|_| SimCallResult {
                return_data: Bytes::from_static(&[0x00; 32]),
                logs: Vec::new(),
                gas_used: 100_000,
                max_used_gas: None,
                status: true,
                error: None,
            })
            .collect()
    }

    #[test]
    fn parse_seven_successful_calls() {
        let block = SimulatedBlock {
            inner: RpcEthBlock::default(),
            calls: seven_ok_calls(),
        };
        let result = parse_simulation_result(block).unwrap();
        assert_eq!(result.calls.len(), SIM_CALL_COUNT);
        assert!(result.calls.iter().all(|c| c.status));
        assert_eq!(result.first_failure, None);
        assert_eq!(result.calls[3].gas_used, 100_000);
    }

    #[test]
    fn parse_records_first_failure_index() {
        let mut calls = seven_ok_calls();
        calls[3] = SimCallResult {
            return_data: Bytes::new(),
            logs: Vec::new(),
            gas_used: 21_000,
            max_used_gas: None,
            status: false,
            error: Some(SimulateError {
                code: 3,
                message: "execution reverted".into(),
                data: Some(Bytes::from_static(&[
                    0x08, 0xc3, 0x79, 0xa0, 0x00, 0x00, 0x00, 0x20,
                ])),
            }),
        };
        let block = SimulatedBlock {
            inner: RpcEthBlock::default(),
            calls,
        };
        let result = parse_simulation_result(block).unwrap();
        assert_eq!(result.first_failure, Some(3));
        assert!(!result.calls[3].status);
        // returnData empty → revert falls back to error.data.
        assert!(result.calls[3].return_data.is_empty());
        assert!(result.calls[3].revert_data.is_some());
        assert_eq!(
            &result.calls[3].revert_data.as_ref().unwrap()[..4],
            &[0x08, 0xc3, 0x79, 0xa0]
        );
    }

    #[test]
    fn parse_rejects_wrong_call_count() {
        let block = SimulatedBlock {
            inner: RpcEthBlock::default(),
            calls: vec![SimCallResult::default()],
        };
        assert!(parse_simulation_result(block).is_err());
    }

    #[tokio::test]
    async fn simulate_v1_dispatch_round_trips_a_canned_response() {
        // The typed dispatch sends the 7-call payload over the mock transport;
        // the transport returns a canned 7-call simulated block. The parsed
        // [`SimulationResult`] must surface all 7 calls + no failure.
        let asserter = Asserter::new();
        let canned: Vec<SimulatedBlock<RpcEthBlock>> = vec![SimulatedBlock {
            inner: RpcEthBlock::default(),
            calls: seven_ok_calls(),
        }];
        asserter.push_success(&canned);
        let provider = mock_provider(&asserter);

        let result = simulate_v1(&provider, &sim_params(), overrides())
            .await
            .unwrap();
        assert_eq!(result.calls.len(), SIM_CALL_COUNT);
        assert!(result.calls.iter().all(|c| c.status));
        assert_eq!(result.first_failure, None);
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

    #[test]
    fn simulate_block_is_pending() {
        // block_identifier="pending" — L2048.
        assert_eq!(SIMULATE_BLOCK, BlockNumberOrTag::Pending);
    }

    #[test]
    fn simulate_block_id_is_pending_tag_not_genesis_zero() {
        // eth_simulateV1 against the genesis block ("0x0") is REJECTED by the
        // geth/erigon RPC layer with code -38015 ("Block gas limit exceeded by
        // the block's transactions") — simulate-v1 aggregates the simulated
        // block's gas usage against the parent (genesis) block's gas limit, and
        // genesis has a non-standard gas accounting that rejects every
        // non-trivial simulation. Tools experimenting with bug-finding fit the
        // genesis probe here: "All-sim failed with rpc-failed" in dry-run
        // integration runs.
        //
        // The deleted Python oracle (L2048) used the tag `"pending"`; the Rust
        // port must preserve that tag AT THE WIRE — NOT flatten it via
        // `SIMULATE_BLOCK.as_number().unwrap_or(0)` (which turns `Pending` →
        // `0`, i.e. `BlockId::Number(0)` = genesis). The BlockId reaches the
        // wire as the second params entry: alloy serializes
        // `BlockId::Number(BlockNumberOrTag::Pending)` as the string
        // `"pending"` (NOT `"0x0"`), matching the parity intent. This const
        // pins the wire shape: any change that reintroduces the flattening-
        // to-genesis bug breaks this assertion at COMPILE time (the type
        // wouldn't unify) or at RUN time (the const comparison flips).
        assert_eq!(
            SIMULATE_BLOCK_ID,
            alloy::eips::BlockId::Number(BlockNumberOrTag::Pending)
        );
    }

    #[test]
    fn error_data_bytes_decodes_0x_prefixed_hex() {
        let err = SimulateError {
            code: 3,
            message: "execution reverted".into(),
            data: Some(hex_decode("0x08c379a0").unwrap()),
        };
        let bytes = error_data_bytes(&err).unwrap();
        assert_eq!(&bytes[..4], &[0x08, 0xc3, 0x79, 0xa0]);
    }

    #[test]
    fn error_data_bytes_returns_none_for_empty() {
        let err = SimulateError {
            code: 3,
            message: "execution reverted".into(),
            data: Some(Bytes::new()),
        };
        assert!(error_data_bytes(&err).is_none());
        let err2 = SimulateError {
            code: 3,
            message: "x".into(),
            data: None,
        };
        assert!(error_data_bytes(&err2).is_none());
    }
}

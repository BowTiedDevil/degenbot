//! Driver-side executor wiring — parity-ledger rows 15 + 16 (Gap G4; the
//! executor cut-over).
//!
//! The per-batch dispatch choreography is CORE-owned
//! (`degenbot::batch_executor::BatchExecutor`): the typed pre-sim policy
//! (the `AssemblyVerdict` skips), the bounded sim fan-out, and the ordered
//! submit lane all run inside the executor. This module keeps only the
//! driver boundary:
//!
//! - the seam extraction ([`raw_row`] / [`payload_row`] / [`batch_work`]):
//!   the engine's `SolvePathResult` / `SimulatedPathResult` rows translated
//!   into the executor's input rows — the Rust twin of the `PyO3` shell's
//!   dict → row extraction at its seam, not a re-implementation of the
//!   shaping (candidate building is the executor's assembly stage);
//! - the path-resolver adapters ([`MapResolver`] for maps,
//!   [`DriverResolver`] for the live engine registry);
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
use degenbot::batch_executor::{BatchWork, PathResolver, PayloadFailure, PayloadRow, RawResult};
use degenbot::bot::arb_engine::{ResultBatch, SimulatedPathResult};
use degenbot::cmd_executor::composers::PathInfo;
use degenbot::rpc::provider::AlloyProvider;
use degenbot::solvers::mixed::SolvePathResult;
use degenbot::submission::{Dispatcher, ReceiptProbe, SubmissionError, SubmissionResult};

/// Translate one engine solve row into the executor's raw row — the seam
/// extraction. `U256` values saturate into `u128` (the driver's display and
/// threshold arithmetic is `u128`; the sim seam works in `U256`).
#[must_use]
pub fn raw_row(path_id: u64, result: &SolvePathResult, solve_block: u64) -> RawResult {
    let saturate = |value: &U256| u128::try_from(*value).unwrap_or(u128::MAX);
    RawResult {
        path_id,
        optimal_input: saturate(&result.optimal_input),
        profit: saturate(&result.profit),
        hop_outputs: result.hop_outputs.iter().map(saturate).collect(),
        consumed_inputs: result.consumed_inputs.iter().map(saturate).collect(),
        solve_block,
        state_nonces: result.state_nonces.clone(),
    }
}

/// Translate one engine inline-sim payload into the executor's payload row —
/// the seam extraction. The join + categorization run crate-side
/// (`degenbot_batch_executor::assembly`): a payload row's resolve miss is
/// the loud-abort arm there, so this extraction never fabricates a path.
#[must_use]
pub fn payload_row(sim: &SimulatedPathResult) -> PayloadRow {
    PayloadRow {
        path_id: sim.path_id,
        gross_profit: sim.gross_profit,
        net_profit: sim.net_profit,
        gas_used: sim.gas_used,
        priority_fee: sim.priority_fee,
        base_fee_next: sim.base_fee_next,
        execute_calldata: alloy::primitives::Bytes::from(sim.execute_calldata.clone()),
        access_list: sim.access_list.as_ref().map(|rows| {
            alloy::rpc::types::AccessList(
                rows.iter()
                    .map(|row| alloy::rpc::types::AccessListItem {
                        address: row.address,
                        storage_keys: row
                            .storage_keys
                            .iter()
                            .map(|key| B256::from(*key))
                            .collect(),
                    })
                    .collect(),
            )
        }),
        failure: sim.failure.as_ref().map(|failure| PayloadFailure {
            bucket: failure.bucket.clone(),
            fail_index: failure.fail_index,
            revert_data: alloy::primitives::Bytes::from(failure.revert_data.clone()),
        }),
    }
}

/// Build one batch's executor work from a consumed `ResultBatch` + the
/// session block clock.
///
/// The `fresh` and `updated` engine rows join the raw batch; the inline-sim
/// payloads join in path-id order (a deterministic record order for the
/// drain). The payload rows' path ids ARE the batch-local payload-served set
/// the executor's assembly reads — per-entry presence decides, so a mixed
/// batch only degrades the payload-less entries.
///
/// `block_priority_fees` stays `None`: the payload arm carries its own
/// market-aware fees, and the FFI arm prices through the core's target-fee
/// fallback (`compute_priority_fee`'s no-history path). The percentile
/// fetch itself stays a driver reach proof ([`fetch_priority_fees`], row 17).
#[must_use]
pub fn batch_work(batch: &ResultBatch, clock: &crate::consume::BlockClock) -> BatchWork {
    let rows: Vec<RawResult> = batch
        .fresh
        .iter()
        .chain(batch.updated.iter())
        .map(|(path_id, result)| raw_row(*path_id, result, batch.solve_block))
        .collect();
    let mut payload_ids: Vec<u64> = batch.payloads.keys().copied().collect();
    payload_ids.sort_unstable();
    let payloads = payload_ids
        .iter()
        .filter_map(|id| batch.payloads.get(id).map(payload_row))
        .collect();
    BatchWork {
        rows,
        payloads,
        current_block: clock.current_block,
        base_fee_next: clock.base_fee_next,
        block_timestamp: clock.block_timestamp,
        block_priority_fees: None,
    }
}

/// A map-backed [`PathResolver`] (offline drivers + tests).
#[derive(Debug, Default, Clone)]
pub struct MapResolver(pub HashMap<u64, PathInfo>);

impl PathResolver for MapResolver {
    fn resolve(&self, path_id: u64) -> Option<PathInfo> {
        self.0.get(&path_id).cloned()
    }
}

/// The live resolver: the engine registry projection through
/// `EngineDriver::path_info_for` — the same registry the result batch's
/// path ids name.
#[derive(Clone)]
pub struct DriverResolver {
    /// The session driver (shared with the engine handshake).
    pub driver: Arc<degenbot::EngineDriver>,
}

impl PathResolver for DriverResolver {
    fn resolve(&self, path_id: u64) -> Option<PathInfo> {
        self.driver
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
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-valid inputs; parse_address fixtures are valid"
)]
mod tests {
    use super::*;
    use crate::consume::BlockClock;
    use degenbot::bot::arb_engine::{AccessListRow, InlineSimFailure};
    use degenbot::cmd_executor::composers::{HopInfo, V2HopInfo, V3HopInfo};
    use degenbot::core::address_utils::parse_address;

    fn fixture_info() -> PathInfo {
        let pool = parse_address("0x1111111111111111111111111111111111111111").unwrap();
        let t0 = parse_address("0x2222222222222222222222222222222222222222").unwrap();
        let t1 = parse_address("0x3333333333333333333333333333333333333333").unwrap();
        PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: pool,
                token0_address: t0,
                token1_address: t1,
                fee: 30,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: pool,
                token0_address: t1,
                token1_address: t0,
                fee: 3000,
                zfo: false,
            }),
        ])
    }

    fn solve_result(optimal_input: u64, profit: u64) -> SolvePathResult {
        SolvePathResult {
            optimal_input: U256::from(optimal_input),
            profit: U256::from(profit),
            hop_outputs: vec![U256::from(profit), U256::from(profit * 2)],
            consumed_inputs: vec![U256::from(optimal_input), U256::from(optimal_input)],
            state_nonces: vec![1, 2],
            solver_pool_states: Vec::new(),
        }
    }

    #[test]
    fn raw_row_projects_the_engine_row_with_u128_saturation() {
        let row = raw_row(9, &solve_result(500, 42), 100);
        assert_eq!(row.path_id, 9);
        assert_eq!(row.optimal_input, 500);
        assert_eq!(row.profit, 42);
        assert_eq!(row.hop_outputs, vec![42, 84]);
        assert_eq!(row.consumed_inputs, vec![500, 500]);
        assert_eq!(row.solve_block, 100);
        assert_eq!(row.state_nonces, vec![1, 2]);

        // A u64-saturating U256 clamps to u128::MAX instead of wrapping.
        let huge = SolvePathResult {
            optimal_input: U256::MAX,
            profit: U256::from(1_u64),
            hop_outputs: vec![U256::MAX],
            consumed_inputs: vec![U256::from(1_u64)],
            state_nonces: vec![7],
            solver_pool_states: Vec::new(),
        };
        let row = raw_row(1, &huge, 100);
        assert_eq!(row.optimal_input, u128::MAX);
        assert_eq!(row.hop_outputs, vec![u128::MAX]);
    }

    #[test]
    fn payload_row_projects_the_inline_sim_row() {
        let sim = SimulatedPathResult {
            path_id: 4,
            gross_profit: U256::from(1_000_u64),
            net_profit: U256::from(900_u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 7,
            execute_calldata: vec![0xde, 0xad],
            access_list: Some(vec![AccessListRow {
                address: parse_address("0x1111111111111111111111111111111111111111").unwrap(),
                storage_keys: vec![U256::from(3_u64)],
            }]),
            captured_swaps: Vec::new(),
            hop_count: 2,
            failure: Some(InlineSimFailure {
                fail_index: Some(3),
                revert_data: vec![0x01, 0x02],
                bucket: "no-profit".to_string(),
            }),
        };
        let row = payload_row(&sim);
        assert_eq!(row.path_id, 4);
        assert_eq!(row.net_profit, U256::from(900_u64));
        assert_eq!(row.gas_used, 300_000);
        assert_eq!(row.base_fee_next, 7);
        assert_eq!(row.execute_calldata.as_ref(), &[0xde, 0xad][..]);
        let list = row.access_list.expect("access list projected");
        assert_eq!(list.0.len(), 1);
        assert_eq!(list.0[0].storage_keys, vec![B256::from(U256::from(3_u64))]);
        let failure = row.failure.expect("failure projected");
        assert_eq!(failure.bucket, "no-profit");
        assert_eq!(failure.fail_index, Some(3));
        assert_eq!(failure.revert_data.as_ref(), &[0x01, 0x02][..]);

        // A healthy payload carries no failure.
        let healthy = SimulatedPathResult {
            failure: None,
            access_list: None,
            ..sim
        };
        let row = payload_row(&healthy);
        assert!(row.failure.is_none());
        assert!(row.access_list.is_none());
    }

    #[test]
    #[expect(
        clippy::default_trait_access,
        reason = "the payload map is hashbrown's (not nameable without a direct dep); Default::default() is the only dep-free spelling"
    )]
    fn batch_work_joins_rows_and_orders_payloads_by_path_id() {
        // The payload map is the engine's hashbrown map: build the batch
        // first, then insert through the field.
        let mut batch = ResultBatch {
            solve_block: 101,
            timestamp: 1_700_000_101,
            base_fee_per_gas: Some(1_000_000_000),
            gas_used: 15_000_000,
            gas_limit: 30_000_000,
            fresh: vec![(1_u64, solve_result(100, 10))],
            updated: vec![(2_u64, solve_result(200, 20))],
            expired: Vec::new(),
            removed: Vec::new(),
            payloads: Default::default(),
        };
        batch.payloads.insert(
            20_u64,
            SimulatedPathResult {
                path_id: 20,
                gross_profit: U256::from(2_u64),
                net_profit: U256::from(2_u64),
                gas_used: 1,
                priority_fee: 1,
                base_fee_next: 1,
                execute_calldata: Vec::new(),
                access_list: None,
                captured_swaps: Vec::new(),
                hop_count: 1,
                failure: None,
            },
        );
        batch.payloads.insert(
            5_u64,
            SimulatedPathResult {
                path_id: 5,
                ..batch.payloads[&20].clone()
            },
        );
        let clock = BlockClock {
            current_block: 101,
            block_timestamp: 1_700_000_101,
            base_fee_next: 1_041_666_666,
        };
        let work = batch_work(&batch, &clock);
        // fresh + updated join, in stream order.
        assert_eq!(
            work.rows.iter().map(|r| r.path_id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        // The payloads join in path-id order (deterministic record order).
        assert_eq!(
            work.payloads.iter().map(|p| p.path_id).collect::<Vec<_>>(),
            vec![5, 20]
        );
        // Per-block facts ride the clock.
        assert_eq!(work.current_block, 101);
        assert_eq!(work.base_fee_next, 1_041_666_666);
        assert_eq!(work.block_timestamp, 1_700_000_101);
        assert!(work.block_priority_fees.is_none());
    }

    #[test]
    fn map_resolver_answers_from_the_map() {
        let mut map = HashMap::new();
        map.insert(7_u64, fixture_info());
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

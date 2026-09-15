//! Driver-side dispatch policy — parity-ledger row 15 (Gap G4).
//!
//! Mirrors `src/degenbot/runner/_dispatch.py` at the *driver* boundary: shape
//! a `ResultBatch`'s raw engine rows into `DispatchCandidate`s, apply the
//! driver-owned pre-filters (empty-hop skip, `PathSuppression` skip,
//! thin-margin via the core `filter_thin_margin_results`), then hand the
//! survivors to the core fan-out (`dispatch_profitable_results`). Fee
//! determination wraps the core `compute_priority_fee` +
//! `degenbot_core::eip_1559::next_base_fee`.
//!
//! The fan-out itself is core-owned (ADR-019 D4); everything in this module is
//! the driver's decision log — the typed `DispatchDecision` outcome the RSP-8
//! gate diffs against the Python driver.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use alloy::primitives::U256;
use degenbot::arbitrage::{
    compute_priority_fee, dispatch_profitable_results, filter_thin_margin_results,
    BlockPriorityFees, DispatchCandidate, DispatchOutcome, FeeOnTransferRegistry, PoolDivergence,
    SimulateContext, SolveStep,
};
use degenbot::cmd_executor::composers::{EncodeOptions, PathInfo};
use degenbot::rpc::provider::AlloyProvider;
use degenbot::solvers::mixed::SolvePathResult;
use degenbot::submission::{Dispatcher, PathSuppression};

/// The driver's typed per-candidate decision (the RSP-8 diff surface).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchDecision {
    /// `hop_outputs` was empty — the `[sim-none]` skip (Python
    /// `_build_dispatch_candidates`).
    SkipEmptyHops {
        /// The path id.
        path_id: u64,
    },
    /// The path is suppressed by `PathSuppression::is_suppressed`.
    Suppressed {
        /// The path id.
        path_id: u64,
    },
    /// The candidate was dropped by the thin-margin pre-filter.
    ThinMargin {
        /// The path id.
        path_id: u64,
    },
    /// The candidate is ready for the sim fan-out.
    Sim {
        /// The path id.
        path_id: u64,
    },
}

impl DispatchDecision {
    /// The stable machine label (the RSP-8 diff column).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SkipEmptyHops { .. } => "skip-empty-hops",
            Self::Suppressed { .. } => "suppressed",
            Self::ThinMargin { .. } => "thin-margin",
            Self::Sim { .. } => "sim",
        }
    }

    /// The candidate's path id.
    #[must_use]
    pub const fn path_id(self) -> u64 {
        match self {
            Self::SkipEmptyHops { path_id }
            | Self::Suppressed { path_id }
            | Self::ThinMargin { path_id }
            | Self::Sim { path_id } => path_id,
        }
    }
}

/// One raw engine-result row, mirroring the Python `_RawResult` tuple
/// `(path_id, optimal_input, profit, hop_outputs, consumed_inputs, solve_block,
/// state_nonces)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawResult {
    /// `path_id`.
    pub path_id: u64,
    /// The solver's optimal input.
    pub optimal_input: u128,
    /// The solver's expected gross profit.
    pub profit: u128,
    /// Per-hop expected outputs.
    pub hop_outputs: Vec<u128>,
    /// Per-hop consumed inputs.
    pub consumed_inputs: Vec<u128>,
    /// The block the solver produced the result on.
    pub solve_block: u64,
    /// Per-hop solve-time state nonces.
    pub state_nonces: Vec<u64>,
}

impl RawResult {
    /// Build a raw row from a `ResultBatch` entry (`SolvePathResult`).
    ///
    /// `U256` values saturate into `u128` (the driver's decision log is
    /// display/threshold arithmetic; the sim seam works in `U256`).
    #[must_use]
    pub fn from_solve_path(path_id: u64, result: &SolvePathResult, solve_block: u64) -> Self {
        let hop_outputs: Vec<u128> = result
            .hop_outputs
            .iter()
            .map(|v| u128::try_from(*v).unwrap_or(u128::MAX))
            .collect();
        let consumed_inputs: Vec<u128> = result
            .consumed_inputs
            .iter()
            .map(|v| u128::try_from(*v).unwrap_or(u128::MAX))
            .collect();
        Self {
            path_id,
            optimal_input: u128::try_from(result.optimal_input).unwrap_or(u128::MAX),
            profit: u128::try_from(result.profit).unwrap_or(u128::MAX),
            hop_outputs,
            consumed_inputs,
            solve_block,
            state_nonces: result.state_nonces.clone(),
        }
    }

    /// Whether the row is dispatchable (non-empty hop outputs).
    #[must_use]
    pub fn has_hops(&self) -> bool {
        !self.hop_outputs.is_empty()
    }

    /// Build a [`DispatchCandidate`] from the row + its resolved [`PathInfo`].
    #[must_use]
    pub fn to_candidate(&self, path_info: PathInfo, opts: EncodeOptions) -> DispatchCandidate {
        let count = self
            .hop_outputs
            .len()
            .min(self.consumed_inputs.len())
            .min(self.state_nonces.len());
        let steps: Vec<SolveStep> = (0..count)
            .map(|i| SolveStep {
                output: self.hop_outputs[i],
                consumed_input: self.consumed_inputs[i],
                state_nonce: self.state_nonces[i],
            })
            .collect();
        DispatchCandidate {
            path_id: self.path_id,
            optimal_input: self.optimal_input,
            engine_profit: self.profit,
            steps: steps.into_boxed_slice(),
            solve_block: self.solve_block,
            path_info,
            opts,
        }
    }
}

/// The result of planning a batch: the per-path decisions + the `Sim`-ready
/// candidates.
#[derive(Debug, Default)]
pub struct BatchPlan {
    /// One decision per input row, in input (path-id) order.
    pub decisions: Vec<DispatchDecision>,
    /// The candidates that reached the `Sim` stage, in input order.
    pub candidates: Vec<DispatchCandidate>,
}

/// Shape + pre-filter one batch of raw rows into the driver's typed plan.
///
/// Applies, in Python order: the empty-hop skip
/// (`_build_dispatch_candidates`), then the suppression skip
/// ([`PathSuppression::is_suppressed`]), then the thin-margin filter
/// (core [`filter_thin_margin_results`]). A row with no resolvable
/// [`PathInfo`] is skipped (a driver guard; the engine always resolves).
#[must_use]
pub fn plan_batch(
    results: &[RawResult],
    resolve: &dyn Fn(u64) -> Option<PathInfo>,
    opts: EncodeOptions,
    suppression: &mut PathSuppression,
    current_block: u64,
    min_profit_margin_bps: u64,
) -> BatchPlan {
    let mut plan = BatchPlan::default();
    let mut candidates: Vec<DispatchCandidate> = Vec::new();
    for row in results {
        if !row.has_hops() {
            plan.decisions.push(DispatchDecision::SkipEmptyHops {
                path_id: row.path_id,
            });
            continue;
        }
        let Some(path_info) = resolve(row.path_id) else {
            plan.decisions.push(DispatchDecision::SkipEmptyHops {
                path_id: row.path_id,
            });
            continue;
        };
        if suppression.is_suppressed(row.path_id, current_block) {
            plan.decisions.push(DispatchDecision::Suppressed {
                path_id: row.path_id,
            });
            continue;
        }
        candidates.push(row.to_candidate(path_info, opts));
    }

    // Thin-margin filter over the survivors (core leaf, pure int).
    let survivors: Vec<u64> = candidates.iter().map(|c| c.path_id).collect();
    let (kept, _dropped) = filter_thin_margin_results(candidates, min_profit_margin_bps);
    let kept_ids: HashSet<u64> = kept.iter().map(|c| c.path_id).collect();
    for id in survivors {
        if kept_ids.contains(&id) {
            plan.decisions.push(DispatchDecision::Sim { path_id: id });
        } else {
            plan.decisions
                .push(DispatchDecision::ThinMargin { path_id: id });
        }
    }
    plan.candidates = kept;
    plan
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

/// The typed failure category for a `FailBuckets` label, mirroring the driver's
/// `classify_revert` taxonomy (Python `runner/config.py` + the
/// `tests/arbitrage/test_revert_taxonomy.py` fixture set).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// A revert label produced by `classify_revert`.
    Revert,
    /// `no-profit` — the sim ran but the path was unprofitable.
    NoProfit,
    /// `int128-overflow` — a V4 amount exceeded `int128`.
    Int128Overflow,
    /// `encode-failed` — the encoder refused the path.
    EncodeFailed,
    /// `rpc-failed` — the sim's RPC cold-miss failed.
    RpcFailed,
    /// `stale` — the solve snapshot advanced before the sim.
    Stale,
    /// Anything else (never silently dropped).
    Other,
}

impl FailureKind {
    /// Classify a `FailBuckets` label.
    #[must_use]
    pub fn from_bucket(bucket: &str) -> Self {
        match bucket {
            "no-profit" => Self::NoProfit,
            "int128-overflow" => Self::Int128Overflow,
            "encode-failed" => Self::EncodeFailed,
            "rpc-failed" => Self::RpcFailed,
            "stale" => Self::Stale,
            "empty" | "numeric-revert" => Self::Revert,
            other => {
                // The taxonomy's custom-error + Error(string) + Panic labels
                // are all non-orchestration strings; treat them as reverts.
                if other.starts_with("unknown:0x")
                    || other.starts_with("short:")
                    || other.starts_with("Panic(")
                    || other.starts_with("Error(")
                    || other.ends_with("NotSettled")
                {
                    Self::Revert
                } else {
                    Self::Other
                }
            }
        }
    }

    /// The stable machine label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Revert => "revert",
            Self::NoProfit => "no-profit",
            Self::Int128Overflow => "int128-overflow",
            Self::EncodeFailed => "encode-failed",
            Self::RpcFailed => "rpc-failed",
            Self::Stale => "stale",
            Self::Other => "other",
        }
    }
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

/// Run the core sim fan-out over the planned candidates.
///
/// RPC-gated: requires a live [`SimulateContext`] + `BotState`. This is the
/// live-arm path only; the offline tests exercise [`plan_batch`] +
/// [`priority_fee`] + the taxonomy helpers.
#[expect(
    clippy::too_many_arguments,
    reason = "linear mirror of the dispatch_profitable_results seam signature"
)]
#[must_use]
pub fn run_sim_fanout(
    candidates: Vec<DispatchCandidate>,
    ctx: &SimulateContext<'_>,
    suppression: &Arc<Mutex<PathSuppression>>,
    pool_divergence: &Arc<Mutex<PoolDivergence>>,
    fot_registry: &Arc<Mutex<FeeOnTransferRegistry>>,
    current_block: u64,
    min_profit_net: u128,
    min_profit_margin_bps: u64,
) -> DispatchOutcome {
    dispatch_profitable_results(
        candidates,
        ctx,
        suppression,
        current_block,
        min_profit_net,
        min_profit_margin_bps,
        pool_divergence,
        fot_registry,
        None,
        None,
    )
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

/// Resolve a map keyed by path id into the `plan_batch` resolver shape.
pub fn map_resolver(map: HashMap<u64, PathInfo>) -> impl Fn(u64) -> Option<PathInfo> {
    move |id| map.get(&id).cloned()
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "tests assert on known-valid inputs; parse_address fixtures are valid"
)]
mod tests {
    use super::*;
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

    fn raw(path_id: u64, optimal_input: u128, profit: u128) -> RawResult {
        RawResult {
            path_id,
            optimal_input,
            profit,
            hop_outputs: vec![profit, profit],
            consumed_inputs: vec![optimal_input, optimal_input],
            solve_block: 100,
            state_nonces: vec![1, 2],
        }
    }

    #[test]
    fn empty_hops_are_skipped_before_any_policy() {
        let mut row = raw(7, 1_000, 100);
        row.hop_outputs.clear();
        let mut suppression = PathSuppression::new();
        let plan = plan_batch(
            &[row],
            &|_| Some(fixture_info()),
            EncodeOptions::default(),
            &mut suppression,
            100,
            0,
        );
        assert_eq!(plan.decisions.len(), 1);
        assert_eq!(plan.decisions[0].label(), "skip-empty-hops");
        assert!(plan.candidates.is_empty());
    }

    #[test]
    fn thin_margin_filter_drops_razor_thin_after_suppression() {
        // 50 bps margin: profit*10000 >= input*50. 100/1_000_000 = 1 bps → dropped.
        let fine = raw(1, 1_000_000, 100);
        let healthy = raw(2, 1_000_000, 10_000); // 100 bps → kept
        let mut suppression = PathSuppression::new();
        let plan = plan_batch(
            &[fine, healthy],
            &|_| Some(fixture_info()),
            EncodeOptions::default(),
            &mut suppression,
            100,
            50,
        );
        assert_eq!(plan.decisions.len(), 2);
        assert_eq!(plan.decisions[0].label(), "thin-margin");
        assert_eq!(plan.decisions[1].label(), "sim");
        assert_eq!(plan.candidates.len(), 1);
        assert_eq!(plan.candidates[0].path_id, 2);
    }

    #[test]
    fn suppressed_path_decides_suppressed_not_sim() {
        let mut suppression = PathSuppression::new();
        for _ in 0..degenbot::submission::PATH_SUPPRESS_THRESHOLD {
            suppression.record_failure(5);
        }
        let plan = plan_batch(
            &[raw(5, 1_000_000, 10_000)],
            &|_| Some(fixture_info()),
            EncodeOptions::default(),
            &mut suppression,
            1,
            0,
        );
        assert_eq!(plan.decisions[0].label(), "suppressed");
        assert!(plan.candidates.is_empty());
    }

    #[test]
    fn suppressed_path_retries_after_the_retry_interval() {
        let mut suppression = PathSuppression::new();
        for _ in 0..degenbot::submission::PATH_SUPPRESS_THRESHOLD {
            suppression.record_failure(5);
        }
        // `current_block - last_retry(0) >= PATH_SUPPRESS_RETRY_INTERVAL` → the
        // path is due for a retry this block, so `is_suppressed` returns
        // false and the candidate reaches the sim stage (Python semantics).
        let plan = plan_batch(
            &[raw(5, 1_000_000, 10_000)],
            &|_| Some(fixture_info()),
            EncodeOptions::default(),
            &mut suppression,
            degenbot::submission::PATH_SUPPRESS_RETRY_INTERVAL,
            0,
        );
        assert_eq!(plan.decisions[0].label(), "sim");
        assert_eq!(plan.candidates.len(), 1);
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
    fn failure_kind_taxonomy_is_total() {
        assert_eq!(FailureKind::from_bucket("no-profit"), FailureKind::NoProfit);
        assert_eq!(
            FailureKind::from_bucket("int128-overflow"),
            FailureKind::Int128Overflow
        );
        assert_eq!(
            FailureKind::from_bucket("CurrencyNotSettled"),
            FailureKind::Revert
        );
        assert_eq!(FailureKind::from_bucket("Panic(0x11)"), FailureKind::Revert);
        assert_eq!(
            FailureKind::from_bucket("ERC20: transfer amount exceeds balance"),
            FailureKind::Other
        );
    }

    #[test]
    fn fail_index_maps_the_7_call_bundle() {
        assert_eq!(fail_index_category(Some(0)), "pre-balance");
        assert_eq!(fail_index_category(Some(3)), "execute");
        assert_eq!(fail_index_category(Some(6)), "post-balance");
        assert_eq!(fail_index_category(None), "orchestration");
    }

    #[test]
    fn raw_result_to_candidate_keeps_step_correspondence() {
        let candidate = raw(9, 500, 42).to_candidate(fixture_info(), EncodeOptions::default());
        assert_eq!(candidate.path_id, 9);
        assert_eq!(candidate.steps.len(), 2);
        assert_eq!(candidate.steps[1].state_nonce, 2);
        assert_eq!(candidate.steps[0].output, 42);
    }
}

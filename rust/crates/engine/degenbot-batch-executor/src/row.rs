//! The executor's input records: one raw solver row per engine result, one
//! payload row per engine inline-sim record.
//!
//! The pure-Rust driver maps its `SolvePathResult`s into [`RawResult`]; the
//! `PyO3` shell extracts its `RawEngineResult` dataclass + payload dicts into
//! these rows at the seam (the dict/getattr translation stays at the boundary;
//! the choreography below is pyo3-free).

use crate::record::FailureDetail;
use alloy::primitives::U256;
use degenbot_arbitrage::{DispatchCandidate, SimResult, SolveStep};
use degenbot_executor::composers::{EncodeOptions, PathInfo};

/// One raw engine-result row — the solver's result for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawResult {
    /// The unique arb path identifier.
    pub path_id: u64,
    /// The solver's optimal swap input.
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
    /// Whether the row is dispatchable (non-empty hop outputs — an empty hop
    /// list is not an encodable path).
    #[must_use]
    pub fn has_hops(&self) -> bool {
        !self.hop_outputs.is_empty()
    }

    /// Build the pre-sim candidate from the row + its resolved path.
    ///
    /// The per-hop rows truncate to the shortest of the three parallel lists
    /// (a length mismatch against the RESOLVED hop count is the loud-abort
    /// shape check the assembly stage applies BEFORE this builder runs).
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

/// One inline-sim payload row — the engine already simulated this path; the
/// row reassembles the primitive result fields the payload arm joins into the
/// submit-lane shape.
#[derive(Debug, Clone, PartialEq)]
pub struct PayloadRow {
    /// The unique arb path identifier.
    pub path_id: u64,
    /// Gross on-chain profit (wei).
    pub gross_profit: U256,
    /// Net profit = gross − gas cost (wei).
    pub net_profit: U256,
    /// The inline sim's raw `gasUsed` (UN-inflated).
    pub gas_used: u64,
    /// The market-aware priority fee.
    pub priority_fee: u128,
    /// The base fee of the next block.
    pub base_fee_next: u128,
    /// The `execute()` calldata.
    pub execute_calldata: alloy::primitives::Bytes,
    /// The pre-sim access list rows.
    pub access_list: Option<alloy::rpc::types::AccessList>,
    /// The inline sim's failure record, when the path failed.
    pub failure: Option<PayloadFailure>,
}

/// The payload arm's failure record — the same keys the FFI failures row
/// carries (`path_id` rides on the row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadFailure {
    /// The bucket label (`classify_revert` output or the inline-fail tag).
    pub bucket: String,
    /// The failing call's index in the 7-call vector, when attributable.
    pub fail_index: Option<usize>,
    /// The raw revert bytes.
    pub revert_data: alloy::primitives::Bytes,
}

impl PayloadRow {
    /// Reassemble the core `SimResult` the FFI batch join consumes
    /// (field-for-field with an FFI survivor). The inline path never carries
    /// inspector-captured swaps — they ride the engine's own render dict.
    #[must_use]
    pub fn sim_result(&self) -> SimResult {
        SimResult {
            path_id: self.path_id,
            gross_profit: self.gross_profit,
            net_profit: self.net_profit,
            gas_used: self.gas_used,
            priority_fee: self.priority_fee,
            base_fee_next: self.base_fee_next,
            execute_calldata: self.execute_calldata.clone(),
            access_list: self.access_list.clone(),
            captured_swaps: Vec::new(),
            hop_count: 0,
        }
    }

    /// Build the failure detail record (the inline sim surfaces revert detail
    /// through the bucket + revert bytes; the EVM-diagnostic extras default
    /// empty — the inline sim never ran an inspector capture).
    #[must_use]
    pub fn failure_detail(&self) -> Option<FailureDetail> {
        self.failure.as_ref().map(|f| FailureDetail {
            path_id: self.path_id,
            bucket: f.bucket.clone(),
            fail_index: f.fail_index,
            revert_data: f.revert_data.clone(),
            reverting_frame: None,
            captured_swaps: Vec::new(),
            log_full_count: 0,
            reverted_swaps: Vec::new(),
            optimal_input: 0,
            hop_outputs: Vec::new(),
            call_trace: Vec::new(),
            weth_before: 0,
            weth_after: 0,
            eth_before: 0,
            eth_after: 0,
            erc6909_before: 0,
            erc6909_after: 0,
        })
    }
}

//! The executor's input records: one raw solver row per engine result, one
//! payload row per engine inline-sim record.
//!
//! The pure-Rust driver maps its `SolvePathResult`s into [`RawResult`]; the
//! `PyO3` shell extracts its `RawEngineResult` dataclass + payload dicts into
//! these rows at the seam (the dict/getattr translation stays at the boundary;
//! the choreography below is pyo3-free).

use crate::record::FailureDetail;
use alloy::primitives::U256;
use degenbot_arbitrage::{CapturedSwap, DispatchCandidate, SimResult, SolveStep};
use degenbot_execution::{CandidateId, DeploymentId, PathInfo};

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

    /// Build the pre-sim candidate from the row + its resolved path, bound to
    /// the deployment that will execute it.
    ///
    /// The per-hop rows truncate to the shortest of the three parallel lists
    /// (a length mismatch against the RESOLVED hop count is the loud-abort
    /// shape check the assembly stage applies BEFORE this builder runs).
    ///
    /// `deployment_id` is the session deployment's own identity: the row is
    /// path-keyed (the solver has no deployment in view), so the candidate's
    /// [`CandidateId`] is minted HERE, at the one place a row becomes a
    /// target-bound candidate.
    #[must_use]
    pub fn to_candidate(
        &self,
        path_info: PathInfo,
        deployment_id: DeploymentId,
    ) -> DispatchCandidate {
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
            candidate_id: CandidateId::new(self.path_id, deployment_id),
            optimal_input: self.optimal_input,
            engine_profit: self.profit,
            steps: steps.into_boxed_slice(),
            solve_block: self.solve_block,
            path_info,
        }
    }
}

/// One inline-sim payload row — the engine already simulated this path; the
/// row reassembles the primitive result fields the payload arm joins into the
/// submit-lane shape.
#[derive(Debug, Clone, PartialEq)]
pub struct PayloadRow {
    /// The candidate the engine simulated inline: the path bound to the
    /// deployment that encoded and simulated it.
    pub candidate_id: CandidateId,
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
    /// The inspector-captured swap events the inline sim decoded — the ACTUAL
    /// side of the expected-vs-actual comparison a failure dump needs. Carried
    /// on the ROW rather than on [`PayloadFailure`] because a survivor's
    /// capture is equally part of the sim's record, and because the failure's
    /// `captured_swaps` is a `SimFailure` field no payload row otherwise
    /// reaches.
    pub captured_swaps: Vec<CapturedSwap>,
}

/// The payload arm's failure record — the same keys the FFI failures row
/// carries (`path_id` rides on the row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadFailure {
    /// The bucket label (`classify_revert` output or the inline-fail tag).
    pub bucket: String,
    /// The failing call's index in the adapter-declared sequence, when
    /// attributable.
    pub fail_index: Option<usize>,
    /// The raw revert bytes.
    pub revert_data: alloy::primitives::Bytes,
}

impl PayloadRow {
    /// Reassemble the core `SimResult` the FFI batch join consumes
    /// (field-for-field with an FFI survivor, including the capture).
    #[must_use]
    pub fn sim_result(&self) -> SimResult {
        SimResult {
            candidate_id: self.candidate_id,
            gross_profit: self.gross_profit,
            net_profit: self.net_profit,
            gas_used: self.gas_used,
            priority_fee: self.priority_fee,
            base_fee_next: self.base_fee_next,
            execute_calldata: self.execute_calldata.clone(),
            access_list: self.access_list.clone(),
            captured_swaps: self.captured_swaps.clone(),
            // The payload arm carries a deployment the engine already assessed.
            // The built-in deployment is the one that opts into market-aware
            // sizing, and its verdict IS the dispatch floor, so the floor
            // comparison reconstructs the same pass flag the fan-out stored.
            passed: degenbot_arbitrage::is_gas_profitable(self.net_profit),
            hop_count: 0,
        }
    }

    /// Build the failure detail record.
    ///
    /// `request` is the batch's OWN solver row for this path — the figures the
    /// inline sim was issued from (`optimal_input` + the expected per-hop
    /// outputs). The payload's failure record carries only the bucket + revert
    /// bytes, so without the request the dump renders `optimal_input=0` and
    /// `hop_outputs=[]` for every inline failure; the values are read from the
    /// batch beside the payload row rather than from the engine's result store,
    /// which a retried or expired candidate has already left.
    ///
    /// The captured swaps come from the payload itself (the sim's own capture),
    /// and the EVM-diagnostic extras (reverting frame, reverted swaps, call
    /// trace) stay empty: the inline sim keeps no inspector trace, so those are
    /// `[sim-diag]`-class fields the FFI arm fills.
    ///
    /// A missing request row (`None`) yields `optimal_input: 0` /
    /// `hop_outputs: []` — the record still emits (dropping a failure record
    /// would hide the failure outright), and a solved candidate never carries a
    /// zero input, so the zero is readable as "this row had no request beside
    /// it" rather than as a measurement.
    #[must_use]
    pub fn failure_detail(&self, request: Option<&RawResult>) -> Option<FailureDetail> {
        self.failure.as_ref().map(|f| FailureDetail {
            candidate_id: self.candidate_id,
            bucket: f.bucket.clone(),
            fail_index: f.fail_index,
            revert_data: f.revert_data.clone(),
            reverting_frame: None,
            captured_swaps: self.captured_swaps.clone(),
            log_full_count: 0,
            reverted_swaps: Vec::new(),
            optimal_input: request.map_or(0, |r| r.optimal_input),
            hop_outputs: request.map_or_else(Vec::new, |r| r.hop_outputs.clone()),
            call_trace: Vec::new(),
        })
    }
}

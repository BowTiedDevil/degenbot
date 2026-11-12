//! The pre-simulation candidate vocabulary.
//!
//! [`DispatchCandidate`] and [`SolveStep`] are the two types every later stage
//! speaks: the admission policy reads them, the target-selection walk annotates
//! one with its composed encoding, and the dispatch leaf simulates it. They are
//! owned HERE — neutral ground — so the admission pass and the dispatch leaf
//! both depend on one candidate definition instead of on each other.
//!
//! Published at [`crate::candidate`], at the crate root, and at its
//! `degenbot_arbitrage::dispatch::*` spellings (re-exported by
//! [`crate::dispatch`]), so a consumer reaches one type three ways.

use degenbot_execution::{CandidateId, PathInfo};

use crate::selection::PreparedEncoding;
use crate::simulator::SimulatePath;

/// One per-hop row of a candidate (a Cloudflare-style fewer-lists row):
/// the solver's output, the executable input, and the solve-time state
/// nonce, contiguous per hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolveStep {
    /// The solver's output after this hop (the former `hop_outputs[i]`).
    pub output: u128,
    /// The executable input fed into this hop's pool (the solver's concentrated-liquidity-hop
    /// clamp) — the former `consumed_inputs[i]`. For V3/V4 hops this may be
    /// less than `output` of the prior hop when the range boundary is hit;
    /// the encoder must feed the clamped forward, not the full prior output.
    pub consumed_input: u128,
    /// Per-hop state nonce captured at solve time (the staleness gate).
    pub state_nonce: u64,
}

/// A pre-simulation candidate — the engine result + the resolved [`PathInfo`].
///
/// Carries the `EngineResult` tuple `(path_id, opt_input, profit, hop_outputs,
/// consumed_inputs, solve_block)`. The `PathInfo` arrives pre-resolved: the
/// registry lookup is the caller's concern (the `degenbot` umbrella's `Bot`
/// owns the engine registry), so no stage here reaches into solver state.
#[derive(Debug, Clone)]
pub struct DispatchCandidate {
    /// The candidate identity — the solver path bound to the deployment that
    /// will encode and submit it. Two deployments on one path are two
    /// candidates.
    pub candidate_id: CandidateId,
    /// `optimal_input` — the solver's optimal swap input.
    pub optimal_input: u128,
    /// `engine_profit` — the solver's expected gross profit (used for sorting
    /// and the thin-margin filter; NOT the on-chain gross — that's
    /// [`crate::SimResult::gross_profit`]).
    pub engine_profit: u128,
    /// Per-hop rows (one allocation instead of the former three parallel
    /// `Vec`s).
    pub steps: Box<[SolveStep]>,
    /// `solve_block` — the block the solver produced the result on.
    pub solve_block: u64,
    /// `path_info` — the resolved path hops the deployment's Encode part encodes.
    pub path_info: PathInfo,
    /// The Encode decision a target-selection walk already took for this
    /// candidate ([`PreparedEncoding`]).
    ///
    /// `Some` means the deployment's `encode` callback has ALREADY run on this
    /// candidate's path, so simulation reuses that exact transaction instead of
    /// composing a second time. `None` is the explicit all-target fan-out's
    /// shape: the simulation leaf composes for its own deployment.
    pub prepared: Option<PreparedEncoding>,
}

impl DispatchCandidate {
    /// The solver's per-hop expected outputs as a flat `Vec` for the
    /// failure-telemetry intake (`FailBuckets::record` takes `Vec<u128>`);
    /// a failure-path-only view of the merged rows.
    #[must_use]
    pub fn expected_outputs_vec(&self) -> Vec<u128> {
        self.steps.iter().map(|s| s.output).collect()
    }

    /// Build the [`SimulatePath`] the fan-out hands to
    /// [`crate::simulate_path_on_evm`].
    #[must_use]
    pub(crate) fn to_simulate_path(&self) -> SimulatePath {
        SimulatePath {
            candidate_id: self.candidate_id,
            optimal_input: self.optimal_input,
            steps: self.steps.clone(),
            path_info: self.path_info.clone(),
            solve_block: self.solve_block,
        }
    }
}

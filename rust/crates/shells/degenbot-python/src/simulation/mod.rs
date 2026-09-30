//! `PyO3` seam over the `degenbot-arbitrage` core crate.
//!
//! The settlement-arbitrage strategy is the per-block profitability pipeline — it takes a
//! batch of solved arbitrage candidates, runs each through the in-process
//! revm sim (the engine's `BlockSimHandle` EVM driven by the strategy's
//! `simulate_path_on_evm`), classifies the outcome
//! (gas-profitable / unprofitable / revert), computes gross/net profit +
//! the market-aware age-decay priority fee, and hands the winners to the
//! submission seam. It is a pyo3-free core leaf (ADR-019 D4/D7, decision R —
//! the strategy stays in Rust; ADR-019 retired the legacy `eth_simulateV1`
//! RPC executor — the in-process revm path is the sole executor).
//!
//! ## Layering (ADR-005 / ADR-019 D7)
//!
//! - **Engine** (`degenbot-simulation`): the in-process revm EVM handle
//!   (`BlockSimHandle`, layered DB, overrides, AL collector, warm cache).
//!   Zero pyo3.
//! - **Strategy** (`degenbot-arbitrage`): the settlement-arbitrage bundle —
//!   `dispatch_profitable_results`, `SimResult`, `SimulateContext`,
//!   `DispatchCandidate`, `DispatchOutcome`, `FailBuckets`,
//!   `compute_priority_fee`, the 7-call `simulate_path_on_evm`. Zero pyo3.
//!   The Python driver is a thin cockpit over this — NOT a co-implementation
//!   (AGENTS.md).
//! - **`PyO3` wrapper** (this module): `#[pyclass]`/`#[pyfunction]` only —
//!   arg-extract → GIL release (`py.detach`) → strategy call → result wrap.
//!   No business logic. Mirrors the `submission/` subtree's discipline exactly.
//! - **Python companion** (`examples/eth_settlement_arbitrage_v2_v3_v4_rust.py`): the
//!   cockpit renders the `[sim]` summary from `PyDispatchOutcome` and chains
//!   `dispatch_profitable_py` → `dispatch_and_submit_py`.
//!
//! The seam's output is the submission seam's input shape: the wrapper joins
//! each surviving `SimResult` → `PySubmitCandidate` at result-wrap time, so the
//! cockpit chains simulate → submit with no field reshuffling.
//!
//! ## Surface
//!
//! Three pyclasses: [`PySimulateContext`] (the session-static config bag),
//! [`PyDispatchCandidate`] (the per-path builder), and [`PyDispatchOutcome`]
//! (the read-only result), plus the `dispatch_profitable_py` pyfunction.

use pyo3::prelude::*;

pub mod assembly;
pub mod batch;
pub mod candidate;
pub mod context;
pub mod dispatch;
pub mod in_process_probe;
mod inline_hook;
pub mod outcome;

pub use candidate::PyDispatchCandidate;
pub use context::PySimulateContext;
pub use outcome::PyDispatchOutcome;

/// The `degenbot._ffi.simulation` Python submodule (declarative
/// `#[pymodule]`), carrying the simulation pyclasses + the
/// dispatch/assembly/probe pyfunctions. The parent module registers the
/// submodule itself and its `sys.modules` entry.
#[pymodule(submodule)]
#[pyo3(module = "degenbot._ffi")]
pub mod simulation {
    #[pymodule_export]
    use super::{PyDispatchCandidate, PyDispatchOutcome, PySimulateContext};

    #[pymodule_export]
    use super::assembly::{assemble_dispatch_candidates_py, PyCandidateAssembly};

    #[pymodule_export]
    use super::batch::{
        build_batch_executor_py, executor_policy_py, ExecutorPolicyValues, PyAssemblyVerdict,
        PyBatchExecutor, PyBatchOutcome, PyBatchOutcomeSet, PyFailureDetail, PyFailureKind,
        PySimReceipt, PySimulateVerdict, PySubmitVerdict,
    };

    #[pymodule_export]
    use super::dispatch::{
        dispatch_profitable_py, merge_payload_results_py, PyPayloadOutcome, PyPayloadVerdict,
    };

    #[pymodule_export]
    use super::in_process_probe::{
        simulate_in_process_revert_probe, simulate_in_process_success_probe,
    };
}

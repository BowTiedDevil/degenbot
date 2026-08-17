//! `PyO3` seam over the `degenbot-settlement-strategy` core crate.
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
//! - **Strategy** (`degenbot-settlement-strategy`): the settlement-arbitrage bundle —
//!   `dispatch_profitable_results`, `SimResult`, `SimulateContext`,
//!   `DispatchCandidate`, `DispatchOutcome`, `FailBuckets`,
//!   `compute_priority_fee`, the 7-call `simulate_path_on_evm`. Zero pyo3.
//!   The Python driver is a thin cockpit over this — NOT a co-implementation
//!   (AGENTS.md).
//! - **`PyO3` wrapper** (this module): `#[pyclass]`/`#[pyfunction]` only —
//!   arg-extract → GIL release (`py.detach`) → strategy call → result wrap.
//!   No business logic. Mirrors the `submission/` subtree's discipline exactly.
//! - **Python companion** (`examples/eth_backrun_v2_v3_v4_rust.py`): the
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
use pyo3::types::PyModule;

pub mod candidate;
pub mod context;
pub mod dispatch;
pub mod in_process_probe;
pub mod outcome;

pub use candidate::PyDispatchCandidate;
pub use context::PySimulateContext;
pub use outcome::PyDispatchOutcome;

/// Register the simulation pyclasses + the `dispatch_profitable_py`
/// pyfunction on the module.
///
/// # Errors
///
/// Returns `PyErr` if a class or function fails to register on the module.
pub fn add_simulation_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.simulation")?;
    submod.add_class::<PySimulateContext>()?;
    submod.add_class::<PyDispatchCandidate>()?;
    submod.add_class::<PyDispatchOutcome>()?;
    submod.add_function(wrap_pyfunction!(
        crate::simulation::dispatch::dispatch_profitable_py,
        &submod
    )?)?;
    submod.add_function(wrap_pyfunction!(
        crate::simulation::in_process_probe::simulate_in_process_revert_probe,
        &submod
    )?)?;
    submod.add_function(wrap_pyfunction!(
        crate::simulation::in_process_probe::simulate_in_process_success_probe,
        &submod
    )?)?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.simulation", &submod)?;
    Ok(())
}

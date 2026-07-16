//! `PyO3` seam over the `degenbot-simulation` core crate.
//!
//! The simulation crate is the per-block profitability pipeline — it takes a
//! batch of solved arbitrage candidates, runs each through `eth_simulateV1`
//! (`simulate_one`), classifies the outcome (gas-profitable / unprofitable /
//! revert), computes gross/net profit + the market-aware age-decay priority
//! fee, and hands the winners to the submission seam. It is a pyo3-free core
//! leaf (3660 lines, six modules) that ports the Python `dispatch_profitable`
//! → `simulate_one` chain wholesale.
//!
//! ## Layering (ADR-005)
//!
//! - **Core** (`degenbot-simulation`): the typed pipeline — `simulate_one`,
//!   `dispatch_profitable_results`, `SimResult`, `SimulateContext`,
//!   `DispatchCandidate`, `DispatchOutcome`, `FailBuckets`. Zero pyo3.
//! - **`PyO3` wrapper** (this module): `#[pyclass]`/`#[pyfunction]` only —
//!   arg-extract → GIL release (`py.detach`) → core call → result wrap. No
//!   business logic. Mirrors the `submission/` subtree's discipline exactly.
//! - **Python companion** (`examples/eth_backrun_v2_v3_v4_rust.py`): the
//!   cockpit renders the `[sim]` summary from `PyDispatchOutcome` and chains
//!   `dispatch_profitable_py` → `dispatch_and_submit_py`.
//!
//! The seam's output is the submission seam's input shape: the wrapper joins
//! each surviving `SimResult` → `PySubmitCandidate` at result-wrap time, so the
//! cockpit chains simulate → submit with no field reshuffling.
//!
//! ## A2 scope (`TCZ47Z`)
//!
//! Three pyclasses land here: [`PySimulateContext`] (the session-static config
//! bag), [`PyDispatchCandidate`] (the per-path builder), and
//! [`PyDispatchOutcome`] (the read-only result). The
//! `dispatch_profitable_py` pyfunction (A4, `QQFTB4`) is not yet registered —
//! it depends on the A3 `PathSuppression` extraction.

use pyo3::prelude::*;
use pyo3::types::PyModule;

pub mod candidate;
pub mod context;
pub mod dispatch;
pub mod outcome;

pub use candidate::PyDispatchCandidate;
pub use context::PySimulateContext;
pub use outcome::PyDispatchOutcome;

/// Register the simulation pyclasses on the module.
///
/// A2 registers the three pyclasses. A4 (`QQFTB4`) will additionally register
/// the `dispatch_profitable_py` pyfunction.
///
/// # Errors
///
/// Returns `PyErr` if a class fails to register on the module.
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
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.simulation", &submod)?;
    Ok(())
}

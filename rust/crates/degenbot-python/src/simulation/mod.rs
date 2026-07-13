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
//! This module is the **A1 skeleton** (ergo `7P4AKF`): the Cargo dependency +
//! module registration land here, with no symbols yet. The pyclasses
//! (`PySimulateContext` / `PyDispatchCandidate` / `PyDispatchOutcome` /
//! internal `PySimResult`) land in A2 (`TCZ47Z`), and the
//! `dispatch_profitable_py` pyfunction lands in A4 (`QQFTB4`).
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
//! # Errors
//!
//! `add_simulation_module` is a no-op registration stub until A2/A4 land.
//! Returns `Ok(())` unconditionally.

use pyo3::prelude::*;

/// Register the simulation pyclasses + `dispatch_profitable_py` pyfunction on
/// the module.
///
/// **A1 skeleton** — no symbols to register yet. The body fills in as A2
/// (pyclasses) and A4 (`dispatch_profitable_py`) land. Keeping the function
/// in place now lets `c_api::register` wire the module behind the
/// `simulation` feature gate without waiting on the seam implementation.
///
/// # Errors
///
/// Returns `PyErr` once symbols are added (none today).
#[allow(clippy::unnecessary_wraps)]
pub fn add_simulation_module(_m: &Bound<'_, PyModule>) -> PyResult<()> {
    // A2 registers: PySimulateContext, PyDispatchCandidate, PyDispatchOutcome.
    // A4 registers: dispatch_profitable_py.
    Ok(())
}

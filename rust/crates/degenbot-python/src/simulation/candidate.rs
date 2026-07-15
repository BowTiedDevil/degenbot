//! `PyDispatchCandidate` — the builder the cockpit constructs from each solved
//! arb path before handing the batch to `dispatch_profitable_py` (A4).
//!
//! Mirrors [`PySubmitCandidate`]'s `#[new]` shape (a builder pyclass holding
//! the core [`DispatchCandidate`]). The `path_info` arrives as the Python
//! `PathInfo` dataclass + is extracted to the Rust [`PathInfo`] via the SAME
//! [`crate::executor::extract_path_info`] the encode seam uses — DRY: the
//! candidate the cockpit hands to `dispatch_profitable_py` must encode
//! byte-for-byte identically to what `encode_cmd_stream` would produce.
//!
//! The `EncodeOptions` (`erc6909_profit` / `use_v4_batch`) come in as bool
//! kw-flags (mirrors the encode seam's signature).
//!
//! # GIL discipline
//!
//! `#[new]` runs under the GIL (it must iterate the Python `hops` list +
//! dispatch `is_instance` on each hop). The A4 pyfunction then extracts the
//! held `DispatchCandidate` (clone) into the async block + releases the GIL.

use crate::executor::{extract_path_info, HopTypes};
use crate::prelude::*;
use degenbot_executor::composers::{EncodeOptions, PathInfo};
use degenbot_simulation::dispatch_profitable::DispatchCandidate;

/// The pre-simulation candidate builder — the engine result + the resolved
/// `PathInfo` + the encode options.
///
/// Ports the Python `EngineResult` tuple `(path_id, opt_input, profit,
/// hop_outputs, solve_block)` + the `engine_registry.paths.get(path_id)`
/// `PathInfo` resolve. Construct one per solved path, per block.
#[pyclass(
    name = "PyDispatchCandidate",
    skip_from_py_object,
    module = "degenbot._ffi"
)]
// `inner` is read by `dispatch_profitable_py` (A4, QQFTB4) — not yet landed;
// the field is dead until then.
#[allow(dead_code)]
pub struct PyDispatchCandidate {
    pub(crate) inner: DispatchCandidate,
}

#[pymethods]
impl PyDispatchCandidate {
    /// Build a candidate from the engine result + the resolved path info.
    ///
    /// Args:
    ///     `path_id`: the unique arb path identifier.
    ///     `optimal_input`: the solver's optimal swap input (u128).
    ///     `engine_profit`: the solver's expected gross profit (u128) — used
    ///         for sorting + the thin-margin filter; NOT the on-chain gross
    ///         (that's `SimResult::gross_profit`).
    ///     `hop_outputs`: the per-hop solver outputs (`list[int]`).
    ///     `solve_block`: the block the solver produced the result on.
    ///     `path_info`: the Python `PathInfo` dataclass (`hops` list of
    ///         `V2HopInfo`/`V3HopInfo`/`V4HopInfo`).
    ///     `erc6909_profit`: encode the V4 profit as an ERC6909 transfer
    ///         (default `False`).
    ///     `use_v4_batch`: encode V4 hops as a batched `unlock`-callback
    ///         (default `False`).
    ///
    /// # Errors
    /// `TypeError`: if `path_info` is not a `PathInfo` or a hop is not
    ///         `V2HopInfo`/`V3HopInfo`/`V4HopInfo`.
    #[new]
    #[pyo3(signature = (path_id, optimal_input, engine_profit, hop_outputs, solve_block, path_info, *, erc6909_profit=false, use_v4_batch=false))]
    #[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
    fn new(
        py: Python<'_>,
        path_id: u64,
        optimal_input: u128,
        engine_profit: u128,
        hop_outputs: Vec<u128>,
        solve_block: u64,
        path_info: &Bound<'_, PyAny>,
        erc6909_profit: bool,
        use_v4_batch: bool,
    ) -> PyResult<Self> {
        let types = HopTypes::load(py)?;
        let rust_path: PathInfo = extract_path_info(path_info, &types)?;
        let opts = EncodeOptions {
            erc6909_profit,
            use_v4_batch,
        };
        Ok(Self {
            inner: DispatchCandidate {
                path_id,
                optimal_input,
                engine_profit,
                hop_outputs,
                solve_block,
                path_info: rust_path,
                opts,
            },
        })
    }
}

//! The render-wire dict shapes the FFI seam hands the Python driver.
//!
//! One serializer per record a Python renderer reads: the hop/path projection
//! the batch seam's `path_info_dict()` emits (the `[profit]` hop-detail log's
//! input) and the failure record the batch seam's `FailureDetail.record()`
//! emits (the `[sim-fail]`/`[sim-diag]` renderers' input, with its
//! captured-swap rows). Every serializer builds a plain `dict`, so the driver
//! reads named keys — no pyclass and no dataclass reconstruction crosses the
//! boundary.

use crate::prelude::*;
use alloy::primitives::U256;
use degenbot_arbitrage::{CapturedSwap, SimFailure};
use degenbot_executor::composers::{HopInfo, PathInfo};
use pyo3::types::{PyDict, PyList};

/// Build a plain-Python-dict view of a `PathInfo` (the render shape).
///
/// `path_type` is the combined pool-type label (`"V2-V3"`, `"V4-V2"`, …);
/// `hops` is a list of per-hop dicts built by [`hop_to_py_dict`]. No Python
/// dataclass is reconstructed — the cockpit reads the dict fields directly.
pub(crate) fn path_info_to_py_dict<'py>(
    py: Python<'py>,
    path: &PathInfo,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    let mut type_names: Vec<&'static str> = Vec::with_capacity(path.hops.len());
    let hops_list = PyList::empty(py);
    for hop in &path.hops {
        let (family, hop_dict) = hop_to_py_dict(py, hop)?;
        type_names.push(family);
        hops_list.append(hop_dict)?;
    }
    dict.set_item("path_type", type_names.join("-"))?;
    dict.set_item("hops", hops_list)?;
    Ok(dict)
}

/// Build a plain-Python-dict view of a `HopInfo` (the per-hop render shape).
///
/// Returns the `family` discriminator alongside the dict so the caller
/// (`path_info_to_py_dict`) can assemble the `path_type` label without reading
/// the dict back. The dict carries that variant's display fields; addresses
/// are EIP-55 checksummed strings (alloy `Address::Display`).
fn hop_to_py_dict<'py>(
    py: Python<'py>,
    hop: &HopInfo,
) -> PyResult<(&'static str, Bound<'py, PyDict>)> {
    let dict = PyDict::new(py);
    let family = match hop {
        HopInfo::V2(v2) => {
            dict.set_item("family", "V2")?;
            dict.set_item("pool_address", format!("{}", v2.pool_address))?;
            dict.set_item("token0_address", format!("{}", v2.token0_address))?;
            dict.set_item("token1_address", format!("{}", v2.token1_address))?;
            dict.set_item("fee", v2.fee)?;
            dict.set_item("zfo", v2.zfo)?;
            "V2"
        }
        HopInfo::V3(v3) => {
            dict.set_item("family", "V3")?;
            dict.set_item("pool_address", format!("{}", v3.pool_address))?;
            dict.set_item("token0_address", format!("{}", v3.token0_address))?;
            dict.set_item("token1_address", format!("{}", v3.token1_address))?;
            dict.set_item("fee", v3.fee)?;
            dict.set_item("zfo", v3.zfo)?;
            "V3"
        }
        HopInfo::V4(v4) => {
            dict.set_item("family", "V4")?;
            dict.set_item(
                "pool_manager_address",
                format!("{}", v4.pool_manager_address),
            )?;
            dict.set_item("pool_id_hex", v4.pool_id_hex.clone())?;
            dict.set_item("currency0_address", format!("{}", v4.currency0_address))?;
            dict.set_item("currency1_address", format!("{}", v4.currency1_address))?;
            dict.set_item("fee", v4.fee)?;
            dict.set_item("tick_spacing", v4.tick_spacing)?;
            dict.set_item("hook_address", format!("{}", v4.hook_address))?;
            dict.set_item("zfo", v4.zfo)?;
            "V4"
        }
    };
    Ok((family, dict))
}

/// Build the per-failure render dict for one `SimFailure` — the ONE
/// failure-record shape the batch-executor seam's `FailureDetail.record()`
/// emits, so the Python `[sim-fail]` renderers see that shape on every entry
/// arm.
pub(crate) fn sim_failure_to_dict<'py>(
    py: Python<'py>,
    f: &SimFailure,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("path_id", f.path_id)?;
    dict.set_item("bucket", &f.bucket)?;
    match f.fail_index {
        Some(idx) => dict.set_item("fail_index", idx)?,
        None => dict.set_item("fail_index", py.None())?,
    }
    let hex_str = alloy::hex::encode_prefixed(&f.revert_data);
    dict.set_item("revert_data", hex_str)?;
    match &f.reverting_frame {
        Some(rf) => {
            let rdict = PyDict::new(py);
            rdict.set_item("depth", rf.depth)?;
            rdict.set_item("target", format!("{:#x}", rf.target))?;
            rdict.set_item(
                "selector",
                format!("0x{}", alloy::primitives::hex::encode(rf.selector)),
            )?;
            rdict.set_item("revert_data", alloy::hex::encode_prefixed(&rf.revert_data))?;
            rdict.set_item("label", &rf.label)?;
            rdict.set_item("outcome_kind", rf.outcome_kind)?;
            rdict.set_item("gas_used", rf.gas_used)?;
            dict.set_item("reverting_frame", rdict)?;
        }
        None => dict.set_item("reverting_frame", py.None())?,
    }
    // The captured swaps (before the revert) — per-swap dicts.
    let swaps_list = PyList::empty(py);
    for s in &f.captured_swaps {
        swaps_list.append(captured_swap_to_dict(py, s)?)?;
    }
    dict.set_item("captured_swaps", swaps_list)?;
    dict.set_item("log_full_count", f.log_full_count)?;
    // Swaps emitted inside REVERTED frames (frame-misclassification diag).
    let rsw = PyList::empty(py);
    for s in &f.reverted_swaps {
        rsw.append(captured_swap_to_dict(py, s)?)?;
    }
    dict.set_item("reverted_swaps", rsw)?;
    // Compact per-frame EVM call-trace summary (no-profit diagnostic).
    let ct_list = PyList::empty(py);
    for s in &f.call_trace {
        ct_list.append(s)?;
    }
    dict.set_item("call_trace", ct_list)?;
    dict.set_item("weth_before", f.weth_before)?;
    dict.set_item("weth_after", f.weth_after)?;
    dict.set_item("eth_before", f.eth_before)?;
    dict.set_item("eth_after", f.eth_after)?;
    dict.set_item("erc6909_before", f.erc6909_before)?;
    dict.set_item("erc6909_after", f.erc6909_after)?;
    // The solver's expected amounts — the [sim-diag] classifier's
    // EXPECTED half (the ACTUAL half is `captured_swaps`). The gap
    // between `hop_outputs[i]` and the i-th captured swap's amount
    // is the new SolverCalc basis (replaces the deleted recompute).
    // `optimal_input` is the solver's expected input (context for the
    // expected-vs-actual render).
    let optimal_input = alloy_py::u256_to_py(py, &U256::from(f.optimal_input))?;
    dict.set_item("optimal_input", optimal_input)?;
    let hop_outputs_list = PyList::empty(py);
    for ho in &f.hop_outputs {
        let v = alloy_py::u256_to_py(py, &U256::from(*ho))?;
        hop_outputs_list.append(v)?;
    }
    dict.set_item("hop_outputs", hop_outputs_list)?;
    Ok(dict)
}

/// Build the per-swap dict for a captured swap
/// (family/emitter/amount0/amount1/`sqrt_price_x96`/liquidity/tick) — the
/// shape every captured-swap row carries (a failure's `captured_swaps` /
/// `reverted_swaps`, a survivor's capture), so the Python consumer sees one
/// captured-swap dict shape regardless of which arm produced it.
pub(crate) fn captured_swap_to_dict<'py>(
    py: Python<'py>,
    s: &CapturedSwap,
) -> PyResult<Bound<'py, PyDict>> {
    let sdict = PyDict::new(py);
    sdict.set_item("family", format!("{:?}", s.family).to_lowercase())?;
    sdict.set_item("emitter", format!("{:#x}", s.emitter))?;
    let amount0 = alloy_py::i256_to_py(py, &s.amount0)?;
    sdict.set_item("amount0", amount0)?;
    let amount1 = alloy_py::i256_to_py(py, &s.amount1)?;
    sdict.set_item("amount1", amount1)?;
    let sqrt_price = alloy_py::u256_to_py(py, &s.sqrt_price_x96)?;
    sdict.set_item("sqrt_price_x96", sqrt_price)?;
    let liquidity = alloy_py::u256_to_py(py, &s.liquidity)?;
    sdict.set_item("liquidity", liquidity)?;
    sdict.set_item("tick", s.tick)?;
    Ok(sdict)
}

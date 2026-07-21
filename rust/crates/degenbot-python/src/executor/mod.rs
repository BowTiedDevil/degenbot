//! `PyO3` seam for the `degenbot_executor` core crate (feature = `"executor"`).
//!
//! Thin `#[pyfunction]` wrappers over the Rust command-stream encoding core:
//! [`compute_simulation_warmup_slots`], [`pack_config`],
//! [`pack_expected_balance`], [`mapping_slot`], [`nested_mapping_slot`].
//!
//! Architecture (ADR-005 §3.2): each wrapper extracts Python args → releases
//! the GIL via `py.detach()` for the encode/warmup compute → calls the core →
//! wraps the result into `bytes`/`dict`/`int`. No business logic lives here.
//!
//! WEFVGE: the standalone `encode_cmd_stream` / `v4_input_is_native` /
//! `v4_output_is_native` `PyO3` pyfunctions + their `HopTypes` /
//! `extract_hop` / `extract_path_info` / `hop_to_py` / `path_info_to_py`
//! helpers are RETIRED. The encode path moved to the Rust core
//! (`dispatch_profitable_py` calls `composers::encode_cmd_stream` internally —
//! A5 "now called internally by the seam, not from the example"), and the
//! candidate resolves its `composers::PathInfo` from a registered `path_id`
//! via `PyArbitrageEngine::path_info_for_core` (NXM2BF). The `[profit]`
//! hop-detail render reads `outcome.path_infos` as plain `dict`s (built in
//! `simulation/outcome.rs`). The Python `hop_info` dataclasses are deleted.
//! No Python caller reaches `encode_cmd_stream` / `v4_*` on `degenbot_rs`
//! anymore — the §4.5 `_DelegateSpy` test pinned them as Rust-bound builtins,
//! but the example never invoked them post-A5.

use alloy::primitives::U256;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyInt, PyModule};

use degenbot_executor::config::{self, ConfigError};
use degenbot_executor::{
    compute_simulation_warmup_slots as core_warmup_slots, mapping_slot as core_mapping_slot,
    nested_mapping_slot as core_nested_mapping_slot,
};

use crate::address_utils::{parse_address, to_checksum_address_str};
use crate::conversion::alloy::u256_to_py;

// ═══════════════════════════════════════════════════════════════════════════
// Python → Rust arg extraction
// ═══════════════════════════════════════════════════════════════════════════

/// Extract a `U256` from a Python int-like object (`int`, `float`, `str`).
fn extract_u256(obj: &Bound<'_, PyAny>) -> PyResult<U256> {
    if let Ok(i) = obj.extract::<i128>() {
        if i < 0 {
            return Err(PyValueError::new_err("negative value for U256"));
        }
        return U256::try_from(i).map_err(|e| PyValueError::new_err(e.to_string()));
    }
    // Fallback: PyInt → BigInt path via from_str_radix.
    if obj.is_instance_of::<PyInt>() {
        let s_obj = obj.str()?;
        let s = s_obj.to_str()?.trim();
        if let Some(hex) = s.strip_prefix("0x") {
            return U256::from_str_radix(hex, 16).map_err(|e| PyValueError::new_err(e.to_string()));
        }
        return U256::from_str_radix(s, 10).map_err(|e| PyValueError::new_err(e.to_string()));
    }
    // String: try hex (0x…) then decimal.
    if let Ok(s) = obj.extract::<String>() {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix("0x") {
            return U256::from_str_radix(hex, 16).map_err(|e| PyValueError::new_err(e.to_string()));
        }
        return U256::from_str_radix(s, 10).map_err(|e| PyValueError::new_err(e.to_string()));
    }
    Err(PyTypeError::new_err("expected int/str for U256"))
}

// ═══════════════════════════════════════════════════════════════════════════
// PyO3 wrapper functions
// ═══════════════════════════════════════════════════════════════════════════

/// `degenbot_rs.compute_simulation_warmup_slots(executor_address,
/// weth_address, pool_manager_address) -> dict`
///
/// Compute the `eth_simulateV1` `stateDiff` overrides replicating
/// `cmd_executor.initialize()`'s three warmed storage slots (WETH9
/// `balanceOf(executor)`, `PoolManager` ERC6909 `balanceOf(executor, weth_id)`,
/// `PoolManager` ERC6909 `balanceOf(executor, native_id)`). Each is set to 1 wei
/// to warm the slot and avoid cold-access gas penalties.
#[pyfunction]
#[allow(clippy::needless_pass_by_value)]
fn compute_simulation_warmup_slots<'py>(
    py: Python<'py>,
    executor_address: &str,
    weth_address: &str,
    pool_manager_address: &str,
) -> PyResult<Bound<'py, PyDict>> {
    let executor = parse_address(executor_address)
        .map_err(|e| PyValueError::new_err(format!("Invalid executor address: {e}")))?;
    let weth = parse_address(weth_address)
        .map_err(|e| PyValueError::new_err(format!("Invalid weth address: {e}")))?;
    let pm = parse_address(pool_manager_address)
        .map_err(|e| PyValueError::new_err(format!("Invalid pool manager address: {e}")))?;

    let slots = py.detach(|| core_warmup_slots(executor, weth, pm));

    let weth_addr_str =
        to_checksum_address_str(weth_address).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let pm_addr_str = to_checksum_address_str(pool_manager_address)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    let executor_addr_str = to_checksum_address_str(executor_address)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;

    let one_wei_hex = "0x0000000000000000000000000000000000000000000000000000000000000001";
    let weth_slot_hex = format!("0x{:064x}", slots.weth_balance);
    let erc6909_weth_slot_hex = format!("0x{:064x}", slots.erc6909_weth);
    let erc6909_native_slot_hex = format!("0x{:064x}", slots.erc6909_native);

    let dict = PyDict::new(py);

    // WETH9 entry
    let weth_entry = PyDict::new(py);
    weth_entry.set_item("stateDiff", {
        let sd = PyDict::new(py);
        sd.set_item(&weth_slot_hex, one_wei_hex)?;
        sd
    })?;
    dict.set_item(&weth_addr_str, weth_entry)?;

    // PoolManager entry (two ERC6909 slots)
    let pm_entry = PyDict::new(py);
    pm_entry.set_item("stateDiff", {
        let sd = PyDict::new(py);
        sd.set_item(&erc6909_weth_slot_hex, one_wei_hex)?;
        sd.set_item(&erc6909_native_slot_hex, one_wei_hex)?;
        sd
    })?;
    dict.set_item(&pm_addr_str, pm_entry)?;

    // Executor entry: residual balance (0)
    let exec_entry = PyDict::new(py);
    exec_entry.set_item("balance", "0x0")?;
    dict.set_item(&executor_addr_str, exec_entry)?;

    Ok(dict)
}

/// `degenbot_rs.pack_config(check_mode=0, expected_value=0, bribe_bips=0,
/// bribe_recipient_idx=0) -> int`
///
/// Pack the `execute(commands, config)` ABI `config` `uint256`. Thin wrapper
/// over [`config::pack_config`].
#[pyfunction]
#[pyo3(signature = (check_mode, expected_value, *, bribe_bips=0, bribe_recipient_idx=0))]
fn pack_config<'py>(
    py: Python<'py>,
    check_mode: u8,
    expected_value: &Bound<'_, PyAny>,
    bribe_bips: u32,
    bribe_recipient_idx: u8,
) -> PyResult<Bound<'py, PyAny>> {
    let ev = extract_u256(expected_value)?;
    let result = py
        .detach(|| config::pack_config(check_mode, ev, bribe_bips, bribe_recipient_idx))
        .map_err(config_err_to_py)?;
    u256_to_py(py, &result)
}

/// `degenbot_rs.pack_expected_balance(check_mode, expected_value) -> int`
///
/// Deprecated alias for [`pack_config`] with `bribe_bips=0` /
/// `bribe_recipient_idx=0`. Thin wrapper over [`config::pack_expected_balance`].
#[pyfunction]
fn pack_expected_balance<'py>(
    py: Python<'py>,
    check_mode: u8,
    expected_value: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let ev = extract_u256(expected_value)?;
    let result = py
        .detach(|| config::pack_expected_balance(check_mode, ev))
        .map_err(config_err_to_py)?;
    u256_to_py(py, &result)
}

/// `degenbot_rs.mapping_slot(base_slot, key) -> int`
///
/// Compute a Solidity mapping storage slot: `keccak256(pad(key,32) || pad(base,32))`.
/// Thin wrapper over [`core_mapping_slot`] (the Rust warmup-slot leaf).
#[pyfunction]
fn mapping_slot<'py>(
    py: Python<'py>,
    base_slot: &Bound<'_, PyAny>,
    key: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let base = extract_u256(base_slot)?;
    let k = extract_u256(key)?;
    let result = py.detach(|| core_mapping_slot(base, k));
    u256_to_py(py, &result)
}

/// `degenbot_rs.nested_mapping_slot(base_slot, key1, key2) -> int`
///
/// Compute a nested Solidity mapping storage slot:
/// `keccak256(pad(key2,32) || keccak256(pad(key1,32) || pad(base,32)))`.
/// Thin wrapper over [`core_nested_mapping_slot`].
#[pyfunction]
fn nested_mapping_slot<'py>(
    py: Python<'py>,
    base_slot: &Bound<'_, PyAny>,
    key1: &Bound<'_, PyAny>,
    key2: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let base = extract_u256(base_slot)?;
    let k1 = extract_u256(key1)?;
    let k2 = extract_u256(key2)?;
    let result = py.detach(|| core_nested_mapping_slot(base, k1, k2));
    u256_to_py(py, &result)
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Map a [`ConfigError`] to a Python `ValueError`.
fn config_err_to_py(err: ConfigError) -> PyErr {
    PyValueError::new_err(err.to_string())
}

// ═══════════════════════════════════════════════════════════════════════════
// Module registration
// ═══════════════════════════════════════════════════════════════════════════

/// Register the executor seam functions on `m` (feature = "executor").
///
/// # Errors
///
/// Returns a [`PyErr`] if any `add_function` call fails (e.g. a name
/// collision); propagated unchanged to the `#[pymodule]` caller.
pub fn add_executor_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.executor")?;
    submod.add_function(wrap_pyfunction!(compute_simulation_warmup_slots, &submod)?)?;
    submod.add_function(wrap_pyfunction!(pack_config, &submod)?)?;
    submod.add_function(wrap_pyfunction!(pack_expected_balance, &submod)?)?;
    submod.add_function(wrap_pyfunction!(mapping_slot, &submod)?)?;
    submod.add_function(wrap_pyfunction!(nested_mapping_slot, &submod)?)?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.executor", &submod)?;
    Ok(())
}

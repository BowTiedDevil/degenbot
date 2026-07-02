//! `PyO3` seam for the `degenbot_executor` core crate (feature = `"executor"`).
//!
//! Thin `#[pyfunction]` wrappers over the Rust command-stream encoding core:
//! [`encode_cmd_stream`], [`compute_simulation_warmup_slots`], [`pack_config`],
//! [`pack_expected_balance`], [`mapping_slot`], [`nested_mapping_slot`],
//! [`v4_input_is_native`], [`v4_output_is_native`].
//!
//! Architecture (ADR-005 §3.2): each wrapper extracts Python args → releases
//! the GIL via `py.detach()` for the encode/warmup compute → calls the core →
//! wraps the result into `bytes`/`dict`/`int`. No business logic lives here.
//!
//! The Python encoder (`examples/eth_backrun_helpers.py::encode_cmd_stream` +
//! `examples/cmd_stream.py`) was deleted in the §4.3 oracle-retirement
//! cutover after byte-for-byte parity (§4.2) passed 80/80 cases. The frozen
//! `.rs` golden-file tests (`cargo test -p degenbot-executor`, 59 fns) pin
//! the Rust core output. The §4.5 `_DelegateSpy` test
//! (`tests/rust-seam/test_executor_delegate_spy.py`) proves the example
//! routes through `degenbot_rs.*` (Rust-bound builtins), not deleted Python.

use alloy::primitives::{Address, U256};
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyInt, PyModule, PyType};

use degenbot_executor::composers::{
    self, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
};
use degenbot_executor::config::{self, ConfigError};
use degenbot_executor::{
    compute_simulation_warmup_slots as core_warmup_slots, mapping_slot as core_mapping_slot,
    nested_mapping_slot as core_nested_mapping_slot,
};

use crate::address_utils::{parse_address, to_checksum_address_str};
use crate::conversion::alloy::u256_to_py;

// ═══════════════════════════════════════════════════════════════════════════
// Python → Rust HopInfo extraction
// ═══════════════════════════════════════════════════════════════════════════

/// Cached references to the three Python [`HopInfo`] dataclass [`PyType`] objects.
///
/// Built once via `PyModule::import("degenbot.arbitrage.hop_info")` and
/// `isinstance`-checked to dispatch a Python hop dataclass to the right Rust
/// `HopInfo` variant. Mirrors the polars-python pattern for lazy conversion
/// of arbitrary Python objects.
struct HopTypes {
    v2: Py<PyType>,
    v3: Py<PyType>,
    v4: Py<PyType>,
}

impl HopTypes {
    fn load(py: Python<'_>) -> PyResult<Self> {
        let module = PyModule::import(py, "degenbot.arbitrage.hop_info")?;
        Ok(Self {
            v2: module.getattr("V2HopInfo")?.extract()?,
            v3: module.getattr("V3HopInfo")?.extract()?,
            v4: module.getattr("V4HopInfo")?.extract()?,
        })
    }
}

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

/// Extract an `Address` from a Python object (hex string or bytes).
fn extract_address(obj: &Bound<'_, PyAny>) -> PyResult<Address> {
    if let Ok(s) = obj.extract::<String>() {
        return parse_address(&s).map_err(|e| PyValueError::new_err(e.to_string()));
    }
    if let Ok(b) = obj.extract::<Vec<u8>>() {
        if b.len() != 20 {
            return Err(PyValueError::new_err(format!(
                "address must be 20 bytes, got {}",
                b.len()
            )));
        }
        return Ok(Address::from_slice(&b));
    }
    Err(PyTypeError::new_err("expected str/bytes for address"))
}

/// Extract a Rust `HopInfo` from a Python `V2HopInfo`/`V3HopInfo`/`V4HopInfo`
/// dataclass instance.
///
/// Dispatches on `isinstance` to the live Python types from
/// `degenbot.arbitrage.hop_info`, extracting the fields each variant needs.
fn extract_hop(hop: &Bound<'_, PyAny>, types: &HopTypes) -> PyResult<HopInfo> {
    let py = hop.py();
    if hop.is_instance(types.v2.bind(py))? {
        return Ok(HopInfo::V2(V2HopInfo {
            pool_address: extract_address(&hop.getattr("pool_address")?)?,
            token0_address: extract_address(&hop.getattr("token0_address")?)?,
            token1_address: extract_address(&hop.getattr("token1_address")?)?,
            fee: hop.getattr("fee")?.extract::<u16>()?,
            zfo: hop.getattr("zfo")?.extract::<bool>()?,
        }));
    }
    if hop.is_instance(types.v3.bind(py))? {
        return Ok(HopInfo::V3(V3HopInfo {
            pool_address: extract_address(&hop.getattr("pool_address")?)?,
            token0_address: extract_address(&hop.getattr("token0_address")?)?,
            token1_address: extract_address(&hop.getattr("token1_address")?)?,
            fee: hop.getattr("fee")?.extract::<u32>()?,
            zfo: hop.getattr("zfo")?.extract::<bool>()?,
        }));
    }
    if hop.is_instance(types.v4.bind(py))? {
        return Ok(HopInfo::V4(V4HopInfo {
            pool_manager_address: extract_address(&hop.getattr("pool_manager_address")?)?,
            pool_id_hex: hop.getattr("pool_id_hex")?.extract::<String>()?,
            currency0_address: extract_address(&hop.getattr("currency0_address")?)?,
            currency1_address: extract_address(&hop.getattr("currency1_address")?)?,
            fee: hop.getattr("fee")?.extract::<u32>()?,
            tick_spacing: hop.getattr("tick_spacing")?.extract::<i32>()?,
            hook_address: extract_address(&hop.getattr("hook_address")?)?,
            zfo: hop.getattr("zfo")?.extract::<bool>()?,
        }));
    }
    Err(PyTypeError::new_err(
        "hop must be V2HopInfo/V3HopInfo/V4HopInfo",
    ))
}

/// Extract a Rust `PathInfo` from a Python `PathInfo` dataclass, iterating
/// its `hops` list and dispatching each via [`extract_hop`].
fn extract_path_info(path_info: &Bound<'_, PyAny>, types: &HopTypes) -> PyResult<PathInfo> {
    let hops_iter = path_info.getattr("hops")?.try_iter()?;
    let mut hops = Vec::new();
    for hop in hops_iter {
        hops.push(extract_hop(&hop?, types)?);
    }
    Ok(PathInfo::new(hops))
}

// ═══════════════════════════════════════════════════════════════════════════
// PyO3 wrapper functions
// ═══════════════════════════════════════════════════════════════════════════

/// `degenbot_rs.encode_cmd_stream(path_info, optimal_input, hop_outputs,
/// executor_address, pool_manager_address, weth_address, *,
/// erc6909_profit=False, use_v4_batch=False) -> bytes | None`
///
/// Encode an arbitrage path as a `cmd_executor` command stream. Thin wrapper
/// over [`composers::encode_cmd_stream`] — extracts the Python `PathInfo` →
/// Rust `PathInfo`, releases the GIL for the encode compute, wraps the result
/// as `bytes` (or `None` if the path type doesn't encode).
#[pyfunction]
#[pyo3(signature = (
    path_info, optimal_input, hop_outputs, executor_address, pool_manager_address, weth_address,
    *, erc6909_profit=false, use_v4_batch=false
))]
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn encode_cmd_stream<'py>(
    py: Python<'py>,
    path_info: &Bound<'_, PyAny>,
    optimal_input: u128,
    hop_outputs: Vec<u128>,
    executor_address: &str,
    pool_manager_address: &str,
    weth_address: &str,
    erc6909_profit: bool,
    use_v4_batch: bool,
) -> PyResult<Bound<'py, PyAny>> {
    // `u128` is non-negative by construction; the `== 0` guard mirrors the
    // Python oracle's `any(x <= 0 for x in hop_outputs)`. The composers' own
    // `contains(&0)` guard repeats it defensively.
    if hop_outputs.contains(&0) {
        return Ok(py.None().into_bound(py));
    }

    let types = HopTypes::load(py)?;
    let rust_path = extract_path_info(path_info, &types)?;
    let executor = parse_address(executor_address)
        .map_err(|e| PyValueError::new_err(format!("Invalid executor address: {e}")))?;
    let pm = parse_address(pool_manager_address)
        .map_err(|e| PyValueError::new_err(format!("Invalid pool manager address: {e}")))?;
    let weth = parse_address(weth_address)
        .map_err(|e| PyValueError::new_err(format!("Invalid weth address: {e}")))?;
    let opts = EncodeOptions {
        erc6909_profit,
        use_v4_batch,
    };

    // GIL released for the encode compute (no Python access in the core).
    let result = py.detach(|| {
        composers::encode_cmd_stream(
            &rust_path,
            optimal_input,
            &hop_outputs,
            executor,
            pm,
            weth,
            opts,
        )
    });
    Ok(match result {
        Some(bytes) => {
            let b = PyBytes::new(py, &bytes);
            b.into_any()
        }
        None => py.None().into_bound(py),
    })
}

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

/// `degenbot_rs.v4_input_is_native(hop) -> bool`
///
/// True if the V4 hop's input currency is native ETH (address(0)). Thin
/// wrapper over [`composers::v4_input_is_native`].
#[pyfunction]
fn v4_input_is_native(py: Python<'_>, hop: &Bound<'_, PyAny>) -> PyResult<bool> {
    let types = HopTypes::load(py)?;
    let rust_hop = extract_hop(hop, &types)?;
    Ok(match rust_hop {
        HopInfo::V4(ref h) => py.detach(|| composers::v4_input_is_native(h)),
        _ => false,
    })
}

/// `degenbot_rs.v4_output_is_native(hop) -> bool`
///
/// True if the V4 hop's output currency is native ETH (address(0)). Thin
/// wrapper over [`composers::v4_output_is_native`].
#[pyfunction]
fn v4_output_is_native(py: Python<'_>, hop: &Bound<'_, PyAny>) -> PyResult<bool> {
    let types = HopTypes::load(py)?;
    let rust_hop = extract_hop(hop, &types)?;
    Ok(match rust_hop {
        HopInfo::V4(ref h) => py.detach(|| composers::v4_output_is_native(h)),
        _ => false,
    })
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
    m.add_function(wrap_pyfunction!(encode_cmd_stream, m)?)?;
    m.add_function(wrap_pyfunction!(compute_simulation_warmup_slots, m)?)?;
    m.add_function(wrap_pyfunction!(pack_config, m)?)?;
    m.add_function(wrap_pyfunction!(pack_expected_balance, m)?)?;
    m.add_function(wrap_pyfunction!(mapping_slot, m)?)?;
    m.add_function(wrap_pyfunction!(nested_mapping_slot, m)?)?;
    m.add_function(wrap_pyfunction!(v4_input_is_native, m)?)?;
    m.add_function(wrap_pyfunction!(v4_output_is_native, m)?)?;
    Ok(())
}

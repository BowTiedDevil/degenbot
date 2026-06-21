//! `#[pymodule]` registration — the single place `#[pyfunction]`/`#[pyclass]`
//! symbols are bound to the Python `degenbot.degenbot_rs` module.
//!
//! Mirrors `polars-python/src/c_api/mod.rs`: the binding crate's `lib.rs` is a
//! thin module-tree + re-export file, and every `m.add_function(...)` /
//! `m.add_class::<...>()` call lives here. Future `#[pyfunction]`/`#[pyclass]`
//! surface lands in this file (gated by cargo features in step 7 of the
//! binding-layer reorg — UG6FKN).

use pyo3::prelude::*;
use pyo3::wrap_pyfunction;

use crate::{
    abi_decoder_py, abi_encoder_py, address_utils_py, async_contract, async_provider, cl_lib_py,
    contract_py, provider_py, py_binding, py_bot, py_dex_identity, py_erc20_token,
    py_liquidity_pool, subscription_py, tick_math_py,
};

/// Register every Rust-wrapped symbol on the Python module `m`.
///
/// Order and set of registered symbols must stay byte-equivalent to the
/// pre-extraction `#[pymodule]` body (ergo UG6FKN task KFVI5F). The logging
/// bridge init (`pyo3_log::init()`) stays in `lib.rs`'s `#[pymodule]` because it
/// is module-lifecycle setup, not symbol registration.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Tick math functions
    m.add_function(wrap_pyfunction!(tick_math_py::get_sqrt_ratio_at_tick, m)?)?;
    m.add_function(wrap_pyfunction!(tick_math_py::get_tick_at_sqrt_ratio, m)?)?;

    // Address utilities
    m.add_function(wrap_pyfunction!(address_utils_py::to_checksum_address, m)?)?;

    // CL math library
    cl_lib_py::add_cl_lib_module(m)?;

    // ABI decoder functions
    m.add_function(wrap_pyfunction!(abi_decoder_py::decode, m)?)?;
    m.add_function(wrap_pyfunction!(abi_decoder_py::decode_single, m)?)?;

    // ABI encoder functions
    m.add_function(wrap_pyfunction!(abi_encoder_py::encode, m)?)?;
    m.add_function(wrap_pyfunction!(abi_encoder_py::encode_single, m)?)?;

    // Provider module
    provider_py::add_provider_module(m)?;

    // Contract module
    contract_py::add_contract_module(m)?;

    // Uniswap mixed V2/V3/V4 engine
    m.add_class::<py_binding::PyUniswapArbEngine>()?;

    // Typed verification exceptions (TODO-53b7453b): distinct
    // `RuntimeError` subclasses so `build_paths` can classify verification
    // failures by type instead of fragile string matching.
    m.add(
        "VerificationMismatchError",
        m.py().get_type::<py_binding::VerificationMismatchError>(),
    )?;
    m.add(
        "VerificationRpcError",
        m.py().get_type::<py_binding::VerificationRpcError>(),
    )?;

    // Typed V4 pool-admission exceptions (Plan 102, slice 2): distinct
    // `ValueError` subclasses so `build_paths` can classify pool rejections
    // by type instead of fragile string matching. Mirror of the verification
    // pattern above.
    m.add(
        "HookedPoolRejectedError",
        m.py().get_type::<py_binding::HookedPoolRejectedError>(),
    )?;
    m.add(
        "DynamicFeePoolRejectedError",
        m.py().get_type::<py_binding::DynamicFeePoolRejectedError>(),
    )?;

    // Bot — Rust-owned state
    m.add_class::<py_bot::PyBot>()?;
    m.add_class::<py_liquidity_pool::PyLiquidityPool>()?;
    m.add_class::<py_erc20_token::PyErc20Token>()?;

    // DEX identity presets (ADR-005 slice 6)
    py_dex_identity::add_dex_identity(m)?;

    // Async modules
    m.add_class::<async_provider::PyAsyncAlloyProvider>()?;
    m.add_class::<async_contract::PyAsyncContract>()?;

    // Subscription module
    m.add_class::<subscription_py::PyAlloySubscription>()?;

    Ok(())
}

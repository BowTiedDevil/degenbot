//! Rust extension for degenbot.
//!
//! This crate provides high-performance Rust implementations of common operations
//! used by the degenbot Python package.
//!
//! # Modules
//!
//! - [`abi_types`] - Unified ABI type/value representation (`AbiType`, `AbiValue`, `CachedAbiTypes`)
//! - [`abi_decoder`] - High-performance ABI decoding
//! - [`abi_encoder`] - High-performance ABI encoding
//! - [`alloy_py`] - Zero-intermediate-allocation U256/I256 → Python int conversion
//! - [`py_cache`] - Cached Python function/class references (`int.from_bytes`, `HexBytes`)
//! - [`address_utils`] - Ethereum address utilities (EIP-55 checksumming)
//! - [`errors`] - Centralized error types with `thiserror`
//! - [`provider`] - Ethereum RPC provider with Alloy (HTTP, WS, IPC)
//! - [`provider_py`] - `PyO3` bindings for sync provider
//! - [`async_provider`] - Async Ethereum provider wrapper
//! - [`contract`] - Smart contract interface with ABI encoding/decoding
//! - [`contract_py`] - `PyO3` bindings for contract
//! - [`async_contract`] - Async contract wrapper with batch calls
//! - [`signature_parser`] - Robust function signature parsing
//! - [`runtime`] - Shared Tokio runtime singleton
//! - [`hex_utils`] - Pure-Rust hex encoding/decoding (no `PyO3` dependency)
//! - [`py_converters`] - Python object converters for RPC types (block/tx/log dicts, JSON-to-Python with `HexBytes`)
//!
//! See individual module documentation for usage examples.

pub mod abi_decoder_py;
pub mod abi_encoder_py;
pub mod address_utils_py;
pub mod alloy_py;
pub mod async_contract;
pub mod async_provider;

pub mod cl_lib_py;
pub mod contract_py;
pub mod json_converters;
pub mod provider_py;
pub mod py_binding;
pub mod py_bot;
pub mod py_cache;
pub mod py_converters;
pub mod py_dex_identity;
pub mod py_erc20_token;
pub mod py_liquidity_pool;
pub mod subscription_py;
pub mod tick_math_py;

// The foundational core modules live in the `degenbot-core` workspace member.
// Re-exported here as `crate::errors` / `crate::hex_utils` / etc. so every
// existing `crate::errors::` call site in the binding layer keeps resolving
// through the re-export, with zero edits to call sites. Pure-Rust consumers
// depend on `degenbot-core` directly (default features, no pyo3).
pub use degenbot_core::{address_utils, errors, hex_utils, runtime};

// The concentrated-liquidity math library lives in the `degenbot-cl-math`
// workspace member. Re-exported as `crate::cl_lib` so every existing
// `crate::cl_lib::` call site in the binding layer keeps resolving through the
// re-export. Pure-Rust consumers depend on `degenbot-cl-math` directly.
pub use degenbot_cl_math::cl_lib;

// The ABI type/decode/encode + signature-parsing core lives in the
// `degenbot-abi` workspace member. Re-exported as `crate::abi_types` /
// `crate::abi_decoder` / `crate::abi_encoder` / `crate::signature_parser` so
// every existing call site in the binding layer (`contract`, `contract_py`,
// `alloy_py`, `bot_core::v2_encoding`) keeps resolving. The `#[pyfunction]`
// wrappers (`decode`/`encode`) live in `abi_decoder_py` / `abi_encoder_py`.
pub use degenbot_abi::{abi_decoder, abi_encoder, abi_types, signature_parser};

// The RPC provider / contract / subscription core lives in the
// `degenbot-rpc` workspace member. Re-exported as `crate::provider` /
// `crate::contract` / `crate::subscription` so every existing call site in the
// binding layer (`provider_py`, `contract_py`, `subscription_py`,
// `async_provider`, `async_contract`) keeps resolving. The `#[pyfunction]`
// wrappers + the GIL-bound `drain_buffer`/`DrainResult` stay in the root
// `*_py` modules (they need `py_cache` / `py_converters`).
pub use degenbot_rpc::{contract, provider, subscription};

// The bot state (`BotState`, reorg journal, verifier, pump,
// V2/V3/V4 state) + Möbius solvers + the unified `UniswapEngine` live in the
// `degenbot-bot` workspace member — one crate by ADR-003 (the state/solver seam
// is genuine domain coupling, not over-abstracted). Re-exported as
// `crate::bot_core` / `crate::optimizers` so every existing call site in the
// binding layer keeps resolving. The `#[pyclass]`/`#[pyfunction]` wrappers
// (`PyBot`, `PyLiquidityPool`, `PyErc20Token`, `PyDexIdentity`,
// `PyUniswapArbEngine`, the `Verification*Error`/`*RejectedError` exception
// types) live in the root `py_bot` / `py_liquidity_pool` / `py_erc20_token` /
// `py_dex_identity` / `py_binding` modules (they need `alloy_py` / `py_cache`).
pub use degenbot_bot::{bot_core, optimizers};

// The pure Uniswap V2/V3/V4 event-log decoders live in the `degenbot-decoders`
// workspace member (Plan 104) — an alloy-only leaf (no pyo3/tokio/degenbot-core).
// No `pub use` re-export: the binding layer reaches the lone type it needs
// (`degenbot_decoders::v4_swap_decoder::PoolId`) via the direct path dependency
// in `py_binding`. The state-coupled dispatch layer (`LogDecoder`,
// `DecodedPoolEvent`, `LogDispatcher`) stays in `degenbot-bot`'s
// `bot_core::log_dispatcher`.

// Re-export commonly used items at the crate root
pub use address_utils::{parse_address, to_checksum_address_bytes, to_checksum_address_str};
pub use address_utils_py::to_checksum_address;
pub use hex_utils::{decode_hex, encode_hex, HexError};

pub use cl_lib::tick_math::{
    get_sqrt_ratio_at_tick_internal, get_tick_at_sqrt_ratio_internal, MAX_SQRT_RATIO,
    MIN_SQRT_RATIO,
};
pub use cl_lib::{
    bit_math, full_math, functions, liquidity_math, sqrt_price_math, swap_math, unsafe_math,
};
pub use errors::{AbiDecodeError, AddressError, ClMathError, ProviderError, TickMathError};
pub use tick_math_py::{get_sqrt_ratio_at_tick, get_tick_at_sqrt_ratio};

/// Ensure Python is initialized before the test harness spawns threads.
///
/// Without this, multiple test threads racing to call `Python::attach()` can
/// trigger `Py_InitializeEx()` concurrently. CPython's `Py_InitializeEx()` sets
/// `Py_IsInitialized()` before completing all setup (e.g. importing site.py),
/// so a second thread that sees the flag and proceeds can hit
/// `_PyImport_Init: global import state already initialized`.
///
/// The `ctor` attribute runs this before `main()`, guaranteeing single-threaded
/// initialization regardless of how many threads the test harness later spawns.
///
/// # Safety
///
/// This is safe because `Python::initialize()` uses a `std::sync::Once` guard
/// internally and only calls `Py_InitializeEx()` when the interpreter is not
/// yet running, making multiple calls harmless.
#[cfg(test)]
#[ctor::ctor(unsafe)]
fn init_python_before_test_threads() {
    pyo3::Python::initialize();
}

use pyo3::prelude::*;

#[pymodule]
fn degenbot_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize logging bridge from Rust to Python
    pyo3_log::init();

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

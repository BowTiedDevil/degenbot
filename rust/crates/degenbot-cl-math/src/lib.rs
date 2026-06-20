//! Concentrated-liquidity math — pure-Rust ports of Uniswap V3/V4 Solidity
//! libraries.
//!
//! This crate depends only on [`degenbot_core`] (for `ClMathError` /
//! `TickMathError`) and `alloy::primitives`. It has **no `pyo3` and no tokio**,
//! making it the highest-frequency code in the workspace, isolatable from the
//! PyO3 layer, and independently testable without a Python interpreter.
//!
//! The Python binding layer lives in the root `degenbot_rs` crate's
//! `cl_lib_py` module.

pub mod cl_lib;

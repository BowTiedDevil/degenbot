//! Concentrated-liquidity math — pure-Rust ports of Uniswap V3/V4 Solidity
//! libraries.
//!
//! This crate depends only on [`degenbot_core`] (for `ClMathError` /
//! `TickMathError`) and `alloy::primitives`. It has **no `pyo3` and no tokio**,
//! making it the highest-frequency code in the workspace, isolatable from the
//! `PyO3` layer, and independently testable without a Python interpreter.
//!
//! The Python binding layer lives in the `degenbot_rs` crate's
//! `cl_math::cl_lib` module (`rust/crates/degenbot-python/src/cl_math/cl_lib.rs`).

pub mod cl_lib;

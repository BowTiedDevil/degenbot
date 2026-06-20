//! The per-chain Rust-owned bot state + Möbius solvers + the unified
//! Uniswap V2/V3/V4 engine, combined into one crate.
//!
//! Per ADR-003, `bot_core` (the `BotState` single-owner state, decoders,
//! reorg journal, verifier, pump) and `optimizers` (the Möbius solvers and
//! the `UniswapEngine` path/solver/dispatch layer) are a **mutually coupled
//! pair** — ~30 cross-references each way (`BotState` needs
//! `IntHopState`/`IntV3TickRangeSequence`/decoders from `optimizers`; the
//! engine needs `BotState`/`V3PoolState`/`TickInfo`/`PoolStateSubscriber`
//! from `bot_core`). ADR-003 explicitly refuses to extract a `LiquidityMap`
//! generic against this sample-of-one, so the two live in one crate here
//! rather than behind an artificial shared-trait seam.
//!
//! # PyO3 boundary
//!
//! The pure core (this crate's default features) has **no `pyo3` dependency**.
//! The `#[pyclass]`/`#[pyfunction]` bindings (`PyBot`, `PyLiquidityPool`,
//! `PyErc20Token`, `PyDexIdentity`, `PyUniswapArbEngine`, the
//! `Verification*Error`/`*RejectedError` exception types) live in the root
//! `degenbot_rs` cdylib's `py_bot` / `py_liquidity_pool` / `py_erc20_token` /
//! `py_dex_identity` / `py_binding` modules — they need `alloy_py` /
//! `py_cache` (binding-layer concerns). They reach the pure core through
//! `degenbot_bot::{bot_core, optimizers}`.
//!
//! # Modules
//!
//! - [`bot_core`] — `BotState`, decoders, reorg journal, liquidity verifier,
//!   block pump, log/solve/reorg coordinators, V2/V3/V4 state.
//! - [`optimizers`] — Möbius solvers + the unified `UniswapEngine`.

pub mod bot_core;
pub mod optimizers;

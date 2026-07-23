//! The backrun searcher strategy — one example strategy over the
//! `degenbot-simulation` engine.
//!
//! ADR-019 D4/D7 (decision R — Rust-canonical): the strategy stays in Rust
//! (not re-derived in Python — AGENTS.md's "driver shell, not a
//! co-implementation"). This crate owns the backrun bot's strategy logic:
//!
//! - the 7-call pre/post-balance bundle (3 pre-balance → `execute()` →
//!   3-post-balance over WETH9 / Multicall3 / PoolManager ERC6909),
//! - `decode_balance`,
//! - `compute_priority_fee` (TARGET_PROFIT_RATIO / age-decay),
//! - the sim value types (`SimResult`, `SimulateContext`, `SimulatePath`,
//!   `FailBuckets`, the int128 guard),
//! - `dispatch_profitable_results` (the revm-only fan-out) + its thin-margin /
//!   suppression / categorization policy.
//!
//! The engine (revm executor, `BlockSimHandle`, `apply_simulation_overrides`,
//! AL Inspector, `WarmCodeCache`) stays thin + generic in
//! `degenbot-simulation`; this strategy drives it. Other searcher strategies
//! (sandwich, JIT-L, liquidation) would be sibling crates over the same
//! engine.
//!
//! # Standalone-Rust consumer
//!
//! `cargo add degenbot-backrun-strategy` reaches the backrun strategy with
//! zero `pyo3` dependency (ADR-005 standalone-core). The Python driver
//! (`examples/eth_backrun_v2_v3_v4_rust.py`) is a thin cockpit over a PyO3
//! wrapper around `dispatch_profitable_results` — it does NOT re-derive the
//! 7-call bundle.

// Solidity/EVM identifiers (balanceOf, WETH9, PoolManager, ERC6909, …) are
// ubiquitous in this crate's docs; allow the pedantic doc-markdown lint.
#![allow(clippy::doc_markdown)]

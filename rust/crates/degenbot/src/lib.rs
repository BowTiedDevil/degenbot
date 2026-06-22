//! # `degenbot` — umbrella Rust crate (ADR-005 standalone surface)
//!
//! Re-exports the **pyo3-free** degenbot core surface so a standalone Rust
//! consumer can `cargo add degenbot` and reach `BotState`, the `DexIdentity`
//! presets, the V2/V3/V4 pool-state structs, the swap calc math, and the V2
//! swap-call encoder — **with zero Python in the build graph**.
//!
//! This mirrors the `polars` umbrella Rust crate, which `pub use`s
//! `polars_core::{DataFrame, Series, ...}` with no `pyo3`; all `Py*` wrappers
//! live exclusively in `polars-python`, which Rust consumers never touch.
//! Here the binding layer (`degenbot_rs` cdylib, built from
//! `crates/degenbot-python/`) is a separate workspace member that DOES pull
//! `pyo3`; the umbrella never depends on it.
//!
//! # Standalone-Rust consumer
//!
//! See [`examples/standalone_consumer.rs`](../examples/standalone_consumer.rs):
//! it constructs a `BotState`, registers a V2 pool via the `UNISWAP_V2`
//! `DexIdentity` preset, and runs a swap calc — no Python interpreter, no
//! `pyo3` feature, no maturin.
//!
//! # Verifying pyo3-freeness
//!
//! `rg 'use pyo3' rust/crates/degenbot` is empty by construction (the
//! umbrella has no `pyo3` dependency; the gates `just check-no-pyo3-in-cores`
//! covers the core crates — this umbrella is the same class).

/// Foundational utilities — errors, hex, EIP-55 addresses, shared runtime.
pub use degenbot_core::{address_utils, errors, hex_utils, runtime};

/// Per-chain Rust-owned bot state (`BotState`), reorg journal, decoders,
/// liquidity verifier, block pump, log/solve/reorg coordinators, V2/V3/V4
/// state, plus the Möbius solvers + the unified `UniswapEngine`.
pub use degenbot_bot::{bot_core, solvers};

/// Uniswap-protocol domain — `DexIdentity` / `DexVariant` / `ReservesAbi`
/// value objects + `pub const` per-DEX presets, and the V2 swap-call encoder.
pub use degenbot_uniswap::{dex_identity, v2_encoding};

/// Uniswap V2/V3/V4 event-log decoders (alloy-only leaf).
pub use degenbot_decoders;

/// Concentrated-liquidity math (`cl_lib`, `tick_math`).
pub use degenbot_cl_math;

/// ABI encode/decode (`abi_decoder`, `abi_encoder`).
pub use degenbot_abi;

/// RPC provider/contract/subscription seams (the pure core; `pyo3` is an
/// optional feature the umbrella never enables).
pub use degenbot_rpc;

// ---------------------------------------------------------------------------
// Convenience top-level re-exports of the most-used types (mirrors how the
// `polars` umbrella re-exports `DataFrame`/`Series` at the crate root).
// ---------------------------------------------------------------------------

pub use degenbot_bot::bot_core::{
    BotState, PoolEntry, RegisterV2PoolParams, RegisterV3PoolParams, V2PoolState,
};
pub use degenbot_uniswap::dex_identity::{
    preset_for_variant, DexIdentity, DexVariant, ReservesAbi, UNISWAP_V2,
};

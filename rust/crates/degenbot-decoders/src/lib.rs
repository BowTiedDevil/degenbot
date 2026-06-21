//! Pure-Rust Uniswap V2/V3/V4 event-log decoders.
//!
//! This crate is an **alloy-only leaf** — it has no `pyo3`, no `tokio`, no
//! `degenbot-core`, and no `degenbot-abi`. Each decoder hand-slices the EVM
//! log bytes and returns a plain `Option<Event>` struct, making the decoders
//! independently testable without a Python interpreter and reusable in
//! non-Python Rust code. A standalone Rust consumer can decode a V3 `Swap`
//! log without pulling the engine, the pump, or the RPC stack.
//!
//! The state-coupled dispatch layer (`LogDecoder` trait,
//! `DecodedPoolEvent`, `LogDispatcher` bus, `PoolStateSubscriber`) stays in
//! `degenbot-bot`'s `bot_core::log_dispatcher` — those reach `BotState`. This
//! crate holds only the leaf decode functions + their plain return structs +
//! the six topic constants.
//!
//! Extension seam: a future Curve/Aave event decoder lands here alongside the
//! Uniswap ones; `LogDispatcher::register_decoder` consumes any
//! `impl LogDecoder` without `Bot` knowing the event shape.
//!
//! # Modules
//!
//! - [`v2_sync_decoder`] — V2 `Sync(uint112,uint112)`.
//! - [`v3_swap_decoder`] — V3 `Swap(address,address,int256,int256,uint160,uint128,int24)`.
//! - [`v3_mint_burn_decoder`] — V3 `Mint` / `Burn` (both → tick-range liquidity delta).
//! - [`v4_swap_decoder`] — V4 `Swap` (from `PoolManager`).
//! - [`v4_modify_liquidity_decoder`] — V4 `ModifyLiquidity` (signed delta; replaces V3 Mint/Burn).

pub mod v2_sync_decoder;
pub mod v3_mint_burn_decoder;
pub mod v3_swap_decoder;
pub mod v4_modify_liquidity_decoder;
pub mod v4_swap_decoder;

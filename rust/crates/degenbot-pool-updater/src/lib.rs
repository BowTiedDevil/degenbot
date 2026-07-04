//! Pure-Rust pool-updater chunk-loop RPC + decode bridge (epic `2SFL6I`).
//!
//! This crate is the typed async glue between three existing core crates:
//! - [`degenbot_rpc`] (`AlloyProvider` + `LogFilter` + `fetch_logs_chunked`)
//!   for the RPC fetching the chunk loop drives,
//! - [`degenbot_decoders`] (the `PoolCreated`/`Mint`/`Burn`/`ModifyLiquidity`
//!   decode leaves) for hand-slicing the returned `alloy::Log`s,
//! - [`degenbot_db`] (`V2/V3/V4PoolRowInput` + `LiquidityUpdateEvent`) for
//!   the row-input types the chunk-loop writers consume.
//!
//! The fetcher wrappers here are the foundation for Task `CKXCOB` (the
//! `run_pool_update` core function): they expose the chunk loop's exact needs
//! as a typed async API (`fetch_pool_created_logs` + per-family
//! `fetch_*_liquidity_logs`) returning decoder-native structs (for pool
//! creation) and the DB-ready [`LiquidityUpdateEvent`] (for liquidity), so the
//! chunk loop never touches raw `alloy::Log`s itself.
//!
//! The `PoolCreated` decoders themselves live in `degenbot-decoders`
//! (`pool_created_decoder`) — pure alloy-only leaves (this crate wraps them;
//! it does not re-implement decode logic). The decoder-struct →
//! `V*PoolRowInput` mapping (which carries the `fee_denominator`/`kind`
//! DB-specific concerns the leaf correctly avoids) is deferred to Task
//! `CKXCOB`'s `run_pool_update` (the chunk loop is the natural owner — it
//! knows the `exchange_id`/`chain_id`/`fee_denominator` context per batch).
//!
//! No `pyo3` (enforced by `just check-no-pyo3-in-cores`). The `PyO3` seam will
//! live in `degenbot-python` (a later task).

pub mod fetch;

pub use fetch::{
    decode_pool_created_log, decode_v3_liquidity_log, decode_v4_liquidity_log,
    fetch_pool_created_logs, fetch_v3_liquidity_logs, fetch_v4_liquidity_logs, DecodedPoolCreated,
    PoolFamily,
};

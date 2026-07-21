//! Pure-Rust event-log + revert-data decoders for Uniswap V2/V3/V4 and Aave V3.
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
//! the keccak256 topic constants (one per decoded event signature).
//!
//! Extension seam: a future Curve/Pendle event decoder lands here alongside
//! the Uniswap and Aave ones; `LogDispatcher::register_decoder` consumes any
//! `impl LogDecoder` without `Bot` knowing the event shape.
//!
//! # Modules
//!
//! - [`v2_sync_decoder`] — V2 `Sync(uint112,uint112)`.
//! - [`v3_swap_decoder`] — V3 `Swap(address,address,int256,int256,uint160,uint128,int24)`.
//! - [`v3_mint_burn_decoder`] — V3 `Mint` / `Burn` (both → tick-range liquidity delta).
//! - [`v4_swap_decoder`] — V4 `Swap` (from `PoolManager`).
//! - [`v4_modify_liquidity_decoder`] — V4 `ModifyLiquidity` (signed delta; replaces V3 Mint/Burn).
//! - [`pool_created_decoder`] — V2/V3/V4 `PoolCreated`/`Initialize` events
//!   (the pool-discovery events the chunk-loop fetcher consumes; epic
//!   `2SFL6I`, task SBICJJ). Pure leaf — produces decoder-native structs;
//!   mapping to `degenbot-db` row-input types is the chunk loop's job.
//! - [`aave_event_decoder`] — Aave V3 events (Supply/Borrow/Repay/Withdraw/
//!   LiquidationCall/MintedToTreasury/ReserveDataUpdated/ScaledToken Mint-Burn-
//!   BalanceTransfer/EMode/Discount/ProxyCreated/Upgraded/ERC20 Transfer —
//!   34 events across 8 decoded families). The dispatch fn
//!   [`aave_event_decoder::decode_aave_log`] returns a `DecodedAaveEvent`;
//!   epic `AZGJUN`, task `ECFB5C`. Pure leaf — emits raw `Address`/`U256`
//!   fields; id resolution is the orchestrator's job.
//! - [`revert`] — revert-data taxonomy (`classify_revert`): bytes → stable
//!   label for the `[sim]` summary + engine revert tallies.
//! - [`uniswap_tick_range`] — the shared Uniswap V3 tick bounds
//!   (`UNISWAP_V3_MIN_TICK`/`UNISWAP_V3_MAX_TICK`) + the `extract_int24_from_word`
//!   helper consumed by every V3/V4 int24-tick decoder.

pub mod aave_event_decoder;
pub mod pool_created_decoder;
pub mod revert;
pub mod uniswap_tick_range;
pub mod v2_sync_decoder;
pub mod v3_mint_burn_decoder;
pub mod v3_swap_decoder;
pub mod v4_modify_liquidity_decoder;
pub mod v4_swap_decoder;

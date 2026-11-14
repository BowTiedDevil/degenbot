//! Pure-Rust event-log + revert-data decoders for Uniswap V2/V3/V4 and Aave V3.
//!
//! This crate is an **alloy-only leaf** — it has no `pyo3`, no `tokio`, no
//! `degenbot-core`, and no `degenbot-abi`. Each decoder returns a plain
//! `Option<Event>` struct, making the decoders independently testable without a
//! Python interpreter and reusable in non-Python Rust code. A standalone Rust
//! consumer can decode a V3 `Swap` log without pulling the engine, the pump, or
//! the RPC stack.
//!
//! The Uniswap V2/V3/V4 log decoders read their logs through Alloy's
//! **validating** decoders — one `sol!` event declaration per event (or a
//! validating `SolValue` tuple where the decoder keeps only a prefix of the
//! data) — so a narrow ABI word (`uint112`, `uint160`, `uint128`, `uint24`,
//! `int128`, sign-extended `int24`, `address`, `bool`) is refused when it is
//! not canonical for its declared width instead of being truncated.
//!
//! A `sol!` declaration also binds the topic count exactly, so a log carrying
//! too few topics or a surplus topic is a decode failure rather than a
//! best-effort match. The V4 `Initialize` decoder is the one deliberate
//! exception: it keeps a prefix of the event (the four leading topics plus the
//! three data words the row needs) and ignores any surplus topic or trailing
//! word.
//!
//! The state-coupled dispatch layer (`LogDecoder` trait,
//! `DecodedPoolEvent`, `LogDispatcher` bus) stays in
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
//! - [`v2_sync_decoder`] — V2 `Sync(uint112,uint112)` (reserves).
//! - [`v2_swap_decoder`] — V2 `Swap(address,uint256,uint256,uint256,uint256,address)` (in/out amounts — the swap-event-capture source that retires `recompute_v2_amount_out`).
//! - [`v3_swap_decoder`] — V3 `Swap(address,address,int256,int256,uint160,uint128,int24)`.
//! - [`v3_mint_burn_decoder`] — V3 `Mint` / `Burn` (both → tick-range liquidity delta).
//! - [`v4_swap_decoder`] — V4 `Swap` (from `PoolManager`).
//! - [`v4_modify_liquidity_decoder`] — V4 `ModifyLiquidity` (signed delta; replaces V3 Mint/Burn).
//! - [`pool_created_decoder`] — V2/V3/V4 `PoolCreated`/`Initialize` events
//!   (the pool-discovery events the chunk-loop fetcher consumes). Pure leaf — produces decoder-native structs;
//!   mapping to `degenbot-db` row-input types is the chunk loop's job.
//! - [`aave_event_decoder`] — Aave V3 events (Supply/Borrow/Repay/Withdraw/
//!   LiquidationCall/MintedToTreasury/ReserveDataUpdated/ScaledToken Mint-Burn-
//!   BalanceTransfer/EMode/Discount/ProxyCreated/Upgraded/ERC20 Transfer —
//!   34 events across 8 decoded families). The dispatch fn
//!   [`aave_event_decoder::decode_aave_log`] returns a `DecodedAaveEvent`;
//!   pure leaf — emits raw `Address`/`U256` fields; id resolution is the
//!   orchestrator's job.
//! - [`revert`] — revert-data taxonomy (`classify_revert`): bytes → stable
//!   label for the `[sim]` summary + engine revert tallies.
//! - [`uniswap_tick_range`] — the shared Uniswap V3 tick bounds
//!   (`UNISWAP_V3_MIN_TICK`/`UNISWAP_V3_MAX_TICK`) + the `validate_tick`
//!   helper every V3/V4 int24-tick decoder applies to its decoded tick.

pub mod aave_event_decoder;
pub mod pool_created_decoder;
pub mod revert;
pub mod uniswap_tick_range;
pub mod v2_swap_decoder;
pub mod v2_sync_decoder;
pub mod v3_mint_burn_decoder;
pub mod v3_pancakeswap_swap_decoder;
pub mod v3_swap_decoder;
pub mod v4_modify_liquidity_decoder;
pub mod v4_swap_decoder;

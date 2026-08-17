//! # `degenbot-db` — `SQLite` persistence substrate for the degenbot Rust core
//!
//! A pyo3-free read handle over a degenbot `SQLite` DB, with an Alembic-aware
//! schema gate. Owns connection/pool management, the schema DDL (mirroring the
//! current Alembic head), typed row structs for every table, low-level read
//! row functions, and the migration runner that opens an existing
//! Alembic-stamped `SQLite` DB without clobbering its revision.
//!
//! # Open path
//!
//! [`DegenbotDb::open`] sets `PRAGMA journal_mode=WAL; busy_timeout=5000;
//! synchronous=NORMAL;` (matching the Python open path — Phase 0, `2KUI3M`),
//! runs [`migrate::ensure_schema`] (reads the `alembic_version` head; writes
//! nothing on an Alembic DB), then sets `PRAGMA query_only=on;` — the
//! load-bearing guarantee that the Rust reader cannot mutate an
//! Alembic-stamped production DB during the hybrid period (binding #2).
//!
//! # The `VARCHAR(78)` ↔ `U256` boundary
//!
//! Every EVM big-int column is stored as a `VARCHAR(78)` decimal string
//! (mirrors Python's `IntMappedToString` `TypeDecorator`) and decoded to
//! [`alloy::primitives::U256`] at the row boundary via
//! `U256::from_str_radix(s, 10)` (see [`rows::decode`]). Zero schema change
//! vs. the Alembic head; the round-trip is lossless across the full 32-byte
//! EVM range.
//!
//! # Design references
//!
//! Non-goal: writes (`write.rs` + `upsert_*`) are deferred to Epic AZGJUN
//! (Phase 3) until the Rust writer is parity-tested; this crate ships read
//! fns only.

pub mod aave;
pub mod connection;
pub mod discovery;
pub mod error;
pub mod heal;
pub mod liquidity_updater;
pub mod migrate;
pub mod ops;
pub mod pathfinding;
pub mod read;
pub mod rows;
pub mod schema;
pub mod snapshot;
pub mod snapshot_db;
pub mod write;

pub use aave::{
    AaveCollateralPositionRecord, AaveDebtPositionRecord, AaveGhoAsset, AaveUserRecord,
};
pub use connection::DegenbotDb;
/// Re-export the concentrated-liquidity-math in-memory liquidity types (`liquidity_gross: U128`,
/// `liquidity_net: I256`, plus `bitmap: U256`) the liquidity updater applies
/// against — aliased so they don't collide with the snapshot batch-read `U256`
/// flavor above. These are the field types of `ComputedLiquidityUpdate` + the
/// on-chain verifier's comparison inputs.
pub use degenbot_concentrated_liquidity_math::liquidity_mapping::{
    BitmapAtWord as ApplyBitmapAtWord, LiquidityAtTick as ApplyLiquidityAtTick,
};
pub use discovery::{V2PoolRowInput, V3PoolRowInput, V4PoolRowInput};
pub use error::DbError;
pub use heal::{heal_database, HealReport};
pub use liquidity_updater::{
    BlockLog, ComputedLiquidityUpdate, LiquidityUpdateEvent, PoolUpdateState,
};
pub use migrate::SchemaState;
pub use ops::{
    backup_database, compact_database, convert_alembic_to_rust_owned, create_new_database,
    inspect_schema_state, upgrade_database, UpgradeOutcome,
};
pub use pathfinding::{PathEdge, PathGraphData};
pub use read::ExchangeFamily;
pub use rows::{
    InitializationMapRow, LiquidityPoolRow, LiquidityPositionRow, ManagedLiquidityPoolRow,
    ManagedPoolInitializationMapRow, ManagedPoolLiquidityPositionRow, PoolKindRow, PoolManagerRow,
    V2PoolRow, V3PoolRow, V4PoolRow,
};
pub use schema::ALEMBIC_HEAD;
pub use snapshot::{BitmapAtWord, LiquidityAtTick, LiquidityMap, PoolKey};
pub use write::DebtPositionRefreshContext;
pub use write::{
    decode_reserve_configuration_bitmap, AssetRow, ReserveConfiguration, ScaledTokenPosition,
};

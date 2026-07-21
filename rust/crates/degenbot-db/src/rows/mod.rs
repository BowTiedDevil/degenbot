//! Typed row structs — plain `#[derive(Debug, Clone, PartialEq, Eq)]` decoders,
//! NOT ORM/active-records. Each carries the typed projection of one table's
//! columns (EVM integers as [`alloy::primitives::U256`], addresses as
//! [`alloy::primitives::Address`]); a `from_row` impl per type builds the struct
//! from a [`rusqlite::Row`].
//!
//! See `src/degenbot/database/models/*.py` for the canonical column shapes.

pub mod aave;
pub mod decode;
pub mod erc20;
pub mod exchange;
pub mod liquidity;
pub mod pool;

pub use aave::*;
pub use erc20::Erc20TokenRow;
pub use exchange::ExchangeRow;
pub use liquidity::{
    InitializationMapRow, LiquidityPositionRow, ManagedPoolInitializationMapRow,
    ManagedPoolLiquidityPositionRow,
};
pub use pool::{
    LiquidityPoolRow, ManagedLiquidityPoolRow, PoolKindRow, PoolManagerRow, V2PoolRow, V3PoolRow,
    V4PoolRow,
};

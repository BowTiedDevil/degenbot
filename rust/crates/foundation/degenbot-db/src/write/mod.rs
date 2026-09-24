//! Aave V3 lending-market DB writers — the per-event apply fns +
//! `get_or_create_*` upsert substrate (row N4 of the writer scope
//! `RQXEKH` — `port-now`).
//!
//! Port of `src/degenbot/cli/aave/event_handlers.py::_process_*` handlers +
//! `db_market.py` / `db_assets.py` / `db_users.py` / `db_positions.py`
//! `get_or_create_*` upsert helpers. The Python `aave_update` driver loop +
//! RPC event fetch stay as the shell (`stays-python`); the Rust core owns the
//! pure-typed upsert substrate: get-or-create + event-decode→row-write.
//!
//! # The write-capable connection (`open_for_writes`)
//!
//! SLHSM4 binding #2 hard-AC is "every **read** connection opened by
//! degenbot-db MUST set `query_only=on`" — [`DegenbotDb::open`] /
//! [`DegenbotDb::open_in_memory`] stay read-only. Writers use
//! [`DegenbotDb::open_for_writes`] / [`DegenbotDb::open_in_memory_for_writes`]:
//! the same `PRE_SCHEMA_PRAGMAS` + [`ensure_schema`][crate::migrate::ensure_schema]
//! sequence, but `query_only` is NEVER set — the connection is write-capable.
//! The writer methods on [`DegenbotDb`] (`get_or_create_*` / `process_*`)
//! execute `INSERT` / `UPDATE` directly on the locked connection; called on a
//! read-only handle they fail at the `SQLite` layer
//! (`attempt to write a readonly database`), surfacing [`DbError::Sqlite`].
//!
//! # The bit-decode (the pure CPU seam)
//!
//! [`decode_reserve_configuration_bitmap`] ports
//! `_decode_reserve_configuration_bitmap` (`event_handlers.py` L133–L214) verbatim:
//! the Aave V3 reserve-config `uint256` bitmap bits → a typed
//! [`ReserveConfiguration`] (ltv / liquidation-threshold / -bonus / decimals /
//! active / frozen / borrowing-enabled / stable-rate / reserve-factor /
//! borrow-cap / supply-cap / debt-ceiling / liquidation-protocol-fee /
//! unbacked-mint-cap / e-mode-category-id / flash-loan / isolation-mode /
//! borrowable-in-isolation). Pure CPU, no I/O — the §4.2 parity pin ports the
//! exact bit masks + shifts.
//!
//! # The upsert substrate (`get_or_create_*`)
//!
//! Each `get_or_create_*` mirrors the Python `session.scalar(select(...)) →
//! mutate ORM → session.add()` trajectory as a single `SELECT … WHERE …` →
//! `INSERT` (or `UPDATE` for the mutate path). The Python `Erc20TokenTable` /
//! `AaveV3Asset` / `AaveV3User` / `AaveV3EModeCategory` / `AaveV3AssetConfig` /
//! `AaveV3UserCollateralConfig` / `AaveV3CollateralPosition` /
//! `AaveV3DebtPosition` row-create defaults are reproduced verbatim (field
//! defaults match the `SQLAlchemy` model column defaults so byte-identical DB
//! state vs the Python ORM trajectory holds — §4.2 AC).
//!
//! # RPC-coupling carve-out (`stays-python` inside the upserts)
//!
//! The Python `get_or_create_erc20_token` + `get_or_create_user` fetch
//! on-chain metadata (name/symbol/decimals) / GHO discount via `raw_call`.
//! The Rust core fns take these as caller-supplied `Option` params (the
//! Python driver computes them + passes them in); no `RPC` in the core
//! (ADR-005 standalone — `degenbot-db` has no `degenbot-rpc` dependency).

mod aave;
mod erc20;
mod gho;
mod pools;
mod positions;
mod util;

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests;

pub use aave::*;
pub use positions::*;

pub(crate) use alloy::primitives::U256;
pub(crate) use rusqlite::{params, OptionalExtension};

pub(crate) use crate::connection::DegenbotDb;
pub(crate) use crate::error::DbError;

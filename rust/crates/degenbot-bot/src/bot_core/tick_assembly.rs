//! Tick-map assembly: `Store → Db` precedence helper (Candidate 1, epic UHPXSD).
//!
//! One free function per CL family (`assemble_v3_tick_map` /
//! `assemble_v4_tick_map`) that probes a [`SnapshotStore`] (the bulk-loaded DB
//! snapshot, consumed once per pool), and on a miss falls back to a per-pool
//! `DegenbotDb::fetch_liquidity_map` read. The Chain (sparse RPC) arm is
//! intentionally absent — it stays Python-side until the follow-up epic
//! tracked by task `SBFYQ4` ports a `TickBootstrapRpc` trait into Rust.
//!
//! # Lock protocol (A4YUYJ — must-read before editing)
//!
//! The Store arm is a **closure** (`impl FnOnce() -> (HashMap<i32, TickInfo>,
//! PoolTickCoverage)`) rather than `&SnapshotStore<K>`. This is deliberate:
//! `SnapshotStore::take` is `&self` (interior mutability), so a `&store` borrow
//! would tie the caller's `BotState` read guard to the helper's *entire* call
//! — including the Db read inside it. During `build_paths` the live pump holds
//! `state.write()` on the same `BotState` (`resume()` precedes `build_paths`),
//! so a guard held across an `SQLite` read would block pump Mint/Burn applies
//! for every pool registered. The closure sidesteps this: it runs under the
//! guard, returns owned `(ticks, coverage)`, drops the guard, and the helper
//! continues with the `Option<&DegenbotDb>` (a handle to a *separate*
//! `Mutex<Connection>`, decoupled from `BotState`) — no `BotState` guard held
//! across the Db read. Two-phase locking, hidden inside one Rust call so the
//! `PyO3` caller sees a single `assemble_*` function.
//!
//! # Db error handling (Decision 8 (A) — behavior change)
//!
//! Today the Python builder wraps the Db snapshot read in
//! `contextlib.suppress(Exception)`: a transient `database is locked` under
//! the concurrent updater process is swallowed and the builder silently falls
//! through to sparse RPC. This helper **propagates** `DbError` instead — the
//! `PyO3` wrapper (task `A4YUYJ`) raises `RuntimeError`. The trade-off is
//! deliberate: loud failure on Db problems beats silent degradation to a
//! solver-unsafe sparse state. Do NOT "restore" the swallow — if a transient Db
//! error now aborts registration where it previously degraded, that is the
//! intended behavior and must be fixed at the Db layer (e.g. longer
//! `busy_timeout`), not papered over here.

use std::collections::HashMap;

use alloy::primitives::Address;

use degenbot_db::connection::DegenbotDb;
use degenbot_db::error::DbError;
use degenbot_db::snapshot::{LiquidityAtTick, LiquidityMap};
use degenbot_decoders::v4_swap_decoder::PoolId;

// `PoolTickCoverage` + `TickInfo` live in `degenbot_pools` but aren't at its
// crate root; `bot_core::mod` re-exports them via `pub use v3_state::…` +
// `pub use ::degenbot_pools::TickInfo`. Use those re-exports to avoid coupling
// this submodule to `degenbot_pools::v3_state`'s private path.
use crate::bot_core::{PoolTickCoverage, TickInfo};

/// The helper's return shape: an optional hit (`Some((ticks, coverage))` on
/// Store or Db success) or a miss (`None`), with `DbError` propagated from the
/// Db arm (Decision 8 (A) — never swallowed). A type alias keeps the four call
/// sites readable and silences `clippy::type_complexity`.
pub type TickMapAssemblyResult =
    Result<Option<(HashMap<i32, TickInfo>, PoolTickCoverage)>, DbError>;

/// Assemble a V3 pool's tick map with `Store → Db` precedence.
///
/// 1. **Store arm** (probe): if the closure returns
///    [`PoolTickCoverage::Tracked`], the store hit is returned verbatim and
///    the Db arm is skipped (the store entry has been consumed by `take`).
/// 2. **Db arm**: only on a store miss. `db.fetch_liquidity_map(address)`
///    is queried; a non-empty map (both `tick_bitmap` AND `tick_data`
///    populated — mirrors Python's `if not init_maps or not liq_positions`
///    heuristic) converts to `TickInfo` with `Tracked` coverage; an empty map
///    OR a pool-not-found (`Ok(None)`) returns `Ok(None)` (miss → Python runs
///    Branch 3 sparse RPC); a `Err(DbError)` is **propagated** (Decision 8 (A)).
/// 3. `db = None` (cold-start, no Db handle cached): Store-only; miss if the
///    store missed.
///
/// Returns `Ok(Some((ticks, Tracked)))` on a hit (Store or Db), `Ok(None)` on
/// a miss, `Err(DbError)` on a Db read failure.
///
/// # Errors
///
/// Propagates [`DbError`] from `fetch_liquidity_map` (Decision 8 (A) — not
/// swallowed; the caller surfaces it as a typed exception).
pub fn assemble_v3_tick_map(
    store_probe: impl FnOnce() -> (HashMap<i32, TickInfo>, PoolTickCoverage),
    db: Option<&DegenbotDb>,
    address: Address,
) -> TickMapAssemblyResult {
    let (ticks, coverage) = store_probe();
    if coverage == PoolTickCoverage::Tracked {
        return Ok(Some((ticks, coverage)));
    }
    let Some(db) = db else {
        return Ok(None);
    };
    fetch_v3_tick_map_from_db(db, address)
}

/// Assemble a V4 pool's tick map with `Store → Db` precedence.
///
/// V4 twin of [`assemble_v3_tick_map`]: the store is keyed by
/// `(Address, PoolId)`, and the Db arm calls
/// `db.fetch_liquidity_map_v4(pool_manager, pool_id_hash)`. Identical
/// hit/miss/error semantics — see [`assemble_v3_tick_map`] for the full
/// contract.
///
/// # Errors
///
/// Propagates [`DbError`] from `fetch_liquidity_map_v4` (Decision 8 (A)).
pub fn assemble_v4_tick_map(
    store_probe: impl FnOnce() -> (HashMap<i32, TickInfo>, PoolTickCoverage),
    db: Option<&DegenbotDb>,
    pool_manager: Address,
    pool_id: PoolId,
) -> TickMapAssemblyResult {
    let (ticks, coverage) = store_probe();
    if coverage == PoolTickCoverage::Tracked {
        return Ok(Some((ticks, coverage)));
    }
    let Some(db) = db else {
        return Ok(None);
    };
    fetch_v4_tick_map_from_db(db, pool_manager, pool_id)
}

/// Db arm for V3: convert a `LiquidityMap` into the helper's hit/miss shape.
fn fetch_v3_tick_map_from_db(db: &DegenbotDb, address: Address) -> TickMapAssemblyResult {
    let Some(map) = db.fetch_liquidity_map(address)? else {
        return Ok(None);
    };
    Ok(liquidity_map_to_tick_info(map))
}

/// Db arm for V4: identical to V3 but routes through the V4 fetch.
fn fetch_v4_tick_map_from_db(
    db: &DegenbotDb,
    pool_manager: Address,
    pool_id: PoolId,
) -> TickMapAssemblyResult {
    // `fetch_liquidity_map_v4` takes a `B256`; `PoolId` is `[u8; 32]` and
    // `B256` is `FixedBytes<32>` — same layout, so the conversion is infallible.
    let pool_id_hash = alloy::primitives::B256::from(pool_id);
    let Some(map) = db.fetch_liquidity_map_v4(pool_manager, pool_id_hash)? else {
        return Ok(None);
    };
    Ok(liquidity_map_to_tick_info(map))
}

/// Convert a Db `LiquidityMap` into the helper's hit/miss shape.
///
/// Mirrors Python's `if not init_maps or not liq_positions: return ..., False`
/// heuristic: a map with EITHER `tick_bitmap` OR `tick_data` empty is treated
/// as a miss (`None`). In a healthy Db the two are 1:1 (a liquidity position
/// creates its own initialization-map bit), so this is purely defensive — but
/// matching Python preserves the existing degrade-to-RPC behavior on partial
/// rows. A non-empty map converts to `Tracked` coverage; per-tick `block` is
/// pinned to `0` (the Db snapshot is state at block `S`; per-tick block is
/// diagnostic only — the solver math doesn't read it — matching
/// `convert_tick_map` in `bot_core::mod.rs`).
fn liquidity_map_to_tick_info(
    map: LiquidityMap,
) -> Option<(HashMap<i32, TickInfo>, PoolTickCoverage)> {
    if map.tick_bitmap.is_empty() || map.tick_data.is_empty() {
        return None;
    }
    let ticks = convert_liquidity_at_tick(map.tick_data);
    Some((ticks, PoolTickCoverage::Tracked))
}

/// Convert `HashMap<i32, LiquidityAtTick>` → `HashMap<i32, TickInfo>`.
///
/// The Db stores `liquidity_gross` as `U256` (decimal `VARCHAR(78)`); valid
/// on-chain gross liquidity always fits in `U128` (Uniswap's
/// `type(uint128).max` cap on `liquidity_gross`). The narrowing uses
/// `U256::to::<U128>()` which silently truncates out-of-range values —
/// identical to `convert_tick_map` in `bot_core::mod.rs`; a `gross > U128::MAX`
/// row would be corrupt on-chain data, not a parse failure.
fn convert_liquidity_at_tick(tick_data: HashMap<i32, LiquidityAtTick>) -> HashMap<i32, TickInfo> {
    let mut out = HashMap::with_capacity(tick_data.len());
    for (tick, lat) in tick_data {
        out.insert(
            tick,
            TickInfo {
                liquidity_gross: lat.liquidity_gross.to::<alloy::primitives::U128>(),
                liquidity_net: lat.liquidity_net,
                block: 0,
            },
        );
    }
    out
}

#[cfg(test)]
mod tests;

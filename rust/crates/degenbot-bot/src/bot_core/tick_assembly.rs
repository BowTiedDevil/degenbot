//! Tick-map assembly: `Store → Db → Chain` precedence helper (epic `5NT2OC`).
//!
//! One free function per CL family (`assemble_v3_tick_map` /
//! `assemble_v4_tick_map`) that probes a [`SnapshotStore`] (the bulk-loaded DB
//! snapshot, consumed once per pool), then on a miss falls back to a per-pool
//! `DegenbotDb::fetch_liquidity_map` read, then on a further miss falls back
//! to the **Chain arm** — a sparse-RPC word read via
//! [`TickBootstrapRpc`] (`Store → Db → Chain` precedence, epic `5NT2OC`).
//!
//! # Chain arm coverage semantics
//!
//! A Chain hit seeds only the **one** tick-bitmap word containing the pool's
//! current `tick`, so coverage is [`PoolTickCoverage::Sparse`] (NOT `Tracked`
//! — the live-pump miss-detection must still backfill neighbouring words via
//! `TickWordFetcher` during swap simulation). A Chain arm returning `None`
//! (all-zero bitmap) means the pool genuinely has no initialized ticks within
//! the current word — the caller registers with `tick_data=None,
//! coverage="sparse"` (mirrors Python Branch 3 when `bitmap_at_word == 0`).
//!
//! # Lock protocol (A4YUYJ — must-read before editing)
//!
//! The Store arm is a **closure** (`impl FnOnce() -> (HashMap<i32, TickInfo>,
//! PoolTickCoverage)`) rather than `&SnapshotStore<K>`. This is deliberate:
//! `SnapshotStore::take` is `&self` (interior mutability), so a `&store` borrow
//! would tie the caller's `BotState` read guard to the helper's *entire* call
//! — including the Db + Chain reads inside it. During `build_paths` the live
//! pump holds `state.write()` on the same `BotState` (`resume()` precedes
//! `build_paths`), so a guard held across an `SQLite` read or an RPC `eth_call`
//! would block pump Mint/Burn applies for every pool registered. The closure
//! sidesteps this: it runs under the guard, returns owned `(ticks, coverage)`,
//! drops the guard, and the helper continues with the `Option<&DegenbotDb>` (a
//! handle to a *separate* `Mutex<Connection>`, decoupled from `BotState`) and
//! then the `Option<&dyn TickBootstrapRpc>` (an RPC trait object, also no
//! `BotState` guard) — no `BotState` guard held across either subsequent read.
//! Two-phase locking, hidden inside one Rust call so the `PyO3` caller sees a
//! single `assemble_*` function. The Chain arm holds the same invariant: RPC
//! I/O runs with NO `BotState` guard (A4YUYJ's protocol holds end-to-end).
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
//!
//! # Chain error handling (Decision 8 (A) — extended to the Chain arm)
//!
//! A [`BootstrapTickError`] from the Chain arm is **propagated** as
//! [`TickMapAssemblyError::Chain`] — same loud-failure posture as the Db arm.
//! A transient RPC failure (timeout, transport, revert) surfaces as
//! [`BootstrapTickError::Rpc`]; a malformed return as
//! [`BootstrapTickError::InvalidReturn`]. Neither is swallowed: the caller
//! raises a typed exception rather than silently degrading to a solver-unsafe
//! empty state. If a retry-only-on-RPC-errors policy is later needed, match on
//! the variant at the `PyO3` boundary.

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
use degenbot_pools::tick_fetch::{BootstrapTickError, TickBootstrapRpc};

use crate::bot_core::{PoolTickCoverage, TickInfo};

/// The helper's error envelope: a `DbError` from the Db arm OR a
/// [`BootstrapTickError`] from the Chain arm. Both are propagated loudly
/// (Decision 8 (A)) — never swallowed. The `PyO3` wrapper maps each variant to
/// the appropriate Python exception (`RuntimeError` for Db, a typed RPC error
/// for Chain).
#[derive(Debug, thiserror::Error)]
pub enum TickMapAssemblyError {
    /// A Db read failure from the Db arm (Decision 8 (A) — propagated, not
    /// swallowed).
    #[error(transparent)]
    Db(#[from] DbError),
    /// A Chain-arm RPC failure (transport/timeout/revert or malformed return).
    /// Same loud-failure posture as the Db arm — the caller surfaces it as a
    /// typed exception rather than silently degrading.
    #[error(transparent)]
    Chain(#[from] BootstrapTickError),
}

/// The helper's return shape: an optional hit (`Some((ticks, coverage))` on
/// Store, Db, or Chain success) or a miss (`None`), with
/// [`TickMapAssemblyError`] propagated from the Db + Chain arms. A type alias
/// keeps the four call sites readable and silences `clippy::type_complexity`.
pub type TickMapAssemblyResult =
    Result<Option<(HashMap<i32, TickInfo>, PoolTickCoverage)>, TickMapAssemblyError>;

/// Assemble a V3 pool's tick map with `Store → Db → Chain` precedence.
///
/// 1. **Store arm** (probe): if the closure returns
///    [`PoolTickCoverage::Tracked`], the store hit is returned verbatim and
///    the Db + Chain arms are skipped (the store entry has been consumed by
///    `take`).
/// 2. **Db arm**: only on a store miss. `db.fetch_liquidity_map(address)`
///    is queried; a non-empty map (both `tick_bitmap` AND `tick_data`
///    populated — mirrors Python's `if not init_maps or not liq_positions`
///    heuristic) converts to `TickInfo` with `Tracked` coverage; an empty map
///    OR a pool-not-found (`Ok(None)`) falls through to the Chain arm; a
///    `Err(DbError)` is **propagated** (Decision 8 (A)).
/// 3. **Chain arm**: only on Store + Db miss AND `chain = Some`. Calls
///    [`TickBootstrapRpc::bootstrap_v3_tick_word`] for the word containing
///    `tick`; a hit returns `(ticks, Sparse)` (only one word seeded — the
///    live-pump miss-detection backfills neighbours); `None` (all-zero bitmap)
///    → helper returns `Ok(None)`; `Err(BootstrapTickError)` is **propagated**
///    as [`TickMapAssemblyError::Chain`] (Decision 8 (A) — same loud-failure
///    posture as the Db arm).
/// 4. `chain = None` (no RPC bootstrap wired): Store + Db only; miss if both
///    miss. `db = None` (cold-start, no Db handle): Store + Chain only.
///
/// Returns `Ok(Some((ticks, coverage)))` on a hit (Store=`Tracked`, Db=
/// `Tracked`, Chain=`Sparse`), `Ok(None)` on a miss, `Err(_)` on a Db or Chain
/// read failure.
///
/// # Errors
///
/// Propagates [`TickMapAssemblyError::Db`] from `fetch_liquidity_map` and
/// [`TickMapAssemblyError::Chain`] from `bootstrap_v3_tick_word` (Decision 8
/// (A) — neither is swallowed).
#[allow(
    clippy::too_many_arguments,
    reason = "precedence helper composes 3 arms; a params struct would obscure the Store closure"
)]
pub fn assemble_v3_tick_map(
    store_probe: impl FnOnce() -> (HashMap<i32, TickInfo>, PoolTickCoverage),
    db: Option<&DegenbotDb>,
    address: Address,
    tick: i32,
    tick_spacing: i32,
    block: u64,
    chain: Option<&dyn TickBootstrapRpc>,
) -> TickMapAssemblyResult {
    // 1. Store arm — closure holds the BotState read guard briefly.
    let (ticks, coverage) = store_probe();
    if coverage == PoolTickCoverage::Tracked {
        return Ok(Some((ticks, coverage)));
    }
    // 2. Db arm — separate Mutex<Connection>, no BotState guard.
    if let Some(db) = db {
        if let Some(db_hit) = fetch_v3_tick_map_from_db(db, address)? {
            return Ok(Some(db_hit));
        }
    }
    // 3. Chain arm — RPC trait object, no BotState guard. `chain=None` short-
    // circuits (no RPC bootstrap wired — current behavior preserved).
    let Some(chain) = chain else {
        return Ok(None);
    };
    let addr_str = address.to_checksum(None);
    let chain_hit = chain.bootstrap_v3_tick_word(&addr_str, tick, tick_spacing, block)?;
    Ok(chain_hit.map(|bt_word| (bt_word.ticks, PoolTickCoverage::Sparse)))
}

/// Assemble a V4 pool's tick map with `Store → Db → Chain` precedence.
///
/// V4 twin of [`assemble_v3_tick_map`]: the store is keyed by
/// `(Address, PoolId)`, the Db arm calls
/// `db.fetch_liquidity_map_v4(pool_manager, pool_id_hash)`, and the Chain arm
/// calls [`TickBootstrapRpc::bootstrap_v4_tick_word`] with `(state_view,
/// pool_id)` — `state_view` is the V4 `StateView` contract address (the contract
/// exposing `getTickBitmap`/`getTickLiquidity`, NOT the `PoolManager`).
/// Identical hit/miss/error semantics — see [`assemble_v3_tick_map`] for the
/// full contract.
///
/// # Errors
///
/// Propagates [`TickMapAssemblyError::Db`] from `fetch_liquidity_map_v4` and
/// [`TickMapAssemblyError::Chain`] from `bootstrap_v4_tick_word`.
#[allow(
    clippy::too_many_arguments,
    reason = "precedence helper composes 3 arms; a params struct would obscure the Store closure"
)]
pub fn assemble_v4_tick_map(
    store_probe: impl FnOnce() -> (HashMap<i32, TickInfo>, PoolTickCoverage),
    db: Option<&DegenbotDb>,
    pool_manager: Address,
    state_view: Address,
    pool_id: PoolId,
    tick: i32,
    tick_spacing: i32,
    block: u64,
    chain: Option<&dyn TickBootstrapRpc>,
) -> TickMapAssemblyResult {
    // 1. Store arm.
    let (ticks, coverage) = store_probe();
    if coverage == PoolTickCoverage::Tracked {
        return Ok(Some((ticks, coverage)));
    }
    // 2. Db arm.
    if let Some(db) = db {
        if let Some(db_hit) = fetch_v4_tick_map_from_db(db, pool_manager, pool_id)? {
            return Ok(Some(db_hit));
        }
    }
    // 3. Chain arm.
    let Some(chain) = chain else {
        return Ok(None);
    };
    let mgr_str = state_view.to_checksum(None);
    let chain_hit = chain.bootstrap_v4_tick_word(&mgr_str, &pool_id, tick, tick_spacing, block)?;
    Ok(chain_hit.map(|bt_word| (bt_word.ticks, PoolTickCoverage::Sparse)))
}

/// Db arm for V3: convert a `LiquidityMap` into the helper's hit/miss shape.
/// Returns `Ok(None)` on an empty map (falls through to the Chain arm).
fn fetch_v3_tick_map_from_db(db: &DegenbotDb, address: Address) -> TickMapAssemblyResult {
    let Some(map) = db.fetch_liquidity_map(address)? else {
        return Ok(None);
    };
    Ok(liquidity_map_to_tick_info(map))
}

/// Db arm for V4: identical to V3 but routes through the V4 fetch.
/// Returns `Ok(None)` on an empty map (falls through to the Chain arm).
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

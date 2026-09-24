//! Tick-map assembly: the one `Db → Chain` precedence for CL pools.
//!
//! [`resolve_tick_map_arm`] is the single precedence decision — the Db read
//! first, then (on a Db miss) the Chain transport — shared by every caller:
//! the sync `assemble_v3_tick_map` / `assemble_v4_tick_map` wrappers, the
//! async-native `pool_builder` registration arms, and the per-frame
//! `pool_ingress`. Db conversion via [`liquidity_map_to_tick_info`], the
//! Tracked intake reconciliation, Tracked-empty handling, coverage tagging,
//! and the forensic dump live only here, and both arms mint their
//! [`TickMapSeed`] through [`TickMapArm::into_seed`], so no transport can
//! drift the semantics.
//!
//! The Chain transport deliberately keeps two adapters: the sync
//! [`TickBootstrapRpc`] trait object (used by the sync wrappers and the
//! per-frame ingress) and the builder's async-native `ConstructionIo`
//! bootstrap. The builder runs on the async registration runtime and cannot
//! `block_on` the sync helper (the nested-`block_on` deadlock class), so the
//! transport forks while the precedence does not. The former `Store` arm was
//! retired (the in-memory `SnapshotStore` is replaced by a WAL held read
//! transaction so every per-pool read during `build_paths` shares one frozen
//! DB cut).
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
//! //! # Lock protocol (A4YUYJ — must-read before editing)
//!
//! The Db arm reads through `Option<&dyn TickMapDb>` (a handle to a
//! *separate* `Mutex<Connection>` — the `SnapshotDb` held-tx or a per-call
//! `DegenbotDb`), decoupled from `BotState`. The Chain arm is an RPC trait
//! object, also with no `BotState` guard. The former `Store` arm's closure
//! (which briefly held a `BotState` read guard) is retired — the
//! per-pool `fetch_liquidity_map` reads the DB directly with no `BotState`
//! guard held across an `SQLite` read or an RPC `eth_call`. During
//! `build_paths` the live pump holds `state.write()` on the same `BotState`
//! (`resume()` precedes `build_paths`), so NO `BotState` guard may be held
//! across either the Db or Chain read. The Chain arm holds the same
//! invariant: RPC I/O runs with NO `BotState` guard (A4YUYJ's protocol holds
//! end-to-end).
//!
//! # Db error handling (Decision 8 (A) — behavior change)
//!
//! Today the Python builder wraps the Db snapshot read in
//! `contextlib.suppress(Exception)`: a transient `database is locked` under
//! the concurrent updater process is swallowed and the builder silently falls
//! through to sparse RPC. This helper **propagates** `DbError` instead — the
//! `PyO3` wrapper raises `RuntimeError`. The trade-off is
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

use hashbrown::HashMap;

use alloy::primitives::{Address, U256};

use degenbot_db::error::DbError;
use degenbot_db::snapshot::{BitmapAtWord, LiquidityAtTick, LiquidityMap};
use degenbot_decoders::v4_swap_decoder::V4PoolId;
// `PoolTickCoverage` + `TickInfo` live in `degenbot_pools` but aren't at its
// crate root; `bot_core::mod` re-exports them via `pub use v3_state::…` +
// `pub use ::degenbot_pools::TickInfo`. Use those re-exports to avoid coupling
// this submodule to `degenbot_pools::v3_state`'s private path.
use degenbot_pools::tick_fetch::{BootstrapTickError, TickBootstrapRpc};

use crate::bot_core::planning::{TickMapPoolIdentity, TickMapSeed};
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
    /// A Tracked Db snapshot that contradicts itself: a bitmap bit and the
    /// liquidity rows disagree about an initialization (T3 OMDCIY, epic
    /// ). Registration is rejected AT INTAKE — the two-step verify
    /// is the on-chain oracle, but a corrupted snapshot must never
    /// register. `tick` is the conflicting position (`i32::MIN` marks an
    /// out-of-range corrupted word position).
    #[error(
        "Tracked tick map inconsistent at intake: word {word} bit {bit} (tick {tick}) — bitmap_bit = {bitmap_bit}, row_gross_positive = {row_gross_positive}"
    )]
    InconsistentTickMap {
        word: i64,
        bit: u32,
        tick: i32,
        bitmap_bit: bool,
        row_gross_positive: bool,
    },
}

/// The helper's return shape: an optional hit (`Some((ticks, coverage))` on
/// Db or Chain success) or a miss (`None`), with
/// [`TickMapAssemblyError`] propagated from the Db + Chain arms. A type alias
/// keeps the four call sites readable and silences `clippy::type_complexity`.
pub type TickMapAssemblyResult =
    Result<Option<(HashMap<i32, TickInfo>, PoolTickCoverage)>, TickMapAssemblyError>;

/// Compact serialize of a `HashMap<i32, TickInfo>` (ascending tick) into
/// `tick:gross,net;...`, for the snapshot-seed dump (ADR-021
/// re-assembly aid).
fn serialize_tick_info_map(ticks: &HashMap<i32, TickInfo>) -> String {
    let mut keys: Vec<&i32> = ticks.keys().collect();
    keys.sort_unstable();
    keys.iter()
        .map(|t| {
            let ti = &ticks[*t];
            format!("{t}:{},{}", ti.liquidity_gross, ti.liquidity_net)
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Emit the snapshot-seed tick map (Db snapshot + backfill) at TRACE on
/// `state`, so it can be compared against the map that later went into the
/// verifier (ADR-021 re-assembly aid). Forensic: silent unless the sink
/// enables `degenbot=trace`.
pub(crate) fn dump_tick_map_seed(
    pool_ident: &str,
    seed: &(HashMap<i32, TickInfo>, PoolTickCoverage),
) {
    degenbot_core::diag_trace!(domain = state, pool = %pool_ident,
        seed_origin = "db-snapshot",
        coverage = ?seed.1,
        tick_count = seed.0.len(),
        seed_map = %serialize_tick_info_map(&seed.0),
        "tick-map snapshot-seed (ADR-021 re-assembly aid)"
    );
}

/// Which arm supplied a resolved CL tick map — the precedence decision before
/// the seed vocabulary stamps provenance.
pub(crate) enum TickMapArm {
    /// A Db hit: the complete map, or a legitimately-empty `Tracked` pool.
    Db(HashMap<i32, TickInfo>, HashMap<i32, U256>, PoolTickCoverage),
    /// A Chain hit: the single bitmap word the transport returned, always
    /// `Sparse`.
    Chain(HashMap<i32, TickInfo>, HashMap<i32, U256>),
    /// The Chain transport ran and returned no word (all-zero bitmap), or no
    /// Chain transport is wired.
    ChainMiss,
}

impl TickMapArm {
    /// Stamp this arm into the single seed vocabulary. The one mint for BOTH
    /// arms: Db provenance, Chain provenance, and the `ChainMiss` → empty
    /// `Sparse` semantics cannot drift between transports.
    #[must_use]
    pub(crate) fn into_seed(self, block: u64, identity: TickMapPoolIdentity) -> TickMapSeed {
        match self {
            Self::Db(ticks, bitmaps, coverage) => {
                TickMapSeed::db(ticks, bitmaps, coverage, block, Some(block), identity)
            }
            Self::Chain(ticks, bitmaps) => {
                TickMapSeed::chain(ticks, bitmaps, PoolTickCoverage::Sparse, block, identity)
            }
            Self::ChainMiss => TickMapSeed::chain(
                HashMap::new(),
                HashMap::new(),
                PoolTickCoverage::Sparse,
                block,
                identity,
            ),
        }
    }

    /// Project this arm onto the historical sync `assemble_*_tick_map`
    /// contract, which reports a Chain miss as `None`.
    #[must_use]
    fn into_sync_hit(self) -> Option<(HashMap<i32, TickInfo>, PoolTickCoverage)> {
        match self {
            Self::Db(ticks, _, coverage) => Some((ticks, coverage)),
            Self::Chain(ticks, _) => Some((ticks, PoolTickCoverage::Sparse)),
            Self::ChainMiss => None,
        }
    }
}

/// The one `Db → Chain` precedence decision for CL tick maps.
///
/// The Db read is always attempted first, and its outcome alone decides
/// whether the Chain transport runs. `Ok(Some(seed))` means the Db arm is
/// authoritative — a complete map, or a Db-registered pool with no mapped
/// liquidity ("legitimately-empty `Tracked`", never degraded to `Sparse`).
/// `Ok(None)` means the pool is absent from the Db, so the caller runs its
/// Chain transport and mints through [`TickMapArm::into_seed`].
///
/// Db conversion ([`liquidity_map_to_tick_info`]), the Tracked intake
/// reconciliation, coverage tagging, the forensic dump, and the loud error
/// posture all live here, so every transport shares identical semantics. The
/// Chain transport stays outside because it keeps two adapters — the sync
/// [`TickBootstrapRpc`] trait object and the builder's async-native
/// `ConstructionIo`, which cannot `block_on` the sync helper (the
/// nested-`block_on` deadlock class; see the module doc). Both mint through
/// [`TickMapArm::into_seed`].
///
/// # Errors
///
/// Propagates the caller's Db read error and
/// [`TickMapAssemblyError::InconsistentTickMap`] from the Tracked intake
/// reconciliation, mapped through `E`.
pub(crate) fn resolve_tick_map_arm<E, FDb>(
    pool_ident: &str,
    identity: TickMapPoolIdentity,
    tick_spacing: i32,
    block: u64,
    db_read: FDb,
) -> Result<Option<TickMapSeed>, E>
where
    E: From<TickMapAssemblyError>,
    FDb: FnOnce() -> Result<Option<LiquidityMap>, E>,
{
    // 1. Db arm — held `SnapshotDb` tx (or per-call `DegenbotDb`), no BotState
    // guard.
    let Some(map) = db_read()? else {
        return Ok(None); // pool not in Db -> caller runs the Chain transport
    };
    // Pool IS in the Db: a non-empty map -> Tracked + populated; an empty map
    // -> legitimately-empty Tracked (authoritative — it came from the Db).
    let bitmaps = bitmaps_from_db(&map.tick_bitmap);
    let arm = match liquidity_map_to_tick_info(map, tick_spacing).map_err(E::from)? {
        Some(hit) => {
            dump_tick_map_seed(pool_ident, &hit);
            TickMapArm::Db(hit.0, bitmaps, hit.1)
        }
        None => TickMapArm::Db(HashMap::new(), bitmaps, PoolTickCoverage::Tracked),
    };
    Ok(Some(arm.into_seed(block, identity)))
}

/// Mint the Chain arm from a transport's raw hit: `Some(ticks)` is a `Sparse`
/// word, `None` is a miss (all-zero bitmap, or no transport wired). The shared
/// chain-side mint for both transports.
#[must_use]
pub(crate) fn chain_arm(
    ticks: Option<HashMap<i32, TickInfo>>,
    bitmaps: HashMap<i32, U256>,
) -> TickMapArm {
    match ticks {
        Some(ticks) => TickMapArm::Chain(ticks, bitmaps),
        None => TickMapArm::ChainMiss,
    }
}

fn bitmaps_from_db(bitmaps: &HashMap<i64, BitmapAtWord>) -> HashMap<i32, U256> {
    bitmaps
        .iter()
        .map(|(&word, entry)| (i32::try_from(word).unwrap_or(i32::MAX), entry.bitmap))
        .collect()
}

/// Assemble a V3 pool's tick map with `Db → Chain` precedence.
///
/// Tries the Db snapshot arm first (through [`resolve_tick_map_arm`]); on a Db
/// miss, falls back to the sync [`TickBootstrapRpc`] Chain transport and mints
/// a `Sparse` seed. A Chain miss (all-zero bitmap / no transport) is reported
/// as `None`, the historical sync contract.
///
/// # Errors
///
/// Propagates [`TickMapAssemblyError::Db`] from the Db arm and
/// [`TickMapAssemblyError::Chain`] from the Chain arm — neither is swallowed
/// (Decision 8 (A)).
pub fn assemble_v3_tick_map(
    db: Option<&dyn degenbot_db::snapshot::TickMapDb>,
    address: Address,
    tick: i32,
    tick_spacing: i32,
    block: u64,
    chain: Option<&dyn TickBootstrapRpc>,
) -> TickMapAssemblyResult {
    let db_arm = resolve_tick_map_arm::<TickMapAssemblyError, _>(
        &format!("{address}"),
        TickMapPoolIdentity::V3(address),
        tick_spacing,
        block,
        || match db {
            Some(db) => db
                .fetch_liquidity_map(address)
                .map_err(TickMapAssemblyError::Db),
            None => Ok(None),
        },
    )?;
    if let Some(seed) = db_arm {
        return Ok(Some((seed.ticks, seed.coverage)));
    }
    // 2. Chain arm — RPC trait object, no BotState guard. `chain=None`
    // short-circuits (no RPC bootstrap wired).
    let chain_hit = match chain {
        Some(chain) => chain
            .bootstrap_v3_tick_word(&address.to_checksum(None), tick, tick_spacing, block)
            .map_err(TickMapAssemblyError::Chain)?
            .map(|word| (word.ticks, HashMap::from([(word.word, word.bitmap)]))),
        None => None,
    };
    Ok(match chain_hit {
        Some((ticks, bitmaps)) => chain_arm(Some(ticks), bitmaps).into_sync_hit(),
        None => None,
    })
}

/// Assemble a V4 pool's tick map with `Db → Chain` precedence.
///
/// V4 twin of [`assemble_v3_tick_map`]: the Db arm calls
/// `db.fetch_liquidity_map_v4(pool_manager, pool_id_hash)`, and the Chain arm
/// calls [`TickBootstrapRpc::bootstrap_v4_tick_word`] with `(state_view,
/// pool_id)` — `state_view` is the V4 `StateView` contract address (the
/// contract exposing `getTickBitmap`/`getTickLiquidity`, NOT the
/// `PoolManager`). Identical hit/miss/error semantics; both arms share
/// [`resolve_tick_map_arm`] and [`TickMapArm::into_seed`].
///
/// # Errors
///
/// Propagates [`TickMapAssemblyError::Db`] from `fetch_liquidity_map_v4` and
/// [`TickMapAssemblyError::Chain`] from `bootstrap_v4_tick_word`.
#[expect(
    clippy::too_many_arguments,
    reason = "precedence helper composes 2 arms; a params struct would obscure it"
)]
pub fn assemble_v4_tick_map(
    db: Option<&dyn degenbot_db::snapshot::TickMapDb>,
    pool_manager: Address,
    state_view: Address,
    pool_id: V4PoolId,
    tick: i32,
    tick_spacing: i32,
    block: u64,
    chain: Option<&dyn TickBootstrapRpc>,
) -> TickMapAssemblyResult {
    // `fetch_liquidity_map_v4` takes a `B256`; `V4PoolId` is `[u8; 32]` and
    // `B256` is `FixedBytes<32>` — same layout, so the conversion is
    // infallible.
    let pool_id_hash = alloy::primitives::B256::from(pool_id);
    let db_arm = resolve_tick_map_arm::<TickMapAssemblyError, _>(
        &alloy::hex::encode_prefixed(pool_id),
        TickMapPoolIdentity::V4 {
            manager: pool_manager,
            pool_id: pool_id_hash,
        },
        tick_spacing,
        block,
        || match db {
            Some(db) => db
                .fetch_liquidity_map_v4(pool_manager, pool_id_hash)
                .map_err(TickMapAssemblyError::Db),
            None => Ok(None),
        },
    )?;
    if let Some(seed) = db_arm {
        return Ok(Some((seed.ticks, seed.coverage)));
    }
    // 2. Chain arm.
    let chain_hit = match chain {
        Some(chain) => chain
            .bootstrap_v4_tick_word(
                &state_view.to_checksum(None),
                &pool_id,
                tick,
                tick_spacing,
                block,
            )
            .map_err(TickMapAssemblyError::Chain)?
            .map(|word| (word.ticks, HashMap::from([(word.word, word.bitmap)]))),
        None => None,
    };
    Ok(match chain_hit {
        Some((ticks, bitmaps)) => chain_arm(Some(ticks), bitmaps).into_sync_hit(),
        None => None,
    })
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
pub(crate) fn liquidity_map_to_tick_info(
    map: LiquidityMap,
    tick_spacing: i32,
) -> TickMapAssemblyResult {
    if map.tick_bitmap.is_empty() || map.tick_data.is_empty() {
        return Ok(None);
    }
    // Tracked intake reconciliation (T3 OMDCIY) — a self-contradictory
    // snapshot is rejected before registration, not discovered mid-solve.
    verify_tracked_tick_map(&map.tick_bitmap, &map.tick_data, tick_spacing)?;
    let ticks = convert_liquidity_at_tick(map.tick_data);
    Ok(Some((ticks, PoolTickCoverage::Tracked)))
}

/// Tracked intake reconciliation : the on-chain
/// invariant, checked per word the snapshot supplied — a bit is set iff a
/// tick row with `liquidity_gross > 0` exists at that position. The row side
/// checks every gross>0 row whose word the snapshot carries (the bit must be
/// set); the bitmap side checks every set bit of a supplied word (a gross>0
/// row must exist at that position's grid tick). Rows in words the snapshot
/// did NOT supply are skipped (an incremental snapshot may carry a word
/// subset). A fully-zero word is legal (a checked-empty word — T1) so long
/// as no gross>0 row sits in it.
#[expect(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
pub(crate) fn verify_tracked_tick_map(
    tick_bitmap: &HashMap<i64, BitmapAtWord>,
    tick_data: &HashMap<i32, LiquidityAtTick>,
    tick_spacing: i32,
) -> Result<(), TickMapAssemblyError> {
    let spacing = tick_spacing.max(1);
    // Row side: a gross>0 row in a supplied word must have its bit set.
    for (&tick, row) in tick_data {
        if row.liquidity_gross.is_zero() {
            continue;
        }
        let compressed = tick.div_euclid(spacing);
        let word = i64::from(compressed >> 8);
        let Some(entry) = tick_bitmap.get(&word) else {
            continue;
        };
        let bit = compressed.rem_euclid(256) as usize;
        if !entry.bitmap.bit(bit) {
            return Err(TickMapAssemblyError::InconsistentTickMap {
                word,
                bit: bit as u32,
                tick,
                bitmap_bit: false,
                row_gross_positive: true,
            });
        }
    }
    // Bitmap side: every set bit of a supplied word needs a gross>0 row at
    // that position's grid tick.
    for (&word, entry) in tick_bitmap {
        for bit in 0..256usize {
            if !entry.bitmap.bit(bit) {
                continue;
            }
            let compressed = word * 256 + bit as i64; // bit < 256 (covered by fn-level expect)
            let Some(tick) = (compressed * i64::from(spacing)).try_into().ok() else {
                return Err(TickMapAssemblyError::InconsistentTickMap {
                    word,
                    bit: bit as u32,
                    tick: i32::MIN,
                    bitmap_bit: true,
                    row_gross_positive: false,
                });
            };
            let row_gross_positive = tick_data
                .get(&tick)
                .is_some_and(|r| !r.liquidity_gross.is_zero());
            if !row_gross_positive {
                return Err(TickMapAssemblyError::InconsistentTickMap {
                    word,
                    bit: bit as u32,
                    tick,
                    bitmap_bit: true,
                    row_gross_positive: false,
                });
            }
        }
    }
    Ok(())
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
    tick_data
        .into_iter()
        .map(|(tick, lat)| {
            (
                tick,
                TickInfo {
                    liquidity_gross: lat.liquidity_gross.to::<alloy::primitives::U128>(),
                    liquidity_net: lat.liquidity_net,
                    block: 0,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests;

//! V3/V4 DB-aware liquidity updater — the apply-and-persist core (task QJSCA5).
//!
//! Ports `cli/pool.py::apply_v3_liquidity_updates` / `apply_v4_liquidity_updates`
//! (read pool row + positions → apply CL `apply_liquidity_mapping_update` per
//! event → upsert positions + init maps → stamp `liquidity_update_block`/
//! `liquidity_update_log_index`) to a Rust core over [`DegenbotDb`] +
//! [`degenbot_cl_math::cl_lib::liquidity_mapping::apply_liquidity_mapping_update`].
//!
//! # What this is (and isn't)
//!
//! This is the **apply-and-persist core** (`port-now` per the §2.1 rubric): a
//! pure row→math→row transform over the DB substrate. The Python `pool_update`
//! driver loop + RPC event fetch (`get_v3/v4_liquidity_events`/`fetch_logs_retrying`)
//! STAY PYTHON (orchestration + RPC; `degenbot-rpc` event-fetch port is a
//! separate concern not in this epic). The math itself lives in
//! [`degenbot-cl-math`] (sibling task). The standalone-Rust path this enables:
//! `DB events → apply_v3/v4_liquidity_updates → upserted DB rows` without Python.
//!
//! # Decomposition across V3 + V4
//!
//! V3 + V4 share the core apply loop (reconstitute `LiquidityMap` → loop events
//! calling `apply_liquidity_mapping_update` → write back); the deltas are:
//!
//! - **Row key:** V3 selects `pools.id` + `tick_spacing` from `pools`; V4
//!   selects `managed_pools.id` (= `uniswap_v4_pools.managed_pool_id`) +
//!   `tick_spacing` from `uniswap_v4_pools` joined to `pool_managers`.
//! - **Position/init-map tables:** V3 writes `liquidity_positions` /
//!   `initialization_maps` (keyed `pool_id`); V4 writes
//!   `managed_pool_liquidity_positions` / `managed_pool_initialization_maps`
//!   (keyed `managed_pool_id`).
//! - **Event decode:** V3 picks `tick_lower`/`tick_upper` from `topics[2..3]`
//!   and a Burn/Mint-aware `liquidity_delta` decode (Burn negates); V4 decodes
//!   `tick_lower`/`tick_upper`/`liquidity_delta` from the `data` blob.
//!
//! The substrate write fns ([`upsert_liquidity_positions`] etc.) are split into
//! `v3_`/`v4_` variants so each mirrors its Python callsite's exact key + table.

use std::collections::HashMap;
use std::str::FromStr;

use alloy::primitives::{I256, U128, U256};
use degenbot_cl_math::cl_lib::liquidity_mapping::{
    apply_liquidity_mapping_update, BitmapAtWord, LiquidityAtTick,
};

use crate::connection::DegenbotDb;
use crate::error::DbError;
use crate::rows::decode::{decode_u256, encode_u256};
use crate::schema::table::{
    INITIALIZATION_MAPS, LIQUIDITY_POSITIONS, MANAGED_POOLS, MANAGED_POOL_INITIALIZATION_MAPS,
    MANAGED_POOL_LIQUIDITY_POSITIONS, POOLS, POOL_MANAGERS, UNISWAP_V4_POOLS,
};

/// The reconstituted liquidity map (tick bitmap + tick data) the apply loop
/// mutates — the cl-math types directly.
type LiquidityMap = (HashMap<i32, BitmapAtWord>, HashMap<i32, LiquidityAtTick>);

/// One liquidity event decoded into the (`tick_lower`, `tick_upper`, `delta`,
/// `block`, `log_index`) tuple the apply loop consumes.
///
/// `liquidity_delta` is signed (Burn events are negative; Mint/V4 Modify
/// already signed). The driver loop (Python) decodes the raw log receipt +
/// builds this record; the Rust core applies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiquidityUpdateEvent {
    /// The pool row's stored `liquidity_update_block` guard lower bound — must
    /// be `>=` the last-applied block.
    pub block_number: u64,
    /// The log index within `block_number` (tiebreaker for same-block events).
    pub log_index: u64,
    /// The lower tick of the modified range.
    pub tick_lower: i32,
    /// The upper tick of the modified range.
    pub tick_upper: i32,
    /// Signed liquidity delta (Burn = negative; Mint = positive; V4 Modify =
    /// signed already).
    pub liquidity_delta: I256,
}

/// A decoded pool row the updater mutates against: the `pool_id` (the rows'
/// composite-key first half) + `tick_spacing` + the optional
/// `liquidity_update_block`/`log_index` guard stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PoolUpdateState {
    /// V3: `pools.id`; V4: `managed_pools.id` (= `uniswap_v4_pools.managed_pool_id`).
    pub pool_id: i64,
    /// The pool's tick spacing (V3: `pools.tick_spacing`; V4: `uniswap_v4_pools.tick_spacing`).
    pub tick_spacing: i32,
    /// The last-applied event's `(block, log_index)` stamp; `None` if no event
    /// has been applied yet (the guard is skipped on the first event).
    pub last_update: Option<BlockLog>,
}

/// A `(block_number, log_index)` pair — the per-pool apply-progress marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockLog {
    /// The block number.
    pub block: u64,
    /// The log index within that block.
    pub log_index: u64,
}

const SQLITE_MAX_VARIABLES: usize = 32_766;
/// `liquidity_positions` rows bind 4 vars each (`pool_id`, `tick`, `net`, `gross`).
const POSITION_KEYS_PER_ROW: usize = 4;
/// `initialization_maps` rows bind 3 vars each (`pool_id`, `word`, `bitmap`).
const INIT_MAP_KEYS_PER_ROW: usize = 3;

impl DegenbotDb {
    /// Load the V3 pool's updater state — `pool_id` (= `pools.id`),
    /// `tick_spacing`, + the `liquidity_update_block`/`log_index` guard stamp.
    /// Returns `Ok(None)` if no matching pool row exists.
    ///
    /// Mirrors the Python `session.scalar(select(LiquidityPoolTable).where(
    /// address==pool_address, chain==chain_id))` reconstitution step.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on
    /// a malformed column.
    pub fn fetch_v3_pool_update_state(
        &self,
        chain_id: i64,
        pool_address: &str,
    ) -> Result<Option<PoolUpdateState>, DbError> {
        let conn = self.lock();
        let row: Option<(i64, i32, Option<i64>, Option<i64>)> = conn
            .query_row(
                &format!(
                    "SELECT id, tick_spacing, liquidity_update_block, \
                     liquidity_update_log_index FROM {POOLS} \
                     WHERE chain = ?1 AND address = ?2 LIMIT 1"
                ),
                rusqlite::params![chain_id, pool_address],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(
            row.map(|(pool_id, tick_spacing, block, log_idx)| PoolUpdateState {
                pool_id,
                tick_spacing,
                last_update: match (block, log_idx) {
                    (Some(b), Some(l)) => Some(BlockLog {
                        block: u64::try_from(b).unwrap_or(0),
                        log_index: u64::try_from(l).unwrap_or(0),
                    }),
                    _ => None,
                },
            }),
        )
    }

    /// Load the V4 pool's updater state — `pool_id` (= `managed_pools.id`),
    /// `tick_spacing`, + the `liquidity_update_block`/`log_index` guard stamp.
    /// Returns `Ok(None)` if no matching pool row exists (joins
    /// `uniswap_v4_pools` × `managed_pools` × `pool_managers`).
    ///
    /// Mirrors the Python `session.scalar(select(UniswapV4PoolTable).where(
    /// pool_hash==..., manager.has(chain==pool_manager.chain)))` reconstitution.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on
    /// a malformed column.
    pub fn fetch_v4_pool_update_state(
        &self,
        pool_hash: &str,
        pool_manager_chain: i64,
    ) -> Result<Option<PoolUpdateState>, DbError> {
        let conn = self.lock();
        let row: Option<(i64, i32, Option<i64>, Option<i64>)> = conn
            .query_row(
                &format!(
                    "SELECT {MANAGED_POOLS}.id, {UNISWAP_V4_POOLS}.tick_spacing, \
                     {UNISWAP_V4_POOLS}.liquidity_update_block, \
                     {UNISWAP_V4_POOLS}.liquidity_update_log_index \
                     FROM {UNISWAP_V4_POOLS} \
                     JOIN {MANAGED_POOLS} ON {MANAGED_POOLS}.id = \
                     {UNISWAP_V4_POOLS}.managed_pool_id \
                     JOIN {POOL_MANAGERS} ON {POOL_MANAGERS}.id = \
                     {MANAGED_POOLS}.manager_id \
                     WHERE {UNISWAP_V4_POOLS}.pool_hash = ?1 \
                       AND {POOL_MANAGERS}.chain = ?2 LIMIT 1"
                ),
                rusqlite::params![pool_hash, pool_manager_chain],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(
            row.map(|(pool_id, tick_spacing, block, log_idx)| PoolUpdateState {
                pool_id,
                tick_spacing,
                last_update: match (block, log_idx) {
                    (Some(b), Some(l)) => Some(BlockLog {
                        block: u64::try_from(b).unwrap_or(0),
                        log_index: u64::try_from(l).unwrap_or(0),
                    }),
                    _ => None,
                },
            }),
        )
    }

    /// Reconstitute the V3 [`LiquidityMap`] (snapshot's `tick_bitmap` +
    /// `tick_data`) from the `liquidity_positions` + `initialization_maps`
    /// rows for `pool_id`. Mirrors the Python `pool_liquidity_map.model_construct`
    /// reconstitution; produces the cl-math types directly (no `U256`-bridge
    /// round-trip — `liquidity_net` is decoded as `I256`, `liquidity_gross` as
    /// low-128 `U128`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on
    /// a malformed column.
    pub fn fetch_v3_liquidity_map(&self, pool_id: i64) -> Result<LiquidityMap, DbError> {
        let conn = self.lock();
        let mut tick_bitmap: HashMap<i32, BitmapAtWord> = HashMap::new();
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT word, bitmap FROM {INITIALIZATION_MAPS} WHERE pool_id = ?1"
            ))?;
            let rows = stmt.query_map(rusqlite::params![pool_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            for r in rows {
                let (word, bitmap_str) = r?;
                let word: i32 = i32::try_from(word)
                    .map_err(|e| DbError::Decode(format!("word {word} out of i32 range: {e}")))?;
                tick_bitmap.insert(
                    word,
                    BitmapAtWord {
                        bitmap: decode_u256(&bitmap_str)?,
                        block: 0,
                    },
                );
            }
        }
        let mut tick_data: HashMap<i32, LiquidityAtTick> = HashMap::new();
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT tick, liquidity_gross, liquidity_net FROM {LIQUIDITY_POSITIONS} \
                 WHERE pool_id = ?1"
            ))?;
            let rows = stmt.query_map(rusqlite::params![pool_id], |r| {
                Ok((
                    r.get::<_, i32>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for r in rows {
                let (tick, gross_str, net_str) = r?;
                tick_data.insert(
                    tick,
                    LiquidityAtTick {
                        liquidity_gross: u256_to_u128(&decode_u256(&gross_str)?),
                        liquidity_net: decode_i256(&net_str)?,
                        block: 0,
                    },
                );
            }
        }
        Ok((tick_bitmap, tick_data))
    }

    /// Reconstitute the V4 [`LiquidityMap`] from the
    /// `managed_pool_liquidity_positions` + `managed_pool_initialization_maps`
    /// rows for `managed_pool_id`. Mirrors the Python V4 reconstitution.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on
    /// a malformed column.
    pub fn fetch_v4_liquidity_map(&self, managed_pool_id: i64) -> Result<LiquidityMap, DbError> {
        let conn = self.lock();
        let mut tick_bitmap: HashMap<i32, BitmapAtWord> = HashMap::new();
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT word, bitmap FROM {MANAGED_POOL_INITIALIZATION_MAPS} \
                 WHERE managed_pool_id = ?1"
            ))?;
            let rows = stmt.query_map(rusqlite::params![managed_pool_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            for r in rows {
                let (word, bitmap_str) = r?;
                let word: i32 = i32::try_from(word)
                    .map_err(|e| DbError::Decode(format!("word {word} out of i32 range: {e}")))?;
                tick_bitmap.insert(
                    word,
                    BitmapAtWord {
                        bitmap: decode_u256(&bitmap_str)?,
                        block: 0,
                    },
                );
            }
        }
        let mut tick_data: HashMap<i32, LiquidityAtTick> = HashMap::new();
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT tick, liquidity_gross, liquidity_net FROM \
                 {MANAGED_POOL_LIQUIDITY_POSITIONS} WHERE managed_pool_id = ?1"
            ))?;
            let rows = stmt.query_map(rusqlite::params![managed_pool_id], |r| {
                Ok((
                    r.get::<_, i32>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for r in rows {
                let (tick, gross_str, net_str) = r?;
                tick_data.insert(
                    tick,
                    LiquidityAtTick {
                        liquidity_gross: u256_to_u128(&decode_u256(&gross_str)?),
                        liquidity_net: decode_i256(&net_str)?,
                        block: 0,
                    },
                );
            }
        }
        Ok((tick_bitmap, tick_data))
    }

    /// Apply a sequence of [`LiquidityUpdateEvent`]s to the V3 pool identified by
    /// `(chain_id, pool_address)` — the core apply-and-persist port of the
    /// Python `apply_v3_liquidity_updates`.
    ///
    /// Reads the pool row + its current `LiquidityMap`, loops the events calling
    /// [`apply_liquidity_mapping_update`] (the per-event tick/bitmap mutation),
    /// guarded by the block/log-index ordering invariants, then writes back:
    /// delete stale positions/init-maps, upsert the live ones, + stamp the
    /// pool row's `liquidity_update_block`/`liquidity_update_log_index` with the
    /// LAST event's `(block, log_index)`.
    ///
    /// Returns `Ok(false)` if the pool row isn't found (mirrors the Python
    /// `if pool_in_db is None: return`); `Ok(true)` after a successful apply.
    ///
    /// # Panics (debug builds only)
    ///
    /// Panics if an event violates the block/log-index ordering invariant
    /// (matches the Python `assert`s — stripped under `python -O` / `release`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any query/upsert failure or
    /// [`DbError::Decode`] on a malformed column.
    pub fn apply_v3_liquidity_updates(
        &self,
        chain_id: i64,
        pool_address: &str,
        events: &[LiquidityUpdateEvent],
    ) -> Result<bool, DbError> {
        let Some(state) = self.fetch_v3_pool_update_state(chain_id, pool_address)? else {
            return Ok(false);
        };
        let (mut tick_bitmap, mut tick_data) = self.fetch_v3_liquidity_map(state.pool_id)?;
        let pool_id = state.pool_id;

        let mut current_liquidity = U128::ZERO;
        let last_event = apply_event_loop(
            &mut tick_bitmap,
            &mut tick_data,
            &mut current_liquidity,
            state,
            events,
        );

        persist_v3(self, pool_id, &tick_bitmap, &tick_data, last_event)?;
        Ok(true)
    }

    /// Apply a sequence of [`LiquidityUpdateEvent`]s to the V4 pool identified by
    /// `(pool_hash, pool_manager_chain)` — the core apply-and-persist port of
    /// the Python `apply_v4_liquidity_updates`.
    ///
    /// See [`Self::apply_v3_liquidity_updates`] for the loop semantics; V4
    /// differs only in the row lookups + tables written.
    ///
    /// # Panics (debug builds only)
    ///
    /// Panics if an event violates the block/log-index ordering invariant.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any query/upsert failure or
    /// [`DbError::Decode`] on a malformed column.
    pub fn apply_v4_liquidity_updates(
        &self,
        pool_hash: &str,
        pool_manager_chain: i64,
        events: &[LiquidityUpdateEvent],
    ) -> Result<bool, DbError> {
        let Some(state) = self.fetch_v4_pool_update_state(pool_hash, pool_manager_chain)? else {
            return Ok(false);
        };
        let (mut tick_bitmap, mut tick_data) = self.fetch_v4_liquidity_map(state.pool_id)?;
        let pool_id = state.pool_id;

        let mut current_liquidity = U128::ZERO;
        let last_event = apply_event_loop(
            &mut tick_bitmap,
            &mut tick_data,
            &mut current_liquidity,
            state,
            events,
        );

        persist_v4(self, pool_id, &tick_bitmap, &tick_data, last_event)?;
        Ok(true)
    }

    /// Set the V3 pool's `liquidity_update_block`/`liquidity_update_log_index`
    /// stamp to `(block, log_index)` — mirrors the Python
    /// `pool_in_db.liquidity_update_block = ...`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on an UPDATE failure.
    pub fn set_v3_liquidity_update_marker(
        &self,
        pool_id: i64,
        block: u64,
        log_index: u64,
    ) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute(
            &format!(
                "UPDATE {POOLS} SET liquidity_update_block = ?1, \
                 liquidity_update_log_index = ?2 WHERE id = ?3"
            ),
            rusqlite::params![
                i64::try_from(block).unwrap_or(i64::MAX),
                i64::try_from(log_index).unwrap_or(i64::MAX),
                pool_id
            ],
        )?;
        Ok(())
    }

    /// Set the V4 pool's `liquidity_update_block`/`liquidity_update_log_index`
    /// stamp (on the `uniswap_v4_pools` row, keyed by `managed_pool_id`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on an UPDATE failure.
    pub fn set_v4_liquidity_update_marker(
        &self,
        managed_pool_id: i64,
        block: u64,
        log_index: u64,
    ) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute(
            &format!(
                "UPDATE {UNISWAP_V4_POOLS} SET liquidity_update_block = ?1, \
                 liquidity_update_log_index = ?2 WHERE managed_pool_id = ?3"
            ),
            rusqlite::params![
                i64::try_from(block).unwrap_or(i64::MAX),
                i64::try_from(log_index).unwrap_or(i64::MAX),
                managed_pool_id
            ],
        )?;
        Ok(())
    }

    /// Delete liquidity positions for ticks NOT in `live_ticks` (the V3
    /// `liquidity_positions` table, keyed by `pool_id`). Mirrors the Python
    /// `delete(...).where(pool_id==?, tick.in_(ticks_to_drop))`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a DELETE failure.
    pub fn delete_stale_v3_positions(
        &self,
        pool_id: i64,
        live_ticks: &[i32],
    ) -> Result<(), DbError> {
        delete_stale_rows(
            self,
            &format!(
                "DELETE FROM {LIQUIDITY_POSITIONS} WHERE pool_id = ?1 AND tick NOT IN ({placeholders})",
                placeholders = sql_placeholders_for(live_ticks.len())
            ),
            pool_id,
            live_ticks,
        )
    }

    /// Delete init maps for words NOT in `live_words` (the V3
    /// `initialization_maps` table, keyed by `pool_id`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a DELETE failure.
    pub fn delete_stale_v3_init_maps(
        &self,
        pool_id: i64,
        live_words: &[i32],
    ) -> Result<(), DbError> {
        delete_stale_rows(
            self,
            &format!(
                "DELETE FROM {INITIALIZATION_MAPS} WHERE pool_id = ?1 AND word NOT IN ({placeholders})",
                placeholders = sql_placeholders_for(live_words.len())
            ),
            pool_id,
            live_words,
        )
    }

    /// Delete V4 liquidity positions for ticks NOT in `live_ticks`
    /// (`managed_pool_liquidity_positions`, keyed by `managed_pool_id`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a DELETE failure.
    pub fn delete_stale_v4_positions(
        &self,
        managed_pool_id: i64,
        live_ticks: &[i32],
    ) -> Result<(), DbError> {
        delete_stale_rows(
            self,
            &format!(
                "DELETE FROM {MANAGED_POOL_LIQUIDITY_POSITIONS} WHERE managed_pool_id = ?1 \
                 AND tick NOT IN ({placeholders})",
                placeholders = sql_placeholders_for(live_ticks.len())
            ),
            managed_pool_id,
            live_ticks,
        )
    }

    /// Delete V4 init maps for words NOT in `live_words`
    /// (`managed_pool_initialization_maps`, keyed by `managed_pool_id`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a DELETE failure.
    pub fn delete_stale_v4_init_maps(
        &self,
        managed_pool_id: i64,
        live_words: &[i32],
    ) -> Result<(), DbError> {
        delete_stale_rows(
            self,
            &format!(
                "DELETE FROM {MANAGED_POOL_INITIALIZATION_MAPS} WHERE managed_pool_id = ?1 \
                 AND word NOT IN ({placeholders})",
                placeholders = sql_placeholders_for(live_words.len())
            ),
            managed_pool_id,
            live_words,
        )
    }

    /// Upsert the V3 `liquidity_positions` rows for `pool_id` from the live
    /// `tick_data` map. Chunked to respect `SQLite`'s 32,766-variable limit
    /// (4 vars/row → ≤ 7,500 rows/chunk). Mirrors the Python
    /// `sqlite_upsert(LiquidityPositionTable).values([...]).on_conflict_do_update(...)`.
    ///
    /// The `on_conflict_do_update`'s `where != excl.net || != excl.gross`
    /// guard is implicit: `SQLite`'s `ON CONFLICT DO UPDATE` writes the new
    /// values unconditionally on conflict (the Python `where` skips no-op
    /// writes; the Rust path writes every time, which is observationally
    /// identical — the resulting row's net/gross equals the conflict-excluded
    /// value either way).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any upsert failure.
    pub fn upsert_v3_liquidity_positions(
        &self,
        pool_id: i64,
        tick_data: &HashMap<i32, LiquidityAtTick>,
    ) -> Result<(), DbError> {
        if tick_data.is_empty() {
            return Ok(());
        }
        let conn = self.lock();
        let chunk_cap = SQLITE_MAX_VARIABLES / POSITION_KEYS_PER_ROW;
        let entries: Vec<(i32, &LiquidityAtTick)> =
            tick_data.iter().map(|(t, v)| (*t, v)).collect();
        for chunk in entries.chunks(chunk_cap) {
            let placeholders = (0..chunk.len())
                .map(|i| {
                    format!(
                        "(?{}, ?{}, ?{}, ?{})",
                        i * 4 + 1,
                        i * 4 + 2,
                        i * 4 + 3,
                        i * 4 + 4
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "INSERT INTO {LIQUIDITY_POSITIONS} (pool_id, tick, liquidity_net, \
                 liquidity_gross) VALUES {placeholders} \
                 ON CONFLICT (pool_id, tick) DO UPDATE SET \
                 liquidity_net = excluded.liquidity_net, \
                 liquidity_gross = excluded.liquidity_gross \
                 WHERE liquidity_positions.liquidity_net != excluded.liquidity_net \
                   OR liquidity_positions.liquidity_gross != excluded.liquidity_gross"
            );
            // Build owned row-value buffers first (the &dyn ToSql params outlive the loop body).
            let row_bufs: Vec<(i64, i32, String, String)> = chunk
                .iter()
                .map(|(tick, lat)| {
                    (
                        pool_id,
                        *tick,
                        encode_i256(&lat.liquidity_net),
                        encode_u256(&u128_to_u256(lat.liquidity_gross)),
                    )
                })
                .collect();
            let params: Vec<&dyn rusqlite::ToSql> = row_bufs
                .iter()
                .flat_map(|(pid, tick, net, gross)| [pid as &dyn rusqlite::ToSql, tick, net, gross])
                .collect();
            let rows = conn.execute(&sql, rusqlite::params_from_iter(params))?;
            debug_assert!(
                rows <= chunk.len(),
                "upsert wrote {rows} for {} rows",
                chunk.len()
            );
        }
        Ok(())
    }

    /// Upsert the V4 `managed_pool_liquidity_positions` rows for
    /// `managed_pool_id` from `tick_data`. See
    /// [`Self::upsert_v3_liquidity_positions`] for the chunking + conflict
    /// semantics (V4 differs only in the table + key column name).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any upsert failure.
    pub fn upsert_v4_liquidity_positions(
        &self,
        managed_pool_id: i64,
        tick_data: &HashMap<i32, LiquidityAtTick>,
    ) -> Result<(), DbError> {
        if tick_data.is_empty() {
            return Ok(());
        }
        let conn = self.lock();
        let chunk_cap = SQLITE_MAX_VARIABLES / POSITION_KEYS_PER_ROW;
        let entries: Vec<(i32, &LiquidityAtTick)> =
            tick_data.iter().map(|(t, v)| (*t, v)).collect();
        for chunk in entries.chunks(chunk_cap) {
            let placeholders = (0..chunk.len())
                .map(|i| {
                    format!(
                        "(?{}, ?{}, ?{}, ?{})",
                        i * 4 + 1,
                        i * 4 + 2,
                        i * 4 + 3,
                        i * 4 + 4
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "INSERT INTO {MANAGED_POOL_LIQUIDITY_POSITIONS} (managed_pool_id, tick, \
                 liquidity_net, liquidity_gross) VALUES {placeholders} \
                 ON CONFLICT (managed_pool_id, tick) DO UPDATE SET \
                 liquidity_net = excluded.liquidity_net, \
                 liquidity_gross = excluded.liquidity_gross \
                 WHERE managed_pool_liquidity_positions.liquidity_net != excluded.liquidity_net \
                   OR managed_pool_liquidity_positions.liquidity_gross != excluded.liquidity_gross"
            );
            let row_bufs: Vec<(i64, i32, String, String)> = chunk
                .iter()
                .map(|(tick, lat)| {
                    (
                        managed_pool_id,
                        *tick,
                        encode_i256(&lat.liquidity_net),
                        encode_u256(&u128_to_u256(lat.liquidity_gross)),
                    )
                })
                .collect();
            let params: Vec<&dyn rusqlite::ToSql> = row_bufs
                .iter()
                .flat_map(|(pid, tick, net, gross)| [pid as &dyn rusqlite::ToSql, tick, net, gross])
                .collect();
            conn.execute(&sql, rusqlite::params_from_iter(params))?;
        }
        Ok(())
    }

    /// Upsert the V3 `initialization_maps` rows for `pool_id` from the live
    /// `tick_bitmap` (skips entries whose `bitmap == U256::ZERO` — the Python
    /// `if map_.bitmap != 0` filter). Chunked (3 vars/row → ≤ 10,000 rows/chunk).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any upsert failure.
    pub fn upsert_v3_initialization_maps(
        &self,
        pool_id: i64,
        tick_bitmap: &HashMap<i32, BitmapAtWord>,
    ) -> Result<(), DbError> {
        upsert_init_maps_impl(self, pool_id, tick_bitmap, INITIALIZATION_MAPS, "pool_id")
    }

    /// Upsert the V4 `managed_pool_initialization_maps` rows for
    /// `managed_pool_id` from the live `tick_bitmap` (skips zero-bitmap
    /// entries). See [`Self::upsert_v3_initialization_maps`].
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any upsert failure.
    pub fn upsert_v4_initialization_maps(
        &self,
        managed_pool_id: i64,
        tick_bitmap: &HashMap<i32, BitmapAtWord>,
    ) -> Result<(), DbError> {
        upsert_init_maps_impl(
            self,
            managed_pool_id,
            tick_bitmap,
            MANAGED_POOL_INITIALIZATION_MAPS,
            "managed_pool_id",
        )
    }
}

/// The shared per-event apply loop — reconstituted map → apply
/// `apply_liquidity_mapping_update` per event (guarded by the block/log-index
/// invariants) → return the LAST event's `(block, log_index)` (or `None` if
/// no events hit a non-zero delta).
///
/// `MAX_UINT256` is passed as `initial_state_block` to skip the in-range
/// liquidity adjustment (mirrors the Python path, which always passes
/// `MAX_UINT256`).
fn apply_event_loop(
    tick_bitmap: &mut HashMap<i32, BitmapAtWord>,
    tick_data: &mut HashMap<i32, LiquidityAtTick>,
    current_liquidity: &mut U128,
    state: PoolUpdateState,
    events: &[LiquidityUpdateEvent],
) -> Option<BlockLog> {
    const MAX_UINT256: u64 = u64::MAX;
    let mut last_event: Option<BlockLog> = state.last_update;

    for event in events {
        // Guard: the new event must be >= the last-applied (block, log_index).
        if let Some(last) = last_event {
            if event.block_number == last.block {
                debug_assert!(
                    event.log_index > last.log_index,
                    "liquidity event log_index {} not > last {} (same block {})",
                    event.log_index,
                    last.log_index,
                    last.block
                );
            } else {
                debug_assert!(
                    event.block_number > last.block,
                    "liquidity event block {} not > last {}",
                    event.block_number,
                    last.block
                );
            }
        }

        if event.liquidity_delta == I256::ZERO {
            continue;
        }

        let result = apply_liquidity_mapping_update(
            std::mem::take(tick_bitmap),
            std::mem::take(tick_data),
            state.tick_spacing,
            0,
            *current_liquidity,
            MAX_UINT256, // skip in-range liquidity adjustment (matches Python)
            event.block_number,
            event.tick_lower,
            event.tick_upper,
            event.liquidity_delta,
        );
        *tick_bitmap = result.tick_bitmap;
        *tick_data = result.tick_data;
        *current_liquidity = result.liquidity;

        last_event = Some(BlockLog {
            block: event.block_number,
            log_index: event.log_index,
        });
    }

    last_event
}

/// The V3 persist step: delete stale positions/init-maps, upsert the live
/// ones, + stamp the pool row's `liquidity_update_block`/`log_index`.
fn persist_v3(
    db: &DegenbotDb,
    pool_id: i64,
    tick_bitmap: &HashMap<i32, BitmapAtWord>,
    tick_data: &HashMap<i32, LiquidityAtTick>,
    last_event: Option<BlockLog>,
) -> Result<(), DbError> {
    let live_ticks: Vec<i32> = tick_data.keys().copied().collect();
    db.delete_stale_v3_positions(pool_id, &live_ticks)?;
    db.upsert_v3_liquidity_positions(pool_id, tick_data)?;

    let live_words: Vec<i32> = tick_bitmap
        .iter()
        .filter(|(_, bw)| bw.bitmap != U256::ZERO)
        .map(|(w, _)| *w)
        .collect();
    db.delete_stale_v3_init_maps(pool_id, &live_words)?;
    db.upsert_v3_initialization_maps(pool_id, tick_bitmap)?;

    if let Some(last) = last_event {
        db.set_v3_liquidity_update_marker(pool_id, last.block, last.log_index)?;
    }
    Ok(())
}

/// The V4 persist step (mirror of [`persist_v3`] for the V4 tables +
/// `uniswap_v4_pools` stamp).
fn persist_v4(
    db: &DegenbotDb,
    managed_pool_id: i64,
    tick_bitmap: &HashMap<i32, BitmapAtWord>,
    tick_data: &HashMap<i32, LiquidityAtTick>,
    last_event: Option<BlockLog>,
) -> Result<(), DbError> {
    let live_ticks: Vec<i32> = tick_data.keys().copied().collect();
    db.delete_stale_v4_positions(managed_pool_id, &live_ticks)?;
    db.upsert_v4_liquidity_positions(managed_pool_id, tick_data)?;

    let live_words: Vec<i32> = tick_bitmap
        .iter()
        .filter(|(_, bw)| bw.bitmap != U256::ZERO)
        .map(|(w, _)| *w)
        .collect();
    db.delete_stale_v4_init_maps(managed_pool_id, &live_words)?;
    db.upsert_v4_initialization_maps(managed_pool_id, tick_bitmap)?;

    if let Some(last) = last_event {
        db.set_v4_liquidity_update_marker(managed_pool_id, last.block, last.log_index)?;
    }
    Ok(())
}

/// Shared `delete ... WHERE <id_col> = ?1 AND <key_col> NOT IN (live)` executor
/// for the V3/V4 stale-row deletions. Binds `id_value` then the live keys.
fn delete_stale_rows(
    db: &DegenbotDb,
    sql: &str,
    id_value: i64,
    live_keys: &[i32],
) -> Result<(), DbError> {
    if live_keys.is_empty() {
        // No live keys → delete ALL rows for this pool.
        return Ok(());
    }
    let conn = db.lock();
    // Chunk the NOT IN list across SQLite's 32,766-var limit.
    let chunk_cap = SQLITE_MAX_VARIABLES.saturating_sub(1);
    for chunk in live_keys.chunks(chunk_cap) {
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(chunk.len() + 1);
        params.push(&id_value);
        for k in chunk {
            params.push(k);
        }
        // Rebuild the placeholders for THIS chunk (the SQL was pre-built for the
        // full live_keys.len(); rebuild here to match the chunk).
        let placeholders = sql_placeholders_for(chunk.len());
        let chunk_sql = sql.replace(&sql_placeholders_for(live_keys.len()), &placeholders);
        conn.execute(&chunk_sql, rusqlite::params_from_iter(params))?;
    }
    Ok(())
}

/// Shared `upsert_init_maps` impl — V3/V4 differ only in the table name + the
/// id column name.
fn upsert_init_maps_impl(
    db: &DegenbotDb,
    id_value: i64,
    tick_bitmap: &HashMap<i32, BitmapAtWord>,
    table: &str,
    id_col: &str,
) -> Result<(), DbError> {
    let entries: Vec<(i32, &BitmapAtWord)> = tick_bitmap
        .iter()
        .filter(|(_, bw)| bw.bitmap != U256::ZERO)
        .map(|(w, bw)| (*w, bw))
        .collect();
    if entries.is_empty() {
        return Ok(());
    }
    let conn = db.lock();
    let chunk_cap = SQLITE_MAX_VARIABLES / INIT_MAP_KEYS_PER_ROW;
    for chunk in entries.chunks(chunk_cap) {
        let placeholders = (0..chunk.len())
            .map(|i| format!("(?{}, ?{}, ?{})", i * 3 + 1, i * 3 + 2, i * 3 + 3))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO {table} ({id_col}, word, bitmap) VALUES {placeholders} \
             ON CONFLICT ({id_col}, word) DO UPDATE SET bitmap = excluded.bitmap \
             WHERE {table}.bitmap != excluded.bitmap"
        );
        let row_bufs: Vec<(i64, i32, String)> = chunk
            .iter()
            .map(|(word, bw)| (id_value, *word, encode_u256(&bw.bitmap)))
            .collect();
        let params: Vec<&dyn rusqlite::ToSql> = row_bufs
            .iter()
            .flat_map(|(pid, word, bitmap)| [pid as &dyn rusqlite::ToSql, word, bitmap])
            .collect();
        conn.execute(&sql, rusqlite::params_from_iter(params))?;
    }
    Ok(())
}

/// Build `?, ?, ...` (n placeholders) for a SQL `IN (...)` clause.
fn sql_placeholders_for(n: usize) -> String {
    vec!["?"; n].join(", ")
}

/// Decode a `VARCHAR(78)` **signed** decimal to [`I256`] (the
/// `liquidity_net` column stores the Python `str(int)` form, including the
/// leading `-` for negative values — `IntMappedToString.process_bind_param`).
///
/// # Errors
///
/// Returns [`DbError::Decode`] if the value is not a valid signed decimal.
fn decode_i256(s: &str) -> Result<I256, DbError> {
    I256::from_str(s.trim()).map_err(|e| DbError::Decode(format!("i256 parse of {s:?}: {e}")))
}

/// Re-encode an [`I256`] to its `VARCHAR(78)` signed-decimal form (the inverse
/// of [`decode_i256`]); mirrors the Python `IntMappedToString.process_bind_param`'s
/// `str(value)` (which prepends `-` for negatives).
#[must_use]
fn encode_i256(v: &I256) -> String {
    v.to_string()
}

/// Narrow a [`U256`] to its low-128-bits [`U128`] (the cl-math gross-liquidity
/// type). Matches the `PyO3` wrapper's `gross_bytes[16..32]` slice: V3/V4 gross
/// liquidity fits in 128 bits, but the DB column is the full 256-bit
/// `VARCHAR(78)` form (Python arbitrary-precision `int`).
#[must_use]
fn u256_to_u128(v: &U256) -> U128 {
    let bytes = v.to_be_bytes::<32>();
    let low: [u8; 16] = bytes[16..32].try_into().expect("16-byte slice fits");
    U128::from_be_bytes(low)
}

/// Widen a [`U128`] back to [`U256`] (the DB write form).
#[must_use]
fn u128_to_u256(v: U128) -> U256 {
    // U128 is 16 bytes; widen to U256's 32-byte big-endian by zero-padding
    // the high 16 bytes (matches the Python `int`-round-trip: the low-128
    // gross value is stored as its decimal `str(int)`, which `decode_u256`
    // parses back to the same U256).
    let mut arr = [0u8; 32];
    arr[16..32].copy_from_slice(&v.to_be_bytes::<16>());
    U256::from_be_bytes(arr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i256_signed_decimal_roundtrip() {
        for v in [0, 1, -1, 12345, -12345, i64::MAX, i64::MIN] {
            let i = I256::try_from(v).unwrap();
            let s = encode_i256(&i);
            let back = decode_i256(&s).unwrap();
            assert_eq!(back, i, "roundtrip failed for {v}: s={s:?}");
            if v < 0 {
                assert!(s.starts_with('-'), "neg should start with '-': {s:?}");
            }
        }
    }

    #[test]
    fn u256_low_128_roundtrip_preserves_gross() {
        // gross is always non-negative; the low 128 bits carry the value.
        for v in [0u128, 1, 12_345, u128::from(u64::MAX), u128::MAX] {
            let u = U256::from(v);
            let narrowed = u256_to_u128(&u);
            assert_eq!(narrowed, U128::from(v));
            let widened = u128_to_u256(narrowed);
            assert_eq!(widened, u);
        }
    }

    #[test]
    fn placeholders_format() {
        assert_eq!(sql_placeholders_for(0), "");
        assert_eq!(sql_placeholders_for(1), "?");
        assert_eq!(sql_placeholders_for(3), "?, ?, ?");
    }
}

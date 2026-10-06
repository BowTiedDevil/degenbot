//! V3/V4 DB-aware liquidity updater — the apply-and-persist core.
//!
//! Ports `cli/pool.py::apply_v3_liquidity_updates` / `apply_v4_liquidity_updates`
//! (read pool row + positions → apply CL `apply_liquidity_mapping_update` per
//! event → upsert positions + init maps → stamp `liquidity_update_block`/
//! `liquidity_update_log_index`) to a Rust core over [`DegenbotDb`] +
//! [`degenbot_math::cl::liquidity_mapping::apply_liquidity_mapping_update`].
//!
//! # What this is (and isn't)
//!
//! This is the **apply-and-persist core** (`port-now` per the §2.1 rubric): a
//! pure row→math→row transform over the DB substrate. RPC event fetch is
//! Rust-owned (`degenbot-rpc::provider::LogFetcher::fetch_logs_chunked`);
//! the retired Python fetch path (`provider/log_fetching.py`,
//! `fetch_logs_retrying*`) is removed. The math itself lives in
//! [`degenbot-concentrated-liquidity-math`] (sibling task). The standalone-Rust path this enables:
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

use hashbrown::HashMap;

use alloy::primitives::{I256, U128, U256};
use degenbot_math::cl::liquidity_mapping::{
    apply_liquidity_mapping_update, get_tick_word_and_bit_position, BitmapAtWord, LiquidityAtTick,
};

use crate::connection::DegenbotDb;
use crate::error::DbError;
use crate::rows::decode::{decode_i128_net, decode_u256, encode_u256};
use crate::schema::table::v2_v3_subclass_table;
use crate::schema::table::{
    INITIALIZATION_MAPS, LIQUIDITY_POSITIONS, MANAGED_POOLS, MANAGED_POOL_INITIALIZATION_MAPS,
    MANAGED_POOL_LIQUIDITY_POSITIONS, POOLS, POOL_MANAGERS, UNISWAP_V4_POOLS,
};

/// The reconstituted liquidity map (tick bitmap + tick data) the apply loop
/// mutates — the concentrated-liquidity-math types directly.
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

/// The event-touched (dirty) write set of one pool's apply — the O(events)
/// shape the delta persist writes (Perf B: the apply loop can only mutate
/// each event's `tick_lower`/`tick_upper` and their bitmap words, so the
/// chunk's write set is exactly the dirty keys' post-apply state, never the
/// pool's full map). Produced by
/// [`DegenbotDb::compute_v3_liquidity_delta_on_conn`] (a dirty-subset read
/// feeding the same `apply_event_loop`) or
/// [`derive_liquidity_delta_from_computed`] (the dirty projection of a full
/// computed map — the verification-armed path, which needs the FULL map for
/// the pre-commit gate and only the delta for the write).
#[derive(Debug, Clone)]
pub struct LiquidityDelta {
    /// V3: `pools.id`; V4: `managed_pools.id` (the 0 sentinel on a new-pool
    /// compute — the consumer resolves the real id post-upsert).
    pub pool_id: i64,
    /// The pool's tick spacing (the dirty-key derivation is spacing-keyed).
    pub tick_spacing: i32,
    /// Dirty ticks that SURVIVE post-apply (`liquidity_gross != 0`) — upserted.
    pub tick_data: HashMap<i32, LiquidityAtTick>,
    /// Dirty words that SURVIVE post-apply (`bitmap != U256::ZERO`) — upserted.
    pub tick_bitmap: HashMap<i32, BitmapAtWord>,
    /// Dirty ticks drained to zero — deleted by key (EXACTLY these rows; the
    /// complement `NOT IN` full-map delete is NOT issued on the delta path).
    pub drained_ticks: Vec<i32>,
    /// Dirty words whose post-apply bitmap is zero — deleted by key.
    pub drained_words: Vec<i32>,
    /// `true` iff the FULL post-apply map is empty (the fully-drained pool):
    /// the persist then fires the explicit complement delete-all (the pinned
    /// `delete_stale_rows` empty-live semantic) instead of per-key deletes.
    /// Knowable only where the full map is (the derive path); the
    /// dirty-subset compute cannot see untouched ticks and always reports
    /// `false` — complete there by drain-implies-touch (a tick's gross only
    /// changes via an event touching it, so a fully-drained pool's every base
    /// tick is dirty and lands in `drained_ticks`).
    pub drained_pool: bool,
    /// The last-applied event's stamp (the persist's marker write).
    pub last_event: Option<BlockLog>,
}

/// The per-family row targets the delta paths read/write — V3 keys
/// `liquidity_positions`/`initialization_maps` by `pool_id`; V4 keys the
/// `managed_pool_*` pair by `managed_pool_id`. The read/delete/upsert shapes
/// are identical; only the table + id column differ.
struct LiquidityTables {
    positions: &'static str,
    init_maps: &'static str,
    id_col: &'static str,
}

const V3_TABLES: LiquidityTables = LiquidityTables {
    positions: LIQUIDITY_POSITIONS,
    init_maps: INITIALIZATION_MAPS,
    id_col: "pool_id",
};

const V4_TABLES: LiquidityTables = LiquidityTables {
    positions: MANAGED_POOL_LIQUIDITY_POSITIONS,
    init_maps: MANAGED_POOL_INITIALIZATION_MAPS,
    id_col: "managed_pool_id",
};

/// The post-apply, **pre-persist** in-memory liquidity state for one pool —
/// the seam the on-chain-truth verifier inspects *before* `persist_*` commits.
///
/// Produced by [`DegenbotDb::compute_v3_liquidity_update_on_conn`] /
/// [`DegenbotDb::compute_v4_liquidity_update_on_conn`] (reconstitute →
/// `apply_event_loop`, NO persist). The caller may run the on-chain-truth gate
/// over `tick_data` + `tick_bitmap` at `chunk_end`; on GREEN it calls
/// [`DegenbotDb::persist_v3_liquidity_update_on_conn`] /
/// [`DegenbotDb::persist_v4_liquidity_update_on_conn`]. The combined
/// `apply_*_on_conn` helpers keep the no-gate path (compute → persist) for
/// backward compatibility.
#[derive(Debug, Clone)]
pub struct ComputedLiquidityUpdate {
    /// V3: `pools.id`; V4: `managed_pools.id`.
    pub pool_id: i64,
    /// The pool's tick spacing (passed to the on-chain bitmap word↔tick math).
    pub tick_spacing: i32,
    /// In-memory initialized ticks (gross + net) after the chunk's apply —
    /// exactly the state about to be persisted.
    pub tick_data: HashMap<i32, LiquidityAtTick>,
    /// In-memory initialization-map words after the chunk's apply.
    pub tick_bitmap: HashMap<i32, BitmapAtWord>,
    /// The last-applied event's stamp (advanced by `persist_*`).
    pub last_event: Option<BlockLog>,
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
        Self::fetch_v3_pool_update_state_on_conn(&conn, chain_id, pool_address)
    }

    /// The single-transaction-bound variant of [`Self::fetch_v3_pool_update_state`]
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn fetch_v3_pool_update_state_on_conn(
        conn: &rusqlite::Connection,
        chain_id: i64,
        pool_address: &str,
    ) -> Result<Option<PoolUpdateState>, DbError> {
        // The V3 pool is polymorphic: `pools` holds the base columns (id,
        // address, chain, kind, token*_id, exchange_id) + a per-DEX subclass
        // table (`uniswap_v3_pools` / `pancakeswap_v3_pools` /
        // `sushiswap_v3_pools` / `aerodrome_v3_pools`) holds the V3-specific
        // columns (tick_spacing, liquidity_update_block, liquidity_update_log_index,
        // fee_*). The subclass table is resolved at runtime from `pools.kind`;
        // hardcoding `uniswap_v3_pools` would silently skip every fork pool.
        //
        // Two-step lookup: (1) fetch `pools.id` + `pools.kind` by (chain,
        // address), (2) resolve the subclass table via
        // `v2_v3_subclass_table(kind)` and read the V3-specific columns. Returns
        // `Ok(None)` if the base row is absent or the kind is not a V3 family
        // discriminator.
        let base_row: Option<(i64, String)> = conn
            .query_row(
                &format!(
                    "SELECT {POOLS}.id, {POOLS}.kind \
                     FROM {POOLS} \
                     WHERE {POOLS}.chain = ?1 AND {POOLS}.address = ?2 LIMIT 1"
                ),
                rusqlite::params![chain_id, pool_address],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        let Some((pool_id, kind)) = base_row else {
            return Ok(None);
        };
        let Some(subclass_table) = v2_v3_subclass_table(&kind) else {
            return Ok(None);
        };
        let row: Option<(i32, Option<i64>, Option<i64>)> = conn
            .query_row(
                &format!(
                    "SELECT tick_spacing, liquidity_update_block, \
                     liquidity_update_log_index FROM {subclass_table} \
                     WHERE pool_id = ?1 LIMIT 1"
                ),
                rusqlite::params![pool_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(row.map(|(tick_spacing, block, log_idx)| PoolUpdateState {
            pool_id,
            tick_spacing,
            last_update: match (block, log_idx) {
                (Some(b), Some(l)) => Some(BlockLog {
                    block: u64::try_from(b).unwrap_or(0),
                    log_index: u64::try_from(l).unwrap_or(0),
                }),
                _ => None,
            },
        }))
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
        Self::fetch_v4_pool_update_state_on_conn(&conn, pool_hash, pool_manager_chain)
    }

    /// The single-transaction-bound variant of [`Self::fetch_v4_pool_update_state`]
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn fetch_v4_pool_update_state_on_conn(
        conn: &rusqlite::Connection,
        pool_hash: &str,
        pool_manager_chain: i64,
    ) -> Result<Option<PoolUpdateState>, DbError> {
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
    /// reconstitution; produces the concentrated-liquidity-math types directly (no `U256`-bridge
    /// round-trip — `liquidity_net` is decoded as `I256`, `liquidity_gross` as
    /// low-128 `U128`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on
    /// a malformed column.
    pub fn fetch_v3_liquidity_map(&self, pool_id: i64) -> Result<LiquidityMap, DbError> {
        let conn = self.lock();
        Self::fetch_v3_liquidity_map_on_conn(&conn, pool_id)
    }

    /// The single-transaction-bound variant of [`Self::fetch_v3_liquidity_map`]
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn fetch_v3_liquidity_map_on_conn(
        conn: &rusqlite::Connection,
        pool_id: i64,
    ) -> Result<LiquidityMap, DbError> {
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
                        liquidity_net: I256::try_from(decode_i128_net(&net_str)?)
                            .map_err(|e| DbError::Decode(format!("net decode: {e}")))?,
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
        Self::fetch_v4_liquidity_map_on_conn(&conn, managed_pool_id)
    }

    /// The single-transaction-bound variant of [`Self::fetch_v4_liquidity_map`]
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn fetch_v4_liquidity_map_on_conn(
        conn: &rusqlite::Connection,
        managed_pool_id: i64,
    ) -> Result<LiquidityMap, DbError> {
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
                        liquidity_net: I256::try_from(decode_i128_net(&net_str)?)
                            .map_err(|e| DbError::Decode(format!("net decode: {e}")))?,
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
        let conn = self.lock();
        Self::apply_v3_liquidity_updates_on_conn(&conn, chain_id, pool_address, events)
    }

    /// The **compute** half of [`Self::apply_v3_liquidity_updates_on_conn`]:
    /// reconstitute the pool's persisted base tick state, run `apply_event_loop`,
    /// and return the in-memory result **without persisting** — the seam the
    /// on-chain-truth verifier inspects *before* the commit. The caller runs the
    /// gate over the returned `tick_data` + `tick_bitmap`, then calls
    /// [`Self::persist_v3_liquidity_update_on_conn`] on GREEN (or drops the chunk's
    /// transaction on RED, leaving the prior committed state intact).
    ///
    /// Returns `Ok(None)` when the pool row is absent (no-op, mirrors the
    /// combined helper's `Ok(false)`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any query failure or [`DbError::Decode`]
    /// on a malformed column.
    pub fn compute_v3_liquidity_update_on_conn(
        conn: &rusqlite::Connection,
        chain_id: i64,
        pool_address: &str,
        events: &[LiquidityUpdateEvent],
    ) -> Result<Option<ComputedLiquidityUpdate>, DbError> {
        let Some(state) = Self::fetch_v3_pool_update_state_on_conn(conn, chain_id, pool_address)?
        else {
            return Ok(None);
        };
        let tick_spacing = state.tick_spacing;
        let pool_id = state.pool_id;
        let (mut tick_bitmap, mut tick_data) = Self::fetch_v3_liquidity_map_on_conn(conn, pool_id)?;

        let mut current_liquidity = U128::ZERO;
        let last_event = apply_event_loop(
            &mut tick_bitmap,
            &mut tick_data,
            &mut current_liquidity,
            state,
            events,
        );

        Ok(Some(ComputedLiquidityUpdate {
            pool_id,
            tick_spacing,
            tick_data,
            tick_bitmap,
            last_event,
        }))
    }

    /// The **persist** half: `delete_stale` + upsert + stamp the marker. Exposed
    /// so a verifier-gated caller (compute → verify → persist) can commit only
    /// after the on-chain-truth gate is GREEN. Delegates to the private
    /// `persist_v3` (byte-identical behavior).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any delete/upsert/stamp failure.
    pub fn persist_v3_liquidity_update_on_conn(
        conn: &rusqlite::Connection,
        pool_id: i64,
        tick_bitmap: &HashMap<i32, BitmapAtWord>,
        tick_data: &HashMap<i32, LiquidityAtTick>,
        last_event: Option<BlockLog>,
    ) -> Result<(), DbError> {
        persist_v3(conn, pool_id, tick_bitmap, tick_data, last_event)
    }

    /// The single-transaction-bound variant of [`Self::apply_v3_liquidity_updates`]
    /// compute → persist with no on-chain gate (backward compat).
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn apply_v3_liquidity_updates_on_conn(
        conn: &rusqlite::Connection,
        chain_id: i64,
        pool_address: &str,
        events: &[LiquidityUpdateEvent],
    ) -> Result<bool, DbError> {
        let Some(c) =
            Self::compute_v3_liquidity_update_on_conn(conn, chain_id, pool_address, events)?
        else {
            return Ok(false);
        };
        let delta = derive_liquidity_delta_from_computed(&c, events);
        Self::persist_v3_liquidity_delta_on_conn(conn, &delta)?;
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
        let conn = self.lock();
        Self::apply_v4_liquidity_updates_on_conn(&conn, pool_hash, pool_manager_chain, events)
    }

    /// The **compute** half of [`Self::apply_v4_liquidity_updates_on_conn`] (V4
    /// mirror of [`Self::compute_v3_liquidity_update_on_conn`]): reconstitute the
    /// V4 pool's base tick state + run `apply_event_loop`, returning the
    /// in-memory map **without persisting** — the pre-commit verifier seam.
    ///
    /// Returns `Ok(None)` when the managed-pool row is absent.
    ///
    /// # Panics (debug builds only)
    ///
    /// Panics if an event violates the block/log-index ordering invariant.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any query failure or [`DbError::Decode`]
    /// on a malformed column.
    pub fn compute_v4_liquidity_update_on_conn(
        conn: &rusqlite::Connection,
        pool_hash: &str,
        pool_manager_chain: i64,
        events: &[LiquidityUpdateEvent],
    ) -> Result<Option<ComputedLiquidityUpdate>, DbError> {
        let Some(state) =
            Self::fetch_v4_pool_update_state_on_conn(conn, pool_hash, pool_manager_chain)?
        else {
            return Ok(None);
        };
        let tick_spacing = state.tick_spacing;
        let pool_id = state.pool_id;
        let (mut tick_bitmap, mut tick_data) = Self::fetch_v4_liquidity_map_on_conn(conn, pool_id)?;

        let mut current_liquidity = U128::ZERO;
        let last_event = apply_event_loop(
            &mut tick_bitmap,
            &mut tick_data,
            &mut current_liquidity,
            state,
            events,
        );

        Ok(Some(ComputedLiquidityUpdate {
            pool_id,
            tick_spacing,
            tick_data,
            tick_bitmap,
            last_event,
        }))
    }

    /// The **persist** half (V4 mirror of
    /// [`Self::persist_v3_liquidity_update_on_conn`]). Delegates to the private
    /// `persist_v4` (byte-identical behavior).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any delete/upsert/stamp failure.
    pub fn persist_v4_liquidity_update_on_conn(
        conn: &rusqlite::Connection,
        managed_pool_id: i64,
        tick_bitmap: &HashMap<i32, BitmapAtWord>,
        tick_data: &HashMap<i32, LiquidityAtTick>,
        last_event: Option<BlockLog>,
    ) -> Result<(), DbError> {
        persist_v4(conn, managed_pool_id, tick_bitmap, tick_data, last_event)
    }

    /// The single-transaction-bound variant of [`Self::apply_v4_liquidity_updates`]
    /// compute → persist with no on-chain gate (backward compat).
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn apply_v4_liquidity_updates_on_conn(
        conn: &rusqlite::Connection,
        pool_hash: &str,
        pool_manager_chain: i64,
        events: &[LiquidityUpdateEvent],
    ) -> Result<bool, DbError> {
        let Some(c) =
            Self::compute_v4_liquidity_update_on_conn(conn, pool_hash, pool_manager_chain, events)?
        else {
            return Ok(false);
        };
        let delta = derive_liquidity_delta_from_computed(&c, events);
        Self::persist_v4_liquidity_delta_on_conn(conn, &delta)?;
        Ok(true)
    }

    /// The **dirty-key delta compute** (V3 — Perf B): read ONLY the rows the
    /// chunk's events can touch (each event's `tick_lower`/`tick_upper` and
    /// their bitmap words), run the SAME [`apply_event_loop`] over that dirty
    /// subset, and return the dirty write set — survive rows + drained keys —
    /// WITHOUT persisting. The full map is neither read nor built: the
    /// chunk-loop persist needs exactly this delta, and the pre-commit gate
    /// (which needs the full map) takes
    /// [`Self::compute_v3_liquidity_update_on_conn`] + the derive path
    /// instead, so the gate's verification semantics are untouched.
    ///
    /// Returns `Ok(None)` when the pool row is absent.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any query failure or [`DbError::Decode`]
    /// on a malformed column.
    pub fn compute_v3_liquidity_delta_on_conn(
        conn: &rusqlite::Connection,
        chain_id: i64,
        pool_address: &str,
        events: &[LiquidityUpdateEvent],
    ) -> Result<Option<LiquidityDelta>, DbError> {
        let Some(state) = Self::fetch_v3_pool_update_state_on_conn(conn, chain_id, pool_address)?
        else {
            return Ok(None);
        };
        Ok(Some(compute_liquidity_delta_on_tables(
            conn, &V3_TABLES, state, events,
        )?))
    }

    /// The dirty-key delta compute (V4 mirror of
    /// [`Self::compute_v3_liquidity_delta_on_conn`]).
    ///
    /// Returns `Ok(None)` when the managed-pool row is absent.
    ///
    /// # Errors
    ///
    /// Same conditions as the V3 variant.
    pub fn compute_v4_liquidity_delta_on_conn(
        conn: &rusqlite::Connection,
        pool_hash: &str,
        pool_manager_chain: i64,
        events: &[LiquidityUpdateEvent],
    ) -> Result<Option<LiquidityDelta>, DbError> {
        let Some(state) =
            Self::fetch_v4_pool_update_state_on_conn(conn, pool_hash, pool_manager_chain)?
        else {
            return Ok(None);
        };
        Ok(Some(compute_liquidity_delta_on_tables(
            conn, &V4_TABLES, state, events,
        )?))
    }

    /// The **delta persist** (V3 — Perf B): write ONLY the event-touched keys.
    /// Upsert the dirty survivors (multi-row, deterministic order — the same
    /// upserts the full-map persist uses), `DELETE` exactly the drained keys
    /// (`... AND tick IN (...)` — no complement scan), and stamp the marker.
    /// The fully-drained pool keeps the EXPLICIT complement delete-all branch
    /// (`drained_pool`, the derive path's pinned semantic); the delta path's
    /// per-key deletes are complete there by drain-implies-touch (see
    /// [`LiquidityDelta::drained_pool`]).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any delete/upsert/stamp failure.
    pub fn persist_v3_liquidity_delta_on_conn(
        conn: &rusqlite::Connection,
        delta: &LiquidityDelta,
    ) -> Result<(), DbError> {
        persist_delta_tables(conn, &V3_TABLES, delta)?;
        if let Some(last) = delta.last_event {
            DegenbotDb::set_v3_liquidity_update_marker_on_conn(
                conn,
                delta.pool_id,
                last.block,
                last.log_index,
            )?;
        }
        Ok(())
    }

    /// The delta persist (V4 mirror of
    /// [`Self::persist_v3_liquidity_delta_on_conn`]).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any delete/upsert/stamp failure.
    pub fn persist_v4_liquidity_delta_on_conn(
        conn: &rusqlite::Connection,
        delta: &LiquidityDelta,
    ) -> Result<(), DbError> {
        persist_delta_tables(conn, &V4_TABLES, delta)?;
        if let Some(last) = delta.last_event {
            DegenbotDb::set_v4_liquidity_update_marker_on_conn(
                conn,
                delta.pool_id,
                last.block,
                last.log_index,
            )?;
        }
        Ok(())
    }

    /// The **new-pool** compute half (V3): the in-memory map for a pool whose
    /// row is created by THIS chunk's `PoolCreated` upsert (no committed base
    /// state exists yet). Applies `events` to an EMPTY base map under the same
    /// `apply_event_loop` semantics, returning the map WITHOUT persisting -
    /// the pre-transaction verification seam (Perf A: the verify RPC must not
    /// hold the write lock, so the map is computed before the chunk
    /// `Transaction` opens; the in-transaction apply re-derives it from the
    /// just-upserted row - byte-equal by construction: same empty base, same
    /// events, same spacing).
    ///
    /// `pool_id` is a 0 sentinel (no row exists to key yet); the consumer
    /// verifies `tick_data`/`tick_bitmap`/`last_event`, never the id - the
    /// in-transaction apply resolves the real id after the upsert.
    #[must_use]
    pub fn compute_v3_liquidity_update_for_new_pool(
        tick_spacing: i32,
        events: &[LiquidityUpdateEvent],
    ) -> ComputedLiquidityUpdate {
        let state = PoolUpdateState {
            pool_id: 0,
            tick_spacing,
            last_update: None,
        };
        Self::compute_liquidity_update_from_base(state, events)
    }

    /// The **new-pool** compute half (V4 mirror of
    /// [`Self::compute_v3_liquidity_update_for_new_pool`]): empty base +
    /// events, no persist, sentinel `pool_id`. The V4 event math is
    /// spacing-keyed and identical to V3's (the table target is the only
    /// difference, chosen at persist time).
    #[must_use]
    pub fn compute_v4_liquidity_update_for_new_pool(
        tick_spacing: i32,
        events: &[LiquidityUpdateEvent],
    ) -> ComputedLiquidityUpdate {
        Self::compute_v3_liquidity_update_for_new_pool(tick_spacing, events)
    }

    /// The shared empty-base compute: `apply_event_loop` over an EMPTY
    /// tick/bitmap pair with the caller's synthetic state. Private - the two
    /// `compute_*_for_new_pool` fns are the public surface.
    fn compute_liquidity_update_from_base(
        state: PoolUpdateState,
        events: &[LiquidityUpdateEvent],
    ) -> ComputedLiquidityUpdate {
        let mut tick_bitmap = HashMap::new();
        let mut tick_data = HashMap::new();
        let mut current_liquidity = U128::ZERO;
        let last_event = apply_event_loop(
            &mut tick_bitmap,
            &mut tick_data,
            &mut current_liquidity,
            state,
            events,
        );
        ComputedLiquidityUpdate {
            pool_id: state.pool_id,
            tick_spacing: state.tick_spacing,
            tick_data,
            tick_bitmap,
            last_event,
        }
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
        Self::set_v3_liquidity_update_marker_on_conn(&conn, pool_id, block, log_index)
    }

    /// The single-transaction-bound variant of
    /// [`Self::set_v3_liquidity_update_marker`].
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn set_v3_liquidity_update_marker_on_conn(
        conn: &rusqlite::Connection,
        pool_id: i64,
        block: u64,
        log_index: u64,
    ) -> Result<(), DbError> {
        // Resolve the per-DEX subclass table from `pools.kind` (the row was
        // guaranteed V3-family by `fetch_v3_pool_update_state_on_conn` earlier
        // in the compute→persist pipeline, but this fn is independently callable,
        // so re-derive to avoid writing to the wrong table).
        let kind: String = conn.query_row(
            &format!("SELECT kind FROM {POOLS} WHERE id = ?1"),
            rusqlite::params![pool_id],
            |r| r.get(0),
        )?;
        let subclass_table = v2_v3_subclass_table(&kind).ok_or_else(|| {
            DbError::Decode(format!(
                "pool {pool_id} has kind {kind:?}, not a V3 family discriminator"
            ))
        })?;
        conn.execute(
            &format!(
                "UPDATE {subclass_table} SET liquidity_update_block = ?1, \
                 liquidity_update_log_index = ?2 WHERE pool_id = ?3"
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
        Self::set_v4_liquidity_update_marker_on_conn(&conn, managed_pool_id, block, log_index)
    }

    /// The single-transaction-bound variant of
    /// [`Self::set_v4_liquidity_update_marker`].
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn set_v4_liquidity_update_marker_on_conn(
        conn: &rusqlite::Connection,
        managed_pool_id: i64,
        block: u64,
        log_index: u64,
    ) -> Result<(), DbError> {
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
        let conn = self.lock();
        Self::delete_stale_v3_positions_on_conn(&conn, pool_id, live_ticks)
    }

    /// The single-transaction-bound variant of [`Self::delete_stale_v3_positions`]
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn delete_stale_v3_positions_on_conn(
        conn: &rusqlite::Connection,
        pool_id: i64,
        live_ticks: &[i32],
    ) -> Result<(), DbError> {
        delete_stale_rows(
            conn,
            LIQUIDITY_POSITIONS,
            "pool_id",
            "tick",
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
        let conn = self.lock();
        Self::delete_stale_v3_init_maps_on_conn(&conn, pool_id, live_words)
    }

    /// The single-transaction-bound variant of [`Self::delete_stale_v3_init_maps`]
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn delete_stale_v3_init_maps_on_conn(
        conn: &rusqlite::Connection,
        pool_id: i64,
        live_words: &[i32],
    ) -> Result<(), DbError> {
        delete_stale_rows(
            conn,
            INITIALIZATION_MAPS,
            "pool_id",
            "word",
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
        let conn = self.lock();
        Self::delete_stale_v4_positions_on_conn(&conn, managed_pool_id, live_ticks)
    }

    /// The single-transaction-bound variant of [`Self::delete_stale_v4_positions`]
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn delete_stale_v4_positions_on_conn(
        conn: &rusqlite::Connection,
        managed_pool_id: i64,
        live_ticks: &[i32],
    ) -> Result<(), DbError> {
        delete_stale_rows(
            conn,
            MANAGED_POOL_LIQUIDITY_POSITIONS,
            "managed_pool_id",
            "tick",
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
        let conn = self.lock();
        Self::delete_stale_v4_init_maps_on_conn(&conn, managed_pool_id, live_words)
    }

    /// The single-transaction-bound variant of [`Self::delete_stale_v4_init_maps`]
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn delete_stale_v4_init_maps_on_conn(
        conn: &rusqlite::Connection,
        managed_pool_id: i64,
        live_words: &[i32],
    ) -> Result<(), DbError> {
        delete_stale_rows(
            conn,
            MANAGED_POOL_INITIALIZATION_MAPS,
            "managed_pool_id",
            "word",
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
        let conn = self.lock();
        Self::upsert_v3_liquidity_positions_on_conn(&conn, pool_id, tick_data)
    }

    /// The single-transaction-bound variant of
    /// [`Self::upsert_v3_liquidity_positions`].
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn upsert_v3_liquidity_positions_on_conn(
        conn: &rusqlite::Connection,
        pool_id: i64,
        tick_data: &HashMap<i32, LiquidityAtTick>,
    ) -> Result<(), DbError> {
        if tick_data.is_empty() {
            return Ok(());
        }
        let chunk_cap = SQLITE_MAX_VARIABLES / POSITION_KEYS_PER_ROW;
        let mut entries: Vec<(i32, &LiquidityAtTick)> =
            tick_data.iter().map(|(t, v)| (*t, v)).collect();
        // Deterministic write order: the chunk apply is replayed against
        // golden captures (ADR-068 D3/D6), so both the statement's VALUES
        // order and the autoincrement row ids it assigns must not depend on
        // hashbrown iteration order.
        entries.sort_by_key(|(t, _)| *t);
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
                        lat.liquidity_net.to_string(),
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
        let conn = self.lock();
        Self::upsert_v4_liquidity_positions_on_conn(&conn, managed_pool_id, tick_data)
    }

    /// The single-transaction-bound variant of
    /// [`Self::upsert_v4_liquidity_positions`].
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn upsert_v4_liquidity_positions_on_conn(
        conn: &rusqlite::Connection,
        managed_pool_id: i64,
        tick_data: &HashMap<i32, LiquidityAtTick>,
    ) -> Result<(), DbError> {
        if tick_data.is_empty() {
            return Ok(());
        }
        let chunk_cap = SQLITE_MAX_VARIABLES / POSITION_KEYS_PER_ROW;
        // Deterministic write order (see the V3 positions upsert — golden
        // captures replay this statement, so VALUES order + row ids must be
        // stable across runs).
        let mut entries: Vec<(i32, &LiquidityAtTick)> =
            tick_data.iter().map(|(t, v)| (*t, v)).collect();
        entries.sort_by_key(|(t, _)| *t);
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
                        lat.liquidity_net.to_string(),
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
        let conn = self.lock();
        Self::upsert_v3_initialization_maps_on_conn(&conn, pool_id, tick_bitmap)
    }

    /// The single-transaction-bound variant of
    /// [`Self::upsert_v3_initialization_maps`].
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn upsert_v3_initialization_maps_on_conn(
        conn: &rusqlite::Connection,
        pool_id: i64,
        tick_bitmap: &HashMap<i32, BitmapAtWord>,
    ) -> Result<(), DbError> {
        upsert_init_maps_impl(conn, pool_id, tick_bitmap, INITIALIZATION_MAPS, "pool_id")
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
        let conn = self.lock();
        Self::upsert_v4_initialization_maps_on_conn(&conn, managed_pool_id, tick_bitmap)
    }

    /// The single-transaction-bound variant of
    /// [`Self::upsert_v4_initialization_maps`].
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn upsert_v4_initialization_maps_on_conn(
        conn: &rusqlite::Connection,
        managed_pool_id: i64,
        tick_bitmap: &HashMap<i32, BitmapAtWord>,
    ) -> Result<(), DbError> {
        upsert_init_maps_impl(
            conn,
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
/// accepts a borrowed [`rusqlite::Connection`] so the whole
/// persist step runs on the chunk's single transaction.
fn persist_v3(
    conn: &rusqlite::Connection,
    pool_id: i64,
    tick_bitmap: &HashMap<i32, BitmapAtWord>,
    tick_data: &HashMap<i32, LiquidityAtTick>,
    last_event: Option<BlockLog>,
) -> Result<(), DbError> {
    let live_ticks: Vec<i32> = tick_data.keys().copied().collect();
    DegenbotDb::delete_stale_v3_positions_on_conn(conn, pool_id, &live_ticks)?;
    DegenbotDb::upsert_v3_liquidity_positions_on_conn(conn, pool_id, tick_data)?;

    let live_words: Vec<i32> = tick_bitmap
        .iter()
        .filter(|(_, bw)| bw.bitmap != U256::ZERO)
        .map(|(w, _)| *w)
        .collect();
    DegenbotDb::delete_stale_v3_init_maps_on_conn(conn, pool_id, &live_words)?;
    DegenbotDb::upsert_v3_initialization_maps_on_conn(conn, pool_id, tick_bitmap)?;

    if let Some(last) = last_event {
        DegenbotDb::set_v3_liquidity_update_marker_on_conn(
            conn,
            pool_id,
            last.block,
            last.log_index,
        )?;
    }
    Ok(())
}

/// The V4 persist step (mirror of [`persist_v3`] for the V4 tables +
/// `uniswap_v4_pools` stamp). Accepts a borrowed
/// [`rusqlite::Connection`].
fn persist_v4(
    conn: &rusqlite::Connection,
    managed_pool_id: i64,
    tick_bitmap: &HashMap<i32, BitmapAtWord>,
    tick_data: &HashMap<i32, LiquidityAtTick>,
    last_event: Option<BlockLog>,
) -> Result<(), DbError> {
    let live_ticks: Vec<i32> = tick_data.keys().copied().collect();
    DegenbotDb::delete_stale_v4_positions_on_conn(conn, managed_pool_id, &live_ticks)?;
    DegenbotDb::upsert_v4_liquidity_positions_on_conn(conn, managed_pool_id, tick_data)?;

    let live_words: Vec<i32> = tick_bitmap
        .iter()
        .filter(|(_, bw)| bw.bitmap != U256::ZERO)
        .map(|(w, _)| *w)
        .collect();
    DegenbotDb::delete_stale_v4_init_maps_on_conn(conn, managed_pool_id, &live_words)?;
    DegenbotDb::upsert_v4_initialization_maps_on_conn(conn, managed_pool_id, tick_bitmap)?;

    if let Some(last) = last_event {
        DegenbotDb::set_v4_liquidity_update_marker_on_conn(
            conn,
            managed_pool_id,
            last.block,
            last.log_index,
        )?;
    }
    Ok(())
}

/// Shared stale-row deleter for the V3/V4 liquidity positions + init-maps.
/// Deletes every row for `id_value` whose `key_col` is NOT in `live_keys`
/// (the ticks/words still present in the freshly-applied `tick_data`/
/// `tick_bitmap`). Binds `id_value` then the live keys. Accepts a
/// borrowed [`rusqlite::Connection`] (the chunk-loop `Transaction` derefs to
/// one) so the delete runs on the chunk's single owned connection — no re-lock.
///
/// Builds the SQL internally from the table + column identifiers so the
/// `live_keys.is_empty()` branch (a fully-drained pool — every tick pruned)
/// can issue the bare `DELETE FROM {table} WHERE {id_col} = ?1` (drop ALL rows),
/// matching the Python `db_keys - helper_keys` complement-delete-all semantics
/// (helper empty → drop all). The bare form avoids the SQL syntax error of an
/// empty `NOT IN ()` placeholder list.
fn delete_stale_rows(
    conn: &rusqlite::Connection,
    table: &str,
    id_col: &str,
    key_col: &str,
    id_value: i64,
    live_keys: &[i32],
) -> Result<(), DbError> {
    if live_keys.is_empty() {
        // Fully-drained pool → no live keys → delete ALL rows for this pool.
        conn.execute(
            &format!("DELETE FROM {table} WHERE {id_col} = ?1"),
            rusqlite::params![id_value],
        )?;
        return Ok(());
    }
    // Chunk the NOT IN list across SQLite's 32,766-var limit.
    let chunk_cap = SQLITE_MAX_VARIABLES.saturating_sub(1);
    for chunk in live_keys.chunks(chunk_cap) {
        let placeholders = sql_placeholders_for(chunk.len());
        let chunk_sql = format!(
            "DELETE FROM {table} WHERE {id_col} = ?1 AND {key_col} NOT IN ({placeholders})"
        );
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(chunk.len() + 1);
        params.push(&id_value);
        for k in chunk {
            params.push(k);
        }
        conn.execute(&chunk_sql, rusqlite::params_from_iter(params))?;
    }
    Ok(())
}

/// The event-touched (dirty) key sets for a chunk's liquidity events: the
/// `tick_lower`/`tick_upper` pairs of every NON-zero-delta event (the apply
/// loop skips zero-delta events before any mutation) and the bitmap words
/// containing them, both sorted + deduped. The delta paths' read filter and
/// write set — O(events) keys, never O(map).
fn event_dirty_keys(events: &[LiquidityUpdateEvent], tick_spacing: i32) -> (Vec<i32>, Vec<i32>) {
    let mut ticks: Vec<i32> = events
        .iter()
        .filter(|event| event.liquidity_delta != I256::ZERO)
        .flat_map(|event| [event.tick_lower, event.tick_upper])
        .collect();
    ticks.sort_unstable();
    ticks.dedup();
    let mut words: Vec<i32> = ticks
        .iter()
        .map(|&tick| get_tick_word_and_bit_position(tick, tick_spacing).0)
        .collect();
    words.sort_unstable();
    words.dedup();
    (ticks, words)
}

/// Read ONE table's rows for `id_value` restricted to `keys` (the delta
/// compute's dirty-subset read). Chunked across SQLite's variable limit
/// (`id_value` + the IN list); EMPTY `keys` issues NO statement and returns
/// no rows. Rows return in ascending key order.
fn fetch_rows_for_keys(
    conn: &rusqlite::Connection,
    table: &str,
    id_col: &str,
    key_col: &str,
    select_cols: &str,
    id_value: i64,
    keys: &[i32],
) -> Result<Vec<(i32, Vec<String>)>, DbError> {
    let mut out = Vec::new();
    if keys.is_empty() {
        return Ok(out);
    }
    let chunk_cap = SQLITE_MAX_VARIABLES.saturating_sub(1);
    for chunk in keys.chunks(chunk_cap) {
        let placeholders = sql_placeholders_for(chunk.len());
        let sql = format!(
            "SELECT {key_col}, {select_cols} FROM {table} \
             WHERE {id_col} = ?1 AND {key_col} IN ({placeholders})"
        );
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(chunk.len() + 1);
        params.push(&id_value);
        for k in chunk {
            params.push(k);
        }
        let mut stmt = conn.prepare(&sql)?;
        let rest_cols = stmt.column_count().saturating_sub(1);
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
            let key: i64 = r.get(0)?;
            let rest: Vec<String> = (1..=rest_cols)
                .map(|c| r.get(c))
                .collect::<Result<_, _>>()?;
            Ok((key, rest))
        })?;
        for row in rows {
            let (key, rest) = row?;
            let key: i32 = i32::try_from(key)
                .map_err(|e| DbError::Decode(format!("{key_col} {key} out of i32 range: {e}")))?;
            out.push((key, rest));
        }
    }
    out.sort_by_key(|(k, _)| *k);
    Ok(out)
}

/// The dirty-key delta compute over one family's tables: dirty-subset read →
/// the SAME [`apply_event_loop`] → survive/drained split. `drained_pool` is
/// always `false` here (the full map is not read — see
/// [`LiquidityDelta::drained_pool`]).
fn compute_liquidity_delta_on_tables(
    conn: &rusqlite::Connection,
    tables: &LiquidityTables,
    state: PoolUpdateState,
    events: &[LiquidityUpdateEvent],
) -> Result<LiquidityDelta, DbError> {
    let (dirty_ticks, dirty_words) = event_dirty_keys(events, state.tick_spacing);

    let mut tick_bitmap: HashMap<i32, BitmapAtWord> = HashMap::new();
    for (word, rest) in fetch_rows_for_keys(
        conn,
        tables.init_maps,
        tables.id_col,
        "word",
        "bitmap",
        state.pool_id,
        &dirty_words,
    )? {
        let bitmap_str = rest
            .into_iter()
            .next()
            .ok_or_else(|| DbError::Decode("init-map row missing bitmap".to_string()))?;
        tick_bitmap.insert(
            word,
            BitmapAtWord {
                bitmap: decode_u256(&bitmap_str)?,
                block: 0,
            },
        );
    }
    let mut tick_data: HashMap<i32, LiquidityAtTick> = HashMap::new();
    for (tick, rest) in fetch_rows_for_keys(
        conn,
        tables.positions,
        tables.id_col,
        "tick",
        "liquidity_gross, liquidity_net",
        state.pool_id,
        &dirty_ticks,
    )? {
        let [gross_str, net_str] = rest.as_slice() else {
            return Err(DbError::Decode("position row missing columns".to_string()));
        };
        let gross_str = gross_str.clone();
        let net_str = net_str.clone();
        drop(rest);
        tick_data.insert(
            tick,
            LiquidityAtTick {
                liquidity_gross: u256_to_u128(&decode_u256(&gross_str)?),
                liquidity_net: I256::try_from(decode_i128_net(&net_str)?)
                    .map_err(|e| DbError::Decode(format!("net decode: {e}")))?,
                block: 0,
            },
        );
    }

    let mut current_liquidity = U128::ZERO;
    let last_event = apply_event_loop(
        &mut tick_bitmap,
        &mut tick_data,
        &mut current_liquidity,
        state,
        events,
    );

    Ok(split_dirty_delta(
        state.pool_id,
        state.tick_spacing,
        dirty_ticks,
        dirty_words,
        &tick_data,
        &tick_bitmap,
        last_event,
    ))
}

/// Project a FULL post-apply map onto the events' dirty keys — the
/// verification-armed path's delta (the full map feeds the pre-commit gate;
/// only this projection feeds the persist). Sets `drained_pool` from the full
/// map's live sets (the explicit drained-pool branch's trigger).
#[must_use]
pub fn derive_liquidity_delta_from_computed(
    computed: &ComputedLiquidityUpdate,
    events: &[LiquidityUpdateEvent],
) -> LiquidityDelta {
    let (dirty_ticks, dirty_words) = event_dirty_keys(events, computed.tick_spacing);
    let drained_pool = computed.tick_data.is_empty()
        && computed
            .tick_bitmap
            .values()
            .all(|bw| bw.bitmap == U256::ZERO);
    split_dirty_delta(
        computed.pool_id,
        computed.tick_spacing,
        dirty_ticks,
        dirty_words,
        &computed.tick_data,
        &computed.tick_bitmap,
        computed.last_event,
    )
    .with_drained_pool(drained_pool)
}

/// The survive/drained split over the post-apply dirty maps: a dirty tick
/// SURVIVES iff present in `tick_data` (the loop prunes zero-gross ticks —
/// presence implies gross != 0); a dirty word SURVIVES iff its post-apply
/// bitmap is non-zero. Absent/zero keys are the drained set (deleted by key).
fn split_dirty_delta(
    pool_id: i64,
    tick_spacing: i32,
    dirty_ticks: Vec<i32>,
    dirty_words: Vec<i32>,
    tick_data: &HashMap<i32, LiquidityAtTick>,
    tick_bitmap: &HashMap<i32, BitmapAtWord>,
    last_event: Option<BlockLog>,
) -> LiquidityDelta {
    let mut survive_ticks = HashMap::new();
    let mut drained_ticks = Vec::new();
    for tick in dirty_ticks {
        match tick_data.get(&tick) {
            Some(value) => {
                survive_ticks.insert(tick, value.clone());
            }
            None => drained_ticks.push(tick),
        }
    }
    let mut survive_words = HashMap::new();
    let mut drained_words = Vec::new();
    for word in dirty_words {
        match tick_bitmap.get(&word) {
            Some(value) if value.bitmap != U256::ZERO => {
                survive_words.insert(word, value.clone());
            }
            _ => drained_words.push(word),
        }
    }
    LiquidityDelta {
        pool_id,
        tick_spacing,
        tick_data: survive_ticks,
        tick_bitmap: survive_words,
        drained_ticks,
        drained_words,
        drained_pool: false,
        last_event,
    }
}

impl LiquidityDelta {
    /// Clone-with override for the derive path's `drained_pool` (the only
    /// producer that knows it).
    fn with_drained_pool(mut self, drained_pool: bool) -> Self {
        self.drained_pool = drained_pool;
        self
    }
}

/// The delta persist's table half: the drained-pool complement delete-all
/// (explicit branch) OR the dirty upserts + per-key drained deletes.
fn persist_delta_tables(
    conn: &rusqlite::Connection,
    tables: &LiquidityTables,
    delta: &LiquidityDelta,
) -> Result<(), DbError> {
    if delta.drained_pool {
        // Fully-drained pool: the pinned complement semantic — empty live set
        // → DELETE ALL rows for the pool (the `delete_stale_rows` explicit
        // branch). The survive upserts below are empty maps → no statements.
        delete_stale_rows(
            conn,
            tables.positions,
            tables.id_col,
            "tick",
            delta.pool_id,
            &[],
        )?;
        delete_stale_rows(
            conn,
            tables.init_maps,
            tables.id_col,
            "word",
            delta.pool_id,
            &[],
        )?;
    }
    if tables.positions == LIQUIDITY_POSITIONS {
        DegenbotDb::upsert_v3_liquidity_positions_on_conn(conn, delta.pool_id, &delta.tick_data)?;
    } else {
        DegenbotDb::upsert_v4_liquidity_positions_on_conn(conn, delta.pool_id, &delta.tick_data)?;
    }
    if !delta.drained_pool {
        delete_keys_on_conn(
            conn,
            tables.positions,
            tables.id_col,
            "tick",
            delta.pool_id,
            &delta.drained_ticks,
        )?;
    }
    if tables.init_maps == INITIALIZATION_MAPS {
        DegenbotDb::upsert_v3_initialization_maps_on_conn(conn, delta.pool_id, &delta.tick_bitmap)?;
    } else {
        DegenbotDb::upsert_v4_initialization_maps_on_conn(conn, delta.pool_id, &delta.tick_bitmap)?;
    }
    if !delta.drained_pool {
        delete_keys_on_conn(
            conn,
            tables.init_maps,
            tables.id_col,
            "word",
            delta.pool_id,
            &delta.drained_words,
        )?;
    }
    Ok(())
}

/// Delete EXACTLY the keys in `keys` for `id_value` — the delta persist's
/// drained-key delete (the complement of the full-map `delete_stale_rows`:
/// `IN`, not `NOT IN`). Empty `keys` issues NO statement. Chunked across
/// SQLite's variable limit.
fn delete_keys_on_conn(
    conn: &rusqlite::Connection,
    table: &str,
    id_col: &str,
    key_col: &str,
    id_value: i64,
    keys: &[i32],
) -> Result<(), DbError> {
    if keys.is_empty() {
        return Ok(());
    }
    let chunk_cap = SQLITE_MAX_VARIABLES.saturating_sub(1);
    for chunk in keys.chunks(chunk_cap) {
        let placeholders = sql_placeholders_for(chunk.len());
        let sql =
            format!("DELETE FROM {table} WHERE {id_col} = ?1 AND {key_col} IN ({placeholders})");
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(chunk.len() + 1);
        params.push(&id_value);
        for k in chunk {
            params.push(k);
        }
        conn.execute(&sql, rusqlite::params_from_iter(params))?;
    }
    Ok(())
}

/// Shared `upsert_init_maps` impl — V3/V4 differ only in the table name + the
/// id column name. Accepts a borrowed [`rusqlite::Connection`].
fn upsert_init_maps_impl(
    conn: &rusqlite::Connection,
    id_value: i64,
    tick_bitmap: &HashMap<i32, BitmapAtWord>,
    table: &str,
    id_col: &str,
) -> Result<(), DbError> {
    let mut entries: Vec<(i32, &BitmapAtWord)> = tick_bitmap
        .iter()
        .filter(|(_, bw)| bw.bitmap != U256::ZERO)
        .map(|(w, bw)| (*w, bw))
        .collect();
    // Deterministic write order (see the positions upserts — golden captures
    // replay this statement, so VALUES order + row ids must be stable).
    entries.sort_by_key(|(w, _)| *w);
    if entries.is_empty() {
        return Ok(());
    }
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
#[cfg(test)]
fn decode_i256(s: &str) -> Result<I256, DbError> {
    I256::from_dec_str(s.trim()).map_err(|e| DbError::Decode(format!("i256 parse of {s:?}: {e}")))
}

/// Re-encode an [`I256`] to its `VARCHAR(78)` signed-decimal form (the inverse
/// of [`decode_i256`]); mirrors the Python `IntMappedToString.process_bind_param`'s
/// `str(value)` (which prepends `-` for negatives).
#[must_use]
#[cfg(test)]
fn encode_i256(v: &I256) -> String {
    v.to_string()
}

/// Narrow a [`U256`] to its low-128-bits [`U128`] (the concentrated-liquidity-math gross-liquidity
/// type). Matches the `PyO3` wrapper's `gross_bytes[16..32]` slice: V3/V4 gross
/// liquidity fits in 128 bits, but the DB column is the full 256-bit
/// `VARCHAR(78)` form (Python arbitrary-precision `int`).
#[must_use]
fn u256_to_u128(v: &U256) -> U128 {
    let bytes = v.to_be_bytes::<32>();
    // `bytes[16..32]` is exactly 16 bytes (fixed `[u8; 32]` source), so this
    // slice→array copy is infallible — no `try_into().expect` needed.
    let mut low = [0u8; 16];
    low.copy_from_slice(&bytes[16..32]);
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
#[expect(clippy::unwrap_used)]
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

    // ── full-drain deletes ALL rows on empty `live_keys` ────────
    //
    // A fully-drained pool (every position burned to gross=0 → every tick
    // pruned from `tick_data`) produces `live_ticks = []` / `live_words = []`
    // in `persist_v3`/`persist_v4`. The shared `delete_stale_rows` helper MUST
    // then issue `DELETE FROM {table} WHERE {id_col} = ?1` (drop everything),
    // matching the Python `db_ticks - helper_ticks` complement-delete-all
    // semantics (helper empty → drop all). The pre-fix code early-returned
    // `Ok(())` on the empty path, deleting nothing → ghost rows linger →
    // compounding corruption on the next apply's reconstituted base.

    use crate::discovery::{V3PoolRowInput, V4PoolRowInput};
    use crate::migrate::SchemaState;
    use alloy::primitives::address;

    /// A fresh in-memory write-capable DB seeded with one V3 exchange + pool.
    /// Returns `(db, pool_id, pool_address)`.
    fn v3_db_with_pool() -> (DegenbotDb, i64, String) {
        let (db, state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        assert!(matches!(state, SchemaState::FreshStandalone { .. }));
        let factory = address!("0x1F98431c8aD98523631AE4a59f267346ea31F984");
        db.upsert_exchange(1, "uniswap_v3", factory, None).unwrap();
        let pool_address = address!("0xaAaAaAaaAaAaAaaAaAAAAAAAAaaaAaAaAaaAaaAa");
        db.upsert_v3_pools(
            1,
            "uniswap_v3",
            1,
            1_000_000,
            &[V3PoolRowInput {
                address: pool_address,
                token0_address: address!("0x1111111111111111111111111111111111111111"),
                token1_address: address!("0x2222222222222222222222222222222222222222"),
                fee: 0,
                tick_spacing: 10,
            }],
        )
        .unwrap();
        let addr_s = pool_address.to_checksum(None);
        let pool_id: i64 = {
            let conn = db.lock();
            conn.query_row(
                "SELECT id FROM pools WHERE address = ?1 AND chain = 1",
                rusqlite::params![&addr_s],
                |r| r.get(0),
            )
            .unwrap()
        };
        (db, pool_id, addr_s)
    }

    /// Seed a V3 position row directly (decimal `VARCHAR(78)` form, matching the
    /// Python `IntMappedToString` bind + the Rust decode path).
    fn seed_v3_position(db: &DegenbotDb, pool_id: i64, tick: i32, net: I256, gross: U128) {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO liquidity_positions (pool_id, tick, liquidity_net, liquidity_gross) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                pool_id,
                tick,
                encode_i256(&net),
                encode_u256(&u128_to_u256(gross))
            ],
        )
        .unwrap();
    }

    fn seed_v3_init_map(db: &DegenbotDb, pool_id: i64, word: i32, bitmap: U256) {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO initialization_maps (pool_id, word, bitmap) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![pool_id, word, encode_u256(&bitmap)],
        )
        .unwrap();
    }

    fn count_rows(db: &DegenbotDb, table: &str, id_col: &str, id_val: i64) -> i64 {
        let conn = db.lock();
        conn.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE {id_col} = ?1"),
            rusqlite::params![id_val],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Direct unit repro: an empty `live_keys` slice MUST delete every row for
    /// the pool (the "delete all" semantic). Pre-fix this early-returned and
    /// deleted nothing → ghost rows.
    #[test]
    fn delete_stale_v3_empty_live_deletes_all_rows() {
        let (db, pool_id, _addr) = v3_db_with_pool();
        seed_v3_position(
            &db,
            pool_id,
            -10,
            I256::try_from(1_000_000).unwrap(),
            U128::from(1_000_000u64),
        );
        seed_v3_position(
            &db,
            pool_id,
            10,
            I256::try_from(-1_000_000).unwrap(),
            U128::from(1_000_000u64),
        );
        seed_v3_init_map(&db, pool_id, -1, U256::from(1u64) << 255);
        seed_v3_init_map(&db, pool_id, 0, U256::from(1u64) << 1);
        assert_eq!(
            count_rows(&db, "liquidity_positions", "pool_id", pool_id),
            2
        );
        assert_eq!(
            count_rows(&db, "initialization_maps", "pool_id", pool_id),
            2
        );

        {
            let conn = db.lock();
            DegenbotDb::delete_stale_v3_positions_on_conn(&conn, pool_id, &[]).unwrap();
            DegenbotDb::delete_stale_v3_init_maps_on_conn(&conn, pool_id, &[]).unwrap();
        }

        assert_eq!(
            count_rows(&db, "liquidity_positions", "pool_id", pool_id),
            0,
            "empty live set must drop ALL position rows"
        );
        assert_eq!(
            count_rows(&db, "initialization_maps", "pool_id", pool_id),
            0,
            "empty live set must drop ALL init-map rows"
        );
    }

    /// End-to-end repro through the public apply flow: a Mint grows the seeded
    /// position + opens a second pair, then two Burns fully drain EVERY tick →
    /// `live_ticks`/`live_words` empty → the drained pool's tables must be empty
    /// + the marker stamped. Pre-fix, the seed rows lingered as ghosts (count 2).
    #[test]
    fn apply_v3_full_drain_empties_tables_and_stamps_marker() {
        let (db, pool_id, addr) = v3_db_with_pool();
        seed_v3_position(
            &db,
            pool_id,
            -10,
            I256::try_from(1_000_000).unwrap(),
            U128::from(1_000_000u64),
        );
        seed_v3_position(
            &db,
            pool_id,
            10,
            I256::try_from(-1_000_000).unwrap(),
            U128::from(1_000_000u64),
        );
        seed_v3_init_map(&db, pool_id, -1, U256::from(1u64) << 255);
        seed_v3_init_map(&db, pool_id, 0, U256::from(1u64) << 1);

        let pos = |n: i64| I256::try_from(n).unwrap();
        let events = [
            LiquidityUpdateEvent {
                block_number: 100,
                log_index: 0,
                tick_lower: -10,
                tick_upper: 10,
                liquidity_delta: pos(500_000),
            },
            // A second pair (ticks 100/110) so the partial-drain NOT IN delete
            // path is exercised before the final full drain.
            LiquidityUpdateEvent {
                block_number: 100,
                log_index: 1,
                tick_lower: 100,
                tick_upper: 110,
                liquidity_delta: pos(250_000),
            },
            // Burn the -10..10 pair back to gross=0 (pruned → dropped first).
            LiquidityUpdateEvent {
                block_number: 101,
                log_index: 0,
                tick_lower: -10,
                tick_upper: 10,
                liquidity_delta: pos(-1_500_000),
            },
            // Burn the 100..110 pair to gross=0 too → every tick pruned → empty.
            LiquidityUpdateEvent {
                block_number: 102,
                log_index: 0,
                tick_lower: 100,
                tick_upper: 110,
                liquidity_delta: pos(-250_000),
            },
        ];
        db.apply_v3_liquidity_updates(1, &addr, &events).unwrap();

        assert_eq!(
            count_rows(&db, "liquidity_positions", "pool_id", pool_id),
            0,
            "fully-drained pool must have NO ghost position rows"
        );
        assert_eq!(
            count_rows(&db, "initialization_maps", "pool_id", pool_id),
            0,
            "fully-drained pool must have NO ghost init-map rows"
        );
        // The marker is still stamped even on a full drain (last event wins).
        let conn = db.lock();
        let marker: (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT liquidity_update_block, liquidity_update_log_index FROM uniswap_v3_pools WHERE pool_id = ?1",
                rusqlite::params![pool_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            marker,
            (Some(102), Some(0)),
            "marker must stamp the last event"
        );
    }

    // ── V4 mirror ───────────────────────────────────────────────────────

    fn v4_db_with_pool() -> (DegenbotDb, i64, String) {
        let (db, state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        assert!(matches!(state, SchemaState::FreshStandalone { .. }));
        let manager = address!("0xbBbBbBbbBbBbBbbBbBBBBBBBBbbbBbBbBbbBbbBb");
        db.upsert_exchange(1, "uniswap_v4", manager, None).unwrap();
        db.upsert_pool_manager(manager, 1, "uniswap_v4", None, 1)
            .unwrap();
        let pool_hash = "0x".to_string() + &"c".repeat(64);
        db.upsert_v4_pools(
            1,
            &manager.to_checksum(None),
            1_000_000,
            &[V4PoolRowInput {
                pool_hash: pool_hash.clone(),
                hooks: address!("0x0000000000000000000000000000000000000000"),
                currency0_address: address!("0x1111111111111111111111111111111111111111"),
                currency1_address: address!("0x2222222222222222222222222222222222222222"),
                fee: 0,
                tick_spacing: 10,
            }],
        )
        .unwrap();
        let managed_pool_id: i64 = {
            let conn = db.lock();
            conn.query_row(
                "SELECT managed_pool_id FROM uniswap_v4_pools WHERE pool_hash = ?1",
                rusqlite::params![&pool_hash],
                |r| r.get(0),
            )
            .unwrap()
        };
        (db, managed_pool_id, pool_hash)
    }

    fn seed_v4_position(db: &DegenbotDb, managed_pool_id: i64, tick: i32, net: I256, gross: U128) {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO managed_pool_liquidity_positions (managed_pool_id, tick, liquidity_net, liquidity_gross) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![managed_pool_id, tick, encode_i256(&net), encode_u256(&u128_to_u256(gross))],
        )
        .unwrap();
    }

    fn seed_v4_init_map(db: &DegenbotDb, managed_pool_id: i64, word: i32, bitmap: U256) {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO managed_pool_initialization_maps (managed_pool_id, word, bitmap) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![managed_pool_id, word, encode_u256(&bitmap)],
        )
        .unwrap();
    }

    #[test]
    fn delete_stale_v4_empty_live_deletes_all_rows() {
        let (db, managed_pool_id, _hash) = v4_db_with_pool();
        seed_v4_position(
            &db,
            managed_pool_id,
            -10,
            I256::try_from(1_000_000).unwrap(),
            U128::from(1_000_000u64),
        );
        seed_v4_position(
            &db,
            managed_pool_id,
            10,
            I256::try_from(-1_000_000).unwrap(),
            U128::from(1_000_000u64),
        );
        seed_v4_init_map(&db, managed_pool_id, -1, U256::from(1u64) << 255);
        seed_v4_init_map(&db, managed_pool_id, 0, U256::from(1u64) << 1);
        assert_eq!(
            count_rows(
                &db,
                "managed_pool_liquidity_positions",
                "managed_pool_id",
                managed_pool_id
            ),
            2
        );
        assert_eq!(
            count_rows(
                &db,
                "managed_pool_initialization_maps",
                "managed_pool_id",
                managed_pool_id
            ),
            2
        );

        {
            let conn = db.lock();
            DegenbotDb::delete_stale_v4_positions_on_conn(&conn, managed_pool_id, &[]).unwrap();
            DegenbotDb::delete_stale_v4_init_maps_on_conn(&conn, managed_pool_id, &[]).unwrap();
        }

        assert_eq!(
            count_rows(
                &db,
                "managed_pool_liquidity_positions",
                "managed_pool_id",
                managed_pool_id
            ),
            0
        );
        assert_eq!(
            count_rows(
                &db,
                "managed_pool_initialization_maps",
                "managed_pool_id",
                managed_pool_id
            ),
            0
        );
    }

    // ── Perf B: the delta persist paths ────────────────────────────────
    //
    // The delta persist must leave the DB in EXACTLY the state the
    // full-map persist leaves: the strongest possible pin is an
    // equivalence test — both persists run on identical inputs into two
    // DBs, then every row of both tables must match. Plus: the drained-key
    // delete removes EXACTLY the drained rows (untouched rows survive —
    // the property the complement delete used to guarantee wholesale), and
    // the dirty-subset compute agrees with the full compute on every key
    // both can see.

    /// Snapshot `(tick, net, gross)` rows for one pool, sorted.
    fn snapshot_positions(conn: &rusqlite::Connection, pool_id: i64) -> Vec<(i32, String, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT tick, liquidity_net, liquidity_gross FROM liquidity_positions \
                 WHERE pool_id = ?1 ORDER BY tick",
            )
            .unwrap();
        let rows = stmt
            .query_map(rusqlite::params![pool_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    /// Snapshot `(word, bitmap)` rows for one pool, sorted.
    fn snapshot_words(conn: &rusqlite::Connection, pool_id: i64) -> Vec<(i32, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT word, bitmap FROM initialization_maps WHERE pool_id = ?1 ORDER BY word",
            )
            .unwrap();
        let rows = stmt
            .query_map(rusqlite::params![pool_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    /// Seed `n` spaced positions + their words (deterministic values).
    fn seed_v3_deep_map(db: &DegenbotDb, pool_id: i64, n: usize) {
        let mut ticks: Vec<i32> = (0..i32::try_from(n).unwrap())
            .map(|k| 1_000 + 10 * k)
            .collect();
        ticks.sort_unstable();
        let conn = db.lock();
        for (k, &tick) in ticks.iter().enumerate() {
            let net: i64 = if k % 2 == 0 { 500_000 } else { -500_000 };
            conn.execute(
                "INSERT INTO liquidity_positions (pool_id, tick, liquidity_net, liquidity_gross) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    pool_id,
                    tick,
                    encode_i256(&I256::try_from(net).unwrap()),
                    encode_u256(&U256::from(1_000_000u64))
                ],
            )
            .unwrap();
            let (word, bit) = get_tick_word_and_bit_position(tick, 10);
            // Accumulate this tick's bit into the word row (insert-or-update).
            let existing: Option<String> = conn
                .query_row(
                    "SELECT bitmap FROM initialization_maps WHERE pool_id = ?1 AND word = ?2",
                    rusqlite::params![pool_id, word],
                    |r| r.get(0),
                )
                .ok();
            let bitmap = match existing {
                Some(s) => decode_u256(&s).unwrap() | (U256::from(1u64) << u64::from(bit)),
                None => U256::from(1u64) << u64::from(bit),
            };
            conn.execute(
                "INSERT INTO initialization_maps (pool_id, word, bitmap) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (pool_id, word) DO UPDATE SET bitmap = excluded.bitmap",
                rusqlite::params![pool_id, word, encode_u256(&bitmap)],
            )
            .unwrap();
        }
    }

    /// The delta persist leaves the DB in exactly the state the full-map
    /// persist leaves: identical inputs → identical rows (the strongest
    /// pin for the replacement's correctness).
    #[test]
    fn delta_persist_matches_full_persist_state() {
        let (db_full, pool_id_full, addr) = v3_db_with_pool();
        let (db_delta, pool_id_delta, _addr2) = v3_db_with_pool();
        assert_eq!(pool_id_full, pool_id_delta, "same seed → same pool ids");
        for (db, pool_id) in [(&db_full, pool_id_full), (&db_delta, pool_id_delta)] {
            seed_v3_deep_map(db, pool_id, 40);
        }
        // Seeded base: ticks 1000..=1390 step 10 (40 ticks, gross 1_000_000
        // each, one bitmap word).
        let pos = |n: i64| I256::try_from(n).unwrap();
        let events = [
            // Grow an existing tick pair (dirty, survives).
            LiquidityUpdateEvent {
                block_number: 100,
                log_index: 0,
                tick_lower: 1000,
                tick_upper: 1100,
                liquidity_delta: pos(250_000),
            },
            // Mint a brand-new pair OUTSIDE the seeded range (dirty,
            // survives — the create path).
            LiquidityUpdateEvent {
                block_number: 100,
                log_index: 1,
                tick_lower: 1400,
                tick_upper: 1500,
                liquidity_delta: pos(50_000),
            },
            // Burn an existing seeded pair to zero (dirty, DRAINED — the
            // per-key delete's target; each tick's gross hits exactly 0).
            LiquidityUpdateEvent {
                block_number: 101,
                log_index: 0,
                tick_lower: 1300,
                tick_upper: 1390,
                liquidity_delta: pos(-1_000_000),
            },
        ];

        {
            let conn = db_full.lock();
            let c = DegenbotDb::compute_v3_liquidity_update_on_conn(&conn, 1, &addr, &events)
                .unwrap()
                .unwrap();
            persist_v3(&conn, c.pool_id, &c.tick_bitmap, &c.tick_data, c.last_event).unwrap();
        }
        {
            let conn = db_delta.lock();
            let delta = DegenbotDb::compute_v3_liquidity_delta_on_conn(&conn, 1, &addr, &events)
                .unwrap()
                .unwrap();
            assert!(
                !delta.drained_pool,
                "a partial drain must not trip the drained-pool branch"
            );
            assert!(
                delta.drained_ticks.contains(&1300) && delta.drained_ticks.contains(&1390),
                "the burned pair must be the drained set"
            );
            DegenbotDb::persist_v3_liquidity_delta_on_conn(&conn, &delta).unwrap();
        }

        let (snap_full, snap_delta) = {
            let c1 = db_full.lock();
            let c2 = db_delta.lock();
            (
                (
                    snapshot_positions(&c1, pool_id_full),
                    snapshot_words(&c1, pool_id_full),
                ),
                (
                    snapshot_positions(&c2, pool_id_delta),
                    snapshot_words(&c2, pool_id_delta),
                ),
            )
        };
        assert_eq!(
            snap_full, snap_delta,
            "delta persist must reach the full persist's exact state"
        );
        // And the untouched rows must have SURVIVED on both paths (the map
        // was deep; the events touched a few keys).
        assert_eq!(
            snap_full.0.len(),
            40,
            "40 seeded + 2 minted - 2 drained = 40 rows"
        );
    }

    /// The dirty-subset compute agrees with the full compute on every key
    /// the full map contains (the dirty overlay is the full map's projection
    /// restricted to the touched keys; untouched keys pass through).
    #[test]
    fn delta_compute_matches_full_compute_projection() {
        let (db, pool_id, addr) = v3_db_with_pool();
        seed_v3_deep_map(&db, pool_id, 30);
        let pos = |n: i64| I256::try_from(n).unwrap();
        let events = [
            LiquidityUpdateEvent {
                block_number: 100,
                log_index: 0,
                tick_lower: 1000,
                tick_upper: 1100,
                liquidity_delta: pos(125_000),
            },
            LiquidityUpdateEvent {
                block_number: 100,
                log_index: 1,
                tick_lower: 1160,
                tick_upper: 1200,
                liquidity_delta: pos(-1_000_000),
            },
        ];
        let (full, delta) = {
            let conn = db.lock();
            let full = DegenbotDb::compute_v3_liquidity_update_on_conn(&conn, 1, &addr, &events)
                .unwrap()
                .unwrap();
            let delta = DegenbotDb::compute_v3_liquidity_delta_on_conn(&conn, 1, &addr, &events)
                .unwrap()
                .unwrap();
            (full, delta)
        };
        // Every dirty tick the full map tracks must appear in the delta with
        // the SAME value; drained keys must match absence.
        let projected = derive_liquidity_delta_from_computed(&full, &events);
        assert_eq!(projected.tick_data, delta.tick_data);
        assert_eq!(projected.tick_bitmap, delta.tick_bitmap);
        assert_eq!(projected.drained_ticks, delta.drained_ticks);
        assert_eq!(projected.drained_words, delta.drained_words);
        assert_eq!(projected.last_event, delta.last_event);
    }

    #[test]
    fn apply_v4_full_drain_empties_tables_and_stamps_marker() {
        let (db, managed_pool_id, hash) = v4_db_with_pool();
        seed_v4_position(
            &db,
            managed_pool_id,
            -10,
            I256::try_from(1_000_000).unwrap(),
            U128::from(1_000_000u64),
        );
        seed_v4_position(
            &db,
            managed_pool_id,
            10,
            I256::try_from(-1_000_000).unwrap(),
            U128::from(1_000_000u64),
        );
        seed_v4_init_map(&db, managed_pool_id, -1, U256::from(1u64) << 255);
        seed_v4_init_map(&db, managed_pool_id, 0, U256::from(1u64) << 1);

        let pos = |n: i64| I256::try_from(n).unwrap();
        let events = [
            LiquidityUpdateEvent {
                block_number: 200,
                log_index: 0,
                tick_lower: -10,
                tick_upper: 10,
                liquidity_delta: pos(500_000),
            },
            LiquidityUpdateEvent {
                block_number: 200,
                log_index: 1,
                tick_lower: 100,
                tick_upper: 110,
                liquidity_delta: pos(250_000),
            },
            LiquidityUpdateEvent {
                block_number: 201,
                log_index: 0,
                tick_lower: -10,
                tick_upper: 10,
                liquidity_delta: pos(-1_500_000),
            },
            LiquidityUpdateEvent {
                block_number: 202,
                log_index: 0,
                tick_lower: 100,
                tick_upper: 110,
                liquidity_delta: pos(-250_000),
            },
        ];
        db.apply_v4_liquidity_updates(&hash, 1, &events).unwrap();

        assert_eq!(
            count_rows(
                &db,
                "managed_pool_liquidity_positions",
                "managed_pool_id",
                managed_pool_id
            ),
            0
        );
        assert_eq!(
            count_rows(
                &db,
                "managed_pool_initialization_maps",
                "managed_pool_id",
                managed_pool_id
            ),
            0
        );
        let conn = db.lock();
        let marker: (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT liquidity_update_block, liquidity_update_log_index FROM uniswap_v4_pools WHERE managed_pool_id = ?1",
                rusqlite::params![managed_pool_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(marker, (Some(202), Some(0)));
    }
}

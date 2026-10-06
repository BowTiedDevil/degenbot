//! The chunk-level substrate cache (Perf C): set-shaped prefetch + a
//! write-overlay consulted before the prefetched indexes.
//!
//! The aave chunk loop's per-event substrate reads (the asset-resolution
//! JOINs, the `get_or_create_user`/`get_or_create_position` SELECT-then-INSERT
//! chains, the per-apply balance reads, the per-tx GHO re-resolve, the
//! per-parse POOL-revision read) issue ~100 single-row statements per chunk
//! against a working set of a handful of entities. This cache collapses them
//! to a handful of set-shaped `SELECT ... IN` prefetches at chunk start plus
//! in-memory map hits.
//!
//! # The §3.4 read-your-own-writes contract
//!
//! The chunk loop applies each log-transaction's events to `conn` BEFORE the
//! next log-transaction's parse reads (`process.rs` step (f)), and the
//! config dispatch applies each event intra-dispatch — both under the ONE
//! chunk `Transaction`. A prefetch snapshot taken at chunk start reflects all
//! COMMITTED state; reads for log-transaction N may consult the cache ONLY
//! where the cached state reflects every write of transactions < N. This
//! struct guarantees that by routing EVERY cache-visible write through an
//! overlay update at the apply site (the dispatch arms in `apply.rs` +
//! `config_dispatch.rs`):
//!
//! - **asset rows** — the `ReserveInitialized` apply re-reads the fresh row
//!   (one projection SELECT) and re-binds all three address indexes; the
//!   `Upgraded` apply bumps the cached revision in place (the address indexes
//!   point at the row id, so every lookup re-reads the fresh revision).
//! - **users / positions / collateral configs** — the get-or-create shells
//!   record each INSERT's row id (with its zero defaults) so the next read —
//!   this log-transaction or the next — is a map hit, byte-exact with what
//!   the SQL probe would have returned.
//! - **position balances** — every scaled-token apply records the new
//!   `(balance, last_index)` it just wrote; the next read (the per-tx GHO
//!   running-state miss, the next apply's read-modify-write) is served from
//!   the overlay, not from a stale prefetch.
//! - **the GHO row** — every write touching `aave_gho_tokens` (the
//!   `ReserveInitialized` GHO link, the discount-config applies, the
//!   deprecation clear) marks the cached row dirty; the next per-tx fetch
//!   re-queries `conn` once and re-caches. Reads NEVER serve a stale GHO row
//!   across a write.
//! - **contract revisions** — the revision applies/inserts update the cached
//!   `(market, name)` revision; the parser's per-parse POOL read sees the
//!   prior log-transaction's `ContractRevisionUpdated` exactly as the SQL
//!   probe would.
//!
//! Reads with NO cache-visible writer (the discount pre-pass's
//! `gho_discount` read, the refresh pass's post-apply context read, the
//! erc20-token upserts) stay on `conn` — direct SQL against the transaction
//! is always correct and stays out of the cache's blast radius.
//!
//! # The one deferred write class
//!
//! `ReserveDataUpdated` writes (`UPDATE aave_v3_assets SET liquidity_rate =
//! ..., last_update_block = ...`) have no mid-chunk reader (the asset
//! lookups read only id/revisions/addresses; the verification gate runs
//! post-commit; the position math uses the EVENT's index), so the applies
//! buffer into [`ChunkSubstrate::reserve_data_updates`] and flush as ONE
//! multi-row `UPDATE ... FROM (VALUES ...)` at end-of-chunk — the
//! `liquidity_updater.rs` sorted multi-row idiom (deterministic asset-id
//! order; the golden-captured ledger replays this statement). Same-asset
//! events collapse to their LAST values (each event overwrites all five
//! columns — the collapsed write is the same end state). The buffer is
//! dropped with the struct on rollback (§3.4: the flush sits inside the
//! chunk's transaction, so an error at flush reverts the chunk exactly as
//! the per-event write would have).

use std::collections::{HashMap, HashSet};

use alloy::primitives::U256;
use degenbot_db::{AaveGhoAsset, AssetRow, DbError, DegenbotDb, ScaledTokenPosition};
use rusqlite::Connection;
use rusqlite::OptionalExtension;

use crate::updater::operations_parser::addr_to_hex;

/// The per-statement bind-parameter ceiling (the `liquidity_updater.rs`
/// constant; the bundled SQLite's `SQLITE_MAX_VARIABLE_NUMBER`).
const SQLITE_MAX_VARIABLES: usize = 32_766;

/// Which position table a cache key refers to (a `Hash`-capable mirror of
/// [`ScaledTokenPosition`], which the substrate fns take).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PositionTable {
    /// `aave_v3_collateral_positions`.
    Collateral,
    /// `aave_v3_debt_positions`.
    Debt,
}

impl From<ScaledTokenPosition> for PositionTable {
    fn from(p: ScaledTokenPosition) -> Self {
        match p {
            ScaledTokenPosition::Collateral => Self::Collateral,
            ScaledTokenPosition::Debt => Self::Debt,
        }
    }
}

impl PositionTable {
    /// The literal table name (the substrate fns' compile-time dispatch).
    #[must_use]
    pub(crate) const fn table(self) -> &'static str {
        match self {
            Self::Collateral => "aave_v3_collateral_positions",
            Self::Debt => "aave_v3_debt_positions",
        }
    }
}

/// A cached position's balance state: `(balance, last_index)` — the same
/// shape [`degenbot_db`'s balance reads] return.
pub(crate) type PositionState = (U256, Option<U256>);

/// Parse a decimal-`VARCHAR` `U256` value (the substrate's
/// `parse_decimal_u256` shape — same error text).
fn parse_decimal_u256(s: &str) -> Result<U256, DbError> {
    U256::from_str_radix(s, 10).map_err(|e| DbError::Decode(format!("bad decimal U256 {s:?}: {e}")))
}

/// The three asset-lookup address kinds (the substrate lookup fns'
/// `token_type` column selectors; `"underlying"` is
/// `lookup_asset_by_underlying_address_on_conn`'s JOIN column).
pub(crate) const ASSET_KIND_UNDERLYING: &str = "underlying";
pub(crate) const ASSET_KIND_A_TOKEN: &str = "a_token";
pub(crate) const ASSET_KIND_V_TOKEN: &str = "v_token";

/// One buffered `ReserveDataUpdated` write: the asset id + its five
/// overwritten columns (the flush collapses same-asset events to the last).
type ReserveDataUpdateEntry = (i64, (U256, U256, U256, U256, i64));

/// The chunk-level substrate cache. Built once per chunk inside the chunk's
/// `Transaction`; threaded `&mut` through the per-tx parse + the apply
/// dispatch (single-threaded under `block_on`'s single-thread poll).
#[derive(Debug)]
pub struct ChunkSubstrate {
    chain_id: i64,
    /// `aave_v3_assets` rows by row id — the asset overlay's source of truth
    /// (the address indexes point here, so a revision bump in place is
    /// visible to every lookup).
    assets_by_id: HashMap<i64, AssetRow>,
    /// `(lookup kind, address)` → asset id; `None` = probed-absent (the
    /// negative cache that stands in for the SQL probe's `Ok(None)`).
    assets_by_address: HashMap<(&'static str, String), Option<i64>>,
    /// `(market_id, address)` → `aave_v3_users.id` (positive-only: a miss
    /// for a candidate address — the get-or-create INSERT runs without the
    /// SQL probe; a key ABSENT from the map means probe-then-insert,
    /// exactly today's get-or-create).
    users: HashMap<(i64, String), Option<i64>>,
    /// `(table, user_id, asset_id)` → position row id.
    positions_by_key: HashMap<(PositionTable, i64, i64), i64>,
    /// `(table, position id)` → `(balance, last_index)`.
    position_state: HashMap<(PositionTable, i64), PositionState>,
    /// `(user_id, asset_id)` → `aave_v3_user_collateral_configs.id`.
    collateral_configs: HashMap<(i64, i64), i64>,
    /// `(market_id, contract name)` → revision (the decoded
    /// `lookup_pool_revision` shape: `None` for a missing row OR a NULL
    /// column — the substrate read flattens both).
    contract_revisions: HashMap<(i64, String), Option<u32>>,
    /// The chain's GHO row (the per-tx re-resolve). `gho_queried == false`
    /// means the cached value is absent/stale — the next read re-queries.
    gho: Option<AaveGhoAsset>,
    gho_queried: bool,
    /// The deferred `ReserveDataUpdated` writes: asset id → the LAST event's
    /// `(liquidity_rate, variable_borrow_rate, liquidity_index,
    /// variable_borrow_index, block)`. Flushed as one sorted multi-row
    /// `UPDATE ... FROM (VALUES ...)` at end-of-chunk.
    reserve_data_updates: HashMap<i64, (U256, U256, U256, U256, i64)>,
}

impl ChunkSubstrate {
    /// An empty, lazily-probing cache (no prefetch). The batched-apply
    /// entrypoint and empty chunks start here; every read falls through to
    /// its SQL probe once and caches.
    pub(crate) fn lazy(chain_id: i64) -> Self {
        Self {
            chain_id,
            assets_by_id: HashMap::new(),
            assets_by_address: HashMap::new(),
            users: HashMap::new(),
            positions_by_key: HashMap::new(),
            position_state: HashMap::new(),
            collateral_configs: HashMap::new(),
            contract_revisions: HashMap::new(),
            gho: None,
            gho_queried: false,
            reserve_data_updates: HashMap::new(),
        }
    }

    /// Prefetch the chunk's touched entity sets in a handful of set-shaped
    /// `SELECT ... IN` statements (plus the market-scoped asset/contract/GHO
    /// reads — a market's reserve set is tens of rows):
    ///
    /// - the market's `aave_v3_assets` rows (the full six-column projection
    ///   the lookup fns return), indexed by id + all three address columns;
    /// - the candidate users (the chunk's topic-derived address superset —
    ///   the same extraction `process_chunk_on_conn` reports as
    ///   `touched_user_addresses`) + their positions + their collateral
    ///   configs;
    /// - the market's contract revisions + the chain's GHO row.
    ///
    #[expect(clippy::too_many_lines)] // the seven set-shaped prefetches, one body
    /// `candidate_user_addresses` MUST be checksummed (`addr_to_hex`) — the
    /// address columns use BINARY collation (the duplicate-user §4.2 guard).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on any prefetch query failure.
    pub(crate) fn prefetch_on_conn(
        conn: &Connection,
        market_id: i64,
        chain_id: i64,
        candidate_user_addresses: &[String],
        asset_candidate_addresses: &[String],
    ) -> Result<Self, DbError> {
        let mut cache = Self::lazy(chain_id);

        // (1) the market's asset rows — the EXACT projection the substrate
        //     lookups return (column order + NULL-revision decode included).
        let mut stmt = conn.prepare(
            "SELECT a.id, a.a_token_revision, a.v_token_revision, \
                t_underlying.address AS underlying, t_a.address AS a_token, t_v.address AS v_token \
             FROM aave_v3_assets a \
             JOIN erc20_tokens t_underlying ON t_underlying.id = a.underlying_asset_id \
             JOIN erc20_tokens t_a ON t_a.id = a.a_token_id \
             JOIN erc20_tokens t_v ON t_v.id = a.v_token_id \
             WHERE a.market_id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![market_id])?;
        while let Some(row) = rows.next()? {
            let asset = decode_asset_row(row)?;
            cache.index_asset(asset);
        }

        // Negative-cache the candidate addresses against the asset indexes:
        // the market's asset rows are the only JOIN rows a lookup with
        // this market_id can return, so a candidate address absent from the
        // indexes is a proven SQL `Ok(None)` — cached per kind, zero
        // statements. The candidate set covers the parse-time askings:
        // topic words (users/liquidation assets) + log emitters (the
        // scaled-token classification).
        for addr in asset_candidate_addresses {
            for kind in [
                ASSET_KIND_UNDERLYING,
                ASSET_KIND_A_TOKEN,
                ASSET_KIND_V_TOKEN,
            ] {
                cache
                    .assets_by_address
                    .entry((kind, addr.clone()))
                    .or_insert(None);
            }
        }

        // (2) the market's contract revisions.
        let mut stmt =
            conn.prepare("SELECT name, revision FROM aave_v3_contracts WHERE market_id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![market_id])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            let revision: Option<i64> = row.get(1)?;
            cache.contract_revisions.insert(
                (market_id, name),
                revision.map(|v| u32::try_from(v).unwrap_or(0)),
            );
        }

        // (3) the chain's GHO row.
        cache.gho = DegenbotDb::fetch_aave_gho_asset_on_conn(conn, chain_id)?;
        cache.gho_queried = true;

        // (4..7) the candidate users + their positions + their collateral
        // configs. Skipped when the chunk's logs carry no address topics —
        // an empty `IN ()` is invalid SQL, and the lazy probes cover the
        // (degenerate) reads anyway.
        if candidate_user_addresses.is_empty() {
            return Ok(cache);
        }
        // Sorted + deduped: the IN-list's ORDER defines the statement's
        // bind-parameter shape, and the statement ledger is golden-captured —
        // the list must not depend on hashbrown iteration order.
        let candidates: Vec<&String> = {
            let mut seen: HashSet<&String> = HashSet::new();
            let mut out: Vec<&String> = Vec::new();
            for addr in candidate_user_addresses {
                if seen.insert(addr) {
                    out.push(addr);
                }
            }
            out.sort();
            out
        };
        // (4) the candidate users.
        let mut chunk_start = 0;
        while chunk_start < candidates.len() {
            let chunk_end = usize::min(chunk_start + SQLITE_MAX_VARIABLES - 1, candidates.len());
            let slice = &candidates[chunk_start..chunk_end];
            let placeholders = vec!["?"; slice.len()].join(", ");
            let sql = format!(
                "SELECT id, address FROM aave_v3_users \
                 WHERE market_id = ?1 AND address IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut bind: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(slice.len() + 1);
            bind.push(&market_id);
            for addr in slice {
                bind.push(addr);
            }
            let mut rows = stmt.query(bind.as_slice())?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                let address: String = row.get(1)?;
                cache.users.insert((market_id, address), Some(id));
            }
            chunk_start = chunk_end;
        }

        // Negative-cache the absent candidates: the candidate set is a
        // SUPERSET of the addresses the parse will ask get-or-create for,
        // so an absent candidate is a proven DB absence — the INSERT runs
        // without the probe (the INSERT's UNIQUE constraint stays the
        // loud backstop).
        for addr in &candidates {
            cache
                .users
                .entry((market_id, (*addr).clone()))
                .or_insert(None);
        }

        // (5)+(6) the candidates' positions (both tables) — with their
        // balance state (the per-apply read-modify-write's seed).
        for table in [PositionTable::Collateral, PositionTable::Debt] {
            chunk_start = 0;
            while chunk_start < candidates.len() {
                let chunk_end =
                    usize::min(chunk_start + SQLITE_MAX_VARIABLES - 1, candidates.len());
                let slice = &candidates[chunk_start..chunk_end];
                let placeholders = vec!["?"; slice.len()].join(", ");
                let sql = format!(
                    "SELECT p.id, p.user_id, p.asset_id, p.balance, p.last_index \
                     FROM {} p \
                     JOIN aave_v3_users u ON u.id = p.user_id \
                     WHERE u.market_id = ?1 AND u.address IN ({placeholders})",
                    table.table(),
                );
                let mut stmt = conn.prepare(&sql)?;
                let mut bind: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(slice.len() + 1);
                bind.push(&market_id);
                for addr in slice {
                    bind.push(addr);
                }
                let mut rows = stmt.query(bind.as_slice())?;
                while let Some(row) = rows.next()? {
                    let id: i64 = row.get(0)?;
                    let user_id: i64 = row.get(1)?;
                    let asset_id: i64 = row.get(2)?;
                    let balance: String = row.get(3)?;
                    let last_index: Option<String> = row.get(4)?;
                    let state = (
                        parse_decimal_u256(&balance)?,
                        last_index.as_deref().map(parse_decimal_u256).transpose()?,
                    );
                    cache
                        .positions_by_key
                        .insert((table, user_id, asset_id), id);
                    cache.position_state.insert((table, id), state);
                }
                chunk_start = chunk_end;
            }
        }

        // (7) the candidates' collateral configs (the
        // `ReserveUsedAsCollateral` probe's rows).
        chunk_start = 0;
        while chunk_start < candidates.len() {
            let chunk_end = usize::min(chunk_start + SQLITE_MAX_VARIABLES - 1, candidates.len());
            let slice = &candidates[chunk_start..chunk_end];
            let placeholders = vec!["?"; slice.len()].join(", ");
            let sql = format!(
                "SELECT cc.user_id, cc.asset_id, cc.id \
                 FROM aave_v3_user_collateral_configs cc \
                 JOIN aave_v3_users u ON u.id = cc.user_id \
                 WHERE u.market_id = ?1 AND u.address IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut bind: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(slice.len() + 1);
            bind.push(&market_id);
            for addr in slice {
                bind.push(addr);
            }
            let mut rows = stmt.query(bind.as_slice())?;
            while let Some(row) = rows.next()? {
                let user_id: i64 = row.get(0)?;
                let asset_id: i64 = row.get(1)?;
                let id: i64 = row.get(2)?;
                cache.collateral_configs.insert((user_id, asset_id), id);
            }
            chunk_start = chunk_end;
        }

        Ok(cache)
    }

    /// Index one prefetched/refreshed asset row (id map + all three address
    /// kinds).
    fn index_asset(&mut self, asset: AssetRow) {
        let id = asset.id;
        self.assets_by_address.insert(
            (
                ASSET_KIND_UNDERLYING,
                asset.underlying_token_address.clone(),
            ),
            Some(id),
        );
        self.assets_by_address.insert(
            (ASSET_KIND_A_TOKEN, asset.a_token_address.clone()),
            Some(id),
        );
        self.assets_by_address.insert(
            (ASSET_KIND_V_TOKEN, asset.v_token_address.clone()),
            Some(id),
        );
        self.assets_by_id.insert(id, asset);
    }

    /// Drop every address index pointing at `asset_id` (the
    /// `ReserveInitialized` re-bind — a re-initialized reserve's OLD token
    /// addresses must stop resolving after the substrate UPDATE re-points the
    /// row's token ids, exactly as the SQL JOIN would).
    fn unindex_asset(&mut self, asset_id: i64) {
        self.assets_by_address.retain(|_, v| *v != Some(asset_id));
    }

    // ── asset lookups (the per-event resolution JOINs) ──────────────────

    /// The `lookup_asset_by_token_address_on_conn` /
    /// `lookup_asset_by_underlying_address_on_conn` cache shell. `kind` is
    /// one of the `ASSET_KIND_*` constants (a bad kind is the substrate's
    /// loud `DbError::Decode`, BEFORE any map access — the silent-arm rule).
    ///
    /// Map hit → the cached row. Miss → the SQL probe (once per
    /// `(kind, address)`, including a cached negative) → cache → return.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a probe failure or a bad `kind`.
    pub(crate) fn lookup_asset_row(
        &mut self,
        conn: &Connection,
        market_id: i64,
        kind: &str,
        token_address: &str,
    ) -> Result<Option<AssetRow>, DbError> {
        let kind = Self::asset_kind(kind)?;
        let key = (kind, token_address.to_string());
        if let Some(maybe_id) = self.assets_by_address.get(&key) {
            return match *maybe_id {
                Some(id) => Ok(self.assets_by_id.get(&id).cloned()),
                None => Ok(None),
            };
        }
        // Lazy probe — the substrate's own lookup (its SQL text is the
        // golden ledger's), then cache the answer (positive or negative).
        let row = match kind {
            ASSET_KIND_UNDERLYING => DegenbotDb::lookup_asset_by_underlying_address_on_conn(
                conn,
                market_id,
                token_address,
            )?,
            _ => DegenbotDb::lookup_asset_by_token_address_on_conn(
                conn,
                market_id,
                token_address,
                kind,
            )?,
        };
        let probed_id = row.as_ref().map(|r| r.id);
        self.assets_by_address.insert(key, probed_id);
        if let Some(r) = row {
            self.assets_by_id.insert(r.id, r.clone());
            return Ok(Some(r));
        }
        Ok(None)
    }

    /// The `lookup_asset_id_by_token_address_on_conn` cache shell (the
    /// id-only classification lookup — the same index, a cheaper projection).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a probe failure or a bad `token_type`.
    pub(crate) fn lookup_asset_id(
        &mut self,
        conn: &Connection,
        market_id: i64,
        token_type: &str,
        token_address: &str,
    ) -> Result<Option<i64>, DbError> {
        Ok(self
            .lookup_asset_row(conn, market_id, token_type, token_address)?
            .map(|r| r.id))
    }

    /// Loud existence probe for the deferred `ReserveDataUpdated` writes: the
    /// single-row UPDATE's `updated == 0 → DbError::MissingRow` contract,
    /// checked at `buffer` time so the error surfaces at the failing event
    /// (byte-parity with today's rollback semantics).
    ///
    /// # Errors
    ///
    /// Returns the substrate's `MissingRow` (same message) when the asset id
    /// has no row.
    pub(crate) fn require_asset(
        &mut self,
        conn: &Connection,
        asset_id: i64,
    ) -> Result<(), DbError> {
        if self.assets_by_id.contains_key(&asset_id) {
            return Ok(());
        }
        let found: Option<i64> = conn
            .query_row(
                "SELECT id FROM aave_v3_assets WHERE id = ?1",
                rusqlite::params![asset_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => DbError::MissingRow(format!(
                    "aave_v3_assets id={asset_id} (ReserveDataUpdated target)"
                )),
                e => DbError::Sqlite(e),
            })
            .map(Some)
            .or_else(|e| match e {
                DbError::MissingRow(_) => Ok(None),
                e => Err(e),
            })?;
        if found.is_none() {
            return Err(DbError::MissingRow(format!(
                "aave_v3_assets id={asset_id} (ReserveDataUpdated target)"
            )));
        }
        // Present but unprefetched (a foreign-market id or a post-prefetch
        // insert this cache didn't see — both loud-error or overlay-tracked
        // paths) — nothing to index (the lookups don't consult require_asset's
        // result); the write proceeds exactly as the single-row UPDATE would.
        Ok(())
    }

    /// Buffer one `ReserveDataUpdated` write (later events on the same asset
    /// overwrite earlier ones — the flush writes each asset's LAST values).
    pub(crate) fn buffer_reserve_data_update(
        &mut self,
        asset_id: i64,
        liquidity_rate: U256,
        variable_borrow_rate: U256,
        liquidity_index: U256,
        variable_borrow_index: U256,
        block_number: u64,
    ) {
        self.reserve_data_updates.insert(
            asset_id,
            (
                liquidity_rate,
                variable_borrow_rate,
                liquidity_index,
                variable_borrow_index,
                i64::try_from(block_number).unwrap_or(i64::MAX),
            ),
        );
    }

    /// Flush the buffered `ReserveDataUpdated` writes as ONE sorted multi-row
    /// `UPDATE ... FROM (VALUES ...)` per bind-cap-sized slice (the
    /// `liquidity_updater.rs` deterministic-order idiom: the golden ledger
    /// replays this statement, so the VALUES order must not depend on
    /// hashbrown iteration order). A short row-count is a loud
    /// [`DbError::MissingRow`] — never a silent no-op.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on the UPDATE failure or a row-count shortfall.
    pub(crate) fn flush_reserve_data_updates(&mut self, conn: &Connection) -> Result<(), DbError> {
        if self.reserve_data_updates.is_empty() {
            return Ok(());
        }
        // Deterministic write order: sort by asset id (the multi-row idiom).
        let mut entries: Vec<ReserveDataUpdateEntry> = self.reserve_data_updates.drain().collect();
        entries.sort_by_key(|(id, _)| *id);
        for chunk in entries.chunks(SQLITE_MAX_VARIABLES / 6) {
            let placeholders = chunk
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    let base = i * 6;
                    format!(
                        "(?{}, ?{}, ?{}, ?{}, ?{}, ?{})",
                        base + 1,
                        base + 2,
                        base + 3,
                        base + 4,
                        base + 5,
                        base + 6
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            // SQLite VALUES columns are positional (column1..N) — the
            // `AS v(cols)` derived-column list is PostgreSQL-only.
            let sql = format!(
                "UPDATE aave_v3_assets AS a SET \
                    liquidity_rate = v.column2, borrow_rate = v.column3, \
                    liquidity_index = v.column4, borrow_index = v.column5, \
                    last_update_block = v.column6 \
                 FROM (VALUES {placeholders}) AS v \
                 WHERE a.id = v.column1"
            );
            let rows_data: Vec<(i64, [String; 4], i64)> = chunk
                .iter()
                .map(|(id, (lr, br, li, bi, block))| {
                    (
                        *id,
                        [
                            lr.to_string(),
                            br.to_string(),
                            li.to_string(),
                            bi.to_string(),
                        ],
                        *block,
                    )
                })
                .collect();
            let mut bind: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(rows_data.len() * 6);
            for (id, strs, block) in &rows_data {
                bind.push(id);
                bind.push(&strs[0]);
                bind.push(&strs[1]);
                bind.push(&strs[2]);
                bind.push(&strs[3]);
                bind.push(block);
            }
            let updated = conn.execute(&sql, bind.as_slice())?;
            if updated != chunk.len() {
                return Err(DbError::MissingRow(format!(
                    "aave_v3_assets multi-row ReserveDataUpdated flush matched {updated} of {} rows",
                    chunk.len()
                )));
            }
        }
        Ok(())
    }

    /// The `ReserveInitialized` apply's overlay refresh: re-read the fresh
    /// row (one projection SELECT — the create/re-point changed the token
    /// bindings) and re-bind the address indexes.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on the refresh SELECT failure.
    pub(crate) fn refresh_asset_row(
        &mut self,
        conn: &Connection,
        asset_id: i64,
    ) -> Result<(), DbError> {
        let mut stmt = conn.prepare(
            "SELECT a.id, a.a_token_revision, a.v_token_revision, \
                t_underlying.address AS underlying, t_a.address AS a_token, t_v.address AS v_token \
             FROM aave_v3_assets a \
             JOIN erc20_tokens t_underlying ON t_underlying.id = a.underlying_asset_id \
             JOIN erc20_tokens t_a ON t_a.id = a.a_token_id \
             JOIN erc20_tokens t_v ON t_v.id = a.v_token_id \
             WHERE a.id = ?1",
        )?;
        let row = stmt
            .query_row(rusqlite::params![asset_id], decode_asset_row)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => DbError::MissingRow(format!(
                    "aave_v3_assets id={asset_id} (ReserveInitialized overlay refresh target)"
                )),
                e => DbError::Sqlite(e),
            })?;
        self.unindex_asset(asset_id);
        self.index_asset(row);
        Ok(())
    }

    /// The `Upgraded` apply's overlay bump: the revision columns changed in
    /// place (addresses unchanged — the address indexes keep pointing at the
    /// row id, so every lookup sees the fresh revision).
    pub(crate) fn record_asset_revision(
        &mut self,
        asset_id: i64,
        is_a_token: bool,
        new_revision: i64,
    ) {
        let revision = u32::try_from(new_revision).unwrap_or(0);
        if let Some(row) = self.assets_by_id.get_mut(&asset_id) {
            if is_a_token {
                row.a_token_revision = revision;
            } else {
                row.v_token_revision = revision;
            }
        }
    }

    // ── users + positions + collateral configs (get-or-create shells) ────

    /// The `get_or_create_user_on_conn` cache shell: map hit → the id (zero
    /// statements); miss → the substrate fn (probe + INSERT, its own SQL
    /// text) → record.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on the substrate call.
    pub(crate) fn user_id_or_create(
        &mut self,
        conn: &Connection,
        market_id: i64,
        address: &str,
        gho_discount: i64,
    ) -> Result<i64, DbError> {
        match self.users.get(&(market_id, address.to_string())) {
            Some(Some(id)) => return Ok(*id),
            Some(None) => {
                // Proven absent (the prefetch's candidate scan) — the
                // INSERT-only path: byte-identical SQL to the substrate
                // probe-then-insert body's INSERT, minus the probe (the
                // ledger golden pins the text).
                conn.execute(
                    "INSERT INTO aave_v3_users (market_id, address, e_mode, gho_discount, stk_aave_balance, isolation_mode_collateral_asset_id, isolation_mode_debt) VALUES (?1, ?2, 0, ?3, NULL, NULL, '0')",
                    rusqlite::params![market_id, address, gho_discount],
                )?;
                let id = conn.last_insert_rowid();
                self.users
                    .insert((market_id, address.to_string()), Some(id));
                return Ok(id);
            }
            None => {} // not yet probed — the lazy path below.
        }
        let id = DegenbotDb::get_or_create_user_on_conn(conn, market_id, address, gho_discount)?;
        self.users
            .insert((market_id, address.to_string()), Some(id));
        Ok(id)
    }

    /// The `get_or_create_*_position_on_conn` cache shell. A fresh INSERT's
    /// zero defaults are recorded as the position's state (the next balance
    /// read — this log-transaction's apply or the next's — is a map hit).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on the substrate call.
    pub(crate) fn position_id_or_create(
        &mut self,
        conn: &Connection,
        position: ScaledTokenPosition,
        user_id: i64,
        asset_id: i64,
    ) -> Result<i64, DbError> {
        let table = PositionTable::from(position);
        if let Some(id) = self.positions_by_key.get(&(table, user_id, asset_id)) {
            return Ok(*id);
        }
        let id = match position {
            ScaledTokenPosition::Collateral => {
                DegenbotDb::get_or_create_collateral_position_on_conn(conn, user_id, asset_id)?
            }
            ScaledTokenPosition::Debt => {
                DegenbotDb::get_or_create_debt_position_on_conn(conn, user_id, asset_id)?
            }
        };
        self.positions_by_key.insert((table, user_id, asset_id), id);
        self.position_state
            .entry((table, id))
            .or_insert((U256::ZERO, None));
        Ok(id)
    }

    /// The `lookup_position_balance_index_on_conn` cache shell (the parser's
    /// GHO running-state seed read). Missing row → the substrate's
    /// `MissingRow` (same message).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::MissingRow`] when the position has no row, or
    /// [`DbError`] on a probe/decode failure.
    pub(crate) fn lookup_position_balance_index(
        &mut self,
        conn: &Connection,
        position: ScaledTokenPosition,
        position_id: i64,
    ) -> Result<PositionState, DbError> {
        self.position_state_inner(conn, position, position_id, PositionRead::Lookup)
    }

    /// The scaled-token apply's read-modify-write seed read. Missing row →
    /// the apply body's `MissingRow` (same message, the apply-target tag
    /// target)").
    ///
    /// # Errors
    ///
    /// Same as [`Self::lookup_position_balance_index`] with the apply-side
    /// error text.
    pub(crate) fn position_state_for_apply(
        &mut self,
        conn: &Connection,
        position: ScaledTokenPosition,
        position_id: i64,
    ) -> Result<PositionState, DbError> {
        self.position_state_inner(conn, position, position_id, PositionRead::Apply)
    }

    /// The bad-debt reset's read (`reset_debt_position_to_zero_on_conn`'s
    /// max-with-prev seed — debt table only). Missing row → the reset shell's
    /// `MissingRow` (same message).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::MissingRow`] when the position has no row, or
    /// [`DbError`] on a probe/decode failure.
    pub(crate) fn debt_position_state_for_reset(
        &mut self,
        conn: &Connection,
        position_id: i64,
    ) -> Result<PositionState, DbError> {
        self.position_state_inner(
            conn,
            ScaledTokenPosition::Debt,
            position_id,
            PositionRead::Reset,
        )
    }

    /// Record the state a scaled-token apply just WROTE (the overlay's
    /// read-your-own-writes backbone: the next read — the next apply's
    /// seed, the GHO running-state seed — is a map hit carrying exactly what
    /// the SQL probe would return).
    pub(crate) fn record_position_write(
        &mut self,
        position: ScaledTokenPosition,
        position_id: i64,
        balance: U256,
        last_index: Option<U256>,
    ) {
        self.position_state.insert(
            (PositionTable::from(position), position_id),
            (balance, last_index),
        );
    }

    /// The `apply_reserve_used_as_collateral_on_conn` cache shell: map hit →
    /// the inline flag UPDATE (the substrate's exact SQL text, zero probes);
    /// miss → the substrate fn (probe + UPDATE-or-INSERT) → record.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a write failure.
    pub(crate) fn apply_reserve_used_as_collateral(
        &mut self,
        conn: &Connection,
        user_id: i64,
        asset_id: i64,
        enabled: bool,
    ) -> Result<i64, DbError> {
        if let Some(id) = self.collateral_configs.get(&(user_id, asset_id)) {
            conn.execute(
                "UPDATE aave_v3_user_collateral_configs SET enabled = ?1 \
                 WHERE user_id = ?2 AND asset_id = ?3",
                rusqlite::params![enabled, user_id, asset_id],
            )?;
            return Ok(*id);
        }
        let id =
            DegenbotDb::apply_reserve_used_as_collateral_on_conn(conn, user_id, asset_id, enabled)?;
        self.collateral_configs.insert((user_id, asset_id), id);
        Ok(id)
    }

    // ── contracts + the GHO row ─────────────────────────────────────────

    /// The `lookup_pool_revision_on_conn` cache shell (the parser's
    /// per-parse read). Missing row / NULL revision → `None` (the
    /// substrate's flatten).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a probe failure.
    pub(crate) fn pool_revision(
        &mut self,
        conn: &Connection,
        market_id: i64,
        contract_name: &str,
    ) -> Result<Option<u32>, DbError> {
        if let Some(rev) = self
            .contract_revisions
            .get(&(market_id, contract_name.to_string()))
        {
            return Ok(*rev);
        }
        let rev = DegenbotDb::lookup_pool_revision_on_conn(conn, market_id, contract_name)?;
        self.contract_revisions
            .insert((market_id, contract_name.to_string()), rev);
        Ok(rev)
    }

    /// The `ContractRevisionUpdated` apply's overlay write.
    pub(crate) fn record_contract_revision(
        &mut self,
        market_id: i64,
        contract_name: &str,
        new_revision: i64,
    ) {
        self.contract_revisions
            .insert((market_id, contract_name.to_string()), {
                Some(u32::try_from(new_revision).unwrap_or(0))
            });
    }

    /// A mid-chunk contract INSERT's overlay write (a name the cache hasn't
    /// seen gets the event's revision; a seen name keeps its cached value —
    /// the substrate INSERT would have hit the UNIQUE constraint first).
    pub(crate) fn record_contract_insert(
        &mut self,
        market_id: i64,
        contract_name: &str,
        revision: Option<i64>,
    ) {
        self.contract_revisions
            .entry((market_id, contract_name.to_string()))
            .or_insert_with(|| revision.map(|v| u32::try_from(v).unwrap_or(0)));
    }

    /// The per-tx GHO re-resolve (`fetch_aave_gho_asset_on_conn`'s cache
    /// shell): serves the prefetched row until a write marks it dirty.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a re-query failure.
    pub(crate) fn gho_asset(&mut self, conn: &Connection) -> Result<Option<AaveGhoAsset>, DbError> {
        if !self.gho_queried {
            self.gho = DegenbotDb::fetch_aave_gho_asset_on_conn(conn, self.chain_id)?;
            self.gho_queried = true;
        }
        Ok(self.gho.clone())
    }

    /// Mark the cached GHO row dirty (every write touching
    /// `aave_gho_tokens`: the `ReserveInitialized` GHO link, the discount
    /// applies, the deprecation clear). The next read re-queries `conn` —
    /// read-your-own-writes across log transactions.
    pub(crate) fn mark_gho_dirty(&mut self) {
        self.gho_queried = false;
    }

    /// The substrate lookup kinds are the two `token_type` strings plus the
    /// underlying-JOIN kind; anything else is the substrate's loud
    /// `DbError::Decode` (the silent-arm rule — no catch-all default).
    fn asset_kind(kind: &str) -> Result<&'static str, DbError> {
        match kind {
            ASSET_KIND_UNDERLYING => Ok(ASSET_KIND_UNDERLYING),
            ASSET_KIND_A_TOKEN => Ok(ASSET_KIND_A_TOKEN),
            ASSET_KIND_V_TOKEN => Ok(ASSET_KIND_V_TOKEN),
            other => Err(DbError::Decode(format!(
                "unexpected TokenType '{other}' (expected 'a_token' or 'v_token')"
            ))),
        }
    }

    /// The shared position-state read: map hit → clone; miss → the SQL probe
    /// (the table's `SELECT balance, last_index ... WHERE id`) → cache →
    /// return. The error text mirrors the calling substrate fn's
    /// (`lookup_position_balance_index_on_conn` vs the apply body's).
    fn position_state_inner(
        &mut self,
        conn: &Connection,
        position: ScaledTokenPosition,
        position_id: i64,
        read: PositionRead,
    ) -> Result<PositionState, DbError> {
        let table = PositionTable::from(position);
        if let Some(state) = self.position_state.get(&(table, position_id)) {
            return Ok(*state);
        }
        let sql = format!(
            "SELECT balance, last_index FROM {} WHERE id = ?1",
            table.table()
        );
        let row: Option<(Option<String>, Option<String>)> = conn
            .prepare_cached(&sql)?
            .query_row(rusqlite::params![position_id], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            })
            .optional()?;
        let Some((balance_str, last_index_str)) = row else {
            return Err(match read {
                PositionRead::Lookup => {
                    DbError::MissingRow(format!("{} id={position_id}", table.table()))
                }
                PositionRead::Apply => DbError::MissingRow(format!(
                    "{} id={position_id} (ScaledToken apply target)",
                    table.table()
                )),
                PositionRead::Reset => DbError::MissingRow(format!(
                    "{} id={position_id} (reset target)",
                    table.table()
                )),
            });
        };
        let balance = parse_decimal_u256(&balance_str.ok_or_else(|| {
            DbError::Decode(format!(
                "{} id={position_id}: balance is NULL",
                table.table()
            ))
        })?)?;
        let last_index = match last_index_str {
            Some(s) => Some(parse_decimal_u256(&s)?),
            None => None,
        };
        let state = (balance, last_index);
        self.position_state.insert((table, position_id), state);
        Ok(state)
    }
}

/// Which error text a position-state read carries (the substrate readers
/// the cache shells for).
#[derive(Copy, Clone)]
enum PositionRead {
    /// `lookup_position_balance_index_on_conn` (the parser's GHO seed).
    Lookup,
    /// The scaled-token apply body's read.
    Apply,
    /// The bad-debt reset's read (`reset_debt_position_to_zero_on_conn`).
    Reset,
}

/// Decode one row of the asset projection (the substrate lookups' exact
/// column order + NULL-revision rule).
fn decode_asset_row(row: &rusqlite::Row<'_>) -> Result<AssetRow, rusqlite::Error> {
    Ok(AssetRow {
        id: row.get(0)?,
        a_token_revision: row
            .get::<_, Option<i64>>(1)?
            .map_or(0, |v| u32::try_from(v).unwrap_or(0)),
        v_token_revision: row
            .get::<_, Option<i64>>(2)?
            .map_or(0, |v| u32::try_from(v).unwrap_or(0)),
        underlying_token_address: row.get::<_, String>(3)?,
        a_token_address: row.get::<_, String>(4)?,
        v_token_address: row.get::<_, String>(5)?,
    })
}

/// The chunk's candidate substrate-address strings: every `topics[1]`/
/// `topics[2]` word's low-20-bytes address PLUS every log's emitter
/// address across the chunk's logs, checksummed (the substrate's
/// `addr_to_hex` — the BINARY-collation guard), deduped + sorted. The
/// prefetch's IN-list supersets (the parse-time addresses — user
/// get-or-creates, asset lookups by topic and by emitter — are all drawn
/// from these sets; the cache's lazy probes cover any stragglers).
pub(crate) fn candidate_addresses(logs: &[&alloy::rpc::types::Log]) -> (Vec<String>, Vec<String>) {
    let mut out: HashSet<String> = HashSet::new();
    for log in logs {
        let topics = log.topics();
        if let Some(t1) = topics.get(1) {
            out.insert(addr_to_hex(alloy::primitives::Address::from_slice(
                &t1.as_slice()[12..],
            )));
        }
        if let Some(t2) = topics.get(2) {
            out.insert(addr_to_hex(alloy::primitives::Address::from_slice(
                &t2.as_slice()[12..],
            )));
        }
    }
    let mut emitters: HashSet<String> = HashSet::new();
    for log in logs {
        emitters.insert(addr_to_hex(log.address()));
    }
    let mut users: Vec<String> = out.into_iter().collect();
    let mut assets: Vec<String> = emitters.into_iter().collect();
    for addr in &users {
        assets.push(addr.clone());
    }
    // Deterministic IN-list order (the statement ledger is golden-captured).
    users.sort();
    assets.sort();
    (users, assets)
}

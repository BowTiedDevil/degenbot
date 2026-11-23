//! The chunk-level relational state: the seed loader, the resolution read
//! position, and the write overlay persistence advances.
//!
//! [`ChunkSubstrate`] carries three capability levels:
//!
//! - **Seed loading** ([`ChunkSubstrate::load_market_families`],
//!   [`ChunkSubstrate::intern_reserves`],
//!   [`ChunkSubstrate::load_identity_families`],
//!   [`ChunkSubstrate::intern_identities`]): synchronous, on the caller's
//!   connection, BEFORE the chunk's write transaction opens. Every fact
//!   resolution reads is loaded here, and every identity resolution may name
//!   is interned here, so the rows are inert until a delta references them
//!   (ADR-105 D4).
//! - **Resolution reads** ([`ChunkSubstrate::lookup_asset_row`],
//!   [`ChunkSubstrate::user_id_or_create`],
//!   [`ChunkSubstrate::position_id_or_create`],
//!   [`ChunkSubstrate::gho_asset`], ...): no `Connection`, no SQL. A fact the
//!   seed does not hold is an [`IncompleteChunkSeed`] refusal naming the
//!   market and the entity — never a lazy query.
//! - **Persistence** (`*_for_apply`, `refresh_asset_row`, `require_asset`,
//!   `flush_reserve_data_updates`, `apply_reserve_used_as_collateral`): the
//!   apply-side reads and writes, on the chunk's write transaction.
//!
//! # The read-your-own-writes contract (chunk atomicity)
//!
//! Resolution advances this state as it reduces each event — the asset
//! re-binds (`ReserveInitialized`), the revision bumps (`Upgraded`), the
//! contract roles, the GHO row, the users' discount columns, the position
//! balances — so transaction N+1's reads see exactly what transaction N
//! established, with no SQL between them. Persistence then applies the deltas
//! in the chunk's one transaction and records the same writes back here.
//!
//! # The one deferred write class
//!
//! `ReserveDataUpdated` writes buffer into
//! [`ChunkSubstrate::buffer_reserve_data_update`] and flush as ONE sorted
//! multi-row `UPDATE ... FROM (VALUES ...)` at end-of-chunk (no mid-chunk
//! reader of those columns; the golden ledger replays the statement). The
//! buffer is dropped with the struct on rollback.

use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};
use degenbot_db::{AaveGhoAsset, AssetRow, DbError, DegenbotDb, ScaledTokenPosition};
use rusqlite::Connection;
use rusqlite::OptionalExtension;

use super::seed::{ChunkReserve, IdentityDemands, IncompleteChunkSeed, SeedEntity};
use crate::operations::TokenType;
use crate::updater::operations_parser::addr_to_hex;

/// The per-statement bind-parameter ceiling (the `liquidity_updater.rs`
/// constant; the bundled SQLite's `SQLITE_MAX_VARIABLE_NUMBER`).
const SQLITE_MAX_VARIABLES: usize = 32_766;

/// Which position table a cache key refers to (a `Hash`-capable mirror of
/// [`ScaledTokenPosition`], which the substrate fns take).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

    /// The seed refusal's name for this table.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Collateral => "collateral",
            Self::Debt => "debt",
        }
    }
}

/// A cached position's balance state: `(balance, last_index)` — the same
/// shape [`degenbot_db`'s balance reads] return.
pub(crate) type PositionState = (U256, Option<U256>);

/// The state a freshly interned position row carries.
const FRESH_POSITION_STATE: PositionState = (U256::ZERO, None);

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

/// Which side of a stkAAVE `Transfer` a projected balance mutation belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StkAaveLeg {
    From,
    To,
}

/// The read-side projection of one `aave_v3_users` row: the identity plus the
/// two discount columns a later resolution reads.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UserProjection {
    pub(crate) id: i64,
    pub(crate) gho_discount: i64,
    pub(crate) stk_aave_balance: Option<U256>,
}

/// The chunk's complete relational state.
#[derive(Debug)]
pub struct ChunkSubstrate {
    market_id: i64,
    chain_id: i64,
    /// `aave_v3_assets` rows by row id — the asset overlay's source of truth
    /// (the address indexes point here, so a revision bump in place is
    /// visible to every lookup).
    assets_by_id: HashMap<i64, AssetRow>,
    /// `(lookup kind, address)` → asset id; `None` = proven-absent (every
    /// address the chunk's logs can name is indexed, so absence is a fact the
    /// seed states rather than a query it defers).
    assets_by_address: HashMap<(&'static str, String), Option<i64>>,
    /// `(market_id, address)` → the user's CHUNK-START row (`None` = proven
    /// absent). Interning a demanded user commits its row outside the chunk
    /// transaction but leaves this entry `None` until the resolution that asks
    /// for the id, so a transaction-start discount read still sees the state the
    /// chunk began under.
    users: HashMap<(i64, String), Option<UserProjection>>,
    /// `(market_id, address)` → the id of a user row interned for a demand.
    interned_users: HashMap<(i64, String), i64>,
    /// `user id` → `(market_id, address)` for every user this chunk can name.
    users_by_id: HashMap<i64, (i64, String)>,
    /// `(table, user_id, asset_id)` → position row id.
    positions_by_key: HashMap<(PositionTable, i64, i64), i64>,
    /// `(table, position id)` → `(balance, last_index)`.
    position_state: HashMap<(PositionTable, i64), PositionState>,
    /// `(table, position id)` → the user the row belongs to.
    position_users: HashMap<(PositionTable, i64), i64>,
    /// `(user_id, asset_id)` → `aave_v3_user_collateral_configs.id`.
    collateral_configs: HashMap<(i64, i64), i64>,
    /// `(market_id, contract name)` → revision (the decoded
    /// `lookup_pool_revision` shape: `None` for a missing row OR a NULL
    /// column — the substrate read flattens both).
    contract_revisions: HashMap<(i64, String), Option<u32>>,
    /// `(market_id, contract name)` → the role's current endpoint address.
    contract_addresses: HashMap<(i64, String), String>,
    /// The chain's GHO row as the chunk starts (advanced in place by the
    /// overlay's projections).
    gho: Option<AaveGhoAsset>,
    /// `(chain_id, address)` → ERC-20 row id (the interned identities).
    erc20_ids: HashMap<(i64, String), i64>,
    /// `underlying address` → the interned reserve identity.
    reserves: HashMap<String, ChunkReserve>,
    /// `underlying erc20 id` → the reserve's `aave_v3_assets.id`.
    reserves_by_underlying_id: HashMap<i64, i64>,
    /// `aToken or vToken address` → the reserve's scaled leg (table + asset id).
    reserves_by_token: HashMap<String, (PositionTable, i64)>,
    /// The CHUNK-START asset revisions, `(a_token, v_token)` by asset id: the
    /// sealed generations the chunk begins under, captured before the seed's
    /// interning re-points any row.
    seed_asset_revisions: HashMap<i64, (u32, u32)>,
    /// The CHUNK-START contract revisions by role name, captured before the
    /// seed's interning.
    seed_contract_revisions: HashMap<String, Option<u32>>,
    /// Whether this substrate is an in-crate fixture seed whose market was
    /// loaded whole: a miss then states the market's own absence instead of a
    /// seed hole. Production seeds ([`super::seed::ChunkSeed::load`]) never set
    /// it, so a real resolution miss still refuses.
    #[cfg(test)]
    pub(crate) fixture_complete: bool,
    /// The deferred `ReserveDataUpdated` writes: asset id → the LAST event's
    /// `(liquidity_rate, variable_borrow_rate, liquidity_index,
    /// variable_borrow_index, block)`. Flushed as one sorted multi-row
    /// `UPDATE ... FROM (VALUES ...)` at end-of-chunk.
    reserve_data_updates: HashMap<i64, (U256, U256, U256, U256, i64)>,
}
impl ChunkSubstrate {
    /// An empty, lazily-probing cache. The batched-apply entrypoint starts here;
    /// the chunk loop always starts from a loaded [`super::seed::ChunkSeed`].
    pub(crate) fn lazy(chain_id: i64) -> Self {
        Self {
            market_id: 0,
            chain_id,
            assets_by_id: HashMap::new(),
            assets_by_address: HashMap::new(),
            users: HashMap::new(),
            interned_users: HashMap::new(),
            users_by_id: HashMap::new(),
            positions_by_key: HashMap::new(),
            position_state: HashMap::new(),
            position_users: HashMap::new(),
            collateral_configs: HashMap::new(),
            contract_revisions: HashMap::new(),
            contract_addresses: HashMap::new(),
            gho: None,
            erc20_ids: HashMap::new(),
            reserves: HashMap::new(),
            reserves_by_underlying_id: HashMap::new(),
            reserves_by_token: HashMap::new(),
            seed_asset_revisions: HashMap::new(),
            seed_contract_revisions: HashMap::new(),
            #[cfg(test)]
            fixture_complete: false,
            reserve_data_updates: HashMap::new(),
        }
    }

    // ── seed loading (synchronous, before the chunk transaction opens) ──

    /// Load the market-scoped relational families: the market's asset rows (the
    /// full six-column projection the lookup fns return, indexed by id + all
    /// three address columns), the market's contract roles, and the chain's GHO
    /// row.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on any prefetch query failure.
    pub(crate) fn load_market_families(
        &mut self,
        conn: &Connection,
        market_id: i64,
        chain_id: i64,
    ) -> Result<(), DbError> {
        self.market_id = market_id;
        self.chain_id = chain_id;
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
            self.seed_asset_revisions
                .insert(asset.id, (asset.a_token_revision, asset.v_token_revision));
            self.index_asset(asset);
        }
        let mut stmt = conn.prepare(
            "SELECT name, revision, address FROM aave_v3_contracts WHERE market_id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![market_id])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            let address: String = row.get(2)?;
            self.contract_addresses
                .insert((market_id, name.clone()), address);
            let revision: Option<i64> = row.get(1)?;
            let revision = revision.map(|v| u32::try_from(v).unwrap_or(0));
            self.seed_contract_revisions.insert(name.clone(), revision);
            self.contract_revisions.insert((market_id, name), revision);
        }
        self.gho = DegenbotDb::fetch_aave_gho_asset_on_conn(conn, chain_id)?;
        Ok(())
    }

    /// Record the candidate addresses the chunk's logs can name as
    /// proven-absent asset lookups (the market's asset rows are the only JOIN
    /// rows a lookup with this market id can return, so an address outside them
    /// is a fact, not a deferred query).
    pub(crate) fn cache_asset_absences(&mut self, addresses: &[String]) {
        for address in addresses {
            for kind in [
                ASSET_KIND_UNDERLYING,
                ASSET_KIND_A_TOKEN,
                ASSET_KIND_V_TOKEN,
            ] {
                self.assets_by_address
                    .entry((kind, address.clone()))
                    .or_insert(None);
            }
        }
    }

    /// Intern the ERC-20 rows and the asset row of every reserve the chunk
    /// initializes, so resolution names ids the database already holds
    /// (ADR-105 D4: interning is synchronous and outside the chunk transaction).
    ///
    /// The interned reserve is deliberately NOT indexed as an address
    /// resolution can see: a configuration event that precedes the reserve's own
    /// `ReserveInitialized` in the same transaction still observes the reserve
    /// as absent, exactly as it did before the row existed.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on any interning write failure.
    pub(crate) fn intern_reserves(
        &mut self,
        conn: &Connection,
        chain_id: i64,
        reserves: &[super::seed::ReserveFacts],
    ) -> Result<(), DbError> {
        let mut linked_gho = false;
        for facts in reserves {
            let mut ids = [0_i64; 3];
            let tokens = [facts.underlying, facts.a_token, facts.v_token];
            for (index, (token, metadata)) in tokens.iter().zip(facts.metadata.iter()).enumerate() {
                let id = DegenbotDb::get_or_create_erc20_token_on_conn(
                    conn,
                    chain_id,
                    &addr_to_hex(*token),
                    metadata.0.as_deref(),
                    metadata.1.as_deref(),
                    metadata.2,
                )?;
                self.erc20_ids.insert((chain_id, addr_to_hex(*token)), id);
                ids[index] = id;
            }
            let [underlying_id, a_token_id, v_token_id] = ids;
            let underlying_str = addr_to_hex(facts.underlying);
            let gho_link_token_id = self
                .gho
                .as_ref()
                .filter(|gho| gho.gho_token_address.eq_ignore_ascii_case(&underlying_str))
                .map(|gho| gho.id);
            let asset_id = DegenbotDb::apply_reserve_initialized_on_conn(
                conn,
                self.market_id,
                underlying_id,
                a_token_id,
                facts.a_token_revision,
                v_token_id,
                facts.v_token_revision,
                None,
                gho_link_token_id,
            )?;
            if gho_link_token_id.is_some() {
                linked_gho = true;
            }
            self.reserves_by_underlying_id
                .insert(underlying_id, asset_id);
            self.reserves_by_token.insert(
                addr_to_hex(facts.a_token),
                (PositionTable::Collateral, asset_id),
            );
            self.reserves_by_token
                .insert(addr_to_hex(facts.v_token), (PositionTable::Debt, asset_id));
            self.reserves.insert(
                underlying_str,
                super::seed::ChunkReserve {
                    facts: facts.clone(),
                    underlying_id,
                    a_token_id,
                    v_token_id,
                    asset_id,
                },
            );
        }
        if linked_gho {
            self.gho = DegenbotDb::fetch_aave_gho_asset_on_conn(conn, self.chain_id)?;
        }
        Ok(())
    }

    /// Load the chunk's identity demands: the demanded users' chunk-start rows,
    /// the positions both families hold for them, and their collateral configs.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on any prefetch query failure.
    #[expect(clippy::too_many_lines)]
    pub(crate) fn load_identity_families(
        &mut self,
        conn: &Connection,
        market_id: i64,
        demands: &IdentityDemands,
    ) -> Result<(), DbError> {
        let candidates: Vec<&String> = demands.users.iter().collect();
        if candidates.is_empty() {
            return Ok(());
        }
        let mut chunk_start = 0;
        while chunk_start < candidates.len() {
            let chunk_end = usize::min(chunk_start + SQLITE_MAX_VARIABLES - 1, candidates.len());
            let slice = &candidates[chunk_start..chunk_end];
            let placeholders = vec!["?"; slice.len()].join(", ");
            let sql = format!(
                "SELECT id, address, gho_discount, stk_aave_balance FROM aave_v3_users \
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
                let gho_discount: i64 = row.get(2)?;
                let stk_aave_balance: Option<String> = row.get(3)?;
                let stk_aave_balance = stk_aave_balance
                    .as_deref()
                    .map(parse_decimal_u256)
                    .transpose()?;
                self.users.insert(
                    (market_id, address.clone()),
                    Some(UserProjection {
                        id,
                        gho_discount,
                        stk_aave_balance,
                    }),
                );
                self.users_by_id.insert(id, (market_id, address));
            }
            chunk_start = chunk_end;
        }
        for addr in &candidates {
            self.users
                .entry((market_id, (*addr).clone()))
                .or_insert(None);
        }
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
                    self.positions_by_key.insert((table, user_id, asset_id), id);
                    self.position_state.insert((table, id), state);
                    self.position_users.insert((table, id), user_id);
                }
                chunk_start = chunk_end;
            }
        }
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
                self.collateral_configs.insert((user_id, asset_id), id);
            }
            chunk_start = chunk_end;
        }
        Ok(())
    }

    /// The asset row ids this seed holds, in id order — the fixture seeds'
    /// position denominator.
    #[cfg(test)]
    pub(crate) fn asset_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.assets_by_id.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Load every user, position and collateral config this market holds, for
    /// the in-crate fixtures' seeds: a unit test's market is small, and loading
    /// it whole means a fixture that writes rows directly still gets a complete
    /// read position.
    #[cfg(test)]
    pub(crate) fn load_all_identity_families(
        &mut self,
        conn: &Connection,
        market_id: i64,
    ) -> Result<(), DbError> {
        let mut stmt = conn.prepare(
            "SELECT id, address, gho_discount, stk_aave_balance FROM aave_v3_users WHERE market_id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![market_id])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let address: String = row.get(1)?;
            let gho_discount: i64 = row.get(2)?;
            let stk_aave_balance: Option<String> = row.get(3)?;
            let stk_aave_balance = stk_aave_balance
                .as_deref()
                .map(parse_decimal_u256)
                .transpose()?;
            self.users.insert(
                (market_id, address.clone()),
                Some(UserProjection {
                    id,
                    gho_discount,
                    stk_aave_balance,
                }),
            );
            self.users_by_id.insert(id, (market_id, address));
        }
        for table in [PositionTable::Collateral, PositionTable::Debt] {
            let sql = format!(
                "SELECT p.id, p.user_id, p.asset_id, p.balance, p.last_index \
                 FROM {} p JOIN aave_v3_users u ON u.id = p.user_id WHERE u.market_id = ?1",
                table.table()
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query(rusqlite::params![market_id])?;
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
                self.positions_by_key.insert((table, user_id, asset_id), id);
                self.position_state.insert((table, id), state);
                self.position_users.insert((table, id), user_id);
            }
        }
        let mut stmt = conn.prepare(
            "SELECT cc.user_id, cc.asset_id, cc.id FROM aave_v3_user_collateral_configs cc \
             JOIN aave_v3_users u ON u.id = cc.user_id WHERE u.market_id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![market_id])?;
        while let Some(row) = rows.next()? {
            let user_id: i64 = row.get(0)?;
            let asset_id: i64 = row.get(1)?;
            let id: i64 = row.get(2)?;
            self.collateral_configs.insert((user_id, asset_id), id);
        }
        Ok(())
    }

    /// Intern every demanded identity the chunk-start state does not hold: the
    /// user rows (with the zero defaults the resolution's own get-or-create
    /// would have written) and the position rows.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on any interning write failure.
    pub(crate) fn intern_identities(
        &mut self,
        conn: &Connection,
        market_id: i64,
        demands: &IdentityDemands,
    ) -> Result<(), DbError> {
        for address in &demands.users {
            let key = (market_id, address.clone());
            let present = match self.users.get(&key) {
                Some(Some(user)) => Some(user.id),
                _ => None,
            };
            if let Some(id) = present {
                self.users_by_id.insert(id, key);
                continue;
            }
            conn.execute(
                "INSERT INTO aave_v3_users (market_id, address, e_mode, gho_discount, stk_aave_balance, isolation_mode_collateral_asset_id, isolation_mode_debt) VALUES (?1, ?2, 0, 0, NULL, NULL, '0')",
                rusqlite::params![market_id, address],
            )?;
            let id = conn.last_insert_rowid();
            self.interned_users.insert(key.clone(), id);
            self.users_by_id.insert(id, key);
        }
        for (table, address, asset_id) in &demands.positions {
            let key = (market_id, address.clone());
            let user_id = match self.users.get(&key) {
                Some(Some(user)) => user.id,
                _ => match self.interned_users.get(&key) {
                    Some(id) => *id,
                    None => continue,
                },
            };
            if self
                .positions_by_key
                .contains_key(&(*table, user_id, *asset_id))
            {
                continue;
            }
            let id = match table {
                PositionTable::Collateral => {
                    DegenbotDb::get_or_create_collateral_position_on_conn(conn, user_id, *asset_id)?
                }
                PositionTable::Debt => {
                    DegenbotDb::get_or_create_debt_position_on_conn(conn, user_id, *asset_id)?
                }
            };
            self.positions_by_key
                .insert((*table, user_id, *asset_id), id);
            self.position_state
                .insert((*table, id), FRESH_POSITION_STATE);
            self.position_users.insert((*table, id), user_id);
        }
        Ok(())
    }

    // ── resolution reads (no connection, no SQL) ────────────────────────

    /// The asset row an address names under one lookup kind.
    ///
    /// # Errors
    ///
    /// [`IncompleteChunkSeed`] when the seed holds no answer for the address
    /// (a read the seed did not anticipate, never a lazy query).
    pub(crate) fn lookup_asset_row(
        &self,
        kind: &str,
        token_address: &str,
    ) -> Result<Option<AssetRow>, IncompleteChunkSeed> {
        let Some(kind) = Self::asset_kind(kind) else {
            return Err(IncompleteChunkSeed::new(
                self.market_id,
                SeedEntity::Asset {
                    kind: "unknown",
                    address: token_address.to_string(),
                },
            ));
        };
        let Some(maybe_id) = self
            .assets_by_address
            .get(&(kind, token_address.to_string()))
        else {
            #[cfg(test)]
            if self.fixture_complete {
                return Ok(None);
            }
            return Err(IncompleteChunkSeed::new(
                self.market_id,
                SeedEntity::Asset {
                    kind,
                    address: token_address.to_string(),
                },
            ));
        };
        Ok(match *maybe_id {
            Some(id) => self.assets_by_id.get(&id).cloned(),
            None => None,
        })
    }

    /// The id-only classification lookup (the same index, a cheaper projection).
    ///
    /// # Errors
    ///
    /// Same refusal as [`Self::lookup_asset_row`].
    pub(crate) fn lookup_asset_id(
        &self,
        token_type: &str,
        token_address: &str,
    ) -> Result<Option<i64>, IncompleteChunkSeed> {
        Ok(self
            .lookup_asset_row(token_type, token_address)?
            .map(|row| row.id))
    }

    /// The scaled leg a token address names: the position table its emitter
    /// family owns plus the reserve's asset id. A token that is not this
    /// market's scaled leg is `None` (the caller is classifying, not asking
    /// for an id).
    pub(crate) fn scaled_leg(&self, token_address: &str) -> Option<(PositionTable, i64)> {
        if let Some(Some(id)) = self
            .assets_by_address
            .get(&(ASSET_KIND_A_TOKEN, token_address.to_string()))
        {
            return Some((PositionTable::Collateral, *id));
        }
        if let Some(Some(id)) = self
            .assets_by_address
            .get(&(ASSET_KIND_V_TOKEN, token_address.to_string()))
        {
            return Some((PositionTable::Debt, *id));
        }
        self.reserves_by_token.get(token_address).copied()
    }

    /// The user id a demanded address names.
    ///
    /// A user the seed interned but the chunk has not yet named resolves here
    /// into the created row (the zero-default projection the resolution's own
    /// get-or-create established), so a later transaction's reads see the row
    /// exactly as the SQL write would have left it.
    ///
    /// # Errors
    ///
    /// [`IncompleteChunkSeed`] for an address the seed never anticipated.
    pub(crate) fn user_id_or_create(
        &mut self,
        address: &str,
        gho_discount: i64,
    ) -> Result<i64, IncompleteChunkSeed> {
        let key = (self.market_id, address.to_string());
        match self.users.get(&key) {
            Some(Some(user)) => Ok(user.id),
            Some(None) => {
                let Some(id) = self.interned_users.get(&key).copied() else {
                    return Err(IncompleteChunkSeed::user(self.market_id, address));
                };
                self.users.insert(
                    key,
                    Some(UserProjection {
                        id,
                        gho_discount,
                        stk_aave_balance: None,
                    }),
                );
                Ok(id)
            }
            None => Err(IncompleteChunkSeed::user(self.market_id, address)),
        }
    }

    /// The projected user row for a demanded address.
    ///
    /// # Errors
    ///
    /// [`IncompleteChunkSeed`] for an address the seed never anticipated.
    pub(crate) fn user_projection(
        &self,
        address: &str,
    ) -> Result<Option<UserProjection>, IncompleteChunkSeed> {
        match self.users.get(&(self.market_id, address.to_string())) {
            Some(user) => Ok(*user),
            #[cfg(test)]
            None if self.fixture_complete => Ok(None),
            None => Err(IncompleteChunkSeed::user(self.market_id, address)),
        }
    }

    /// The projected `gho_discount` for a user address.
    ///
    /// # Errors
    ///
    /// Same refusal as [`Self::user_projection`].
    pub(crate) fn user_gho_discount(
        &self,
        address: &str,
    ) -> Result<Option<i64>, IncompleteChunkSeed> {
        self.user_projection(address)
            .map(|user| user.map(|user| user.gho_discount))
    }

    /// The projected `stk_aave_balance` for a user address: `Some(None)` for a
    /// user whose column was never seeded, `None` for an address this market
    /// has no row for.
    ///
    /// # Errors
    ///
    /// Same refusal as [`Self::user_projection`].
    #[expect(clippy::option_option)]
    pub(crate) fn user_stk_aave_balance(
        &self,
        address: &str,
    ) -> Result<Option<Option<U256>>, IncompleteChunkSeed> {
        self.user_projection(address)
            .map(|user| user.map(|user| user.stk_aave_balance))
    }

    /// The checksummed address a user id names.
    ///
    /// # Errors
    ///
    /// [`IncompleteChunkSeed`] for an id the seed never interned.
    pub(crate) fn user_address(&self, user_id: i64) -> Result<String, IncompleteChunkSeed> {
        match self.users_by_id.get(&user_id) {
            Some((_, address)) => Ok(address.clone()),
            None => Err(IncompleteChunkSeed::new(
                self.market_id,
                SeedEntity::UserAddress { user_id },
            )),
        }
    }

    /// Project the discount-token replacement's balance invalidation: every
    /// cached balance was denominated in the replaced token's ledger, so the
    /// next resolution reseeds it from the new contract.
    pub(crate) fn invalidate_stk_aave_balances(&mut self) {
        for user in self.users.values_mut().flatten() {
            user.stk_aave_balance = None;
        }
    }

    /// The mutable projection of one user row addressed by id (the
    /// configuration effects carry settled ids rather than addresses). A row the
    /// seed interned but the chunk has not named yet projects at its created
    /// defaults.
    fn user_projection_mut(&mut self, user_id: i64) -> Option<&mut UserProjection> {
        let key = self.users_by_id.get(&user_id)?.clone();
        let entry = self.users.get_mut(&key)?;
        if entry.is_none() {
            *entry = Some(UserProjection {
                id: user_id,
                gho_discount: 0,
                stk_aave_balance: None,
            });
        }
        entry.as_mut()
    }

    /// Project one `stk_aave_balance` write addressed by row id.
    pub(crate) fn record_user_stk_aave_balance_by_id(
        &mut self,
        user_id: i64,
        balance: Option<U256>,
    ) {
        if let Some(user) = self.user_projection_mut(user_id) {
            user.stk_aave_balance = balance;
        }
    }

    /// Project one user write addressed by row id.
    pub(crate) fn record_user_gho_discount_by_id(&mut self, user_id: i64, gho_discount: i64) {
        if let Some(user) = self.user_projection_mut(user_id) {
            user.gho_discount = gho_discount;
        }
    }

    /// Project one `stk_aave_balance` write addressed by row id, with the
    /// apply's own arithmetic: the from-leg subtracts (underflow is a refusal),
    /// the to-leg adds, and a never-seeded column reads as zero.
    ///
    /// # Errors
    ///
    /// [`DbError::Decode`] when a leg leaves the `U256` domain.
    pub(crate) fn project_stk_aave_leg(
        &mut self,
        user_id: i64,
        amount: U256,
        side: StkAaveLeg,
    ) -> Result<(), DbError> {
        let Some(user) = self.user_projection_mut(user_id) else {
            return Ok(());
        };
        let balance = user.stk_aave_balance.unwrap_or(U256::ZERO);
        user.stk_aave_balance = Some(match side {
            StkAaveLeg::From => balance.checked_sub(amount).ok_or_else(|| {
                DbError::Decode(format!(
                    "stk_aave_balance underflow on Transfer from-leg: {balance} - {amount} (user_id={user_id})"
                ))
            })?,
            StkAaveLeg::To => balance.checked_add(amount).ok_or_else(|| {
                DbError::Decode(format!(
                    "stk_aave_balance overflow on Transfer to-leg: {balance} + {amount} (user_id={user_id})"
                ))
            })?,
        });
        Ok(())
    }

    /// The `get_or_create_*_position_on_conn` id for one natural key — the id
    /// the seed interned, never a lazily created row.
    ///
    /// # Errors
    ///
    /// [`IncompleteChunkSeed`] for a position the seed never anticipated.
    pub(crate) fn position_id_or_create(
        &mut self,
        position: ScaledTokenPosition,
        user_id: i64,
        asset_id: i64,
    ) -> Result<i64, IncompleteChunkSeed> {
        let table = PositionTable::from(position);
        match self.positions_by_key.get(&(table, user_id, asset_id)) {
            Some(id) => Ok(*id),
            None => Err(IncompleteChunkSeed::new(
                self.market_id,
                SeedEntity::Position {
                    position: table.name(),
                    user_id,
                    asset_id,
                },
            )),
        }
    }

    /// The cache's KNOWN state for a position — a prefetched row or a write
    /// this chunk recorded — without probing the database. The projection view
    /// seeds from this read.
    pub(crate) fn known_position_state(
        &self,
        position: ScaledTokenPosition,
        position_id: i64,
    ) -> Option<PositionState> {
        self.position_state
            .get(&(PositionTable::from(position), position_id))
            .copied()
    }

    /// The user a position row belongs to.
    ///
    /// # Errors
    ///
    /// [`IncompleteChunkSeed`] naming the position the resolution asked for and
    /// the seed did not hold.
    pub(crate) fn position_user(
        &self,
        position: ScaledTokenPosition,
        position_id: i64,
    ) -> Result<i64, IncompleteChunkSeed> {
        let table = PositionTable::from(position);
        match self.position_users.get(&(table, position_id)) {
            Some(user_id) => Ok(*user_id),
            None => Err(IncompleteChunkSeed::new(
                self.market_id,
                SeedEntity::PositionState {
                    position: table.name(),
                    position_id,
                },
            )),
        }
    }

    /// A position's projected state, refusing when the seed never resolved it.
    ///
    /// # Errors
    ///
    /// [`IncompleteChunkSeed`] naming the position the resolution asked for and
    /// the seed did not hold.
    pub(crate) fn position_state_or_refuse(
        &self,
        position: ScaledTokenPosition,
        position_id: i64,
    ) -> Result<PositionState, IncompleteChunkSeed> {
        self.known_position_state(position, position_id)
            .ok_or_else(|| {
                IncompleteChunkSeed::new(
                    self.market_id,
                    SeedEntity::PositionState {
                        position: PositionTable::from(position).name(),
                        position_id,
                    },
                )
            })
    }

    /// The scaled-token apply's read-modify-write seed read. Missing row →
    /// the apply body's `MissingRow` (same message, tagged with the apply
    /// target).
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
    /// read-your-own-writes backbone: the next read is a map hit carrying
    /// exactly what the SQL probe would return).
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

    // ── overlay projections (resolution-time mutations, no SQL) ─────────

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
    /// addresses must stop resolving after the row re-points its token ids,
    /// exactly as the SQL JOIN would).
    fn unindex_asset(&mut self, asset_id: i64) {
        self.assets_by_address.retain(|_, v| *v != Some(asset_id));
    }

    /// Project a `ReserveInitialized` effect's asset identity onto the
    /// resolution read position: the row the seed interned becomes visible to
    /// the address lookups that follow it, and an earlier generation's addresses
    /// stop resolving.
    ///
    /// # Errors
    ///
    /// [`IncompleteChunkSeed`] when the seed holds no reserve for the
    /// underlying token the effect names.
    pub(crate) fn project_reserve_identity(
        &mut self,
        underlying_asset_id: i64,
    ) -> Result<(), IncompleteChunkSeed> {
        let Some(asset_id) = self
            .reserves_by_underlying_id
            .get(&underlying_asset_id)
            .copied()
        else {
            return Err(IncompleteChunkSeed::new(
                self.market_id,
                SeedEntity::Reserve {
                    underlying: format!("erc20 id={underlying_asset_id}"),
                },
            ));
        };
        let Some(reserve) = self
            .reserves
            .values()
            .find(|r| r.asset_id == asset_id)
            .cloned()
        else {
            return Err(IncompleteChunkSeed::new(
                self.market_id,
                SeedEntity::Reserve {
                    underlying: format!("asset id={asset_id}"),
                },
            ));
        };
        let row = AssetRow {
            id: asset_id,
            a_token_revision: u32::try_from(reserve.facts.a_token_revision).unwrap_or(0),
            v_token_revision: u32::try_from(reserve.facts.v_token_revision).unwrap_or(0),
            underlying_token_address: addr_to_hex(reserve.facts.underlying),
            a_token_address: addr_to_hex(reserve.facts.a_token),
            v_token_address: addr_to_hex(reserve.facts.v_token),
        };
        self.unindex_asset(asset_id);
        self.index_asset(row);
        self.reserves_by_token.insert(
            addr_to_hex(reserve.facts.a_token),
            (PositionTable::Collateral, asset_id),
        );
        self.reserves_by_token.insert(
            addr_to_hex(reserve.facts.v_token),
            (PositionTable::Debt, asset_id),
        );
        Ok(())
    }

    /// Project the GHO vToken link a `ReserveInitialized` effect establishes:
    /// the row's `v_token_id` points at the reserve's vToken, so every later
    /// resolution of the chunk reads the linked token address.
    ///
    /// # Errors
    ///
    /// [`IncompleteChunkSeed`] when the seed holds no reserve for the underlying
    /// token the effect names.
    pub(crate) fn project_reserve_gho_link(
        &mut self,
        underlying_asset_id: i64,
    ) -> Result<(), IncompleteChunkSeed> {
        let Some(asset_id) = self
            .reserves_by_underlying_id
            .get(&underlying_asset_id)
            .copied()
        else {
            return Err(IncompleteChunkSeed::new(
                self.market_id,
                SeedEntity::Reserve {
                    underlying: format!("erc20 id={underlying_asset_id}"),
                },
            ));
        };
        let Some(reserve) = self
            .reserves
            .values()
            .find(|r| r.asset_id == asset_id)
            .cloned()
        else {
            return Err(IncompleteChunkSeed::new(
                self.market_id,
                SeedEntity::Reserve {
                    underlying: format!("asset id={asset_id}"),
                },
            ));
        };
        let v_token_address = addr_to_hex(reserve.facts.v_token);
        self.project_gho(|gho| {
            gho.v_token_id = Some(reserve.v_token_id);
            gho.v_token_address = Some(v_token_address);
        });
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

    /// The generation a revision-changing effect is about to replace: the
    /// CURRENT revision the substrate reads for that leg or role, read fresh at
    /// each projection (a second upgrade in the same chunk seals the revision
    /// the first one installed, not the chunk-start one). An uncached row or an
    /// unchanged revision seals nothing.
    pub(crate) fn superseded_generation(
        &self,
        effect: &crate::projection::ConfigurationEffect,
    ) -> Option<crate::projection::GenerationRef> {
        use crate::projection::{
            ConfigurationEffect, ContractRole, GenerationEntity, GenerationRef, ScaledTokenPosition,
        };
        match effect {
            ConfigurationEffect::Upgraded {
                asset_id,
                is_a_token,
                new_revision,
                ..
            } => {
                let row = self.assets_by_id.get(asset_id)?;
                let (position, revision) = if *is_a_token {
                    (ScaledTokenPosition::Collateral, row.a_token_revision)
                } else {
                    (ScaledTokenPosition::Debt, row.v_token_revision)
                };
                (Some(revision) != u32::try_from(*new_revision).ok()).then(|| {
                    GenerationRef::new(
                        GenerationEntity::ScaledToken {
                            asset_id: *asset_id,
                            position,
                        },
                        revision,
                    )
                })
            }
            ConfigurationEffect::ContractRevisionUpdated {
                market_id,
                contract_name,
                new_revision,
            } => {
                let role = ContractRole::from_contract_name(contract_name)?;
                let revision = *self
                    .contract_revisions
                    .get(&(*market_id, contract_name.clone()))?;
                (revision != u32::try_from(*new_revision).ok()).then(|| match revision {
                    Some(revision) => GenerationRef::new(
                        GenerationEntity::Contract {
                            market_id: *market_id,
                            role,
                        },
                        revision,
                    ),
                    None => GenerationRef::seed(GenerationEntity::Contract {
                        market_id: *market_id,
                        role,
                    }),
                })
            }
            _ => None,
        }
    }

    /// The `ReserveDataUpdated` apply's existence probe: the single-row
    /// UPDATE's `updated == 0 → DbError::MissingRow` contract, checked at
    /// `buffer` time so the error surfaces at the failing event (byte-parity
    /// with the single-row UPDATE's own rollback semantics).
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

    /// The `ReserveInitialized` apply's persistence-side refresh: re-read the
    /// fresh row (one projection SELECT — the create/re-point changed the token
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

    // ── contracts + the GHO row ─────────────────────────────────────────

    /// The market's tracked token census, read from the prefetched asset rows
    /// and the chain's GHO row — deliberately no SQL.
    ///
    /// Every aToken row is a collateral series, every vToken row a debt series,
    /// and the GHO discount token its own series; an address outside this census
    /// is not a token this market tracks, so a receipt log it emits is out of
    /// scope rather than a question for the database.
    pub(crate) fn market_token_census(&self) -> HashMap<Address, TokenType> {
        let mut census: HashMap<Address, TokenType> = HashMap::new();
        for asset in self.assets_by_id.values() {
            if let Ok(address) = asset.a_token_address.parse() {
                census.insert(address, TokenType::Atoken);
            }
            if let Ok(address) = asset.v_token_address.parse() {
                census.insert(address, TokenType::Vtoken);
            }
        }
        for reserve in self.reserves.values() {
            census.insert(reserve.facts.a_token, TokenType::Atoken);
            census.insert(reserve.facts.v_token, TokenType::Vtoken);
        }
        if let Some(gho) = &self.gho {
            if let Some(address) = gho.v_token_address.as_ref().and_then(|a| a.parse().ok()) {
                census.insert(address, TokenType::Vtoken);
            }
            if let Some(address) = gho
                .v_gho_discount_token
                .as_ref()
                .and_then(|a| a.parse().ok())
            {
                census.insert(address, TokenType::GhoDiscount);
            }
        }
        census
    }

    /// Whether the cache holds prefetched asset rows: when it does, the market's
    /// token census is answerable in memory and needs no probe.
    pub(crate) fn has_prefetched_assets(&self) -> bool {
        !self.assets_by_id.is_empty()
    }

    /// The reserve identity a `ReserveInitialized` event names.
    ///
    /// # Errors
    ///
    /// [`IncompleteChunkSeed`] when the seed holds no reserve for the
    /// underlying token.
    pub(crate) fn reserve(&self, underlying: &str) -> Result<&ChunkReserve, IncompleteChunkSeed> {
        match self.reserves.get(underlying) {
            Some(reserve) => Ok(reserve),
            None => Err(IncompleteChunkSeed::new(
                self.market_id,
                SeedEntity::Reserve {
                    underlying: underlying.to_string(),
                },
            )),
        }
    }

    /// The `lookup_pool_revision_on_conn` shape (the parser's per-parse read).
    /// Missing row / NULL revision → `None` (the substrate's flatten).
    ///
    /// # Errors
    ///
    /// [`IncompleteChunkSeed`] when the seed never loaded the role.
    pub(crate) fn pool_revision(
        &self,
        contract_name: &str,
    ) -> Result<Option<u32>, IncompleteChunkSeed> {
        match self
            .contract_revisions
            .get(&(self.market_id, contract_name.to_string()))
        {
            Some(revision) => Ok(*revision),
            #[cfg(test)]
            None if self.fixture_complete => Ok(None),
            None => Err(IncompleteChunkSeed::new(
                self.market_id,
                SeedEntity::Contract {
                    name: contract_name.to_string(),
                },
            )),
        }
    }

    /// The role's projected endpoint address (the resolution read position for
    /// the market's price oracle and the other fetch endpoints).
    ///
    /// # Errors
    ///
    /// Same refusal as [`Self::pool_revision`].
    pub(crate) fn contract_address(
        &self,
        name: &str,
    ) -> Result<Option<String>, IncompleteChunkSeed> {
        match self
            .contract_addresses
            .get(&(self.market_id, name.to_string()))
        {
            Some(address) => Ok(Some(address.clone())),
            #[cfg(test)]
            None if self.fixture_complete => Ok(None),
            None => Err(IncompleteChunkSeed::new(
                self.market_id,
                SeedEntity::Contract {
                    name: name.to_string(),
                },
            )),
        }
    }

    /// Project a contract role's endpoint address onto the resolution read
    /// position.
    pub(crate) fn record_contract_address(&mut self, market_id: i64, name: &str, address: &str) {
        self.contract_addresses
            .insert((market_id, name.to_string()), address.to_string());
    }

    /// The `ContractRevisionUpdated` apply's overlay write.
    pub(crate) fn record_contract_revision(
        &mut self,
        market_id: i64,
        contract_name: &str,
        new_revision: i64,
    ) {
        self.contract_revisions.insert(
            (market_id, contract_name.to_string()),
            Some(u32::try_from(new_revision).unwrap_or(0)),
        );
    }

    /// A mid-chunk contract INSERT's overlay write (a name the cache hasn't
    /// seen gets the event's revision; a seen name keeps its cached value — the
    /// substrate INSERT would have hit the UNIQUE constraint first).
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

    /// The chain's GHO row as the resolution read position holds it.
    pub(crate) fn gho_asset(&self) -> Option<AaveGhoAsset> {
        self.gho.clone()
    }

    /// Project one GHO-row mutation onto the resolution read position without
    /// leaving the read side: a same-chunk link, discount-token replacement,
    /// strategy change or deprecation is read by every later resolution of the
    /// same chunk.
    pub(crate) fn project_gho(&mut self, mutate: impl FnOnce(&mut AaveGhoAsset)) {
        if let Some(gho) = self.gho.as_mut() {
            mutate(gho);
        }
    }

    /// Observe the sealed generations a chunk starts from (ADR-105 D6): every
    /// generation the prefetched rows already carry is a `Seed`-activated
    /// generation of its logical entity, so a replacement later in the chunk
    /// activates a NEW generation instead of mutating the one the chunk began
    /// under.
    pub(crate) fn observe_seed_generations(
        &self,
        view: &mut crate::projection::ProjectionView,
        market_id: i64,
        chain_id: i64,
    ) {
        use crate::projection::{
            ContractRole, GenerationEntity, GenerationRef, ScaledTokenPosition,
        };
        for (name, revision) in &self.seed_contract_revisions {
            let Some(role) = ContractRole::from_contract_name(name) else {
                continue;
            };
            let entity = GenerationEntity::Contract { market_id, role };
            view.observe_generation(match revision {
                Some(revision) => GenerationRef::new(entity, *revision),
                None => GenerationRef::seed(entity),
            });
        }
        for asset in self.assets_by_id.values() {
            let (asset_id, a_token_revision, v_token_revision) =
                (asset.id, asset.a_token_revision, asset.v_token_revision);
            for (position, revision) in [
                (ScaledTokenPosition::Collateral, a_token_revision),
                (ScaledTokenPosition::Debt, v_token_revision),
            ] {
                view.observe_generation(GenerationRef::new(
                    GenerationEntity::ScaledToken { asset_id, position },
                    revision,
                ));
            }
        }
        if let Some(gho) = &self.gho {
            if gho.v_gho_discount_token.is_some() {
                view.observe_generation(GenerationRef::seed(GenerationEntity::DiscountToken {
                    chain_id,
                }));
            }
            if gho.v_token_address.is_some() {
                view.observe_generation(GenerationRef::seed(GenerationEntity::GhoDiscountLink {
                    chain_id,
                }));
            }
        }
    }

    /// The contract endpoints and revisions this resolution cache holds, in role
    /// order — the checkpoint's endpoint family.
    pub(crate) fn capture_contracts(&self) -> Vec<degenbot_db::CheckpointContract> {
        let mut contracts: Vec<degenbot_db::CheckpointContract> = self
            .contract_addresses
            .iter()
            .map(
                |((market_id, name), address)| degenbot_db::CheckpointContract {
                    name: name.clone(),
                    address: address.clone(),
                    revision: self
                        .contract_revisions
                        .get(&(*market_id, name.clone()))
                        .copied()
                        .flatten(),
                },
            )
            .collect();
        contracts.sort_by(|left, right| left.name.cmp(&right.name));
        contracts
    }

    /// The chain's GHO row as resolution read it, if the chain has one — the
    /// checkpoint's GHO family.
    pub(crate) fn capture_gho(&self) -> Option<degenbot_db::CheckpointGho> {
        self.gho.as_ref().map(|gho| degenbot_db::CheckpointGho {
            gho_id: gho.id,
            token_id: gho.token_id,
            v_token_id: gho.v_token_id,
            gho_token_address: gho.gho_token_address.clone(),
            v_token_address: gho.v_token_address.clone(),
            discount_rate_strategy: gho.v_gho_discount_rate_strategy.clone(),
            discount_token: gho.v_gho_discount_token.clone(),
        })
    }

    /// The substrate lookup kinds are the two `token_type` strings plus the
    /// underlying-JOIN kind; anything else is a caller error, reported as an
    /// unknown-kind seed refusal.
    fn asset_kind(kind: &str) -> Option<&'static str> {
        match kind {
            ASSET_KIND_UNDERLYING => Some(ASSET_KIND_UNDERLYING),
            ASSET_KIND_A_TOKEN => Some(ASSET_KIND_A_TOKEN),
            ASSET_KIND_V_TOKEN => Some(ASSET_KIND_V_TOKEN),
            _ => None,
        }
    }

    /// The shared position-state read for the persistence side: map hit →
    /// clone; miss → the SQL probe (the table's `SELECT balance, last_index ...
    /// WHERE id`) → cache → return. The error text mirrors the calling
    /// substrate fn's (`lookup_position_balance_index_on_conn` vs the apply
    /// body's).
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

/// Which error text a position-state read carries (the substrate readers the
/// cache shells for).
#[derive(Copy, Clone)]
enum PositionRead {
    /// The scaled-token apply body's read.
    Apply,
    /// The bad-debt reset's read (`reset_debt_position_to_zero_on_conn`).
    Reset,
}

/// Decode one row of the asset projection (the substrate lookups' exact column
/// order + NULL-revision rule).
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

/// The chunk's candidate substrate-address strings: every `topics[1]`-
/// `topics[3]` word's low-20-bytes address PLUS every log's emitter address
/// across the chunk's logs, checksummed (the substrate's `addr_to_hex` — the
/// BINARY-collation guard), deduped + sorted. The seed's indexes superset the
/// parse-time addresses — user get-or-creates, asset lookups by topic and by
/// emitter — so resolution answers them from memory.
pub(crate) fn candidate_addresses(logs: &[&alloy::rpc::types::Log]) -> (Vec<String>, Vec<String>) {
    let mut out: HashSet<String> = HashSet::new();
    for log in logs {
        for topic in log.topics().iter().skip(1).take(3) {
            out.insert(addr_to_hex(Address::from_slice(&topic.as_slice()[12..])));
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
    users.sort();
    assets.sort();
    (users, assets)
}

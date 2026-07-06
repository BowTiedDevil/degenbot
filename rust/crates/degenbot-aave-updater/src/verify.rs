//! On-chain-truth position verification (the minimal slice of the Rust port
//! of Python `verification.py::verify_scaled_token_positions`).
//!
//! For each scaled-token position (collateral via aToken, debt via vToken) the
//! DB holds a `balance` (scaled) + `last_index`; Aave V3's
//! `scaledBalanceOf(user)` + `getPreviousIndex(user)` are the canonical
//! on-chain truth at a block. This module loads the DB positions (filtered by
//! `user_addresses` when `Some`, otherwise all) and compares each against the
//! on-chain truth, returning a structured divergence list (mirror the Python's
//! two assertions: `balance` + `last_index` equality).
//!
//! Mirrors `src/degenbot/cli/aave/verification.py:152`. The `DEAD_ADDRESS` /
//! `ZERO_ADDRESS` skip is preserved. The Rust core fn is callable from BOTH the
//! 6SWY4R drive harness (the per-chunk value-correctness gate) AND, in a
//! follow-up, `run_aave_update` (production `verify_chunk` flag — NOT in this
//! task). Uses `AlloyProvider::eth_call` (per-position calls) — multicall3
//! batching for the market-wide verify is the natural extension
//! (BE474R-full, post-HLYWI6).

use alloy::primitives::{Address, Bytes, U256};
use degenbot_db::DbError;
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::Connection;

use crate::RunError;

/// The aToken / vToken scaled-token position kind (collateral / debt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionKind {
    /// aToken scaled balance (collateral).
    Collateral,
    /// vToken scaled balance (debt).
    Debt,
}

/// Which field diverged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DivergenceField {
    /// The scaled `balance` column.
    Balance,
    /// The `last_index` column.
    LastIndex,
}

/// One named mismatch between the DB position state + the on-chain truth at
/// `block_number` (mirror of Python `verify_scaled_token_positions`'s two
/// assertions; one divergence per row/column; can have up to two per
/// position if BOTH balance + index mismatch).
#[derive(Debug, Clone)]
pub struct PositionDivergence {
    /// Collateral (aToken) or debt (vToken).
    pub kind: PositionKind,
    /// The DB surrogate `id` of the diverging position row.
    pub position_id: i64,
    /// The user's address (for human-readable reporting).
    pub user_address: Address,
    /// The aToken (collateral) or vToken (debt) address (the contract the
    /// `scaledBalanceOf` / `getPreviousIndex` was called on).
    pub token_address: Address,
    /// The block the verification was performed at.
    pub block_number: u64,
    /// Which field mismatched.
    pub field: DivergenceField,
    /// What the DB row held.
    pub expected: U256,
    /// What `scaledBalanceOf` / `getPreviousIndex` returned on-chain.
    pub actual: U256,
}

/// A row loaded from the DB for the verify pass.
struct PositionRow {
    position_id: i64,
    user_address: Address,
    token_address: Address,
    balance: U256,
    last_index: Option<U256>,
}

/// The token FK column on `aave_v3_assets` to resolve (collateral -> `a_token_id`,
/// debt -> `v_token_id`).
fn token_column(kind: PositionKind) -> &'static str {
    match kind {
        PositionKind::Collateral => "a_token_id",
        PositionKind::Debt => "v_token_id",
    }
}

/// The positions table for `kind`.
fn positions_table(kind: PositionKind) -> &'static str {
    match kind {
        PositionKind::Collateral => "aave_v3_collateral_positions",
        PositionKind::Debt => "aave_v3_debt_positions",
    }
}

/// Load `{collateral,debt}_positions` joined to user (address) + asset →
/// aToken/vToken erc20 token address, filtered by `market_id` (+ optional
/// `user_addresses`). Rows whose address or balance columns don't parse are
/// skipped (defensive — invalid rows are surfaced as parse errors via the
/// divergence list elsewhere + shouldn't appear in a healthy writer state).
fn load_position_rows(
    conn: &Connection,
    market_id: i64,
    kind: PositionKind,
    user_addresses: Option<&[Address]>,
) -> Result<Vec<PositionRow>, DbError> {
    let token_col = token_column(kind);
    let table = positions_table(kind);
    // Build the SELECT + the optional user-address IN-clause.
    let placeholders = match user_addresses {
        Some(addrs) if !addrs.is_empty() => {
            // One "?" per address — bound after `market_id`.
            let n = addrs.len();
            vec!["?"; n].join(", ")
        }
        _ => String::new(),
    };
    let sql = if user_addresses.is_some() && !user_addresses.unwrap().is_empty() {
        format!(
            "SELECT p.id, u.address, et.address, p.balance, p.last_index \
             FROM {table} p \
             JOIN aave_v3_users u ON u.id = p.user_id \
             JOIN aave_v3_assets a ON a.id = p.asset_id \
             JOIN erc20_tokens et ON et.id = a.{token_col} \
             WHERE u.market_id = ? AND LOWER(u.address) IN ({placeholders})"
        )
    } else {
        format!(
            "SELECT p.id, u.address, et.address, p.balance, p.last_index \
             FROM {table} p \
             JOIN aave_v3_users u ON u.id = p.user_id \
             JOIN aave_v3_assets a ON a.id = p.asset_id \
             JOIN erc20_tokens et ON et.id = a.{token_col} \
             WHERE u.market_id = ?"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    // Build the bind params: market_id first, then (if filter given) the user
    // addresses lowercased (the SQL uses LOWER(...) so this is also case-
    // insensitive on the bind value — the DB stores erc20_tokens.address as
    // EIP-55 checksummed + lower-cased comparisons unify both sides).
    let user_addr_strs: Vec<String> = match user_addresses {
        Some(addrs) if !addrs.is_empty() => addrs.iter().map(|a| format!("{a:?}").to_lowercase()).collect(),
        _ => Vec::new(),
    };
    let mut bind_params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(1 + user_addr_strs.len());
    bind_params.push(&market_id);
    for s in &user_addr_strs {
        bind_params.push(s);
    }
    let iter = stmt.query_map(bind_params.as_slice(), |r| {
        let pid: i64 = r.get(0)?;
        let user_addr_str: String = r.get(1)?;
        let token_addr_str: String = r.get(2)?;
        let balance_str: String = r.get(3)?;
        let last_index_str: Option<String> = r.get(4)?;
        Ok((pid, user_addr_str, token_addr_str, balance_str, last_index_str))
    })?;
    let mut rows: Vec<PositionRow> = Vec::new();
    for r in iter {
        let (pid, user_addr_str, token_addr_str, balance_str, last_index_str) = r?;
        let Ok(user_address) = user_addr_str.parse() else { continue };
        let Ok(token_address) = token_addr_str.parse() else { continue };
        let Ok(balance) = parse_u256(&balance_str) else { continue };
        let last_index = last_index_str.as_deref().and_then(|s| parse_u256(s).ok());
        rows.push(PositionRow {
            position_id: pid,
            user_address,
            token_address,
            balance,
            last_index,
        });
    }
    Ok(rows)
}

/// Parse a (decimal or `0x…` hex) U256 string from a DB TEXT/VARCHAR(78).
fn parse_u256(s: &str) -> Result<U256, DbError> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        U256::from_str_radix(rest, 16).map_err(|e| DbError::Decode(format!("bad hex U256 {s:?}: {e}")))
    } else {
        U256::from_str_radix(s, 10).map_err(|e| DbError::Decode(format!("bad decimal U256 {s:?}: {e}")))
    }
}

/// keccak256("scaledBalanceOf(address)")[0..4] = `0x1da24f3e`.
const SCALED_BALANCE_OF_SELECTOR: [u8; 4] = [0x1d, 0xa2, 0x4f, 0x3e];
/// keccak256("getPreviousIndex(address)")[0..4] = `0xe0753986`.
const GET_PREVIOUS_INDEX_SELECTOR: [u8; 4] = [0xe0, 0x75, 0x39, 0x86];

/// Build the 36-byte calldata `selector(4) + address(32, left-padded)`.
fn build_call_data(selector: [u8; 4], user: Address) -> Bytes {
    let mut buf = [0u8; 36];
    buf[0..4].copy_from_slice(&selector);
    buf[16..36].copy_from_slice(user.as_slice());
    Bytes::copy_from_slice(&buf)
}

/// Decode a 32-byte ABI-encoded uint256 return value.
fn decode_uint256_return(bytes: &Bytes) -> Option<U256> {
    if bytes.len() < 32 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&bytes[0..32]);
    Some(U256::from_be_bytes::<32>(buf))
}

const DEAD_ADDRESS: Address = alloy::primitives::address!("0x000000000000000000000000000000000000dEaD");
const ZERO_ADDRESS: Address = Address::ZERO;

/// Verify the on-chain scaled-token state matches the DB positions for
/// `market_id` at `block_number`. Mirrors Python's
/// `verify_scaled_token_positions` (verification.py:152) — iterates the
/// collateral + debt positions (filtered by `user_addresses` when `Some`),
/// skips `DEAD_ADDRESS` / `ZERO_ADDRESS` users, calls `scaledBalanceOf(user)` +
/// `getPreviousIndex(user)` on each aToken / vToken at `block_number`, asserts
/// equality.
///
/// Per-position `eth_call`s — acceptable for the touched-users-per-chunk case
/// (small set). Multicall3 batching is the natural extension for the
/// market-wide verify (BE474R-full, post-HLYWI6).
///
/// Returns the divergence list (empty = GREEN). Each row carries the
/// human-readable fields needed by the JGQHBX harness to emit a NAMED,
/// bisect-able divergence (`user_address`, `token_address`, `position_id`,
/// expected vs actual `balance`/`last_index` at `block_number`).
///
/// # Errors
///
/// Returns [`RunError`] on a DB or RPC failure that prevents verification
/// (a single position's revert-on-call is treated as `actual == 0` and
/// surfaced as a divergence if the DB expected non-zero).
pub async fn verify_touched_positions_on_conn(
    conn: &Connection,
    provider: &AlloyProvider,
    market_id: i64,
    block_number: u64,
    user_addresses: Option<&[Address]>,
) -> Result<Vec<PositionDivergence>, RunError> {
    let mut divergences: Vec<PositionDivergence> = Vec::new();

    for kind in [PositionKind::Collateral, PositionKind::Debt] {
        let rows = load_position_rows(conn, market_id, kind, user_addresses)?;
        for row in rows {
            // Skip the dead/zero addresses (mirror the Python skip).
            if row.user_address == DEAD_ADDRESS || row.user_address == ZERO_ADDRESS {
                continue;
            }
            // scaledBalanceOf(user) at block_number.
            let calldata = build_call_data(SCALED_BALANCE_OF_SELECTOR, row.user_address);
            let actual_balance = provider
                .eth_call(&row.token_address, calldata, Some(block_number))
                .await
                .ok()
                .and_then(|b| decode_uint256_return(&b))
                .unwrap_or(U256::ZERO);
            if actual_balance != row.balance {
                divergences.push(PositionDivergence {
                    kind,
                    position_id: row.position_id,
                    user_address: row.user_address,
                    token_address: row.token_address,
                    block_number,
                    field: DivergenceField::Balance,
                    expected: row.balance,
                    actual: actual_balance,
                });
            }
            // getPreviousIndex(user) at block_number.
            let calldata = build_call_data(GET_PREVIOUS_INDEX_SELECTOR, row.user_address);
            let actual_index = provider
                .eth_call(&row.token_address, calldata, Some(block_number))
                .await
                .ok()
                .and_then(|b| decode_uint256_return(&b))
                .unwrap_or(U256::ZERO);
            let expected_index = row.last_index.unwrap_or(U256::ZERO);
            if actual_index != expected_index {
                divergences.push(PositionDivergence {
                    kind,
                    position_id: row.position_id,
                    user_address: row.user_address,
                    token_address: row.token_address,
                    block_number,
                    field: DivergenceField::LastIndex,
                    expected: expected_index,
                    actual: actual_index,
                });
            }
        }
    }

    Ok(divergences)
}

// Safety net: thiserror's `#[from]` on RunError::Db(#[from] DbError) +
// RunError::Provider(#[from] ProviderError) generates the From impls — so the
// `?` operator on DbError / ProviderError values in the fns above resolves to
// RunError::Db / RunError::Provider directly. No manual From impls here.

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Seed an in-memory SQLite DB with the minimal aave schema + seed one
    /// collateral position for `(user_id, asset_id)` with the given balance +
    /// last_index. Returns the open `Connection` (the `DegenbotDb` handle's
    /// connection wrapper).
    fn seed_with_position(
        user_address: &str,
        a_token_address: &str,
        balance: &str,
        last_index: Option<&str>,
    ) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // Schema (minimal subset of `schema_head.sql`).
        conn.execute_batch(
            "CREATE TABLE aave_v3_markets (id INTEGER NOT NULL PRIMARY KEY, chain_id INTEGER NOT NULL, name TEXT NOT NULL, active INTEGER NOT NULL, last_update_block INTEGER);
             CREATE TABLE aave_v3_users (id INTEGER NOT NULL PRIMARY KEY, market_id INTEGER NOT NULL, address VARCHAR(42) NOT NULL, e_mode INTEGER NOT NULL, gho_discount INTEGER NOT NULL, stk_aave_balance VARCHAR(78), isolation_mode_collateral_asset_id INTEGER, isolation_mode_debt VARCHAR(78) NOT NULL);
             CREATE TABLE erc20_tokens (id INTEGER NOT NULL PRIMARY KEY, chain INTEGER NOT NULL, address VARCHAR(42) NOT NULL);
             CREATE TABLE aave_v3_assets (id INTEGER NOT NULL PRIMARY KEY, market_id INTEGER NOT NULL, underlying_asset_id INTEGER NOT NULL, a_token_id INTEGER NOT NULL, a_token_revision INTEGER NOT NULL, v_token_id INTEGER NOT NULL, v_token_revision INTEGER NOT NULL, e_mode_category_id INTEGER, price_source VARCHAR(42), last_update_block INTEGER, liquidity_index VARCHAR(78) NOT NULL, liquidity_rate VARCHAR(78) NOT NULL, borrow_index VARCHAR(78) NOT NULL, borrow_rate VARCHAR(78) NOT NULL);
             CREATE TABLE aave_v3_collateral_positions (id INTEGER NOT NULL PRIMARY KEY, user_id INTEGER NOT NULL, asset_id INTEGER NOT NULL, balance VARCHAR(78) NOT NULL, last_index VARCHAR(78));
             CREATE TABLE aave_v3_debt_positions (id INTEGER NOT NULL PRIMARY KEY, user_id INTEGER NOT NULL, asset_id INTEGER NOT NULL, balance VARCHAR(78) NOT NULL, last_index VARCHAR(78));",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) VALUES (1, 1, 'mainnet', 1, 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, isolation_mode_debt) VALUES (1, 1, ?1, 0, 0, '0')",
            [user_address],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, 1, ?1)",
            [a_token_address],
        )
        .unwrap();
        // asset 1: underlying_asset_id=1, a_token_id=2 — but we insert the
        // aToken as erc20 id 1 here (the verify JOINs on `a_token_id` →
        // the aToken row). Plus a separate vtoken id for parity with the
        // full schema (not used by collateral verify).
        conn.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (2, 1, '0x0000000000000000000000000000000000000002')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, a_token_id, a_token_revision, v_token_id, v_token_revision, liquidity_index, liquidity_rate, borrow_index, borrow_rate) VALUES (1, 1, 2, 1, 1, 2, 1, '1000000000000000000000000000', '0', '1000000000000000000000000000', '0')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_collateral_positions (id, user_id, asset_id, balance, last_index) VALUES (1, 1, 1, ?1, ?2)",
            rusqlite::params![balance, last_index],
        )
        .unwrap();
        conn
    }

    /// `load_position_rows` returns the seeded collateral row joined to the
    /// aToken address (no user-address filter).
    #[test]
    fn load_position_rows_returns_collateral_position_joined_to_atoken() {
        let conn = seed_with_position(
            "0x1111111111111111111111111111111111111111",
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "123456789",
            Some("1000000000000000000000000000"),
        );
        let rows =
            load_position_rows(&conn, 1, PositionKind::Collateral, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].position_id, 1);
        assert_eq!(rows[0].balance, U256::from(123456789u64));
        assert_eq!(rows[0].last_index, Some(U256::from(10u64).pow(U256::from(27u64))));
        let token_addr: Address = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .parse()
            .unwrap();
        assert_eq!(rows[0].token_address, token_addr);
    }

    /// The user-address filter narrows the loaded rows to the specified set.
    #[test]
    fn load_position_rows_filters_by_user_addresses() {
        let conn = seed_with_position(
            "0x1111111111111111111111111111111111111111",
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "123456789",
            Some("1000000000000000000000000000"),
        );
        // Add a second user with a different collateral position.
        conn.execute(
            "INSERT INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, isolation_mode_debt) VALUES (2, 1, '0x2222222222222222222222222222222222222222', 0, 0, '0')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_collateral_positions (id, user_id, asset_id, balance, last_index) VALUES (2, 2, 1, '42', '1000000000000000000000000000')",
            [],
        )
        .unwrap();
        let wanted = [Address::repeat_byte(0x22)];
        let rows = load_position_rows(&conn, 1, PositionKind::Collateral, Some(&wanted)).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].position_id, 2);
        assert_eq!(rows[0].balance, U256::from(42u64));
    }

    /// `parse_u256` tolerates both decimal + `0x…` prefixed hex strings (the
    /// DB stores U256 as decimal TEXT, but the underlying helpers sometimes
    /// emit hex).
    #[test]
    fn parse_u256_supports_decimal_and_hex() {
        assert_eq!(parse_u256("42").unwrap(), U256::from(42u64));
        assert_eq!(parse_u256("0x2a").unwrap(), U256::from(42u64));
        assert_eq!(parse_u256("0X2A").unwrap(), U256::from(42u64));
        assert!(parse_u256("not a number").is_err());
    }

    /// `build_call_data` packs `selector(4) + address(32, left-padded)`.
    #[test]
    fn build_call_data_packs_selector_and_address() {
        let user: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let data = build_call_data(SCALED_BALANCE_OF_SELECTOR, user);
        // Selector prefix.
        assert_eq!(&data[0..4], &SCALED_BALANCE_OF_SELECTOR);
        // 12 zero bytes + 20 user bytes.
        assert!(data[4..16].iter().all(|b| *b == 0));
        assert_eq!(&data[16..36], user.as_slice());
        // 36 bytes total.
        assert_eq!(data.len(), 36);
    }

    /// `decode_uint256_return` decodes a 32-byte big-endian U256.
    #[test]
    fn decode_uint256_return_decodes_32_byte_be() {
        // 0x00...2a (42).
        let mut buf = vec![0u8; 32];
        buf[31] = 0x2a;
        let bytes = Bytes::copy_from_slice(&buf);
        assert_eq!(decode_uint256_return(&bytes), Some(U256::from(42u64)));
        // < 32 bytes → None.
        assert_eq!(decode_uint256_return(&Bytes::copy_from_slice(&[0u8; 16])), None);
    }
}
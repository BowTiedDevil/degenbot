//! On-chain-truth position verification (the Rust port of Python
//! `verification.py` + `db_verification.py`).
//!
//! Four verification checks, mirroring Python's `verify_all_positions`:
//! 1. Collateral scaled-token balance + `last_index` (`scaledBalanceOf` +
//!    `getPreviousIndex` on each aToken).
//! 2. Debt scaled-token balance + `last_index` (same calls on each vToken).
//! 3. stkAAVE balance (`balanceOf` on the discount token).
//! 4. GHO discount percent (`getDiscountPercent` on the GHO vToken, with a
//!    revision-based skip guard — `v_token_revision >= 4` → skip).
//!
//! Plus `cleanup_zero_balance_positions_on_conn` — DELETEs zero-balance
//! collateral + debt rows (the Python `cleanup_zero_balance_positions`).
//!
//! Mirrors `src/degenbot/cli/aave/verification.py` +
//! `src/degenbot/cli/aave/db_verification.py`. The `DEAD_ADDRESS` /
//! `ZERO_ADDRESS` skip is preserved. Uses `AlloyProvider::eth_call`
//! (per-position calls) — multicall3 batching is the natural extension
//! (BE474R-full, post-HLYWI6).

use alloy::primitives::{Address, Bytes, U256};
use degenbot_db::{DegenbotDb, DbError};
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
        Some(addrs) if !addrs.is_empty() => addrs
            .iter()
            .map(|a| format!("{a:?}").to_lowercase())
            .collect(),
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
        Ok((
            pid,
            user_addr_str,
            token_addr_str,
            balance_str,
            last_index_str,
        ))
    })?;
    let mut rows: Vec<PositionRow> = Vec::new();
    for r in iter {
        let (pid, user_addr_str, token_addr_str, balance_str, last_index_str) = r?;
        let Ok(user_address) = user_addr_str.parse() else {
            continue;
        };
        let Ok(token_address) = token_addr_str.parse() else {
            continue;
        };
        let Ok(balance) = parse_u256(&balance_str) else {
            continue;
        };
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
        U256::from_str_radix(rest, 16)
            .map_err(|e| DbError::Decode(format!("bad hex U256 {s:?}: {e}")))
    } else {
        U256::from_str_radix(s, 10)
            .map_err(|e| DbError::Decode(format!("bad decimal U256 {s:?}: {e}")))
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

const DEAD_ADDRESS: Address =
    alloy::primitives::address!("0x000000000000000000000000000000000000dEaD");
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

// ── stkAAVE balance + GHO discount verification ─────────────────────────

/// The GHO vToken revision at which the discount mechanism is deprecated
/// (mirrors `cli/aave/constants.py::GHO_DISCOUNT_DEPRECATION_REVISION`).
/// At revision ≥ 4, `getDiscountPercent` is removed; the verify skips.
const GHO_DISCOUNT_DEPRECATION_REVISION: i64 = 4;

/// keccak256("balanceOf(address)")[0..4] = `0x70a08231`.
const BALANCE_OF_SELECTOR: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];
/// keccak256("getDiscountPercent(address)")[0..4] = `0x6c53272b`.
const GET_DISCOUNT_PERCENT_SELECTOR: [u8; 4] = [0x6c, 0x53, 0x27, 0x2b];

/// A unified divergence from any of the 4 verification checks. Returned by
/// [`verify_all_positions_on_conn`].
#[derive(Debug, Clone)]
pub enum VerificationDivergence {
    /// Scaled-token position (collateral or debt) balance/index mismatch
    /// (checks 1 + 2 — the existing `verify_touched_positions_on_conn` output).
    ScaledToken(PositionDivergence),
    /// stkAAVE balance mismatch on `aave_v3_users.stk_aave_balance`
    /// (check 3 — `balanceOf` on the discount token).
    StkAaveBalance {
        user_id: i64,
        user_address: Address,
        token_address: Address,
        block_number: u64,
        expected: U256,
        actual: U256,
    },
    /// GHO discount percent mismatch on `aave_v3_users.gho_discount`
    /// (check 4 — `getDiscountPercent` on the GHO vToken).
    GhoDiscount {
        user_id: i64,
        user_address: Address,
        token_address: Address,
        block_number: u64,
        expected: i64,
        actual: i64,
    },
}



/// Build the optional user-address WHERE clause + bind params.
fn user_address_clause(user_addresses: Option<&[Address]>) -> (String, Vec<String>) {
    match user_addresses {
        Some(addrs) if !addrs.is_empty() => {
            let placeholders = vec!["?"; addrs.len()].join(", ");
            let strs: Vec<String> = addrs.iter().map(|a| format!("{a:?}").to_lowercase()).collect();
            (
                format!(" AND LOWER(u.address) IN ({placeholders})"),
                strs,
            )
        }
        _ => (String::new(), Vec::new()),
    }
}

/// Verify that `aave_v3_users.stk_aave_balance` matches on-chain `balanceOf`
/// for each user with a tracked stkAAVE balance. Mirrors Python's
/// `verify_stk_aave_balances` (`db_verification.py:61`). No-op when the market
/// has no `v_gho_discount_token` (the GHO asset row's discount token is
/// `None`).
///
/// # Errors
///
/// Returns [`RunError`] on a DB or RPC failure that prevents verification.
pub async fn verify_stk_aave_balances_on_conn(
    conn: &Connection,
    provider: &AlloyProvider,
    market_id: i64,
    chain_id: i64,
    block_number: u64,
    user_addresses: Option<&[Address]>,
) -> Result<Vec<VerificationDivergence>, RunError> {
    let gho_asset = DegenbotDb::fetch_aave_gho_asset_on_conn(conn, chain_id)?;
    let Some(gho_asset) = gho_asset else {
        return Ok(Vec::new());
    };
    let Some(discount_token_str) = &gho_asset.v_gho_discount_token else {
        return Ok(Vec::new());
    };
    let Ok(token_address) = discount_token_str.parse::<Address>() else {
        return Ok(Vec::new());
    };

    let (addr_clause, addr_strs) = user_address_clause(user_addresses);
    let sql = format!(
        "SELECT u.id, u.address, u.stk_aave_balance \
         FROM aave_v3_users u \
         WHERE u.market_id = ?1 AND u.stk_aave_balance IS NOT NULL{addr_clause}"
    );
    let mut stmt = conn.prepare(&sql).map_err(DbError::from)?;
    let mut bind_params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(1 + addr_strs.len());
    bind_params.push(&market_id);
    for s in &addr_strs {
        bind_params.push(s);
    }
    let iter = stmt.query_map(bind_params.as_slice(), |r| {
        let id: i64 = r.get(0)?;
        let addr: String = r.get(1)?;
        let bal: Option<String> = r.get(2)?;
        Ok((id, addr, bal))
    }).map_err(DbError::from)?;

    let mut divergences = Vec::new();
    for row in iter {
        let (user_id, user_addr_str, bal_str) = row.map_err(DbError::from)?;
        let Ok(user_address) = user_addr_str.parse() else { continue };
        let expected = bal_str.as_deref().and_then(|s| parse_u256(s).ok()).unwrap_or(U256::ZERO);

        let calldata = build_call_data(BALANCE_OF_SELECTOR, user_address);
        let actual = provider
            .eth_call(&token_address, calldata, Some(block_number))
            .await
            .ok()
            .and_then(|b| decode_uint256_return(&b))
            .unwrap_or(U256::ZERO);

        if actual != expected {
            divergences.push(VerificationDivergence::StkAaveBalance {
                user_id,
                user_address,
                token_address,
                block_number,
                expected,
                actual,
            });
        }
    }
    Ok(divergences)
}

/// Verify that `aave_v3_users.gho_discount` matches the GHO vToken's
/// `getDiscountPercent` for each user with GHO debt. Mirrors Python's
/// `verify_gho_discount_amounts` (`db_verification.py:22`). Skips when the
/// GHO vToken's revision is `None` or `>= GHO_DISCOUNT_DEPRECATION_REVISION` (4).
///
/// # Errors
///
/// Returns [`RunError`] on a DB or RPC failure that prevents verification.
pub async fn verify_gho_discount_amounts_on_conn(
    conn: &Connection,
    provider: &AlloyProvider,
    market_id: i64,
    chain_id: i64,
    block_number: u64,
    user_addresses: Option<&[Address]>,
) -> Result<Vec<VerificationDivergence>, RunError> {
    let gho_asset = DegenbotDb::fetch_aave_gho_asset_on_conn(conn, chain_id)?;
    let Some(gho_asset) = &gho_asset else {
        return Ok(Vec::new());
    };
    let Some(v_token_id) = gho_asset.v_token_id else {
        return Ok(Vec::new());
    };
    let Some(v_token_address_str) = &gho_asset.v_token_address else {
        return Ok(Vec::new());
    };
    let Ok(v_token_address) = v_token_address_str.parse::<Address>() else {
        return Ok(Vec::new());
    };

    // Resolve the vToken's revision; skip if >= deprecation revision.
    let revision: Option<i64> = conn
        .query_row(
            "SELECT a.v_token_revision FROM aave_v3_assets a \
             WHERE a.market_id = ?1 AND a.v_token_id = ?2",
            rusqlite::params![market_id, v_token_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten();
    if revision.is_none_or(|r| r >= GHO_DISCOUNT_DEPRECATION_REVISION) {
        return Ok(Vec::new());
    }

    // Load users with GHO debt positions.
    let (addr_clause, addr_strs) = user_address_clause(user_addresses);
    let sql = format!(
        "SELECT DISTINCT u.id, u.address, u.gho_discount \
         FROM aave_v3_users u \
         JOIN aave_v3_debt_positions dp ON dp.user_id = u.id \
         JOIN aave_v3_assets a ON a.id = dp.asset_id \
         WHERE a.market_id = ?1 AND a.v_token_id = ?2{addr_clause}"
    );
    let mut stmt = conn.prepare(&sql).map_err(DbError::from)?;
    let mut bind_params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(2 + addr_strs.len());
    bind_params.push(&market_id);
    bind_params.push(&v_token_id);
    for s in &addr_strs {
        bind_params.push(s);
    }
    let iter = stmt.query_map(bind_params.as_slice(), |r| {
        let id: i64 = r.get(0)?;
        let addr: String = r.get(1)?;
        let discount: i64 = r.get(2)?;
        Ok((id, addr, discount))
    }).map_err(DbError::from)?;

    let mut divergences = Vec::new();
    for row in iter {
        let (user_id, user_addr_str, expected) = row.map_err(DbError::from)?;
        let Ok(user_address) = user_addr_str.parse() else { continue };

        let calldata = build_call_data(GET_DISCOUNT_PERCENT_SELECTOR, user_address);
        let actual_u256 = provider
            .eth_call(&v_token_address, calldata, Some(block_number))
            .await
            .ok()
            .and_then(|b| decode_uint256_return(&b))
            .unwrap_or(U256::ZERO);
        let actual: i64 = actual_u256.try_into().unwrap_or(i64::MAX);

        if actual != expected {
            divergences.push(VerificationDivergence::GhoDiscount {
                user_id,
                user_address,
                token_address: v_token_address,
                block_number,
                expected,
                actual,
            });
        }
    }
    Ok(divergences)
}

/// Run all 4 verification checks (scaled-token collateral + debt, stkAAVE
/// balance, GHO discount) and return a unified divergence list. Mirrors
/// the Python `verify_all_positions` (verification.py:110) +
/// `verify_positions_for_users` (verification.py:44).
///
/// When `user_addresses` is `None`, verifies ALL positions/users in the market
/// (the post-run `--verify-all` path). When `Some`, verifies only the
/// specified users (the JGQHBX per-chunk path).
///
/// # Errors
///
/// Returns [`RunError`] on a DB or RPC failure that prevents verification.
pub async fn verify_all_positions_on_conn(
    conn: &Connection,
    provider: &AlloyProvider,
    market_id: i64,
    chain_id: i64,
    block_number: u64,
    user_addresses: Option<&[Address]>,
) -> Result<Vec<VerificationDivergence>, RunError> {
    let mut divergences = Vec::new();

    // Checks 1 + 2: scaled-token balance + last_index (collateral + debt).
    let scaled = verify_touched_positions_on_conn(
        conn,
        provider,
        market_id,
        block_number,
        user_addresses,
    )
    .await?;
    for d in scaled {
        divergences.push(VerificationDivergence::ScaledToken(d));
    }

    // Check 3: stkAAVE balance.
    let stk = verify_stk_aave_balances_on_conn(
        conn,
        provider,
        market_id,
        chain_id,
        block_number,
        user_addresses,
    )
    .await?;
    divergences.extend(stk);

    // Check 4: GHO discount percent.
    let gho = verify_gho_discount_amounts_on_conn(
        conn,
        provider,
        market_id,
        chain_id,
        block_number,
        user_addresses,
    )
    .await?;
    divergences.extend(gho);

    Ok(divergences)
}

/// Delete all zero-balance collateral + debt positions for `market_id`.
/// Mirrors the Python `cleanup_zero_balance_positions`
/// (verification.py:19). A WRITE operation — must be called on a writable
/// connection; the caller commits.
///
/// # Errors
///
/// Returns [`DbError`] on any SQL failure.
pub fn cleanup_zero_balance_positions_on_conn(
    conn: &Connection,
    market_id: i64,
) -> Result<(), DbError> {
    conn.execute(
        "DELETE FROM aave_v3_collateral_positions \
         WHERE id IN ( \
             SELECT p.id FROM aave_v3_collateral_positions p \
             JOIN aave_v3_users u ON u.id = p.user_id \
             WHERE u.market_id = ?1 AND p.balance = '0' \
         )",
        rusqlite::params![market_id],
    )?;
    conn.execute(
        "DELETE FROM aave_v3_debt_positions \
         WHERE id IN ( \
             SELECT p.id FROM aave_v3_debt_positions p \
             JOIN aave_v3_users u ON u.id = p.user_id \
             WHERE u.market_id = ?1 AND p.balance = '0' \
         )",
        rusqlite::params![market_id],
    )?;
    Ok(())
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

    /// Seed an in-memory `SQLite` `DB` with the minimal aave schema + seed one
    /// collateral position for `(user_id, asset_id)` with the given balance +
    /// `last_index`. Returns the open `Connection` (the `DegenbotDb` handle's
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
        let rows = load_position_rows(&conn, 1, PositionKind::Collateral, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].position_id, 1);
        assert_eq!(rows[0].balance, U256::from(123_456_789_u64));
        assert_eq!(
            rows[0].last_index,
            Some(U256::from(10u64).pow(U256::from(27u64)))
        );
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
        assert_eq!(
            decode_uint256_return(&Bytes::copy_from_slice(&[0u8; 16])),
            None
        );
    }
}

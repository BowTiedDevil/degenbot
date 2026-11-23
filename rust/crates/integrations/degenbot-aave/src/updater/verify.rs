//! On-chain-truth position verification.
//!
//! Four verification checks:
//! 1. Collateral scaled-token balance + `last_index` (`scaledBalanceOf` +
//!    `getPreviousIndex` on each aToken).
//! 2. Debt scaled-token balance + `last_index` (same calls on each vToken).
//! 3. stkAAVE balance (`balanceOf` on the discount token).
//! 4. GHO discount percent (`getDiscountPercent` on the GHO vToken, with a
//!    revision-based skip guard — `v_token_revision >= 4` → skip).
//!
//! Plus `cleanup_zero_balance_positions_on_conn` — DELETEs zero-balance
//! collateral + debt rows.
//!
//! The verification pass reads each position's on-chain state and reconciles it
//! against the committed rows; the `DEAD_ADDRESS` / `ZERO_ADDRESS` skip is part
//! of the contract (an address with no state has nothing to reconcile). The
//! reads are batched, not per-position: every check flattens its calls into one
//! `BlockReads::batch` per position kind, hash-pinned to the exact anchor the
//! gate states (ADR-105 D3).

use alloy::primitives::{Address, Bytes, U256};
use degenbot_db::{DbError, DegenbotDb};
use degenbot_rpc::chain_evidence::{resolve_anchor_at, BlockReads};
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::Connection;

use crate::RunError;
use degenbot_rpc::multicall3::MulticallOutcome;

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
/// `block_number` (one divergence per row/column; can have up to two per
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

/// A row loaded from the DB for the verify pass: one position, the identity the
/// on-chain read is issued for, and the DB's expectation.
#[derive(Debug, Clone, Copy)]
pub struct VerificationPosition {
    pub kind: PositionKind,
    pub position_id: i64,
    pub user_address: Address,
    pub token_address: Address,
    pub balance: U256,
    pub last_index: Option<U256>,
}

/// What one position's token reported on chain at the verify block - the
/// acquisition phase's whole product, joined to its position by identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionTruth {
    pub kind: PositionKind,
    pub position_id: i64,
    pub balance: U256,
    pub last_index: U256,
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
/// `user_addresses`).
///
/// # Errors
///
/// A row whose address, balance, or `last_index` column does not parse is a
/// [`DbError::Decode`] naming the table, the row id, the column, and the
/// stored value. The load refuses rather than dropping the row: the verify
/// pass would otherwise compare a position set the DB does not hold and
/// report the dropped position as green.
fn load_position_rows(
    conn: &Connection,
    market_id: i64,
    kind: PositionKind,
    user_addresses: Option<&[Address]>,
) -> Result<Vec<VerificationPosition>, DbError> {
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
    let filtered = user_addresses
        .as_ref()
        .is_some_and(|addrs| !addrs.is_empty());
    let sql = if filtered {
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
    let mut rows: Vec<VerificationPosition> = Vec::new();
    for r in iter {
        let (pid, user_addr_str, token_addr_str, balance_str, last_index_str) = r?;
        let user_address = user_addr_str
            .parse()
            .map_err(|e| decode_position_row(table, pid, "address", &user_addr_str, e))?;
        let token_address = token_addr_str
            .parse()
            .map_err(|e| decode_position_row(table, pid, "token_address", &token_addr_str, e))?;
        let balance = parse_u256(&balance_str)
            .map_err(|e| decode_position_row(table, pid, "balance", &balance_str, e))?;
        let last_index = match last_index_str.as_deref() {
            None => None,
            Some(s) => Some(
                parse_u256(s).map_err(|e| decode_position_row(table, pid, "last_index", s, e))?,
            ),
        };
        rows.push(VerificationPosition {
            kind,
            position_id: pid,
            user_address,
            token_address,
            balance,
            last_index,
        });
    }
    Ok(rows)
}

/// The typed refusal for one unparseable position column: the table, the row,
/// the column, and the stored value.
fn decode_position_row(
    table: &str,
    position_id: i64,
    column: &str,
    value: &str,
    why: impl std::fmt::Display,
) -> DbError {
    DbError::Decode(format!(
        "{table} id={position_id}: {column} {value:?} does not parse: {why}"
    ))
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

/// keccak256("balanceOf(address)")[0..4] = `0x70a08231`.
const BALANCE_OF_SELECTOR: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];

/// Build the 36-byte calldata `selector(4) + address(32, left-padded)`.
fn build_call_data(selector: [u8; 4], user: Address) -> Bytes {
    let mut buf = [0u8; 36];
    buf[0..4].copy_from_slice(&selector);
    buf[16..36].copy_from_slice(user.as_slice());
    Bytes::copy_from_slice(&buf)
}

const DEAD_ADDRESS: Address =
    alloy::primitives::address!("0x000000000000000000000000000000000000dEaD");
const ZERO_ADDRESS: Address = Address::ZERO;

/// Verify the on-chain scaled-token state matches the DB positions for
/// `market_id` at `block_number` — iterates the
/// collateral + debt positions (filtered by `user_addresses` when `Some`),
/// skips `DEAD_ADDRESS` / `ZERO_ADDRESS` users, calls `scaledBalanceOf(user)` +
/// `getPreviousIndex(user)` on each aToken / vToken at `block_number`, asserts
/// equality.
///
/// Per-position `eth_call`s are batched through Multicall3 `aggregate3`
/// (with `allowFailure = true` — a reverted sub-call is a typed
/// [`MulticallOutcome::Reverted`], treated as `actual == 0`, preserving the
/// prior per-call revert semantics). The market-wide `--verify-all` path is the
/// win; the per-chunk touched-set path is a wash but harmless.
///
/// Returns the divergence list (empty = no divergence). Each row carries the
/// human-readable fields needed by the drive harness to emit a NAMED,
/// bisect-able divergence (`user_address`, `token_address`, `position_id`,
/// expected vs actual `balance`/`last_index` at `block_number`).
///
/// # Errors
///
/// Returns [`RunError`] on a DB or RPC failure that prevents verification: a
/// single position's revert-on-call is treated as `actual == 0` and surfaced as
/// a divergence if the DB expected non-zero, while a transport fault aborts the
/// verification instead of reporting every position as diverged.
pub async fn verify_touched_positions_on_conn(
    conn: &Connection,
    provider: &AlloyProvider,
    market_id: i64,
    block_number: u64,
    user_addresses: Option<&[Address]>,
) -> Result<Vec<PositionDivergence>, RunError> {
    let positions = load_verification_positions(conn, market_id, user_addresses)?;
    let anchor = resolve_anchor_at(provider, block_number).await?;
    let truth = acquire_position_truth(&BlockReads::at(provider, anchor), &positions).await?;
    Ok(compare_position_truth(&positions, &truth, block_number))
}

/// The verify pass's read side: every position row the comparison will judge,
/// collateral family first then debt, dead/zero-address users dropped (no call
/// is ever issued for them). This is the SQL half and issues no RPC, so a
/// caller can load it inside a write transaction while the chain reads happened
/// earlier.
///
/// # Errors
///
/// A row whose address, balance or `last_index` column does not parse refuses
/// the load (never a dropped row).
pub fn load_verification_positions(
    conn: &Connection,
    market_id: i64,
    user_addresses: Option<&[Address]>,
) -> Result<Vec<VerificationPosition>, DbError> {
    let mut positions: Vec<VerificationPosition> = Vec::new();
    for kind in [PositionKind::Collateral, PositionKind::Debt] {
        positions.extend(
            load_position_rows(conn, market_id, kind, user_addresses)?
                .into_iter()
                .filter(|row| row.user_address != DEAD_ADDRESS && row.user_address != ZERO_ADDRESS),
        );
    }
    Ok(positions)
}

/// The verify pass's chain half: one Multicall3 batch reading
/// `scaledBalanceOf` + `getPreviousIndex` for every position at
/// `block_number`. Pure RPC - no SQL - so it runs in resolution, outside the
/// chunk's write transaction, and its product is joined back by identity.
///
/// # Errors
///
/// A transport fault aborts the acquisition instead of reading every position
/// as zero.
pub async fn acquire_position_truth(
    reads: &BlockReads<'_>,
    positions: &[VerificationPosition],
) -> Result<Vec<PositionTruth>, RunError> {
    let mut truth: Vec<PositionTruth> = Vec::with_capacity(positions.len());
    for kind in [PositionKind::Collateral, PositionKind::Debt] {
        let rows: Vec<&VerificationPosition> =
            positions.iter().filter(|row| row.kind == kind).collect();
        if rows.is_empty() {
            continue;
        }
        // Flatten two calls per row — [scaledBalanceOf, getPreviousIndex] —
        // into one Multicall3 batch, then map the results back in stride.
        let mut calls: Vec<(Address, Bytes)> = Vec::with_capacity(rows.len() * 2);
        for row in &rows {
            calls.push((
                row.token_address,
                build_call_data(SCALED_BALANCE_OF_SELECTOR, row.user_address),
            ));
            calls.push((
                row.token_address,
                build_call_data(GET_PREVIOUS_INDEX_SELECTOR, row.user_address),
            ));
        }
        let results = reads.batch(&calls).await?;
        // The batch yields exactly one outcome per call, so pairing the
        // stride is exact and the remainder is structurally empty.
        let (result_pairs, _remainder) = results.as_chunks::<2>();
        for (row, chunk) in rows.iter().zip(result_pairs) {
            truth.push(PositionTruth {
                kind,
                position_id: row.position_id,
                balance: result_to_u256(&chunk[0]),
                last_index: result_to_u256(&chunk[1]),
            });
        }
    }
    Ok(truth)
}

/// The verify pass's comparison: the DB rows against the acquired truth. Pure -
/// no RPC - so it runs on the chunk's uncommitted transaction while the chain
/// reads finished in resolution.
///
/// A position the truth names but the rows no longer hold is a divergence: the
/// chain reported state the DB has no row for.
#[must_use]
pub fn compare_position_truth(
    positions: &[VerificationPosition],
    truth: &[PositionTruth],
    block_number: u64,
) -> Vec<PositionDivergence> {
    let mut divergences: Vec<PositionDivergence> = Vec::new();
    for truth_row in truth {
        let Some(row) = positions
            .iter()
            .find(|row| row.kind == truth_row.kind && row.position_id == truth_row.position_id)
        else {
            divergences.push(PositionDivergence {
                kind: truth_row.kind,
                position_id: truth_row.position_id,
                user_address: ZERO_ADDRESS,
                token_address: ZERO_ADDRESS,
                block_number,
                field: DivergenceField::Balance,
                expected: U256::ZERO,
                actual: truth_row.balance,
            });
            continue;
        };
        if truth_row.balance != row.balance {
            divergences.push(PositionDivergence {
                kind: row.kind,
                position_id: row.position_id,
                user_address: row.user_address,
                token_address: row.token_address,
                block_number,
                field: DivergenceField::Balance,
                expected: row.balance,
                actual: truth_row.balance,
            });
        }
        let expected_index = row.last_index.unwrap_or(U256::ZERO);
        if truth_row.last_index != expected_index {
            divergences.push(PositionDivergence {
                kind: row.kind,
                position_id: row.position_id,
                user_address: row.user_address,
                token_address: row.token_address,
                block_number,
                field: DivergenceField::LastIndex,
                expected: expected_index,
                actual: truth_row.last_index,
            });
        }
    }
    divergences
}

/// Map one sub-call outcome to a U256: a returned payload of at least 32 bytes
/// decodes to its big-endian word, while a revert or a short return is
/// `U256::ZERO` — this verifier's policy that there is nothing to compare
/// against, matching the prior per-call
/// `eth_call().ok().and_then(decode).unwrap_or(ZERO)` semantics.
///
/// A transport fault never reaches here: it has no per-position meaning, so the
/// batch propagates it and the verification aborts, instead of reading every
/// position as zero and reporting that the DB disagrees with chain on
/// everything. The read is a caller-declared `uint256` word rather than a named
/// getter's answer: Aave's `scaledBalanceOf` / `getPreviousIndex` /
/// `balanceOf` returns are all generic `uint256` words, so no getter's return
/// type describes them.
fn result_to_u256(result: &MulticallOutcome) -> U256 {
    match result {
        MulticallOutcome::Returned(bytes) if bytes.len() >= 32 => U256::from_be_slice(&bytes[..32]),
        MulticallOutcome::Returned(_) | MulticallOutcome::Reverted(_) => U256::ZERO,
    }
}

// ── stkAAVE balance + GHO discount verification ─────────────────────────

/// The GHO vToken revision at which the discount mechanism is deprecated
/// At revision ≥ 4, `getDiscountPercent` is removed; the verify skips.
const GHO_DISCOUNT_DEPRECATION_REVISION: i64 = 4;

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
            let strs: Vec<String> = addrs
                .iter()
                .map(|a| format!("{a:?}").to_lowercase())
                .collect();
            (format!(" AND LOWER(u.address) IN ({placeholders})"), strs)
        }
        _ => (String::new(), Vec::new()),
    }
}

/// The chain truth for one user's stkAAVE balance, joined by user id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StkAaveTruth {
    pub user_id: i64,
    pub balance: U256,
}

/// The chain truth for one user's GHO discount percent, joined by user id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GhoDiscountTruth {
    pub user_id: i64,
    pub discount: i64,
}

/// Every chain fact a market-wide verification compares: the acquisition
/// phase's whole product, keyed so the comparison phase can join it to the
/// rows it reads on the (possibly uncommitted) connection.
#[derive(Debug, Default, Clone)]
pub struct VerificationTruth {
    pub positions: Vec<PositionTruth>,
    pub stk_aave: Vec<StkAaveTruth>,
    pub gho_discount: Vec<GhoDiscountTruth>,
}

/// One row of the stkAAVE check's read side.
struct StkAaveRow {
    user_id: i64,
    user_address: Address,
    expected: U256,
}

/// One row of the GHO discount check's read side.
struct GhoDiscountRow {
    user_id: i64,
    user_address: Address,
    expected: i64,
}

/// The stkAAVE token the balance check reads, when the market carries one.
fn stk_aave_discount_token(conn: &Connection, chain_id: i64) -> Result<Option<Address>, DbError> {
    let Some(gho_asset) = DegenbotDb::fetch_aave_gho_asset_on_conn(conn, chain_id)? else {
        return Ok(None);
    };
    Ok(gho_asset
        .v_gho_discount_token
        .as_deref()
        .and_then(|token| token.parse::<Address>().ok()))
}

/// The GHO vToken's discount-check target, when the mechanism is live at the
/// market's recorded revision.
fn gho_discount_target(
    conn: &Connection,
    market_id: i64,
    chain_id: i64,
) -> Result<Option<(i64, Address)>, DbError> {
    let Some(gho_asset) = DegenbotDb::fetch_aave_gho_asset_on_conn(conn, chain_id)? else {
        return Ok(None);
    };
    let Some(v_token_id) = gho_asset.v_token_id else {
        return Ok(None);
    };
    let Some(v_token_address) = gho_asset
        .v_token_address
        .as_deref()
        .and_then(|token| token.parse::<Address>().ok())
    else {
        return Ok(None);
    };
    let revision: Option<i64> = conn
        .query_row(
            "SELECT a.v_token_revision FROM aave_v3_assets a WHERE a.market_id = ?1 AND a.v_token_id = ?2",
            rusqlite::params![market_id, v_token_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten();
    if revision.is_none_or(|r| r >= GHO_DISCOUNT_DEPRECATION_REVISION) {
        return Ok(None);
    }
    Ok(Some((v_token_id, v_token_address)))
}

fn load_stk_aave_rows(
    conn: &Connection,
    market_id: i64,
    user_addresses: Option<&[Address]>,
) -> Result<Vec<StkAaveRow>, DbError> {
    let (addr_clause, addr_strs) = user_address_clause(user_addresses);
    let sql = format!(
        "SELECT u.id, u.address, u.stk_aave_balance FROM aave_v3_users u WHERE u.market_id = ?1 AND u.stk_aave_balance IS NOT NULL{addr_clause}"
    );
    let mut stmt = conn.prepare(&sql).map_err(DbError::from)?;
    let mut bind_params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(1 + addr_strs.len());
    bind_params.push(&market_id);
    for s in &addr_strs {
        bind_params.push(s);
    }
    let iter = stmt
        .query_map(bind_params.as_slice(), |r| {
            let id: i64 = r.get(0)?;
            let addr: String = r.get(1)?;
            let bal: Option<String> = r.get(2)?;
            Ok((id, addr, bal))
        })
        .map_err(DbError::from)?;
    let mut rows = Vec::new();
    for row in iter {
        let (user_id, user_addr_str, bal_str) = row.map_err(DbError::from)?;
        let Ok(user_address) = user_addr_str.parse() else {
            continue;
        };
        let expected = bal_str
            .as_deref()
            .and_then(|s| parse_u256(s).ok())
            .unwrap_or(U256::ZERO);
        rows.push(StkAaveRow {
            user_id,
            user_address,
            expected,
        });
    }
    Ok(rows)
}

fn load_gho_discount_rows(
    conn: &Connection,
    market_id: i64,
    v_token_id: i64,
    user_addresses: Option<&[Address]>,
) -> Result<Vec<GhoDiscountRow>, DbError> {
    let (addr_clause, addr_strs) = user_address_clause(user_addresses);
    let sql = format!(
        "SELECT DISTINCT u.id, u.address, u.gho_discount FROM aave_v3_users u JOIN aave_v3_debt_positions dp ON dp.user_id = u.id JOIN aave_v3_assets a ON a.id = dp.asset_id WHERE a.market_id = ?1 AND a.v_token_id = ?2{addr_clause}"
    );
    let mut stmt = conn.prepare(&sql).map_err(DbError::from)?;
    let mut bind_params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(2 + addr_strs.len());
    bind_params.push(&market_id);
    bind_params.push(&v_token_id);
    for s in &addr_strs {
        bind_params.push(s);
    }
    let iter = stmt
        .query_map(bind_params.as_slice(), |r| {
            let id: i64 = r.get(0)?;
            let addr: String = r.get(1)?;
            let discount: i64 = r.get(2)?;
            Ok((id, addr, discount))
        })
        .map_err(DbError::from)?;
    let mut rows = Vec::new();
    for row in iter {
        let (user_id, user_addr_str, expected) = row.map_err(DbError::from)?;
        let Ok(user_address) = user_addr_str.parse() else {
            continue;
        };
        rows.push(GhoDiscountRow {
            user_id,
            user_address,
            expected,
        });
    }
    Ok(rows)
}

/// Acquire every chain fact the market-wide verification compares, in ONE
/// resolution phase: no SQL write, no transaction. The reads are batched
/// (Multicall3 for the scaled-token and discount checks, one `balanceOf` batch
/// for stkAAVE) exactly as the single-shot verifier issued them.
///
/// # Errors
///
/// A transport fault aborts the acquisition rather than reading every fact as
/// zero.
pub async fn acquire_verification_truth(
    conn: &Connection,
    reads: &BlockReads<'_>,
    market_id: i64,
    chain_id: i64,
    user_addresses: Option<&[Address]>,
) -> Result<VerificationTruth, RunError> {
    let positions = load_verification_positions(conn, market_id, user_addresses)?;
    let positions = acquire_position_truth(reads, &positions).await?;

    let stk_aave = match stk_aave_discount_token(conn, chain_id)? {
        Some(token_address) => {
            let rows = load_stk_aave_rows(conn, market_id, user_addresses)?;
            if rows.is_empty() {
                Vec::new()
            } else {
                let calls: Vec<(Address, Bytes)> = rows
                    .iter()
                    .map(|row| {
                        (
                            token_address,
                            build_call_data(BALANCE_OF_SELECTOR, row.user_address),
                        )
                    })
                    .collect();
                let results = reads.batch(&calls).await?;
                rows.iter()
                    .zip(results.iter())
                    .map(|(row, result)| StkAaveTruth {
                        user_id: row.user_id,
                        balance: result_to_u256(result),
                    })
                    .collect()
            }
        }
        None => Vec::new(),
    };

    let gho_discount = match gho_discount_target(conn, market_id, chain_id)? {
        Some((v_token_id, v_token_address)) => {
            let rows = load_gho_discount_rows(conn, market_id, v_token_id, user_addresses)?;
            if rows.is_empty() {
                Vec::new()
            } else {
                let calls: Vec<(Address, Bytes)> = rows
                    .iter()
                    .map(|row| {
                        (
                            v_token_address,
                            build_call_data(GET_DISCOUNT_PERCENT_SELECTOR, row.user_address),
                        )
                    })
                    .collect();
                let results = reads.batch(&calls).await?;
                rows.iter()
                    .zip(results.iter())
                    .map(|(row, result)| GhoDiscountTruth {
                        user_id: row.user_id,
                        discount: result_to_u256(result).try_into().unwrap_or(i64::MAX),
                    })
                    .collect()
            }
        }
        None => Vec::new(),
    };

    Ok(VerificationTruth {
        positions,
        stk_aave,
        gho_discount,
    })
}

/// Compare the acquired truth against the rows the connection holds now. Pure:
/// no RPC, so it runs on the chunk's uncommitted transaction.
///
/// # Errors
///
/// Returns [`RunError`] when a row the comparison needs does not parse.
pub fn compare_verification_truth_on_conn(
    conn: &Connection,
    market_id: i64,
    chain_id: i64,
    block_number: u64,
    user_addresses: Option<&[Address]>,
    truth: &VerificationTruth,
) -> Result<Vec<VerificationDivergence>, RunError> {
    let mut divergences = Vec::new();

    let positions = load_verification_positions(conn, market_id, user_addresses)?;
    for divergence in compare_position_truth(&positions, &truth.positions, block_number) {
        divergences.push(VerificationDivergence::ScaledToken(divergence));
    }

    if let Some(token_address) = stk_aave_discount_token(conn, chain_id)? {
        for row in &load_stk_aave_rows(conn, market_id, user_addresses)? {
            let Some(truth_row) = truth
                .stk_aave
                .iter()
                .find(|entry| entry.user_id == row.user_id)
            else {
                continue;
            };
            if truth_row.balance != row.expected {
                divergences.push(VerificationDivergence::StkAaveBalance {
                    user_id: row.user_id,
                    user_address: row.user_address,
                    token_address,
                    block_number,
                    expected: row.expected,
                    actual: truth_row.balance,
                });
            }
        }
    }

    if let Some((v_token_id, v_token_address)) = gho_discount_target(conn, market_id, chain_id)? {
        for row in &load_gho_discount_rows(conn, market_id, v_token_id, user_addresses)? {
            let Some(truth_row) = truth
                .gho_discount
                .iter()
                .find(|entry| entry.user_id == row.user_id)
            else {
                continue;
            };
            if truth_row.discount != row.expected {
                divergences.push(VerificationDivergence::GhoDiscount {
                    user_id: row.user_id,
                    user_address: row.user_address,
                    token_address: v_token_address,
                    block_number,
                    expected: row.expected,
                    actual: truth_row.discount,
                });
            }
        }
    }

    Ok(divergences)
}

/// Run all 4 verification checks (scaled-token collateral + debt, stkAAVE
/// balance, GHO discount) and return a unified divergence list.
///
/// When `user_addresses` is `None`, verifies ALL positions/users in the market
/// (the post-run `--verify-all` path). When `Some`, verifies only the
/// specified users (the per-chunk path).
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
    let anchor = resolve_anchor_at(provider, block_number).await?;
    let truth = acquire_verification_truth(
        conn,
        &BlockReads::at(provider, anchor),
        market_id,
        chain_id,
        user_addresses,
    )
    .await?;
    compare_verification_truth_on_conn(
        conn,
        market_id,
        chain_id,
        block_number,
        user_addresses,
        &truth,
    )
}

/// Delete all zero-balance collateral + debt positions for `market_id`.
/// Delete every zero-balance collateral + debt row for the market. A WRITE
/// operation — must be called on a writable
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

// ── verification-policy predicates (pure logic, no RPC) ───────────────

/// Whether a full (market-wide) verification should run at a block-number
/// interval boundary for the chunk `[working_start, chunk_end]`.
///
/// Fires when `interval` is `Some(n)` and the chunk CROSSES or LANDS-ON a
/// multiple of `n`: i.e. the multiple-of-`n` boundary in `[working_start,
/// chunk_end]` — equivalently `floor((working_start - 1) / n) !=
/// floor(chunk_end / n)`. A chunk larger than `n` (`chunk_size` > n) still
/// fires because it spans a multiple. `interval = None` never fires.
///
/// Edge cases: `chunk_end == 0` → false (no block processed); `n == 0` is
/// the caller's bug (the CLI clamps it to >= 1) but is treated as never-fires
/// to avoid a divide-by-zero panic.
pub(crate) fn should_run_full_verify_at_interval(
    working_start: u64,
    chunk_end: u64,
    interval: Option<u64>,
) -> bool {
    let Some(n) = interval else {
        return false;
    };
    if n == 0 {
        return false;
    }
    if chunk_end == 0 {
        return false;
    }
    let before = working_start.saturating_sub(1) / n;
    let after = chunk_end / n;
    before != after
}

/// Whether `chunk_end` is the run's final block: `chunk_end >= last_block`
/// OR `max_chunks_hit` (the loop broke because `max_chunks` was reached).
pub(crate) fn is_final_chunk(chunk_end: u64, last_block: u64, max_chunks_hit: bool) -> bool {
    max_chunks_hit || chunk_end >= last_block
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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
        // asset 1: underlying_asset_id=2, a_token_id=1 (the verify JOINs on
        // `a_token_id` → the erc20 row inserted above), v_token_id=2 (present
        // for parity with the full schema; the collateral verify never reads
        // it).
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

    /// A dead transport aborts the verification with the provider's own error:
    /// it must never read every position as zero and report that the DB
    /// disagrees with chain on every stored row.
    #[test]
    fn dead_transport_aborts_verification_instead_of_fabricating_divergences() {
        let conn = seed_with_position(
            "0x1111111111111111111111111111111111111111",
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "123456789",
            Some("1000000000000000000000000000"),
        );
        // A recorded chain with no recorded calls: every eth_call misses, which
        // the provider surfaces as a non-revert RPC error.
        let json = serde_json::json!({
            "chain_id": 1,
            "block_number": 100,
            "timestamp": 0,
            "calls": {},
            "code": {}
        });
        let provider = degenbot_rpc::offline::OfflineProvider::from_json_str(&json.to_string())
            .unwrap()
            .as_alloy_provider();

        let result = degenbot_core::runtime::get_runtime().block_on(
            verify_touched_positions_on_conn(&conn, &provider, 1, 100, None),
        );

        // The terminal anchor resolves BEFORE any gate read, so a dead
        // transport aborts here — still an abort, never a fabricated fan of
        // zero-divergence rows.
        assert!(
            matches!(
                result,
                Err(RunError::TerminalAnchor(_) | RunError::Provider(_))
            ),
            "an outage must abort verification instead of fabricating divergences, got {result:?}"
        );
    }

    /// A stored row whose columns don't parse refuses the whole load. Skipping
    /// it would verify a position set the DB does not hold and report the
    /// dropped position as green.
    #[test]
    fn load_position_rows_refuses_an_unparseable_row_naming_it() {
        let conn = seed_with_position(
            "0x1111111111111111111111111111111111111111",
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "1234x56789",
            Some("1000000000000000000000000000"),
        );
        let err = load_position_rows(&conn, 1, PositionKind::Collateral, None)
            .expect_err("an unparseable balance must refuse the load, not drop the row");
        let DbError::Decode(message) = &err else {
            panic!("expected a typed decode error, got {err:?}");
        };
        for named in [
            "aave_v3_collateral_positions",
            "id=1",
            "balance",
            "1234x56789",
        ] {
            assert!(
                message.contains(named),
                "the refusal names {named}: {message}"
            );
        }

        conn.execute(
            "UPDATE aave_v3_collateral_positions SET balance = '42', last_index = 'not-an-index' WHERE id = 1",
            [],
        )
        .unwrap();
        let err = load_position_rows(&conn, 1, PositionKind::Collateral, None)
            .expect_err("an unparseable last_index must refuse the load too");
        let DbError::Decode(message) = &err else {
            panic!("expected a typed decode error, got {err:?}");
        };
        assert!(
            message.contains("last_index") && message.contains("not-an-index"),
            "the refusal names the column and the stored value: {message}"
        );
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

    /// Decode is delegated to the home (`degenbot_rpc::abi::decode_uint256`);
    /// the home carries the independent-oracle test. Kept below: the
    /// calldata-encode test for `scaledBalanceOf(address)` /
    /// `getPreviousIndex(address)`.

    // ── verification-policy predicates ─────────────────────────────────

    #[test]
    fn interval_verify_none_interval_never_fires() {
        assert!(!should_run_full_verify_at_interval(1, 999, None));
        assert!(!should_run_full_verify_at_interval(1, 1_000_000, None));
    }

    #[test]
    fn interval_verify_exact_multiple_lands() {
        assert!(should_run_full_verify_at_interval(
            1,
            1_000_000,
            Some(1_000_000)
        ));
        assert!(should_run_full_verify_at_interval(
            990_001,
            1_000_000,
            Some(1_000_000)
        ));
    }

    #[test]
    fn interval_verify_chunk_crosses_multiple() {
        assert!(should_run_full_verify_at_interval(
            999_999,
            1_000_001,
            Some(1_000_000)
        ));
        assert!(should_run_full_verify_at_interval(
            500_000,
            1_500_000,
            Some(1_000_000)
        ));
    }

    #[test]
    fn interval_verify_sub_interval_no_fire() {
        assert!(!should_run_full_verify_at_interval(
            1,
            999_999,
            Some(1_000_000)
        ));
        assert!(!should_run_full_verify_at_interval(
            100,
            200,
            Some(1_000_000)
        ));
        assert!(!should_run_full_verify_at_interval(
            1,
            5_000,
            Some(1_000_000)
        ));
    }

    #[test]
    fn interval_verify_n_one_fires_every_chunk() {
        assert!(should_run_full_verify_at_interval(1, 1, Some(1)));
        assert!(should_run_full_verify_at_interval(100, 200, Some(1)));
    }

    #[test]
    fn interval_verify_chunk_end_zero_does_not_fire() {
        assert!(!should_run_full_verify_at_interval(0, 0, Some(1_000_000)));
        assert!(!should_run_full_verify_at_interval(1, 0, Some(1_000_000)));
    }

    #[test]
    fn interval_verify_n_zero_does_not_panic() {
        assert!(!should_run_full_verify_at_interval(1, 100, Some(0)));
    }

    #[test]
    fn is_final_chunk_true_when_last_block_reached() {
        assert!(is_final_chunk(1_000_000, 1_000_000, false));
        assert!(is_final_chunk(1_000_005, 1_000_000, false));
    }

    #[test]
    fn is_final_chunk_true_when_max_chunks_hit() {
        assert!(is_final_chunk(50_000, 1_000_000, true));
    }

    #[test]
    fn is_final_chunk_false_mid_run() {
        assert!(!is_final_chunk(10_000, 1_000_000, false));
        assert!(!is_final_chunk(990_000, 1_000_000, false));
    }

    /// The seed plus the empty GHO row the stkAAVE/discount checks read (both
    /// no-op without one; the comparison must still be pure).
    fn seed_with_position_and_gho_row(
        user_address: &str,
        a_token_address: &str,
        balance: &str,
        last_index: Option<&str>,
    ) -> Connection {
        let conn = seed_with_position(user_address, a_token_address, balance, last_index);
        conn.execute_batch(
            "CREATE TABLE aave_gho_tokens (id INTEGER NOT NULL PRIMARY KEY, chain_id INTEGER NOT NULL, token_id INTEGER NOT NULL, v_token_id INTEGER, v_gho_discount_rate_strategy VARCHAR(42), v_gho_discount_token VARCHAR(42));",
        )
        .unwrap();
        conn
    }

    /// The comparison phase is the only part of a verification-enabled chunk
    /// that runs inside the write transaction: it must be pure. A matching
    /// truth produces no divergence and, critically, no provider round trip.
    #[test]
    fn compare_verification_truth_is_rpc_free_and_accepts_matching_truth() {
        let conn = seed_with_position_and_gho_row(
            "0x1111111111111111111111111111111111111111",
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "123456789",
            Some("1000000000000000000000000000"),
        );
        let truth = VerificationTruth {
            positions: vec![PositionTruth {
                kind: PositionKind::Collateral,
                position_id: 1,
                balance: U256::from(123_456_789_u64),
                last_index: U256::from(10u64).pow(U256::from(27u64)),
            }],
            ..VerificationTruth::default()
        };

        let before = degenbot_rpc::provider::rpc_round_trips();
        let divergences =
            compare_verification_truth_on_conn(&conn, 1, 1, 100, None, &truth).unwrap();
        let after = degenbot_rpc::provider::rpc_round_trips();

        assert!(
            divergences.is_empty(),
            "the chain truth agrees with the row: {divergences:?}"
        );
        assert_eq!(
            after, before,
            "the comparison phase must issue no provider round trip - it runs inside the \
             chunk's write transaction"
        );
    }

    /// A disagreement between the acquired truth and the (uncommitted) row is
    /// the divergence the gate rolls the chunk back on, and it is still
    /// detected with no provider access at all.
    #[test]
    fn compare_verification_truth_names_a_mismatch_without_a_provider_round_trip() {
        let conn = seed_with_position_and_gho_row(
            "0x1111111111111111111111111111111111111111",
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "123456789",
            Some("1000000000000000000000000000"),
        );
        let truth = VerificationTruth {
            positions: vec![PositionTruth {
                kind: PositionKind::Collateral,
                position_id: 1,
                balance: U256::from(1u64),
                last_index: U256::from(10u64).pow(U256::from(27u64)),
            }],
            ..VerificationTruth::default()
        };

        let before = degenbot_rpc::provider::rpc_round_trips();
        let divergences =
            compare_verification_truth_on_conn(&conn, 1, 1, 100, None, &truth).unwrap();
        let after = degenbot_rpc::provider::rpc_round_trips();

        assert_eq!(
            after, before,
            "no provider round trip in the comparison phase"
        );
        assert_eq!(
            divergences.len(),
            1,
            "one balance divergence: {divergences:?}"
        );
        let VerificationDivergence::ScaledToken(divergence) = &divergences[0] else {
            panic!("expected a scaled-token divergence");
        };
        assert_eq!(divergence.position_id, 1);
        assert_eq!(divergence.expected, U256::from(123_456_789_u64));
        assert_eq!(divergence.actual, U256::from(1u64));
    }
}

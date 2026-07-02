//! Cross-implementation Aave V3 read-back parity (binding #4 — HARD gate).
//!
//! Opens the frozen Alembic-stamped `fixtures/aave_parity.db` (built by
//! `fixtures/generate_aave_parity.py`) and asserts the Rust Aave read fns
//! produce results identical to the Python `DatabasePositionQuery` oracle
//! (recorded in `fixtures/aave_parity_expected.json`), including the
//! `VARCHAR(78)` ↔ `U256` boundary (debt/balance/index values exceeding
//! `i64::MAX`) and the isolation-debt-ceiling + eMode joins.
//!
//! To regenerate the fixture:
//!   `uv run python rust/crates/degenbot-db/tests/fixtures/generate_aave_parity.py`

use std::collections::HashMap;
use std::path::PathBuf;

use alloy::primitives::U256;
use serde::Deserialize;

use degenbot_db::{DegenbotDb, SchemaState};

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture_db_path() -> PathBuf {
    PathBuf::from(FIXTURE_DIR).join("aave_parity.db")
}

fn fixture_expected() -> Expected {
    let path = PathBuf::from(FIXTURE_DIR).join("aave_parity_expected.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("aave_parity_expected.json is well-formed")
}

fn open_db() -> DegenbotDb {
    let (db, state) =
        DegenbotDb::open(&fixture_db_path()).expect("open aave_parity.db fixture");
    assert!(
        matches!(state, SchemaState::AlembicCurrent),
        "fixture DB should be AlembicCurrent, got {state:?}"
    );
    db
}

fn parse_u256(s: &str) -> U256 {
    U256::from_str_radix(s.trim(), 10).unwrap_or_else(|e| panic!("bad u256 {s:?}: {e}"))
}

#[derive(Debug, Deserialize)]
struct Expected {
    #[allow(dead_code)]
    chain_id: i64,
    market_id: i64,
    users: Vec<UserJson>,
    oracle_address: Option<String>,
    asset_addresses: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct UserJson {
    id: i64,
    address: String,
    market_id: i64,
    e_mode: i64,
    is_isolation_mode: bool,
    isolation_mode_debt: String,
    isolation_debt_ceiling: Option<String>,
    collateral: Vec<CollateralJson>,
    debt: Vec<DebtJson>,
    collateral_config_map: HashMap<String, bool>,
}

#[derive(Debug, Deserialize)]
struct CollateralJson {
    asset_id: i64,
    balance: String,
    underlying_address: String,
    underlying_symbol: Option<String>,
    liquidity_index: String,
    e_mode_category_id: Option<i64>,
    asset_lt: i64,
    asset_ltv: i64,
    emode_lt: Option<i64>,
    emode_ltv: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct DebtJson {
    asset_id: i64,
    balance: String,
    underlying_address: String,
    underlying_symbol: Option<String>,
    borrow_index: String,
    e_mode_category_id: Option<i64>,
}

#[test]
fn fetch_users_with_debt_matches_python() {
    let db = open_db();
    let expected = fixture_expected();
    let users = db
        .fetch_aave_users_with_debt(expected.market_id, None)
        .expect("fetch users");
    assert_eq!(users.len(), expected.users.len(), "user count");
    for (got, exp) in users.iter().zip(&expected.users) {
        assert_eq!(got.id, exp.id);
        assert_eq!(got.address, exp.address);
        assert_eq!(got.market_id, exp.market_id);
        assert_eq!(got.e_mode, exp.e_mode);
        assert_eq!(got.is_isolation_mode, exp.is_isolation_mode);
        assert_eq!(got.isolation_mode_debt, parse_u256(&exp.isolation_mode_debt));
        assert_eq!(
            got.isolation_debt_ceiling,
            exp.isolation_debt_ceiling.as_deref().map(parse_u256),
            "isolation_debt_ceiling for user {}", got.id
        );
    }
}

#[test]
fn fetch_users_with_debt_filters_users_without_debt() {
    // User 3 (collateral only, no debt) must NOT appear. Already covered by the
    // count comparison above, but assert by id explicitly.
    let db = open_db();
    let expected = fixture_expected();
    let users = db
        .fetch_aave_users_with_debt(expected.market_id, None)
        .expect("fetch users");
    let ids: Vec<i64> = users.iter().map(|u| u.id).collect();
    assert!(!ids.contains(&3), "user 3 (no debt) leaked through: {ids:?}");
}

#[test]
fn fetch_collateral_positions_matches_python() {
    let db = open_db();
    let expected = fixture_expected();
    for u in &expected.users {
        let rows = db
            .fetch_aave_collateral_positions(u.id)
            .expect("fetch collateral");
        assert_eq!(rows.len(), u.collateral.len(), "collateral count user {}", u.id);
        for (got, exp) in rows.iter().zip(&u.collateral) {
            assert_eq!(got.asset_id, exp.asset_id);
            assert_eq!(got.balance, parse_u256(&exp.balance));
            assert_eq!(got.underlying_address, exp.underlying_address);
            assert_eq!(got.underlying_symbol, exp.underlying_symbol);
            assert_eq!(got.liquidity_index, parse_u256(&exp.liquidity_index));
            assert_eq!(got.e_mode_category_id, exp.e_mode_category_id);
            assert_eq!(got.asset_lt, exp.asset_lt);
            assert_eq!(got.asset_ltv, exp.asset_ltv);
            assert_eq!(got.emode_lt, exp.emode_lt);
            assert_eq!(got.emode_ltv, exp.emode_ltv);
        }
    }
}

#[test]
fn fetch_debt_positions_matches_python() {
    let db = open_db();
    let expected = fixture_expected();
    for u in &expected.users {
        let rows = db.fetch_aave_debt_positions(u.id).expect("fetch debt");
        assert_eq!(rows.len(), u.debt.len(), "debt count user {}", u.id);
        for (got, exp) in rows.iter().zip(&u.debt) {
            assert_eq!(got.asset_id, exp.asset_id);
            assert_eq!(got.balance, parse_u256(&exp.balance));
            assert_eq!(got.underlying_address, exp.underlying_address);
            assert_eq!(got.underlying_symbol, exp.underlying_symbol);
            assert_eq!(got.borrow_index, parse_u256(&exp.borrow_index));
            assert_eq!(got.e_mode_category_id, exp.e_mode_category_id);
        }
    }
}

#[test]
fn fetch_collateral_config_map_matches_python() {
    let db = open_db();
    let expected = fixture_expected();
    for u in &expected.users {
        let map = db
            .fetch_aave_collateral_config_map(u.id)
            .expect("fetch collateral config map");
        let exp: HashMap<i64, bool> = u
            .collateral_config_map
            .iter()
            .map(|(k, v)| (k.parse::<i64>().unwrap(), *v))
            .collect();
        assert_eq!(map, exp, "config map user {}", u.id);
    }
}

#[test]
fn fetch_oracle_address_matches_python() {
    let db = open_db();
    let expected = fixture_expected();
    let addr = db
        .fetch_aave_oracle_address(expected.market_id)
        .expect("fetch oracle address");
    assert_eq!(addr, expected.oracle_address);
}

#[test]
fn fetch_asset_addresses_matches_python() {
    let db = open_db();
    let expected = fixture_expected();
    let mut addrs = db
        .fetch_aave_asset_addresses(expected.market_id)
        .expect("fetch asset addresses");
    addrs.sort();
    assert_eq!(addrs, expected.asset_addresses);
}

#[test]
fn fetch_users_with_debt_limit_sliced() {
    let db = open_db();
    let expected = fixture_expected();
    let all = db
        .fetch_aave_users_with_debt(expected.market_id, None)
        .unwrap();
    let limited = db
        .fetch_aave_users_with_debt(expected.market_id, Some(1))
        .unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].id, all[0].id);
}
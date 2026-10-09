//! The market-reset module through its real interface: a seeded database plus
//! the cassette transport for the activation RPC, so
//! [`reset_market_to_cold_boot`] runs purge + activate in ONE transaction and
//! reports the facts it read back.
//!
//! The fixtures mirror `aave_market_reset.rs`'s seeded market; the cassette
//! answers the activation RPC (the head block, `getMarketId()`, and the GHO
//! token metadata) that C6.1's provider injection made replayable.

#![expect(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fmt::Write as _;

use degenbot_aave::updater::{reset_market_to_cold_boot, RunError};
use degenbot_aave::ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK;
use degenbot_db::DegenbotDb;
use degenbot_rpc::cassette::{
    entry_digest, entry_key, Cassette, CassetteEntry, CassetteProvenance, CassetteResponse,
    CassetteSpan,
};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use rusqlite::params;

const CHAIN_ID: i64 = 1;
const MARKET_NAME: &str = "Aave Ethereum Market";
const POOL_ADDRESS_PROVIDER: &str = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
const GHO: &str = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";
const POOL: &str = "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2";
const CONFIGURATOR: &str = "0x64b761D848206f447Fe2dd461b0c635Ec39EbB27";

/// The warm cursor the populated fixture carries before a reset.
const WARM_CURSOR: i64 = 26_130_445;

/// The head block the activation metadata reads pin.
const HEAD: u64 = 16_291_071;

/// The no-arg selectors the activation RPC issues.
const GET_MARKET_ID_SELECTOR: &str = "0x568ef470";
const NAME_SELECTOR: &str = "0x06fdde03";
const SYMBOL_SELECTOR: &str = "0x95d89b41";
const DECIMALS_SELECTOR: &str = "0x313ce567";

/// The `(chain_id, name, active, cursor)` shape one reset leaves.
fn market_row(db: &DegenbotDb, market_id: i64) -> (String, bool, Option<i64>) {
    let conn = db.lock();
    conn.query_row(
        "SELECT name, active, last_update_block FROM aave_v3_markets WHERE id = ?1",
        params![market_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}

/// A market with the substrate `aave activate` seeds plus everything an update
/// run populated: extra contract rows, a reserve, a user, both position kinds,
/// and a warm cursor. Mirrors `aave_market_reset.rs`'s `populated` fixture.
fn populated() -> (DegenbotDb, i64) {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let market_id = {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (chain_id, name, active, last_update_block) \
             VALUES (?1, ?2, 1, ?3)",
            params![CHAIN_ID, MARKET_NAME, ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK],
        )
        .unwrap();
        let market_id = conn.last_insert_rowid();
        for (name, address) in [
            ("POOL_ADDRESS_PROVIDER", POOL_ADDRESS_PROVIDER),
            ("POOL", POOL),
            ("POOL_CONFIGURATOR", CONFIGURATOR),
        ] {
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &conn,
                market_id,
                name,
                address,
                Some(1),
            )
            .unwrap();
        }
        let underlying = DegenbotDb::get_or_create_erc20_token_on_conn(
            &conn,
            CHAIN_ID,
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            None,
            None,
            None,
        )
        .unwrap();
        let a_token = DegenbotDb::get_or_create_erc20_token_on_conn(
            &conn,
            CHAIN_ID,
            "0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c",
            None,
            None,
            None,
        )
        .unwrap();
        let v_token = DegenbotDb::get_or_create_erc20_token_on_conn(
            &conn,
            CHAIN_ID,
            "0x72E95b8931767C79bA4EeE721354d6E99a61D004",
            None,
            None,
            None,
        )
        .unwrap();
        let asset_id = DegenbotDb::apply_reserve_initialized_on_conn(
            &conn, market_id, underlying, a_token, 1, v_token, 1, None, None,
        )
        .unwrap();
        let user_id = {
            conn.execute(
                "INSERT INTO aave_v3_users \
                 (market_id, address, e_mode, gho_discount, stk_aave_balance, \
                  isolation_mode_collateral_asset_id, isolation_mode_debt) \
                 VALUES (?1, '0xuser', 0, 0, NULL, NULL, '0')",
                params![market_id],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        conn.execute(
            "INSERT INTO aave_v3_collateral_positions (user_id, asset_id, balance, last_index) \
             VALUES (?1, ?2, '10', '1')",
            params![user_id, asset_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_debt_positions (user_id, asset_id, balance, last_index) \
             VALUES (?1, ?2, '5', '1')",
            params![user_id, asset_id],
        )
        .unwrap();
        DegenbotDb::get_or_create_erc20_token_on_conn(
            &conn,
            CHAIN_ID,
            GHO,
            Some("Gho Token"),
            Some("GHO"),
            Some(18),
        )
        .unwrap();
        DegenbotDb::get_or_create_gho_token_on_conn(&conn, CHAIN_ID, GHO).unwrap();
        conn.execute(
            "UPDATE aave_gho_tokens SET v_token_id = ?1",
            params![v_token],
        )
        .unwrap();
        DegenbotDb::set_market_last_update_block_on_conn(&conn, market_id, WARM_CURSOR).unwrap();
        market_id
    };
    (db, market_id)
}

/// ABI-encode a dynamic `string` return: the 0x20 offset word, the byte-length
/// word, then the bytes right-padded to a 32-byte multiple.
fn abi_string(value: &str) -> String {
    let bytes = value.as_bytes();
    let len = bytes.len();
    let mut padded = bytes.to_vec();
    padded.resize(len + (32 - len % 32) % 32, 0);
    let mut out = format!("0x{:064x}{len:064x}", 32);
    for byte in &padded {
        write!(out, "{byte:02x}").unwrap();
    }
    out
}

/// The activation RPC over a cassette: the head block, `getMarketId()` naming
/// `market_name`, and the GHO token's `name()` / `symbol()` / `decimals()`.
fn activation_cassette(market_name: &str) -> CassetteReplayTransport {
    let mut entries = BTreeMap::new();
    let mut add = |method: &str, params: serde_json::Value, result: serde_json::Value| {
        let response = CassetteResponse::Success { result };
        entries.insert(
            entry_key(method, &params),
            CassetteEntry {
                digest: entry_digest(&response),
                response,
            },
        );
    };

    // `eth_blockNumber` carries no params — the ledger key pairs the method
    // with `null`.
    add(
        "eth_blockNumber",
        serde_json::Value::Null,
        serde_json::json!(format!("0x{HEAD:x}")),
    );

    // `getMarketId()` rides the provider's default block tag (alloy's
    // `Provider::call` pins `pending`).
    add(
        "eth_call",
        serde_json::json!([
            {"input": GET_MARKET_ID_SELECTOR, "to": POOL_ADDRESS_PROVIDER.to_lowercase()},
            "pending"
        ]),
        serde_json::json!(abi_string(market_name)),
    );

    let block = HEAD.to_string();
    add(
        "eth_call",
        serde_json::json!([{"input": NAME_SELECTOR, "to": GHO.to_lowercase()}, block]),
        serde_json::json!(abi_string("Gho Token")),
    );
    add(
        "eth_call",
        serde_json::json!([{"input": SYMBOL_SELECTOR, "to": GHO.to_lowercase()}, block]),
        serde_json::json!(abi_string("GHO")),
    );
    add(
        "eth_call",
        serde_json::json!([{"input": DECIMALS_SELECTOR, "to": GHO.to_lowercase()}, block]),
        serde_json::json!(format!("0x{:064x}", 18)),
    );

    CassetteReplayTransport::new(Cassette {
        schema: degenbot_rpc::cassette::CASSETTE_SCHEMA_V1.to_string(),
        chain_id: 1,
        provenance: CassetteProvenance {
            source: "test".to_string(),
            recorded_at: "1970-01-01T00:00:00Z".to_string(),
            span: CassetteSpan {
                from_block: HEAD,
                to_block: HEAD,
            },
        },
        entries,
    })
}

#[test]
fn reset_reports_read_back_facts_and_seeds_the_cold_boot_substrate() {
    let (db, market_id) = populated();

    // Precondition: the warm market the reset must clear.
    {
        let conn = db.lock();
        let users: i64 = conn
            .query_row("SELECT COUNT(*) FROM aave_v3_users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(users, 1);
    }

    let transport = activation_cassette(MARKET_NAME);
    let provider = transport.as_alloy_provider();

    let facts = {
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        let facts =
            reset_market_to_cold_boot(&tx, market_id, POOL_ADDRESS_PROVIDER, GHO, &provider)
                .unwrap();
        tx.commit().unwrap();
        facts
    };

    let bootstrap = u64::try_from(ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK).unwrap();
    assert!(facts.market_activated);
    assert!(facts.contract_row_present);
    assert_eq!(facts.last_update_block, Some(bootstrap));

    // Each fact matches an independent read of the committed transaction.
    let (name, active, stamp) = market_row(&db, market_id);
    assert_eq!(name, MARKET_NAME);
    assert!(active);
    assert_eq!(stamp, Some(ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK));
    assert_eq!(facts.market_activated, active);
    assert_eq!(
        facts.last_update_block,
        stamp.map(|v| u64::try_from(v).unwrap())
    );

    let conn = db.lock();
    let ap_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM aave_v3_contracts \
             WHERE market_id = ?1 AND name = 'POOL_ADDRESS_PROVIDER'",
            params![market_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ap_rows, 1);
    assert_eq!(facts.contract_row_present, ap_rows == 1);

    // The purged rows are gone; the remaining contracts are re-resolved by the
    // next cold boot.
    let users: i64 = conn
        .query_row("SELECT COUNT(*) FROM aave_v3_users", [], |r| r.get(0))
        .unwrap();
    assert_eq!(users, 0);
    for name in ["POOL", "POOL_CONFIGURATOR"] {
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM aave_v3_contracts WHERE market_id = ?1 AND name = ?2",
                params![market_id, name],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0, "{name} is re-resolved by the cold boot");
    }
}

#[test]
fn reset_completes_a_bare_inactive_market_and_reports_it() {
    // A pre-registered market is an INACTIVE, BARE row; the reset completes it
    // (the row flips active and gains the substrate), and the facts read that
    // completion back.
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let market_id = db.register_aave_market(CHAIN_ID, MARKET_NAME).unwrap().0;
    {
        let conn = db.lock();
        let active: bool = conn
            .query_row(
                "SELECT active FROM aave_v3_markets WHERE id = ?1",
                params![market_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!active, "pre-registered rows seed inactive");
    }

    let transport = activation_cassette(MARKET_NAME);
    let provider = transport.as_alloy_provider();

    let facts = {
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        let facts =
            reset_market_to_cold_boot(&tx, market_id, POOL_ADDRESS_PROVIDER, GHO, &provider)
                .unwrap();
        tx.commit().unwrap();
        facts
    };

    assert!(facts.market_activated);
    assert!(facts.contract_row_present);
    assert_eq!(
        facts.last_update_block,
        Some(u64::try_from(ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK).unwrap())
    );
}

#[test]
fn reset_refuses_and_rolls_back_when_the_onchain_name_names_another_row() {
    let (db, market_id) = populated();
    let transport = activation_cassette("Some Other Market");
    let provider = transport.as_alloy_provider();

    let result = {
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        // Drop the transaction without committing: the rollback under test.
        reset_market_to_cold_boot(&tx, market_id, POOL_ADDRESS_PROVIDER, GHO, &provider)
    };

    assert!(matches!(result, Err(RunError::Db(_))), "got {result:?}");

    let conn = db.lock();
    let markets: i64 = conn
        .query_row("SELECT COUNT(*) FROM aave_v3_markets", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        markets, 1,
        "the refused re-seed must not insert a sibling row"
    );
    let users: i64 = conn
        .query_row("SELECT COUNT(*) FROM aave_v3_users", [], |r| r.get(0))
        .unwrap();
    assert_eq!(users, 1, "the purge rolled back with the refused re-seed");
    drop(conn);

    let (name, active, stamp) = market_row(&db, market_id);
    assert_eq!(name, MARKET_NAME);
    assert!(active);
    assert_eq!(
        stamp,
        Some(WARM_CURSOR),
        "the rollback restores the warm cursor"
    );
}

#[test]
fn reset_of_an_unknown_market_is_a_typed_failure_before_any_rpc() {
    let (db, _market_id) = populated();
    let transport = activation_cassette(MARKET_NAME);
    let provider = transport.as_alloy_provider();

    let mut guard = db.lock();
    let tx = guard.transaction().unwrap();
    let err =
        reset_market_to_cold_boot(&tx, 999, POOL_ADDRESS_PROVIDER, GHO, &provider).unwrap_err();
    assert!(matches!(err, RunError::MarketNotFound(999)), "got {err:?}");
    drop(tx);
    drop(guard);

    assert_eq!(
        transport.served_snapshot().served,
        0,
        "a missing market fails before the metadata RPC"
    );
}

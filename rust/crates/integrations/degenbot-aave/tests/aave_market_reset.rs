//! `aave reset`'s re-init half: a purged market must be observationally
//! identical to a freshly activated one, so the following update run enters the
//! same cold-boot path (the `ProxyCreated` bootstrap) an empty database takes.
//!
//! The equality is asserted on the committed dump of the market-scoped tables,
//! and the cold-boot entry is asserted by behaviour: over an EMPTY replay
//! ledger the bootstrap's address-provider `eth_getLogs` is the first request a
//! reset market makes, while a warm market makes none.
#![expect(clippy::unwrap_used)]

use std::sync::Arc;

use degenbot_aave::updater::{run_aave_update_on_db, NoProgress, RunError};
use degenbot_aave::ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK;
use degenbot_db::sql_ledger::dump_tables_golden_json;
use degenbot_db::{DbError, DegenbotDb};
use degenbot_rpc::cassette::{
    entry_digest, entry_key, Cassette, CassetteEntry, CassetteProvenance, CassetteResponse,
    CassetteSpan,
};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use rusqlite::params;
use std::sync::atomic::AtomicBool;

const CHAIN_ID: i64 = 1;
const MARKET_NAME: &str = "Aave Ethereum Market";
const POOL_ADDRESS_PROVIDER: &str = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
const GHO: &str = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";
const POOL: &str = "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2";
const CONFIGURATOR: &str = "0x64b761D848206f447Fe2dd461b0c635Ec39EbB27";

/// The market-scoped tables the equality is asserted over. `erc20_tokens` is
/// deliberately excluded: it is chain-keyed, not market-keyed, so a purge
/// leaves the token rows other markets and pools share (the same posture
/// `aave activate` takes on a database that already carries them).
const MARKET_TABLES: &[&str] = &[
    "aave_gho_tokens",
    "aave_v3_asset_configs",
    "aave_v3_assets",
    "aave_v3_collateral_positions",
    "aave_v3_contracts",
    "aave_v3_debt_positions",
    "aave_v3_emode_categories",
    "aave_v3_markets",
    "aave_v3_user_collateral_configs",
    "aave_v3_users",
];

/// The substrate the activation seam seeds for a fresh market: the bare market
/// row, the `POOL_ADDRESS_PROVIDER` contract row, the GHO `erc20_tokens` row
/// with its metadata, and the bare `aave_gho_tokens` row. Mirrors
/// `activate_aave_market_on_conn`'s fresh-insert branch (crate-private).
fn fresh_activated() -> (DegenbotDb, i64) {
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
        DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            &conn,
            market_id,
            "POOL_ADDRESS_PROVIDER",
            POOL_ADDRESS_PROVIDER,
            None,
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
        market_id
    };
    (db, market_id)
}

/// The post-sync shape: the activation seed plus everything an update run
/// populates (the remaining contract rows, a reserve, a user, both position
/// kinds, an e-mode category, and the GHO discount columns).
fn populated() -> (DegenbotDb, i64) {
    let (db, market_id) = fresh_activated();
    {
        let conn = db.lock();
        for (name, address) in [
            ("POOL", POOL),
            ("POOL_CONFIGURATOR", CONFIGURATOR),
            ("PRICE_ORACLE", "0x54586bE62E3c3580375aE3723C145253060Ca0C2"),
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
        conn.execute(
            "INSERT INTO aave_v3_emode_categories \
             (market_id, category_id, label, ltv, liquidation_threshold, liquidation_bonus) \
             VALUES (?1, 1, 'e', 1, 2, 3)",
            params![market_id],
        )
        .unwrap();
        // The GHO token's updater-populated columns.
        conn.execute(
            "UPDATE aave_gho_tokens SET v_token_id = ?1, \
             v_gho_discount_rate_strategy = '0xstrategy', v_gho_discount_token = '0xstkaave'",
            params![v_token],
        )
        .unwrap();
        // A cursor advanced by a prior sync.
        DegenbotDb::set_market_last_update_block_on_conn(&conn, market_id, 26_130_445).unwrap();
    }
    (db, market_id)
}

fn dump(db: &DegenbotDb) -> String {
    let conn = db.lock();
    dump_tables_golden_json(&conn, MARKET_TABLES).unwrap()
}

#[test]
fn a_purged_market_is_observationally_identical_to_a_freshly_activated_one() {
    let (populated_db, market_id) = populated();
    let populated_dump = dump(&populated_db);

    {
        let mut guard = populated_db.lock();
        let tx = guard.transaction().unwrap();
        DegenbotDb::reset_aave_market_on_conn(&tx, market_id, ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK)
            .unwrap();
        tx.commit().unwrap();
    }

    let (fresh_db, fresh_id) = fresh_activated();
    assert_eq!(market_id, fresh_id);
    assert_ne!(
        populated_dump,
        dump(&populated_db),
        "the purge must actually change the market's state"
    );
    assert_eq!(
        dump(&populated_db),
        dump(&fresh_db),
        "a reset market must match a freshly activated one on every market-scoped table"
    );
}

#[test]
fn a_reset_market_has_the_cold_boot_precondition_a_fresh_market_has() {
    // The bootstrap hard-errors without the pool/configurator rows; a fresh
    // activation leaves exactly that shape, so a reset must too.
    let (db, market_id) = populated();
    {
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        DegenbotDb::reset_aave_market_on_conn(&tx, market_id, ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK)
            .unwrap();
        tx.commit().unwrap();
    }
    let conn = db.lock();
    let pool_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM aave_v3_contracts WHERE market_id = ?1 AND name = 'POOL'",
            params![market_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pool_rows, 0, "the pool row is re-resolved by the bootstrap");
    let ap_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM aave_v3_contracts \
             WHERE market_id = ?1 AND name = 'POOL_ADDRESS_PROVIDER'",
            params![market_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        ap_rows, 1,
        "the bootstrap's fetch anchor survives the purge"
    );
    let stamp: Option<i64> = conn
        .query_row(
            "SELECT last_update_block FROM aave_v3_markets WHERE id = ?1",
            params![market_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        stamp,
        Some(ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK),
        "the cursor is rewound to the activation block"
    );
}

/// The six event topics `fetch_address_provider_logs` unions, in its order.
const ADDRESS_PROVIDER_TOPICS: [&str; 6] = [
    "0x9ef0e8c8e52743bb38b83b17d9429141d494b8041ca6d616a6c77cebae9cd8b7",
    "0x8932892569eba59c8382a089d9b732d1f49272878775235761a2a6b0309cd465",
    "0xc853974cfbf81487a14a23565917bee63f527853bcb5fa54f2ae1cdf8a38356d",
    "0x90affc163f1a2dfedcd36aa02ed992eeeba8100a4014f0b4cdc20ea265a66627",
    "0x56b5f80d8cac1479698aa7d01605fd6111e90b15fc4d2b377417f46034876cbd",
    "0x4a465a9bd819d9662563c1e11ae958f8109e437e7f4bf1c6ef0b9a7b3f35d478",
];

/// The bootstrap's `ProxyCreated` fetch window (mirrors the updater's
/// `BOOTSTRAP_WINDOW`).
const BOOTSTRAP_WINDOW: u64 = 2_000;

/// The address-provider log filter the bootstrap issues: the seeded
/// `POOL_ADDRESS_PROVIDER` row supplies the address, the whole bootstrap window
/// is the range, and the six topic unions ride one nested topic position.
fn bootstrap_filter(from_block: u64, boot_end: u64) -> serde_json::Value {
    serde_json::json!({
        "address": POOL_ADDRESS_PROVIDER.to_lowercase(),
        "fromBlock": from_block.to_string(),
        "toBlock": boot_end.to_string(),
        "topics": [ADDRESS_PROVIDER_TOPICS.to_vec()],
    })
}

/// A one-entry ledger carrying exactly the bootstrap's address-provider
/// `eth_getLogs`, keyed through the cassette's own canonical key. Its recorded
/// answer is empty, so the bootstrap resolves nothing and reports its failure —
/// the observable that a reset market re-entered the cold boot.
fn bootstrap_only_cassette() -> CassetteReplayTransport {
    let from_block = u64::try_from(ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK).unwrap() + 1;
    let boot_end = from_block + BOOTSTRAP_WINDOW;
    let span = CassetteSpan {
        from_block,
        to_block: from_block,
    };
    let params = serde_json::Value::Array(vec![bootstrap_filter(from_block, boot_end)]);
    let key = entry_key("eth_getLogs", &params);
    let response = CassetteResponse::Success {
        result: serde_json::json!([]),
    };
    let mut entries = std::collections::BTreeMap::new();
    entries.insert(
        key,
        CassetteEntry {
            digest: entry_digest(&response),
            response,
        },
    );
    CassetteReplayTransport::new(Cassette {
        schema: degenbot_rpc::cassette::CASSETTE_SCHEMA_V1.to_string(),
        chain_id: 1,
        provenance: CassetteProvenance {
            source: "test".to_string(),
            recorded_at: "1970-01-01T00:00:00Z".to_string(),
            span,
        },
        entries,
    })
}

fn run_once(db: &DegenbotDb, to_block: u64) -> (Result<(), RunError>, u64, u64) {
    let transport = bootstrap_only_cassette();
    let provider = transport.as_alloy_provider();
    let result = run_aave_update_on_db(
        db,
        CHAIN_ID,
        1,
        Some(to_block),
        BOOTSTRAP_WINDOW + 1,
        provider,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        false,
        None,
        false,
        Some(1),
    );
    let snapshot = transport.served_snapshot();
    (result.map(|_| ()), snapshot.requests, snapshot.served)
}

#[test]
fn a_reset_market_re_enters_the_cold_boot_bootstrap() {
    let (db, market_id) = populated();
    {
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        DegenbotDb::reset_aave_market_on_conn(&tx, market_id, ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK)
            .unwrap();
        tx.commit().unwrap();
    }
    let (result, requests, served) = run_once(
        &db,
        u64::try_from(ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK).unwrap() + 1,
    );
    assert!(
        matches!(result, Err(RunError::BootstrapFailed(_))),
        "the reset market's cold boot must run the bootstrap and find no pool rows, got {result:?}"
    );
    assert_eq!(
        served, 1,
        "the serving ledger carries only the bootstrap's address-provider fetch"
    );
    assert_eq!(
        requests, 1,
        "the bootstrap fetch is the ONLY request a reset market issues"
    );
}

#[test]
fn a_warm_market_skips_the_bootstrap_and_reaches_the_chunk_fetch() {
    // The control: with the pool/configurator rows present the bootstrap is a
    // no-op, so the run proceeds to the chunk fetch instead of stopping at the
    // address-provider pass.
    // The cursor still sits where a prior sync left it (the populated seed's
    // stamp), so the run starts one block past it rather than at the
    // activation block the reset test uses.
    let (db, _market_id) = populated();
    let (result, requests, served) = run_once(&db, 26_130_446);
    assert!(
        !matches!(result, Err(RunError::BootstrapFailed(_))),
        "a warm market's bootstrap is a no-op, so the run reaches the chunk fetch          instead of reporting the bootstrap failed; got {result:?}"
    );
    assert_eq!(
        served, 0,
        "the ledger carries only the bootstrap filter, which a warm market never issues"
    );
    assert!(
        requests >= 1,
        "the warm market still issues the chunk's log fetch"
    );
}

#[test]
fn purging_an_unknown_market_is_a_typed_db_failure() {
    let (db, _market_id) = populated();
    let mut guard = db.lock();
    let tx = guard.transaction().unwrap();
    let err = DegenbotDb::purge_aave_market_on_conn(&tx, 999).unwrap_err();
    assert!(matches!(err, DbError::MissingRow(_)), "got {err:?}");
}

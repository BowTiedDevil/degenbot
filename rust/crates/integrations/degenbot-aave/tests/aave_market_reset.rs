//! `aave reset`'s re-init half: a purged market must be observationally
//! identical to a freshly activated one, so the following update run enters the
//! same cold-boot path (the `ProxyCreated` bootstrap) an empty database takes.
//!
//! The equality is asserted on the committed dump of the market-scoped tables,
//! and the cold-boot entry is asserted by behaviour: over a completing replay
//! ledger the reset state runs the REAL cold boot — the bootstrap's
//! address-provider fetch + revision calls, then the chunk loop's fetch passes —
//! and lands a normal sync from the stamped cursor, while a warm market issues
//! none of the bootstrap's requests.
#![expect(clippy::unwrap_used)]

use std::sync::Arc;

use degenbot_aave::updater::{
    activate_aave_market_on_conn, run_aave_update_on_db, AaveUpdateReport, NoProgress, RunError,
};
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

/// The reset contract the CLI arm drives: the purge (rewinding the cursor to
/// the bootstrap block) followed by the activate seam's one-transaction
/// completion — the exact two primitives `aave reset` composes.
fn reset_market(db: &DegenbotDb, market_id: i64) {
    {
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        DegenbotDb::reset_aave_market_on_conn(&tx, market_id, ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK)
            .unwrap();
        tx.commit().unwrap();
    }
    {
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        let id = activate_aave_market_on_conn(
            &tx,
            CHAIN_ID,
            MARKET_NAME,
            POOL_ADDRESS_PROVIDER,
            GHO,
            None,
            None,
            None,
            ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(id, market_id, "the completion reuses the purged market row");
    }
}

fn dump(db: &DegenbotDb) -> String {
    let conn = db.lock();
    dump_tables_golden_json(&conn, MARKET_TABLES).unwrap()
}

#[test]
fn a_reset_market_is_observationally_identical_to_a_freshly_activated_one() {
    let (populated_db, market_id) = populated();
    let populated_dump = dump(&populated_db);

    reset_market(&populated_db, market_id);

    let (fresh_db, fresh_id) = fresh_activated();
    assert_eq!(market_id, fresh_id);
    assert_ne!(
        populated_dump,
        dump(&populated_db),
        "the reset must actually change the market's state"
    );
    assert_eq!(
        dump(&populated_db),
        dump(&fresh_db),
        "a reset market must match a freshly activated one on every market-scoped table"
    );
}

#[test]
fn a_reset_market_keeps_the_cold_boot_precondition_and_no_populated_rows() {
    let (db, market_id) = populated();
    reset_market(&db, market_id);
    let conn = db.lock();

    // (b) the market row: present, active, cursor at the bootstrap block.
    let (active, stamp): (i64, Option<i64>) = conn
        .query_row(
            "SELECT active, last_update_block FROM aave_v3_markets WHERE id = ?1",
            params![market_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(active, 1, "the re-init re-activates the market");
    assert_eq!(
        stamp,
        Some(ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK),
        "the cursor sits at the bootstrap block the next update cold-boots from"
    );

    // (c) the POOL_ADDRESS_PROVIDER contract row (the bootstrap's fetch anchor).
    let ap_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM aave_v3_contracts \
             WHERE market_id = ?1 AND name = 'POOL_ADDRESS_PROVIDER'",
            params![market_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ap_rows, 1);

    // The populated rows the bootstrap re-resolves are gone.
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

    // (d) no users / positions survive.
    for table in [
        "aave_v3_users",
        "aave_v3_collateral_positions",
        "aave_v3_debt_positions",
    ] {
        let rows: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "{table} must be empty after a reset");
    }
}

// ── the completing cold-boot replay over the fixture window ──────────────

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

/// The single block the fixture window collapses to (bootstrap block + 1).
const FIXTURE_BLOCK: u64 = 16_291_071;

/// The POOL proxy's right-padded ASCII bytes32 id (the `ProxyCreated` topic 1).
const POOL_ID_TOPIC: &str = "0x504f4f4c00000000000000000000000000000000000000000000000000000000";
/// The `POOL_CONFIGURATOR` proxy's right-padded ASCII bytes32 id.
const CONFIGURATOR_ID_TOPIC: &str =
    "0x504f4f4c5f434f4e464947555241544f52000000000000000000000000000000";

/// The `POOL_REVISION()` calldata (the no-arg selector `cast` signs).
const POOL_REVISION_CALLDATA: &str = "0x0148170e";
/// The `CONFIGURATOR_REVISION()` calldata.
const CONFIGURATOR_REVISION_CALLDATA: &str = "0x7af635a6";

/// The chain-verified implementation addresses the fixture's `ProxyCreated`
/// events name (the revision reads target them).
const POOL_IMPLEMENTATION: &str = "0x0c008e6479a83be6a6c49d95c2029a6064136688";
const CONFIGURATOR_IMPLEMENTATION: &str = "0x786dbff3f1292ae8f92ea68cf93c30b34b1ed04b";

/// The five topic sets the chunk loop's fetch passes union over the fixture
/// window (the recorder's canonical sets; the empty scaled-token and stkAAVE
/// passes short-circuit before any request on this substrate).
const CONFIGURATOR_PASS_TOPICS: [&str; 5] = [
    "0x0acf8b4a3cace10779798a89a206a0ae73a71b63acdd3be2801d39c2ef7ab3cb",
    "0x3a0ca721fc364424566385a1aa271ed508cc2c0949c2272575fb3013a163a45f",
    "0x5bb69795b6a2ea222d73a5f8939c23471a1f85a99c7ca43c207f1b71f10c6264",
    "0x637febbda9275aea2e85c0ff690444c8d87eb2e8339bbede9715abcc89cb0995",
    "0x79409190108b26fcb0e4570f8e240f627bf18fd01a55f751010224d5bd486098",
];
const POOL_PASS_TOPICS: [&str; 11] = [
    "0x00058a56ea94653cdf4f152d227ace22d4c00ad99e2a43f58cb7d9e3feb295f2",
    "0x2b627736bca15cd5381dcf80b0bf11fd197d01a037c52b927a881a10fb73ba61",
    "0x2bccfb3fad376d59d7accf970515eb77b2f27b082c90ed0fb15583dd5a942699",
    "0x3115d1449a7b732c986cba18244e897a450f61e1bb8d589cd2e69e6c8924f9f7",
    "0x44c58d81365b66dd4b1a7f36c25aa97b8c71c361ee4937adc1a00000227db5dd",
    "0x804c9b842b2748a22bb64b345453a3de7ca54a6ca45ce00d415894979e22897a",
    "0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051",
    "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0",
    "0xbfa21aa5d5f9a1f0120a95e7c0749f389863cbdbfff531aa7339077a5bc919de",
    "0xd728da875fc88944cbf17638bcbe4af0eedaef63becd1d1c57cc097eb4608d84",
    "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286",
];
const ORACLE_PASS_TOPIC: &str =
    "0x22c5b7b2d8561d39f7f210b6b326a1aa69f15311163082308ac4877db6339dc1";
const DISCOUNT_PASS_TOPICS: [&str; 2] = [
    "0x194bd59f47b230edccccc2be58b92dde3a5dadd835751a621af59006928bccef",
    "0x6b489e1dbfbe36f55c511c098bcc9d92fec7f04f74ceb75018697ab68f7d3529",
];

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

/// The wire form of one `ProxyCreated` log (the recorder's canonical field
/// shape: decimal quantities, verbatim hex, boolean `removed`).
fn proxy_created_log(
    id: &str,
    proxy: &str,
    implementation: &str,
    log_index: u64,
) -> serde_json::Value {
    let word = |addr: &str| format!("0x000000000000000000000000{}", &addr["0x".len()..]);
    serde_json::json!({
        "address": POOL_ADDRESS_PROVIDER.to_lowercase(),
        "blockHash": "0xc698c93cdbe7ab12221088ec36e9ee86f85e11048876814b84cb0a5d558d3314",
        "blockNumber": FIXTURE_BLOCK.to_string(),
        "blockTimestamp": "0",
        "data": "0x",
        "logIndex": log_index.to_string(),
        "removed": false,
        "topics": [
            "0x4a465a9bd819d9662563c1e11ae958f8109e437e7f4bf1c6ef0b9a7b3f35d478",
            id,
            word(proxy),
            word(implementation),
        ],
        "transactionHash": "0xa17567fa201a78b66c43e6ab178559f8c1d5d308fe944c0bd2c39b5e585097dc",
        "transactionIndex": "0",
    })
}

/// A completing replay ledger over the fixture window: everything the REAL
/// cold boot asks from the reset state. The bootstrap's address-provider fetch
/// answers the two `ProxyCreated` events; the revision reads answer the
/// chain-verified revisions; the chunk loop's fetch passes answer empty (the
/// window carries no operation events), so the chunk commits with the two
/// contract inserts and stamps the cursor at the window end.
#[expect(clippy::too_many_lines)]
fn completing_sync_cassette() -> CassetteReplayTransport {
    let from_block = u64::try_from(ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK).unwrap() + 1;
    let boot_end = from_block + BOOTSTRAP_WINDOW;
    let span = CassetteSpan {
        from_block,
        to_block: from_block,
    };
    let mut entries = std::collections::BTreeMap::new();
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
    let block = FIXTURE_BLOCK.to_string();

    // The bootstrap's ProxyCreated fetch over the whole bootstrap window.
    add(
        "eth_getLogs",
        serde_json::Value::Array(vec![bootstrap_filter(from_block, boot_end)]),
        serde_json::json!([
            proxy_created_log(POOL_ID_TOPIC, POOL, POOL_IMPLEMENTATION, 0),
            proxy_created_log(
                CONFIGURATOR_ID_TOPIC,
                CONFIGURATOR,
                CONFIGURATOR_IMPLEMENTATION,
                1
            ),
        ]),
    );

    // The revision reads the two proxy ids resolve through (block-pinned).
    let revision = |add: &mut dyn FnMut(&str, serde_json::Value, serde_json::Value),
                    selector: &str,
                    target: &str,
                    value: u64| {
        add(
            "eth_call",
            serde_json::json!([{"input": selector, "to": target.to_lowercase()}, block]),
            serde_json::json!(format!("0x{:064x}", value)),
        );
    };
    revision(&mut add, POOL_REVISION_CALLDATA, POOL_IMPLEMENTATION, 11);
    revision(
        &mut add,
        CONFIGURATOR_REVISION_CALLDATA,
        CONFIGURATOR_IMPLEMENTATION,
        8,
    );

    // The chunk loop's fetch passes over [FIXTURE_BLOCK, FIXTURE_BLOCK]. The
    // address-provider pass re-serves the same two ProxyCreated events (the
    // idempotent re-encounter the bootstrap's apply is built for).
    let chunk_filter = |address: Option<&str>, topics: serde_json::Value| {
        let mut filter = serde_json::Map::new();
        if let Some(address) = address {
            filter.insert("address".to_string(), serde_json::json!(address));
        }
        filter.insert("fromBlock".to_string(), serde_json::json!(block));
        filter.insert("toBlock".to_string(), serde_json::json!(block));
        filter.insert("topics".to_string(), topics);
        serde_json::Value::Object(filter)
    };
    add(
        "eth_getLogs",
        serde_json::Value::Array(vec![chunk_filter(
            Some(&POOL_ADDRESS_PROVIDER.to_lowercase()),
            serde_json::json!([ADDRESS_PROVIDER_TOPICS.to_vec()]),
        )]),
        serde_json::json!([
            proxy_created_log(POOL_ID_TOPIC, POOL, POOL_IMPLEMENTATION, 0),
            proxy_created_log(
                CONFIGURATOR_ID_TOPIC,
                CONFIGURATOR,
                CONFIGURATOR_IMPLEMENTATION,
                1
            ),
        ]),
    );
    add(
        "eth_getLogs",
        serde_json::Value::Array(vec![chunk_filter(
            Some(&CONFIGURATOR.to_lowercase()),
            serde_json::json!([CONFIGURATOR_PASS_TOPICS.to_vec()]),
        )]),
        serde_json::json!([]),
    );
    add(
        "eth_getLogs",
        serde_json::Value::Array(vec![chunk_filter(
            Some(&POOL.to_lowercase()),
            serde_json::json!([POOL_PASS_TOPICS.to_vec()]),
        )]),
        serde_json::json!([]),
    );
    // The oracle pass runs chain-wide (no PRICE_ORACLE row survives the reset;
    // the discovery fetch omits the address and flattens the single topic).
    add(
        "eth_getLogs",
        serde_json::Value::Array(vec![chunk_filter(
            None,
            serde_json::json!([ORACLE_PASS_TOPIC]),
        )]),
        serde_json::json!([]),
    );
    add(
        "eth_getLogs",
        serde_json::Value::Array(vec![chunk_filter(
            None,
            serde_json::json!([DISCOUNT_PASS_TOPICS.to_vec()]),
        )]),
        serde_json::json!([]),
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

fn run_once(
    db: &DegenbotDb,
    transport: &CassetteReplayTransport,
    to_block: u64,
) -> (Result<AaveUpdateReport, RunError>, u64, u64) {
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
    (result, snapshot.requests, snapshot.served)
}

#[test]
fn a_reset_market_syncs_normally_over_the_fixture_window() {
    let (db, market_id) = populated();
    reset_market(&db, market_id);
    let (result, requests, served) = run_once(&db, &completing_sync_cassette(), FIXTURE_BLOCK);
    assert!(result.is_ok(), "the cold boot must complete: {result:?}");
    let report = result.unwrap();
    assert_eq!(report.chunks_committed, 1);
    assert_eq!(
        report.total_events_applied, 2,
        "the two ProxyCreated inserts"
    );
    assert_eq!(report.to_block, FIXTURE_BLOCK);

    // The run's whole request set is served: the ledger IS the cold-boot
    // request surface a reset state issues (the replay drift gate). Two
    // bootstrap-window fetch passes (bootstrap + chunk), the revision call per
    // proxy id per dispatch phase (the bootstrap's memo and the chunk's), and
    // the three empty passes + the chain-wide oracle pass.
    assert_eq!(requests, 10);
    assert_eq!(served, 10);

    let conn = db.lock();
    let stamp: Option<i64> = conn
        .query_row(
            "SELECT last_update_block FROM aave_v3_markets WHERE id = ?1",
            params![market_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        stamp,
        Some(i64::try_from(FIXTURE_BLOCK).unwrap()),
        "the sync commits and advances the cursor normally"
    );
    let (pool_rev, cfg_rev): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT MAX(CASE WHEN name = 'POOL' THEN revision END), \
                    MAX(CASE WHEN name = 'POOL_CONFIGURATOR' THEN revision END) \
             FROM aave_v3_contracts WHERE market_id = ?1",
            params![market_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(pool_rev, Some(11));
    assert_eq!(cfg_rev, Some(8));
    let users: i64 = conn
        .query_row("SELECT COUNT(*) FROM aave_v3_users", [], |r| r.get(0))
        .unwrap();
    assert_eq!(users, 0, "the empty window carries no users");
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
    let (result, requests, served) = run_once(&db, &bootstrap_only_cassette(), 26_130_446);
    assert!(
        !matches!(result, Err(RunError::BootstrapFailed(_))),
        "a warm market's bootstrap is a no-op, so the run reaches the chunk fetch \
         instead of reporting the bootstrap failed; got {result:?}"
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

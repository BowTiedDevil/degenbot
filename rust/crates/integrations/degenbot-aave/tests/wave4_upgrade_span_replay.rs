//! The wave-4 upgrade-span replay gate: every window the recorder captured
//! (`generate_aave_upgrade_windows`) replays through the REAL
//! `run_aave_update_on_db` multi-chunk loop over the committed cassette BYTES
//! and must reproduce BOTH committed SQL goldens byte-for-byte, with the
//! chunk-committed count and the served round-trip count pinned as
//! independent literals.
//!
//! The captures are live chain answers: each recorded `eth_call` is the
//! node's real answer at its pinned block, and each recorded `getLogs` is the
//! window's real log set. The replay re-issues the updater's exact RPC surface
//! over the seed manifest's harness DB; the statement-ledger + DB-dump goldens
//! pin the SQL trace and the end state.
//!
//! Per-window teeth (the risky-transition surfaces the plan names):
//!
//! `w4_aave_pre_upgrade_control` — the A/B control: no `Upgraded` in the span,
//! so every asset keeps its seeded revision and the spans carry no revision
//! RPC. The served surface is exactly the 12 getLogs passes (6 per chunk).
//!
//! `w4_aave_atoken_upgrade_in_chunk` — a single aToken `Upgraded` interior to
//! chunk 1. The recorded `ATOKEN_REVISION()` answer is the new implementation's
//! revision (2); the apply bumps one asset's `a_token_revision` and leaves the
//! rest. Mutating that answer flips the dump column for exactly that asset.
//!
//! `w4_aave_upgrade_plus_same_block_config` — 100 `Upgraded` plus a
//! `PoolUpdated` in one tx. The `POOL_REVISION()` read and the mass `Upgraded`
//! reads ride the per-chunk memo keyed by `(implementation, selector, block)`.
//!
//! `w4_aave_multi_upgrade_one_chunk` — 120 `Upgraded` + `PoolUpdated` +
//! `PoolConfiguratorUpdated` in one tx. Mutating ONE vToken answer moves
//! exactly that asset's `v_token_revision`.
//!
//! `w4_aave_gho_deprecation_at_chunk_boundary` — the GHO vToken rev 3->4
//! `Upgraded` lands AT chunk 1's boundary. Rev >= 4 fires the GHO-discount
//! deprecation: `aave_gho_tokens` clears `v_gho_discount_token` +
//! `v_gho_discount_rate_strategy` and the bulk `aave_v3_users` reset runs.
//! Flipping the recorded GHO answer to 3 must suppress the deprecation.
#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use degenbot_aave::updater::{run_aave_update_on_db, NoProgress};
use degenbot_db::sql_ledger::{dump_tables_golden_json, ledger_golden_json, LedgerDb};
use degenbot_db::DegenbotDb;
use degenbot_rpc::cassette::{entry_digest, verify_cassette_bytes, Cassette, CassetteResponse};
use degenbot_rpc::cassette_replay::{CassetteReplayTransport, ServedSnapshot};
use serde_json::Value;
use tempfile::TempDir;

/// The chain the recorder captured against.
const CHAIN_ID: i64 = 1;
/// The Aave V3 Ethereum bootstrap block (the seed's market stamp).
const BOOTSTRAP_BLOCK: i64 = 16_291_070;
/// The market name the seed stamps.
const MARKET_NAME: &str = "Aave Ethereum Market";
/// The `PoolAddressesProvider` row.
const POOL_ADDRESS_PROVIDER: &str = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
/// The chain's GHO token (the reserve whose underlying carries the FK link).
const GHO_TOKEN: &str = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";

/// The tables one Aave chunk apply touches, in the dump's fixed order.
const DUMP_TABLES: &[&str] = &[
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
    "erc20_tokens",
];

fn home() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../").to_string()
}

fn cassette_path(name: &str) -> String {
    format!("{}tests/fixtures/cassettes/wave4/{name}.json", home())
}

fn seed_path(name: &str) -> String {
    format!(
        "{}tests/fixtures/sql_goldens/wave4/{name}.seed.json",
        home()
    )
}

fn ledger_golden_path(name: &str) -> String {
    format!(
        "{}tests/fixtures/sql_goldens/wave4/{name}.statement-ledger.json",
        home()
    )
}

fn dump_golden_path(name: &str) -> String {
    format!(
        "{}tests/fixtures/sql_goldens/wave4/{name}.db-dump.json",
        home()
    )
}

/// The per-window independent literals: the committed cassette's served
/// round-trip count and the run's committed chunk count. A regression that
/// changes the RPC surface or the chunk boundaries fails these before the
/// byte-diff consults a golden.
struct WindowExpectations {
    name: &'static str,
    chunks: usize,
    served: u64,
    get_logs: u64,
    eth_calls: u64,
}

const WINDOWS: &[WindowExpectations] = &[
    WindowExpectations {
        name: "w4_aave_pre_upgrade_control",
        chunks: 2,
        served: 12,
        get_logs: 12,
        eth_calls: 0,
    },
    WindowExpectations {
        name: "w4_aave_atoken_upgrade_in_chunk",
        chunks: 2,
        served: 13,
        get_logs: 12,
        eth_calls: 1,
    },
    WindowExpectations {
        name: "w4_aave_upgrade_plus_same_block_config",
        chunks: 2,
        served: 17,
        get_logs: 12,
        eth_calls: 5,
    },
    WindowExpectations {
        name: "w4_aave_multi_upgrade_one_chunk",
        chunks: 2,
        served: 18,
        get_logs: 12,
        eth_calls: 6,
    },
    WindowExpectations {
        name: "w4_aave_gho_deprecation_at_chunk_boundary",
        chunks: 2,
        served: 19,
        get_logs: 12,
        eth_calls: 7,
    },
];

/// One window's seed manifest (the recorder's substrate + cursor).
struct SeedManifest {
    cursor: u64,
    chunk_size: u64,
    to_block: u64,
    pool: String,
    configurator: String,
    price_oracle: String,
    data_provider: String,
    pool_revision: i64,
    configurator_revision: i64,
    reserves: Vec<SeedReserve>,
}

struct SeedReserve {
    underlying: String,
    a_token: String,
    v_token: String,
}

fn read_seed(name: &str) -> SeedManifest {
    let bytes = std::fs::read(seed_path(name)).unwrap_or_else(|e| panic!("{name}: seed: {e}"));
    let v: Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{name}: seed parse: {e}"));
    let field = |k: &str| v.get(k).unwrap_or_else(|| panic!("{name}: seed field {k}"));
    let num_u = |k: &str| {
        field(k)
            .as_u64()
            .unwrap_or_else(|| panic!("{name}: {k} uint"))
    };
    let num_i = |k: &str| {
        field(k)
            .as_i64()
            .unwrap_or_else(|| panic!("{name}: {k} int"))
    };
    let as_str = |k: &str| {
        field(k)
            .as_str()
            .unwrap_or_else(|| panic!("{name}: {k} string"))
            .to_string()
    };
    let reserves = v
        .get("reserves")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{name}: reserves array"))
        .iter()
        .map(|r| SeedReserve {
            underlying: r["underlying"].as_str().unwrap().to_string(),
            a_token: r["a_token"].as_str().unwrap().to_string(),
            v_token: r["v_token"].as_str().unwrap().to_string(),
        })
        .collect();
    SeedManifest {
        cursor: num_u("cursor"),
        chunk_size: num_u("chunk_size"),
        to_block: num_u("to_block"),
        pool: as_str("pool"),
        configurator: as_str("configurator"),
        price_oracle: as_str("price_oracle"),
        data_provider: as_str("data_provider"),
        pool_revision: num_i("pool_revision"),
        configurator_revision: num_i("configurator_revision"),
        reserves,
    }
}

/// Seed the harness DB the recorder's flow built, from the committed seed
/// manifest. Returns `(db path, market id)`.
fn seeded_db(dir: &Path, file_name: &str, seed: &SeedManifest) -> (std::path::PathBuf, i64) {
    let path = dir.join(file_name);
    let (db, _state) = DegenbotDb::open_for_writes(&path).unwrap();
    let market_id = {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (chain_id, name, active, last_update_block) \
             VALUES (?1, ?2, 1, ?3)",
            rusqlite::params![CHAIN_ID, MARKET_NAME, BOOTSTRAP_BLOCK],
        )
        .unwrap();
        let market_id = conn.last_insert_rowid();
        for (name, address, revision) in [
            ("POOL_ADDRESS_PROVIDER", POOL_ADDRESS_PROVIDER, None),
            ("POOL", seed.pool.as_str(), Some(seed.pool_revision)),
            (
                "POOL_CONFIGURATOR",
                seed.configurator.as_str(),
                Some(seed.configurator_revision),
            ),
            ("PRICE_ORACLE", seed.price_oracle.as_str(), None),
            ("POOL_DATA_PROVIDER", seed.data_provider.as_str(), None),
        ] {
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &conn, market_id, name, address, revision,
            )
            .unwrap();
        }
        DegenbotDb::set_market_last_update_block_on_conn(
            &conn,
            market_id,
            i64::try_from(seed.cursor).unwrap(),
        )
        .unwrap();
        for reserve in &seed.reserves {
            let underlying_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                CHAIN_ID,
                &reserve.underlying,
                None,
                None,
                None,
            )
            .unwrap();
            let a_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                CHAIN_ID,
                &reserve.a_token,
                None,
                None,
                None,
            )
            .unwrap();
            let v_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                CHAIN_ID,
                &reserve.v_token,
                None,
                None,
                None,
            )
            .unwrap();
            let gho_link = if reserve.underlying == GHO_TOKEN {
                Some(
                    DegenbotDb::get_or_create_gho_token_on_conn(&conn, CHAIN_ID, GHO_TOKEN)
                        .unwrap(),
                )
            } else {
                None
            };
            DegenbotDb::apply_reserve_initialized_on_conn(
                &conn,
                market_id,
                underlying_id,
                a_token_id,
                1,
                v_token_id,
                1,
                None,
                gho_link,
            )
            .unwrap();
        }
        market_id
    };
    (path, market_id)
}

/// One replay's products.
struct ReplayRun {
    served: ServedSnapshot,
    db_path: std::path::PathBuf,
}

/// Replay one window's committed cassette through the chunk loop over the
/// seeded harness DB + statement ledger, gate BOTH goldens byte-for-byte,
/// and return the serving snapshot + end-state handle.
fn replay_and_gate(name: &str, expect: &WindowExpectations) -> ReplayRun {
    let seed = read_seed(name);
    let bytes = std::fs::read(cassette_path(name))
        .unwrap_or_else(|e| panic!("{name}: committed cassette must exist: {e}"));
    verify_cassette_bytes(&bytes).unwrap_or_else(|e| panic!("{name}: drift gate RED: {e}"));
    let cassette = Cassette::from_json_bytes(&bytes).expect("a valid v1 cassette");
    let transport = CassetteReplayTransport::new(cassette);
    let provider = transport.as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let (db_path, market_id) = seeded_db(dir.path(), "golden.db", &seed);
    let (ledger, _state) = LedgerDb::open_for_writes(&db_path).unwrap();
    let report = run_aave_update_on_db(
        ledger.db(),
        CHAIN_ID,
        market_id,
        Some(seed.to_block),
        seed.chunk_size,
        provider,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        false,
        None,
        false,
        None,
    )
    .unwrap_or_else(|e| panic!("{name}: the replayed span must commit cleanly: {e}"));
    assert_eq!(
        report.chunks_committed, expect.chunks,
        "{name}: the recorded span commits the pinned chunk count"
    );

    let records = ledger.records().expect("the capture session is armed");
    let ledger_json = ledger_golden_json(&records);
    let committed_ledger = std::fs::read_to_string(ledger_golden_path(name)).unwrap();
    assert_eq!(
        ledger_json, committed_ledger,
        "{name}: statement-ledger drift"
    );

    let conn = ledger.db().lock();
    let dump_json = dump_tables_golden_json(&conn, DUMP_TABLES).expect("dump");
    drop(conn);
    let committed_dump = std::fs::read_to_string(dump_golden_path(name)).unwrap();
    assert_eq!(dump_json, committed_dump, "{name}: DB-dump drift");

    let db_path = dir.keep().join("golden.db");
    ReplayRun {
        served: transport.served_snapshot(),
        db_path,
    }
}

/// A probe replay through the statement ledger: no golden gates (the probe
/// mutates inputs on purpose), but the ledger trace IS captured so a probe
/// can compare its SQL shape against the committed golden.
fn replay_probe(name: &str, seed: &SeedManifest, cassette: Cassette) -> ProbeRun {
    let transport = CassetteReplayTransport::new(cassette);
    let provider = transport.as_alloy_provider();
    let dir = TempDir::new().unwrap();
    let (db_path, market_id) = seeded_db(dir.path(), "probe.db", seed);
    let (ledger, _state) = LedgerDb::open_for_writes(&db_path).unwrap();
    run_aave_update_on_db(
        ledger.db(),
        CHAIN_ID,
        market_id,
        Some(seed.to_block),
        seed.chunk_size,
        provider,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        false,
        None,
        false,
        None,
    )
    .unwrap_or_else(|e| panic!("{name}: the probe replay must commit cleanly: {e}"));
    let records = ledger.records().expect("the capture session is armed");
    let ledger_json = ledger_golden_json(&records);
    let db_path = dir.keep().join("probe.db");
    ProbeRun {
        served: transport.served_snapshot(),
        db_path,
        ledger: ledger_json,
    }
}

/// A probe's products: the serving snapshot, the end-state handle, and the
/// statement-ledger trace (the SQL-shape discriminator).
struct ProbeRun {
    served: ServedSnapshot,
    db_path: std::path::PathBuf,
    ledger: String,
}

/// The count of statements whose SQL contains `needle` in a ledger golden.
fn ledger_statement_count(ledger_json: &str, needle: &str) -> usize {
    let v: Value = serde_json::from_str(ledger_json).unwrap();
    v["statements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["sql"].as_str().unwrap_or("").contains(needle))
        .count()
}

fn scalar(db_path: &Path, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// The mutated cassette: rewrite exactly ONE recorded `eth_call` answer and
/// re-sign it through the writer's own digest fn (so the drift gate stays
/// green while the served answer changes).
fn mutate_one_eth_call(cassette: &mut Cassette, key_marker: &str, new_word: u64) -> usize {
    let mut mutated = 0usize;
    for (key, entry) in &mut cassette.entries {
        if !key.contains("eth_call") || key.contains("eth_getLogs") || !key.contains(key_marker) {
            continue;
        }
        if let CassetteResponse::Success { result } = &mut entry.response {
            *result = Value::String(format!("0x{new_word:064x}"));
            entry.digest = entry_digest(&entry.response);
            mutated += 1;
        }
    }
    mutated
}

#[test]
#[expect(clippy::too_many_lines)]
fn wave4_upgrade_span_replays_to_the_committed_goldens() {
    // ── the committed surfaces (pinned before any replay) ────────────────
    for expect in WINDOWS {
        let bytes = std::fs::read(cassette_path(expect.name)).unwrap();
        verify_cassette_bytes(&bytes).unwrap_or_else(|e| panic!("{}: {e}", expect.name));
        let cassette = Cassette::from_json_bytes(&bytes).unwrap();
        let calls = cassette
            .entries
            .keys()
            .filter(|k| k.contains("eth_call") && !k.contains("eth_getLogs"))
            .count();
        let logs = cassette
            .entries
            .keys()
            .filter(|k| k.contains("eth_getLogs"))
            .count();
        assert_eq!(
            calls as u64, expect.eth_calls,
            "{}: committed eth_call surface drifted",
            expect.name
        );
        assert_eq!(
            logs as u64, expect.get_logs,
            "{}: committed getLogs surface drifted",
            expect.name
        );
    }

    // ── the drift-gated replays (one serialized pass) ────────────────────
    for expect in WINDOWS {
        let run = replay_and_gate(expect.name, expect);
        assert_eq!(
            run.served.served, expect.served,
            "{}: the served round-trip count is the pinned surface",
            expect.name
        );
        assert_eq!(
            run.served
                .per_method
                .get("eth_getLogs")
                .map_or(0, |m| m.served),
            expect.get_logs,
            "{}: the six getLogs passes per chunk",
            expect.name
        );
        assert_eq!(
            run.served
                .per_method
                .get("eth_call")
                .map_or(0, |m| m.served),
            expect.eth_calls,
            "{}: the pinned eth_call count",
            expect.name
        );
    }

    // ── control: no Upgraded in the span, revisions unchanged ────────────
    let control = WINDOWS[0].name;
    let control_run = replay_and_gate(control, &WINDOWS[0]);
    assert_eq!(
        scalar(
            &control_run.db_path,
            "SELECT COUNT(*) FROM aave_v3_assets WHERE a_token_revision != 1 OR v_token_revision != 1"
        ),
        0,
        "control: no revision moved"
    );

    // ── W1: a single aToken upgrade; mutate the answer, one asset moves ──
    let w1 = WINDOWS[1].name;
    let w1_seed = read_seed(w1);
    let mut w1_cassette =
        Cassette::from_json_bytes(&std::fs::read(cassette_path(w1)).unwrap()).unwrap();
    let mutated = mutate_one_eth_call(
        &mut w1_cassette,
        "0x366ae337897223aea70e3ebe1862219386f20593",
        1,
    );
    assert_eq!(mutated, 1, "W1: exactly the one upgrade answer was mutated");
    let w1_probe = replay_probe(w1, &w1_seed, w1_cassette);
    assert_eq!(
        w1_probe.served.served, WINDOWS[1].served,
        "W1 probe: the mutated answer is served from the same key (same round trips)"
    );
    assert_eq!(
        scalar(
            &w1_probe.db_path,
            "SELECT COUNT(*) FROM aave_v3_assets WHERE a_token_revision = 2"
        ),
        0,
        "W1 probe: the mutated answer (1) leaves no asset at revision 2 — the dump gate is LOUD"
    );

    // ── W4: mutate ONE of the 120 vToken answers -> exactly that asset ───
    let w4 = WINDOWS[3].name;
    let w4_seed = read_seed(w4);
    let mut w4_cassette =
        Cassette::from_json_bytes(&std::fs::read(cassette_path(w4)).unwrap()).unwrap();
    // The GHO vToken implementation at the W4 block (the recorded 6->5 answer).
    let w4_mutated = mutate_one_eth_call(
        &mut w4_cassette,
        "0xc4bea6ff17879e27f266909397e5e0ad3d301946",
        3,
    );
    assert_eq!(w4_mutated, 1, "W4: exactly one vToken answer was mutated");
    let w4_probe = replay_probe(w4, &w4_seed, w4_cassette);
    assert_eq!(
        scalar(
            &w4_probe.db_path,
            "SELECT COUNT(*) FROM aave_v3_assets WHERE v_token_revision = 3"
        ),
        1,
        "W4 probe: exactly one asset carries the mutated revision"
    );
    assert_eq!(
        scalar(
            &w4_probe.db_path,
            "SELECT COUNT(*) FROM aave_v3_assets WHERE v_token_revision = 5"
        ),
        59,
        "W4 probe: the other 59 vToken-upgraded assets keep the recorded revision"
    );
    assert_ne!(
        w4_probe.ledger,
        std::fs::read_to_string(ledger_golden_path(w4)).unwrap(),
        "W4 probe: the single-asset mutation diverges from the committed ledger"
    );

    // ── W2: flip the GHO vToken answer below the deprecation revision ────
    let w2 = WINDOWS[4].name;
    let w2_seed = read_seed(w2);
    let w2_committed_ledger = std::fs::read_to_string(ledger_golden_path(w2)).unwrap();
    assert!(
        ledger_statement_count(&w2_committed_ledger, "v_gho_discount_token = NULL") > 0,
        "W2 committed: the GHO rev-4 upgrade runs the discount-deprecation clear"
    );
    let mut w2_cassette =
        Cassette::from_json_bytes(&std::fs::read(cassette_path(w2)).unwrap()).unwrap();
    // The GHO vToken impl at the W2 block (the recorded rev-4 answer):
    // flipping it to 3 must suppress the deprecation side effect.
    let w2_mutated = mutate_one_eth_call(
        &mut w2_cassette,
        "0x9b2b73f9ddd830f82d61520388ccf4fc048f9953",
        3,
    );
    assert_eq!(
        w2_mutated, 1,
        "W2: exactly the GHO vToken answer was mutated"
    );
    let w2_probe = replay_probe(w2, &w2_seed, w2_cassette);
    assert_eq!(
        ledger_statement_count(&w2_probe.ledger, "v_gho_discount_token = NULL"),
        0,
        "W2 probe: a rev-3 GHO answer suppresses the deprecation clear"
    );
    assert_eq!(
        scalar(
            &w2_probe.db_path,
            "SELECT COUNT(*) FROM aave_v3_assets WHERE v_token_revision = 4"
        ),
        0,
        "W2 probe: the GHO asset's vToken revision moved off 4 — only that asset changed"
    );
}

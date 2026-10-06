//! The wave-3 generated-capture replay gate: every aave-side capture the
//! node-free generator manufactured
//! (`generate_aave_adversarial_captures`) replays through the REAL
//! `run_aave_update_on_db` chunk loop over the committed cassette BYTES and
//! must reproduce BOTH committed SQL goldens byte-for-byte — the same drift
//! gate the live-recorded corpus and the wave-2 captures run under.
//!
//! The captures are execution products of the scripted actor contracts
//! (`degenbot_simulation::capture::actor` — real `LOG` opcodes, real call
//! frames, real served `eth_call` returns on the fixture EVM). The replay
//! re-issues the updater's exact RPC surface over the seeded harness DB; the
//! statement-ledger + DB-dump goldens pin the SQL trace and the end state.
//!
//! Per-scenario teeth (the adversarial contracts the capture program pins):
//!
//! `w3_aave_cross_tx_fact_dependence` — the `ChunkSubstrate` overlay case:
//! the recorded RPC surface is the QR7QVT rt-gate shape (6 getLogs passes +
//! exactly ONE `getDiscountPercent` `eth_call`, pinned at tx N's block 1001);
//! tx N+1's pre-pass reads the user row tx N's apply created IN-CHUNK. 7
//! served round trips, never 9: a committed-DB fact phase re-issues or drops
//! the call, and both shapes go loud here — the re-issued 1002 call has no
//! recorded entry (fixture gap), the dropped 1001 call shows in the serving
//! counts (the overlay probe).
//!
//! `w3_aave_upgraded_boundary_revision_memo` — the two same-impl
//! `DEBT_TOKEN_REVISION()` probes are TWO recorded entries, one per block
//! tag (the `RevisionMemo` block lane); mutating the recorded post-upgrade
//! answer flows into the applied revision and the dump gate goes LOUD — the
//! stale pre-upgrade memo value is never served (the boundary probe).
//!
//! `w3_aave_discount_revival_fixture` — the named counterfactual: the
//! fixture chain's vToken revision is 1 (< the deprecation revision 4) so
//! the QR7QVT option-(a) fact-run shape stays executable; the provenance
//! label is machine-gated here, and seeding the deprecation revision
//! collapses the RPC shape (the revival probe).

#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use degenbot_aave::updater::{run_aave_update, run_aave_update_on_db, NoProgress};
use degenbot_db::sql_ledger::{dump_tables_golden_json, ledger_golden_json, LedgerDb};
use degenbot_db::DegenbotDb;
use degenbot_rpc::cassette::{entry_digest, verify_cassette_bytes, Cassette, CassetteResponse};
use degenbot_rpc::cassette_replay::{CassetteReplayTransport, ServedSnapshot};
use serde_json::Value;
use tempfile::TempDir;

/// The committed wave-3 captures (machine-emitted, drift-gated).
fn cassette_path(name: &str) -> String {
    format!(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../tests/fixtures/cassettes/wave3/{name}.json"
        ),
        name = name
    )
}

fn ledger_golden_path(name: &str) -> String {
    format!(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../tests/fixtures/sql_goldens/wave3/{name}.statement-ledger.json"
        ),
        name = name
    )
}

fn dump_golden_path(name: &str) -> String {
    format!(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../tests/fixtures/sql_goldens/wave3/{name}.db-dump.json"
        ),
        name = name
    )
}

/// The pinned span (the generator's `DEPLOY_BLOCK`..`SPAN_TO`; the frames
/// land at 1001/1002 on top of the deploy block).
const SPAN_FROM: u64 = 1000;
const SPAN_TO: u64 = 1002;

/// Independent literals: the chunk's recorded RPC surface per scenario — the
/// QR7QVT rt-gate shape (six getLogs passes + the scenario's `eth_call`s).
const S1_EXPECTED_RPC_ROUND_TRIPS: u64 = 7;
const S2_EXPECTED_RPC_ROUND_TRIPS: u64 = 8;
const S3_EXPECTED_RPC_ROUND_TRIPS: u64 = 7;

/// The pinned chain literals (the generator's `ACTOR_PINS` + free literals;
/// a frame-sequence change fails these loudly at generation time). Each
/// scenario deploys on a fresh driver, so the k-th CREATE lands at the same
/// (sender, nonce) address across scenarios. The EIP-55 checksummed forms
/// feed the DB seed (the substrate parses strictly); the recorded ledger
/// keys carry alloy's lowercase serialization, so the key comparisons use
/// the lowercase forms.
const POOL_ACTOR: &str = "0xBd770416a3345F91E4B34576cb804a576fa48EB1";
const SECOND_DEPLOY_ACTOR: &str = "0x5a443704dd4B594B382c22a083e2BD3090A6feF3";
const THIRD_DEPLOY_ACTOR: &str = "0x47e9Fbef8C83A1714F1951F142132E6e90F5fa5D";
const POOL_ACTOR_LOWER: &str = "0xbd770416a3345f91e4b34576cb804a576fa48eb1";
const SECOND_DEPLOY_ACTOR_LOWER: &str = "0x5a443704dd4b594b382c22a083e2bd3090a6fef3";
const S1_USER: &str = "0xa1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
/// The same address in the substrate's stored (EIP-55) form — the pre-seed
/// insert and the row pin use it; a checksum-algorithm change fails these
/// pins loudly (the probe's rt gate then reads 7, not 6).
const S1_USER_CHECKSUMMED: &str = "0xA1A1a1a1A1A1A1A1A1a1a1a1a1a1A1A1a1A1a1a1";
const W3_GHO: &str = "0x4040404040404040404040404040404040404040";
const W3_ATOKEN: &str = "0x4141414141414141414141414141414141414141";
const W3_ADDRESS_PROVIDER: &str = "0x7070707070707070707070707070707070707070";
const W3_CONFIGURATOR: &str = "0x7272727272727272727272727272727272727272";
const W3_PRICE_ORACLE: &str = "0x7474747474747474747474747474747474747474";
const W3_S2_POOL: &str = "0x5050505050505050505050505050505050505050";
const W3_S2_UNDERLYING_1: &str = "0x5151515151515151515151515151515151515151";
const W3_S2_ATOKEN_1: &str = "0x5252525252525252525252525252525252525252";
const W3_S2_UNDERLYING_2: &str = "0x5353535353535353535353535353535353535353";
const W3_S2_ATOKEN_2: &str = "0x5454545454545454545454545454545454545454";

/// The seeded aToken/vToken revision (era-accurate for the discount-era
/// fixture chain; the revival probe seeds the deprecation revision instead).
const SEED_TOKEN_REVISION: i64 = 1;
/// The deprecation revision (the revival probe's premise-killer).
const GHO_DISCOUNT_DEPRECATION_REVISION: i64 = 4;
/// The seeded `POOL`/`POOL_CONFIGURATOR` revisions (the generator's seed shape).
const SEED_POOL_REVISION: i64 = 11;
const SEED_CONFIGURATOR_REVISION: i64 = 8;
/// The Aave V3 Ethereum bootstrap block (the seed's market stamp).
const AAVE_BOOTSTRAP_BLOCK: i64 = 16_291_070;

/// The tables one Aave chunk apply touches (the dump's fixed order).
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

/// One reserve asset's seed row set.
struct ReserveSeed {
    underlying: &'static str,
    a_token: &'static str,
    v_token: String,
    gho_link: bool,
}

/// The scenario under replay: the seeded pool + reserve assets.
enum ScenarioSeed {
    /// `w3_aave_cross_tx_fact_dependence`.
    S1,
    /// `w3_aave_upgraded_boundary_revision_memo`.
    S2,
    /// `w3_aave_discount_revival_fixture`.
    S3,
}

impl ScenarioSeed {
    fn pool(&self) -> &'static str {
        match self {
            Self::S1 | Self::S3 => POOL_ACTOR,
            Self::S2 => W3_S2_POOL,
        }
    }

    fn reserves(&self) -> Vec<ReserveSeed> {
        match self {
            Self::S1 | Self::S3 => vec![ReserveSeed {
                underlying: W3_GHO,
                a_token: W3_ATOKEN,
                v_token: SECOND_DEPLOY_ACTOR.to_string(),
                gho_link: true,
            }],
            Self::S2 => vec![
                ReserveSeed {
                    underlying: W3_S2_UNDERLYING_1,
                    a_token: W3_S2_ATOKEN_1,
                    v_token: SECOND_DEPLOY_ACTOR.to_string(),
                    gho_link: false,
                },
                ReserveSeed {
                    underlying: W3_S2_UNDERLYING_2,
                    a_token: W3_S2_ATOKEN_2,
                    v_token: THIRD_DEPLOY_ACTOR.to_string(),
                    gho_link: false,
                },
            ],
        }
    }
}

/// Seed the harness DB the generator's recording flow builds (the
/// `aave_config_cassette_replay` seeding shape): the market row at the
/// bootstrap block, the warm-boot contract rows with their revisions, the
/// span cursor, and the reserve assets. Returns `(db path, market id)`.
fn seeded_db(
    dir: &Path,
    file_name: &str,
    pool: &str,
    reserves: &[ReserveSeed],
    span_from: u64,
    token_revision: i64,
) -> (std::path::PathBuf, i64) {
    let path = dir.join(file_name);
    let (db, _state) = DegenbotDb::open_for_writes(&path).unwrap();
    let market_id = {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (chain_id, name, active, last_update_block) \
             VALUES (?1, ?2, 1, ?3)",
            rusqlite::params![1i64, "Aave Ethereum Market", AAVE_BOOTSTRAP_BLOCK],
        )
        .unwrap();
        let market_id = conn.last_insert_rowid();
        for (name, address, revision) in [
            ("POOL_ADDRESS_PROVIDER", W3_ADDRESS_PROVIDER, None),
            ("POOL", pool, Some(SEED_POOL_REVISION)),
            (
                "POOL_CONFIGURATOR",
                W3_CONFIGURATOR,
                Some(SEED_CONFIGURATOR_REVISION),
            ),
            ("PRICE_ORACLE", W3_PRICE_ORACLE, None),
        ] {
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &conn, market_id, name, address, revision,
            )
            .unwrap();
        }
        DegenbotDb::set_market_last_update_block_on_conn(
            &conn,
            market_id,
            i64::try_from(span_from - 1).unwrap(),
        )
        .unwrap();
        for reserve in reserves {
            let (underlying_name, underlying_symbol, underlying_decimals) = if reserve.gho_link {
                (Some("Gho Token"), Some("GHO"), Some(18))
            } else {
                (None, None, None)
            };
            let underlying_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                1,
                reserve.underlying,
                underlying_name,
                underlying_symbol,
                underlying_decimals,
            )
            .unwrap();
            let a_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                1,
                reserve.a_token,
                None,
                None,
                None,
            )
            .unwrap();
            let v_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                1,
                &reserve.v_token,
                None,
                None,
                None,
            )
            .unwrap();
            let gho_link = if reserve.gho_link {
                Some(
                    DegenbotDb::get_or_create_gho_token_on_conn(&conn, 1, reserve.underlying)
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
                token_revision,
                v_token_id,
                token_revision,
                None,
                gho_link,
            )
            .unwrap();
        }
        market_id
    };
    (path, market_id)
}

/// One replay's products: the serving snapshot (the rt gate reads it) and
/// the KEPT replayed DB path (the end-state pins read it; the temp dir is
/// `keep()`ed — the wave-2 suite's persistence shape for post-run reads).
struct ReplayRun {
    served: ServedSnapshot,
    db_path: std::path::PathBuf,
}

/// Replay one scenario's committed cassette through the chunk loop over the
/// seeded harness DB + statement ledger, gate BOTH goldens byte-for-byte,
/// and return the serving snapshot + end-state handle.
fn replay_and_gate(name: &str, seed: &ScenarioSeed) -> ReplayRun {
    let bytes = std::fs::read(cassette_path(name))
        .unwrap_or_else(|e| panic!("{name}: committed cassette must exist: {e}"));
    verify_cassette_bytes(&bytes).unwrap_or_else(|e| panic!("{name}: drift gate RED: {e}"));
    let cassette = Cassette::from_json_bytes(&bytes).expect("a valid v1 cassette");
    let transport = CassetteReplayTransport::new(cassette);
    let provider = transport.as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let (db_path, market_id) = seeded_db(
        dir.path(),
        "golden.db",
        seed.pool(),
        &seed.reserves(),
        SPAN_FROM,
        SEED_TOKEN_REVISION,
    );
    let (ledger, _state) = LedgerDb::open_for_writes(&db_path).unwrap();
    let report = run_aave_update_on_db(
        ledger.db(),
        1,
        market_id,
        Some(SPAN_TO),
        SPAN_TO - SPAN_FROM + 1,
        provider.clone(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        false,
        None,
        false,
        None,
    )
    .unwrap_or_else(|e| panic!("{name}: the replayed chunk must commit cleanly: {e}"));
    assert_eq!(
        report.chunks_committed, 1,
        "{name}: the whole recorded span is one chunk"
    );

    let records = ledger.records().expect("the capture session is armed");
    let ledger_json = ledger_golden_json(&records);
    let committed_ledger = std::fs::read_to_string(ledger_golden_path(name))
        .unwrap_or_else(|e| panic!("{name}: committed ledger golden must exist: {e}"));
    assert_eq!(
        ledger_json, committed_ledger,
        "{name}: statement-ledger drift — regenerate with REGENERATE_SQL_GOLDENS=1 \
         only if the chunk loop's SQL surface legitimately changed"
    );

    let conn = ledger.db().lock();
    let dump_json = dump_tables_golden_json(&conn, DUMP_TABLES).expect("dump the touched tables");
    drop(conn);
    let committed_dump = std::fs::read_to_string(dump_golden_path(name))
        .unwrap_or_else(|e| panic!("{name}: committed dump golden must exist: {e}"));
    assert_eq!(
        dump_json, committed_dump,
        "{name}: DB-dump drift — the end state diverges from the committed golden"
    );

    let db_path = dir.keep().join("golden.db");
    ReplayRun {
        served: transport.served_snapshot(),
        db_path,
    }
}

/// A probe replay: no golden gates (the probe mutates the inputs on
/// purpose). `pre_seed_user` inserts a committed `aave_v3_users` row BEFORE
/// the run — the committed-DB fact-phase view; `token_revision` overrides
/// the seeded asset revision (the revival probe's premise killer);
/// `replace_cassette` swaps the parsed cassette wholesale (the boundary
/// probe's mutated answer).
fn replay_probe(
    name: &str,
    seed: &ScenarioSeed,
    token_revision: i64,
    pre_seed_user: Option<&str>,
    replace_cassette: Option<Cassette>,
) -> ReplayRun {
    let bytes = std::fs::read(cassette_path(name))
        .unwrap_or_else(|e| panic!("{name}: committed cassette must exist: {e}"));
    verify_cassette_bytes(&bytes).unwrap_or_else(|e| panic!("{name}: drift gate RED: {e}"));
    let parsed = Cassette::from_json_bytes(&bytes).expect("a valid v1 cassette");
    let transport = CassetteReplayTransport::new(replace_cassette.unwrap_or(parsed));
    let provider = transport.as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let (db_path, market_id) = seeded_db(
        dir.path(),
        "probe.db",
        seed.pool(),
        &seed.reserves(),
        SPAN_FROM,
        token_revision,
    );
    if let Some(user) = pre_seed_user {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let stored = if user == S1_USER {
            S1_USER_CHECKSUMMED
        } else {
            user
        };
        conn.execute(
            "INSERT INTO aave_v3_users (market_id, address, e_mode, gho_discount, \
             isolation_mode_debt) VALUES (?1, ?2, 0, 0, '0')",
            rusqlite::params![market_id, stored],
        )
        .unwrap();
    }
    run_aave_update(
        &db_path,
        1,
        market_id,
        Some(SPAN_TO),
        SPAN_TO - SPAN_FROM + 1,
        provider,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        false,
        None,
        false,
        None,
    )
    .unwrap_or_else(|e| panic!("{name}: the probe replay must commit cleanly: {e}"));
    let db_path = dir.keep().join("probe.db");
    ReplayRun {
        served: transport.served_snapshot(),
        db_path,
    }
}

/// The `eth_call` entries the snapshot served (0 when the run issued no
/// `eth_call` at all — `per_method` only records requested methods).
fn served_eth_calls(served: &ServedSnapshot) -> u64 {
    served.per_method.get("eth_call").map_or(0, |m| m.served)
}

fn row_count(db_path: &Path, table: &str) -> i64 {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn scalar(db_path: &Path, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// The single user row's `(address, gho_discount)` — the row the ops parser
/// created. The address is stored EIP-55-checksummed (the substrate's
/// canonical form; the dump golden normalizes), so the pin lowercases.
fn user_row(db_path: &Path) -> (String, i64) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.query_row(
        "SELECT address, gho_discount FROM aave_v3_users",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

/// The committed cassette's `eth_call` entries as `(block tag, to)` pairs —
/// the recorded-RPC-shape greps (the `aave_config` suite's committed
/// discipline).
fn recorded_eth_calls(cassette: &Cassette) -> Vec<(String, String)> {
    cassette
        .entries
        .keys()
        .filter(|k| k.contains("eth_call") && !k.contains("eth_getLogs"))
        .map(|k| {
            let arr: Vec<Value> = serde_json::from_str(k).unwrap();
            let params = arr[1].as_array().expect("eth_call params are an array");
            let block = params
                .get(1)
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            let to = params
                .first()
                .and_then(|tx| tx.get("to"))
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            (block, to)
        })
        .collect()
}

/// The committed corpus's expected recorded-RPC shapes, pinned verbatim.
fn expected_eth_calls() -> Vec<(String, Vec<(String, String)>)> {
    vec![
        (
            "w3_aave_cross_tx_fact_dependence".to_string(),
            vec![("1001".to_string(), SECOND_DEPLOY_ACTOR_LOWER.to_string())],
        ),
        (
            "w3_aave_upgraded_boundary_revision_memo".to_string(),
            vec![
                ("1001".to_string(), POOL_ACTOR_LOWER.to_string()),
                ("1002".to_string(), POOL_ACTOR_LOWER.to_string()),
            ],
        ),
        (
            "w3_aave_discount_revival_fixture".to_string(),
            vec![("1001".to_string(), SECOND_DEPLOY_ACTOR_LOWER.to_string())],
        ),
    ]
}

/// ONE serialized test: the statement-ledger session is a process-global
/// slot (only one `LedgerDb` may be armed at a time), so the scenario
/// replays — and the per-scenario probes — run in sequence here. The length
/// is the serialization's price: three drift-gated replays + three negative
/// probes in one ordered pass.
#[test]
#[expect(clippy::too_many_lines)]
fn wave3_generated_captures_replay_to_the_committed_goldens() {
    // ── the committed recorded-RPC shapes (pinned before any replay) ─────
    let expected = expected_eth_calls();
    for (name, calls) in &expected {
        let bytes = std::fs::read(cassette_path(name)).unwrap();
        let cassette = Cassette::from_json_bytes(&bytes).unwrap();
        assert_eq!(
            &recorded_eth_calls(&cassette),
            calls,
            "{name}: the committed eth_call surface drifted"
        );
    }

    // ── scenario 1: the cross-tx fact dependence (the overlay case) ──────
    let s1_name = "w3_aave_cross_tx_fact_dependence";
    let s1 = replay_and_gate(s1_name, &ScenarioSeed::S1);
    assert_eq!(
        s1.served.served, S1_EXPECTED_RPC_ROUND_TRIPS,
        "s1: the QR7QVT rt-gate shape — 6 getLogs + exactly ONE discount call"
    );
    assert_eq!(
        s1.served.per_method["eth_getLogs"].served, 6,
        "s1: the six getLogs passes"
    );
    assert_eq!(
        s1.served.per_method["eth_call"].served, 1,
        "s1: the single getDiscountPercent call — tx N+1's pre-pass read the \
         in-chunk row (path #1), no second RPC"
    );
    assert_eq!(
        row_count(&s1.db_path, "aave_v3_users"),
        1,
        "s1: the ops parser created the user row in tx N (the overlay fact)"
    );
    let (s1_row_address, s1_row_discount) = user_row(&s1.db_path);
    assert_eq!(
        s1_row_address, S1_USER_CHECKSUMMED,
        "s1: the created user is the scenario's pinned repeat-burn address \
         (the substrate's stored EIP-55 form)"
    );
    assert_eq!(
        s1_row_discount, 0,
        "s1: the user row is readable from the chunk's own applies (the \
         discount column the pre-pass's path-#1 read consumes)"
    );

    // ── scenario 2: the mid-chunk `Upgraded` boundary (the memo contract) ─
    let s2_name = "w3_aave_upgraded_boundary_revision_memo";
    let s2 = replay_and_gate(s2_name, &ScenarioSeed::S2);
    assert_eq!(
        s2.served.served, S2_EXPECTED_RPC_ROUND_TRIPS,
        "s2: 6 getLogs + TWO same-impl revision probes (one per block tag)"
    );
    assert_eq!(
        s2.served.per_method["eth_call"].served, 2,
        "s2: the memo's block lane kept the post-upgrade probe a real RPC"
    );
    assert_eq!(
        scalar(
            &s2.db_path,
            "SELECT COUNT(*) FROM aave_v3_assets WHERE v_token_revision = 2"
        ),
        2,
        "s2: both `Upgraded` applies wrote the implementation's revision"
    );

    // ── scenario 3: the discount-revival counterfactual (the fixture) ────
    let s3_name = "w3_aave_discount_revival_fixture";
    let s3 = replay_and_gate(s3_name, &ScenarioSeed::S3);
    assert_eq!(
        s3.served.served, S3_EXPECTED_RPC_ROUND_TRIPS,
        "s3: the QR7QVT option-(a) fact-run shape — 6 getLogs + one discount call"
    );
    assert_eq!(
        row_count(&s3.db_path, "aave_v3_debt_positions"),
        1,
        "s3: the borrow created the GHO debt position"
    );
    // The FIXTURE label (not live behavior) is machine-gated off the
    // committed provenance.
    let s3_bytes = std::fs::read(cassette_path(s3_name)).unwrap();
    let s3_cassette = Cassette::from_json_bytes(&s3_bytes).unwrap();
    let source = &s3_cassette.provenance.source;
    assert!(
        source.contains("DISCOUNT-REVIVAL FIXTURE") && source.contains("NOT live behavior"),
        "s3: the provenance must label the capture as the revival FIXTURE: {source}"
    );

    // ── probe 1 (s1 negative): the committed-DB fact-phase shape ─────────
    // Pre-seed the user row — the view a fact phase consulting the COMMITTED
    // DB would hold (the row the chunk's apply has NOT written yet). The
    // discount pre-pass takes the DB-cache path at tx N, the recorded
    // getDiscountPercent call goes UNSERVED, and the rt gate reads 6, not 7.
    let s1_probe = replay_probe(
        s1_name,
        &ScenarioSeed::S1,
        SEED_TOKEN_REVISION,
        Some(S1_USER),
        None,
    );
    assert_eq!(
        s1_probe.served.served, 6,
        "s1 probe: with the user pre-seeded (the committed-DB view) the recorded \
         discount call drops — the rt gate (7) is RED under this mutation"
    );
    assert_eq!(
        served_eth_calls(&s1_probe.served),
        0,
        "s1 probe: no discount RPC issued — the fact source moved off the \
         chunk's own applies and the gate sees it"
    );

    // ── probe 2 (s2 negative): the mutated post-upgrade revision answer ──
    // Mutate the block-1002 recorded answer 2 -> 7 and re-sign through the
    // writer's own digest fn: the replay must serve the MUTATED value (the
    // memo's block lane never serves the stale pre-upgrade memo across the
    // block key), and the end state diverges from the committed golden —
    // the loud failure the boundary probe demonstrates.
    let s2_bytes = std::fs::read(cassette_path(s2_name)).unwrap();
    let mut mutated = Cassette::from_json_bytes(&s2_bytes).unwrap();
    let mut mutated_entries = 0usize;
    for (key, entry) in &mut mutated.entries {
        if !key.contains("eth_call") || key.contains("eth_getLogs") || !key.contains("\"1002\"") {
            continue;
        }
        if let CassetteResponse::Success { result } = &mut entry.response {
            assert_eq!(
                result.as_str(),
                Some("0x0000000000000000000000000000000000000000000000000000000000000002"),
                "the block-1002 recorded revision answer must be the pinned word 2"
            );
            *result = Value::String(format!("0x{:064x}", 7u64));
            entry.digest = entry_digest(&entry.response);
            mutated_entries += 1;
        }
    }
    assert_eq!(
        mutated_entries, 1,
        "exactly the block-1002 entry was mutated"
    );
    let s2_probe = replay_probe(
        s2_name,
        &ScenarioSeed::S2,
        SEED_TOKEN_REVISION,
        None,
        Some(mutated),
    );
    assert_eq!(
        scalar(
            &s2_probe.db_path,
            "SELECT COUNT(*) FROM aave_v3_assets WHERE v_token_revision = 7"
        ),
        1,
        "s2 probe: the block-1002 asset carries the MUTATED revision — the \
         stale pre-upgrade memo value (2) was not served across the boundary"
    );
    let dumped = dump_tables_golden_json(
        &rusqlite::Connection::open(&s2_probe.db_path).unwrap(),
        DUMP_TABLES,
    )
    .unwrap();
    let committed_dump = std::fs::read_to_string(dump_golden_path(s2_name)).unwrap();
    assert_ne!(
        dumped, committed_dump,
        "s2 probe: the mutated post-upgrade answer diverges from the committed \
         golden — the dump gate is LOUD under this mutation (the demonstrated red)"
    );

    // ── probe 3 (s3 negative): the deprecation premise is load-bearing ───
    // Seed the asset revision AT the deprecation value: the pre-pass takes
    // the V4+ snapshot path, the recorded discount call goes unserved, and
    // the rt gate reads 6 — the fixture's premise (revision < 4) is what
    // keeps the option-(a) shape executable.
    let s3_probe = replay_probe(
        s3_name,
        &ScenarioSeed::S3,
        GHO_DISCOUNT_DEPRECATION_REVISION,
        None,
        None,
    );
    assert_eq!(
        s3_probe.served.served, 6,
        "s3 probe: at the deprecation revision the recorded discount call drops"
    );
    assert_eq!(
        served_eth_calls(&s3_probe.served),
        0,
        "s3 probe: the V4+ snapshot path issues no discount RPC"
    );
}

//! The wave-5 config-dispatch span replay gate: the three spans the recorder
//! captured (`generate_aave_config_windows`) replay through the REAL
//! `run_aave_update_on_db` single-chunk loop over the committed cassette
//! BYTES and must reproduce BOTH committed SQL goldens byte-for-byte, with
//! the chunk-committed count and the served round-trip count pinned as
//! independent literals.
//!
//! The captures are live chain answers: each recorded `eth_call` is the
//! node's real answer at its pinned block, and each recorded `getLogs` is the
//! span's real log set. The replay re-issues the updater's exact RPC surface
//! over the seed manifest's harness DB; the statement-ledger + DB-dump goldens
//! pin the SQL trace and the end state.
//!
//! Per-span teeth (the config-dispatch surfaces these spans pin):
//!
//! `w5_aave_pool_data_provider_updated` — the block 20,398,674 transaction
//! rotates the Pool implementation, the configurator implementation, and the
//! data provider in one transaction. The two `*_REVISION()` reads resolve on
//! the NEW implementation addresses and land in `aave_v3_contracts.revision`
//! (the parity gate: revision only, address unchanged); the
//! `PoolDataProviderUpdated` apply takes the update-by-old-address path and
//! rewrites the data-provider row's address. Mutating a recorded revision
//! answer moves exactly that row's revision column.
//!
//! `w5_aave_address_set_registration` — the block 20,977,092 transaction
//! rotates the Pool implementation and registers a fresh `AddressSet` id
//! (`MOCK_STABLE_DEBT`, `old == 0`): the decode names the contract and the
//! apply inserts the row. The committed dump carries the new row; the ledger
//! carries its insert statement.
//!
//! `w5_aave_bootstrap_proxy_created` — the deployment emitted the two
//! resolvable proxy creations (the `POOL` and `POOL_CONFIGURATOR` proxies)
//! with their paired `PoolUpdated`/`PoolConfiguratorUpdated` at blocks
//! 16,291,127 and 16,291,130. The proxy creation resolves the implementation
//! and issues a `POOL_REVISION()`/`CONFIGURATOR_REVISION()` read on that
//! implementation, at the transaction block; the paired `*Updated` event
//! reads the SAME implementation at the SAME block, so the two share one memo
//! entry and the cassette carries exactly one `eth_call` per contract. The
//! committed cassette pins both calls (their targets are the two
//! implementation addresses) and the dump pins the revisions they produced.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::panic
)]

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use degenbot_aave::updater::{run_aave_update_on_db, NoProgress, RunSpec, VerifyPolicy};
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

/// The two bootstrap proxy implementations the `ProxyCreated` resolutions
/// read: the `POOL` implementation (block 16,291,127) and the
/// `POOL_CONFIGURATOR` implementation (block 16,291,130). The paired
/// `*Updated` events read the same addresses at the same blocks.
const BOOTSTRAP_POOL_IMPL: &str = "0xfcc00a1e250644d89af0df661bc6f04891e21585";
const BOOTSTRAP_CONFIGURATOR_IMPL: &str = "0xfda7ffa872bdc906d43079ea134ebc9a511db0c2";

/// The block-20,398,674 rotation targets: the new Pool implementation and the
/// new configurator implementation the `*Updated` reads resolve on.
const PDP_POOL_IMPL: &str = "0x34339f94350ec5274ea44d0c37dae9e968c44081";
const PDP_CONFIGURATOR_IMPL: &str = "0x419226e0ad27f3b2019123f7246a364622b018e5";

/// The block-20,977,092 new Pool implementation the `PoolUpdated` reads.
const AS_POOL_IMPL: &str = "0xef434e4573b90b6ecd4a00f4888381e4d0cc5ccd";

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
    format!("{}tests/fixtures/cassettes/wave5/{name}.json", home())
}

fn seed_path(name: &str) -> String {
    format!(
        "{}tests/fixtures/sql_goldens/wave5/{name}.seed.json",
        home()
    )
}

fn ledger_golden_path(name: &str) -> String {
    format!(
        "{}tests/fixtures/sql_goldens/wave5/{name}.statement-ledger.json",
        home()
    )
}

fn dump_golden_path(name: &str) -> String {
    format!(
        "{}tests/fixtures/sql_goldens/wave5/{name}.db-dump.json",
        home()
    )
}

/// Regenerate-and-diff (ADR-068 D4): with `REGENERATE_SQL_GOLDENS=1` the gate
/// writes the golden instead of comparing — the pipeline that writes the
/// artifact is the same one the gate runs, so a stale comparison is theater.
fn regen_requested() -> bool {
    std::env::var("REGENERATE_SQL_GOLDENS").is_ok_and(|v| v != "0")
}

fn check_or_regen(name: &str, produced: &str, golden_path: &str) {
    if regen_requested() {
        std::fs::write(golden_path, produced).expect("write the regenerated golden");
        eprintln!("regenerated {name} at {golden_path}");
    } else {
        let committed = std::fs::read_to_string(golden_path)
            .unwrap_or_else(|e| panic!("the committed {name} golden must exist: {e}"));
        assert_eq!(
            produced, committed,
            "{name} drift: regenerate-and-diff must be byte-identical \
             (ADR-068 D4); if the chunk loop's SQL surface legitimately \
             changed, re-run with REGENERATE_SQL_GOLDENS=1"
        );
    }
}

/// The per-span independent literals: the committed cassette's served
/// round-trip count and the run's committed chunk count. A regression that
/// changes the RPC surface or the chunk boundaries fails these before the
/// byte-diff consults a golden.
struct SpanExpectations {
    name: &'static str,
    chunks: usize,
    served: u64,
    get_logs: u64,
    eth_calls: u64,
}

const SPANS: &[SpanExpectations] = &[
    SpanExpectations {
        name: "w5_aave_pool_data_provider_updated",
        chunks: 1,
        served: 8,
        get_logs: 6,
        eth_calls: 2,
    },
    SpanExpectations {
        name: "w5_aave_address_set_registration",
        chunks: 1,
        served: 8,
        get_logs: 6,
        eth_calls: 2,
    },
    SpanExpectations {
        name: "w5_aave_bootstrap_proxy_created",
        chunks: 1,
        // The bootstrap span's reserve set is empty, so the scaled-token pass
        // issues no `getLogs` (the empty token set short-circuits); five
        // `getLogs` passes plus the two proxy-creation revision reads.
        served: 7,
        get_logs: 5,
        eth_calls: 2,
    },
];

/// One span's seed manifest (the recorder's substrate + cursor).
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

/// Replay one span's committed cassette through the chunk loop over the
/// seeded harness DB + statement ledger, gate BOTH goldens byte-for-byte,
/// and return the serving snapshot + end-state handle.
fn replay_and_gate(name: &str, expect: &SpanExpectations) -> ReplayRun {
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
        RunSpec {
            chain_id: CHAIN_ID,
            market_id,
            to_block: Some(seed.to_block),
            chunk_size: seed.chunk_size,
            max_block_range: 10_000,
            provider,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(NoProgress),
            verify: VerifyPolicy::default(),
            max_chunks: None,
        },
    )
    .unwrap_or_else(|e| panic!("{name}: the replayed span must commit cleanly: {e}"));
    assert_eq!(
        report.chunks_committed, expect.chunks,
        "{name}: the recorded span commits the pinned chunk count"
    );

    let records = ledger.records().expect("the capture session is armed");
    let ledger_json = ledger_golden_json(&records);
    check_or_regen(name, &ledger_json, &ledger_golden_path(name));

    let conn = ledger.db().lock();
    let dump_json = dump_tables_golden_json(&conn, DUMP_TABLES).expect("dump");
    drop(conn);
    check_or_regen(name, &dump_json, &dump_golden_path(name));

    let db_path = dir.keep().join("golden.db");
    ReplayRun {
        served: transport.served_snapshot(),
        db_path,
    }
}

/// A probe replay whose MUTATION is expected to make the run diverge: no
/// golden gates, but the ledger trace + end-state handle come back so the
/// probe can compare against the committed golden.
struct ProbeRun {
    served: ServedSnapshot,
    db_path: std::path::PathBuf,
    ledger: String,
}

fn replay_probe(name: &str, seed: &SeedManifest, cassette: Cassette) -> ProbeRun {
    let transport = CassetteReplayTransport::new(cassette);
    let provider = transport.as_alloy_provider();
    let dir = TempDir::new().unwrap();
    let (db_path, market_id) = seeded_db(dir.path(), "probe.db", seed);
    let (ledger, _state) = LedgerDb::open_for_writes(&db_path).unwrap();
    run_aave_update_on_db(
        ledger.db(),
        RunSpec {
            chain_id: CHAIN_ID,
            market_id,
            to_block: Some(seed.to_block),
            chunk_size: seed.chunk_size,
            max_block_range: 10_000,
            provider,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(NoProgress),
            verify: VerifyPolicy::default(),
            max_chunks: None,
        },
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

fn contract_revision(db_path: &Path, name: &str) -> i64 {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.query_row(
        "SELECT revision FROM aave_v3_contracts WHERE name = ?1",
        rusqlite::params![name],
        |row| row.get(0),
    )
    .unwrap()
}

fn contract_address(db_path: &Path, name: &str) -> String {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.query_row(
        "SELECT address FROM aave_v3_contracts WHERE name = ?1",
        rusqlite::params![name],
        |row| row.get(0),
    )
    .unwrap()
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

fn load_cassette(name: &str) -> Cassette {
    let bytes = std::fs::read(cassette_path(name)).unwrap();
    Cassette::from_json_bytes(&bytes).unwrap()
}

#[test]
#[expect(clippy::too_many_lines)]
fn wave5_config_spans_replay_to_the_committed_goldens() {
    // ── the committed surfaces (pinned before any replay) ───────────────
    for expect in SPANS {
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

    // ── the drift-gated replays ─────────────────────────────────────────
    for expect in SPANS {
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
            "{}: the pinned getLogs pass count",
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

    // ── the data-provider span: the full `*Updated` triple ──────────────
    //
    // The committed dump pins the two revision updates (revision only, the
    // parity gate) and the data-provider address rewrite. Both revision reads
    // resolve on the NEW implementation; the data-provider apply takes the
    // update-by-old-address path (the old row was seeded at the cursor).
    let pdp = SPANS[0].name;
    let pdp_committed = load_cassette(pdp);
    for impl_addr in [PDP_POOL_IMPL, PDP_CONFIGURATOR_IMPL] {
        assert_eq!(
            pdp_committed
                .entries
                .keys()
                .filter(|k| k.contains("eth_call")
                    && !k.contains("eth_getLogs")
                    && k.contains(impl_addr))
                .count(),
            1,
            "PDP: exactly one `*Updated` revision read on {impl_addr}"
        );
    }
    let pdp_run = replay_and_gate(pdp, &SPANS[0]);
    assert_eq!(
        contract_revision(&pdp_run.db_path, "POOL"),
        4,
        "PDP: the PoolUpdated revision landed"
    );
    assert_eq!(
        contract_revision(&pdp_run.db_path, "POOL_CONFIGURATOR"),
        3,
        "PDP: the PoolConfiguratorUpdated revision landed"
    );
    assert_eq!(
        contract_address(&pdp_run.db_path, "POOL_DATA_PROVIDER").to_lowercase(),
        "0x20e074f62ecbd8bc5e38211adcb6103006113a22",
        "PDP: the data-provider row's address was rewritten by old-address"
    );
    let pdp_ledger = std::fs::read_to_string(ledger_golden_path(pdp)).unwrap();
    assert!(
        ledger_statement_count(&pdp_ledger, "UPDATE aave_v3_contracts SET revision") >= 2,
        "PDP: the committed ledger pins the two revision updates"
    );

    // Probe: mutate the POOL_REVISION() answer (the NEW Pool implementation)
    // and the POOL_CONFIGURATOR revision follows it — the dump golden is LOUD,
    // the served count is unchanged (the same key answers).
    let pdp_seed = read_seed(pdp);
    let mut pdp_cassette = load_cassette(pdp);
    let pdp_mutated = mutate_one_eth_call(&mut pdp_cassette, PDP_POOL_IMPL, 9);
    assert_eq!(
        pdp_mutated, 1,
        "PDP: exactly the Pool revision answer mutated"
    );
    let pdp_probe = replay_probe(pdp, &pdp_seed, pdp_cassette);
    assert_eq!(
        pdp_probe.served.served, SPANS[0].served,
        "PDP probe: the mutated answer is served from the same key"
    );
    assert_eq!(
        contract_revision(&pdp_probe.db_path, "POOL"),
        9,
        "PDP probe: the mutated answer moves the Pool revision column"
    );
    assert_eq!(
        ledger_statement_count(&pdp_probe.ledger, "UPDATE aave_v3_contracts SET revision"),
        ledger_statement_count(&pdp_ledger, "UPDATE aave_v3_contracts SET revision"),
        "PDP probe: the mutated answer changes only the bound value, not the SQL surface"
    );

    // ── the AddressSet span: a fresh registration insert ────────────────
    //
    // No mainnet `AddressSet` rotation (`old != 0`) exists on the deployment,
    // so the S1 rotation arm rests on its unit test; the registration arm is
    // pinned here: the decode names the id and the apply inserts the row.
    let as_name = SPANS[1].name;
    let as_committed = load_cassette(as_name);
    assert_eq!(
        as_committed
            .entries
            .keys()
            .filter(|k| k.contains("eth_call")
                && !k.contains("eth_getLogs")
                && k.contains(AS_POOL_IMPL))
            .count(),
        1,
        "AddressSet: the paired `PoolUpdated` reads the new Pool implementation"
    );
    let as_run = replay_and_gate(as_name, &SPANS[1]);
    assert_eq!(
        contract_address(&as_run.db_path, "MOCK_STABLE_DEBT").to_lowercase(),
        "0x102633152313c81cd80419b6ecf66d14ad68949a",
        "AddressSet: the fresh registration inserted the named contract row"
    );
    let as_ledger = std::fs::read_to_string(ledger_golden_path(as_name)).unwrap();
    assert!(
        ledger_statement_count(&as_ledger, "INSERT INTO aave_v3_contracts") >= 1,
        "AddressSet: the committed ledger pins the registration insert"
    );

    // ── the bootstrap span: the two proxy creations share the memo ───────
    //
    // The `ProxyCreated` resolutions read the two implementation addresses at
    // blocks 16,291,127 / 16,291,130 — the SAME (implementation, selector,
    // block) keys the paired `*Updated` events read, so the cassette carries
    // exactly one `eth_call` per contract. Pin the two call targets.
    let boot = SPANS[2].name;
    let boot_cassette = load_cassette(boot);
    for impl_addr in [BOOTSTRAP_POOL_IMPL, BOOTSTRAP_CONFIGURATOR_IMPL] {
        assert_eq!(
            boot_cassette
                .entries
                .keys()
                .filter(|k| k.contains("eth_call")
                    && !k.contains("eth_getLogs")
                    && k.contains(impl_addr))
                .count(),
            1,
            "bootstrap: exactly one proxy-creation resolution read on {impl_addr}"
        );
    }
    let boot_run = replay_and_gate(boot, &SPANS[2]);
    assert_eq!(
        contract_revision(&boot_run.db_path, "POOL"),
        1,
        "bootstrap: the Pool revision from the proxy creation landed"
    );
    assert_eq!(
        contract_revision(&boot_run.db_path, "POOL_CONFIGURATOR"),
        1,
        "bootstrap: the configurator revision from the proxy creation landed"
    );
    assert_eq!(
        contract_address(&boot_run.db_path, "POOL").to_lowercase(),
        "0x87870bca3f3fd6335c3f4ce8392d69350b4fa4e2",
        "bootstrap: the proxy-creation resolution named the Pool proxy address"
    );

    // Probe: mutate the configurator revision answer (the SAME key the proxy
    // creation and its paired `PoolConfiguratorUpdated` share) — the update
    // lands under the mutated value, so the dump golden is LOUD.
    let boot_seed = read_seed(boot);
    let mut boot_cassette = load_cassette(boot);
    let boot_mutated = mutate_one_eth_call(&mut boot_cassette, BOOTSTRAP_CONFIGURATOR_IMPL, 7);
    assert_eq!(
        boot_mutated, 1,
        "bootstrap: exactly the configurator revision answer mutated"
    );
    let boot_probe = replay_probe(boot, &boot_seed, boot_cassette);
    assert_eq!(
        contract_revision(&boot_probe.db_path, "POOL_CONFIGURATOR"),
        7,
        "bootstrap probe: the shared memo entry moves the configurator revision"
    );
    assert_eq!(
        ledger_statement_count(&boot_probe.ledger, "UPDATE aave_v3_contracts SET revision"),
        ledger_statement_count(
            &std::fs::read_to_string(ledger_golden_path(boot)).unwrap(),
            "UPDATE aave_v3_contracts SET revision"
        ),
        "bootstrap probe: the mutated answer changes only the bound value, not the SQL surface"
    );
}

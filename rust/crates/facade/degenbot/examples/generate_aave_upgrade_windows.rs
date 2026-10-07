//! Record the wave-4 Aave upgrade-span cassettes against the live chain, or
//! run the drift gate.
//!
//! Each window drives the REAL `run_aave_update` multi-chunk loop over the
//! recording transport (`degenbot_rpc::cassette::RecordingTransport`), so the
//! cassette carries exactly the RPC surface an offline replay re-issues. The
//! warm-boot substrate comes from the chain's current reserve set
//! (`resolve_aave_substrate` reads the Pool's `getReservesList()`), not from a
//! window's sparse Pool logs: an upgrade window's Pool pass carries almost no
//! logs, and a span-log-derived reserve set would leave the harness DB empty.
//!
//! Per window: seed a throwaway harness DB, run the chunk loop over the
//! recorder, flush the cassette's canonical bytes; then replay those bytes
//! through the statement-ledger wrapper to emit the per-chunk statement
//! ledger + final canonical DB dump. Every window is recorded TWICE and the
//! two passes must be byte-identical (cassette, ledger, dump) — a
//! nondeterministic capture is a defect.
//!
//! Usage:
//!
//!     cargo run --locked --manifest-path rust/Cargo.toml -p degenbot \
//!         --features degenbot/sql-ledger \
//!         --example generate_aave_upgrade_windows
//!
//! `--check <cassette_home> <golden_home>` regenerates and exits 1 on drift.
//! `--node <uri>` overrides the default endpoint.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::expect_used,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use alloy::primitives::Address;
use degenbot::aave::updater::aave_fetch::resolve_aave_substrate;
use degenbot::aave::updater::{run_aave_update, run_aave_update_on_db, NoProgress};
use degenbot::db::DegenbotDb;
use degenbot::rpc::cassette::{
    rfc3339_utc, Cassette, CassetteProvenance, CassetteSpan, RecordingTransport,
};
use degenbot::rpc::provider::AlloyProvider;
use degenbot_db::sql_ledger::{dump_tables_golden_json, ledger_golden_json, LedgerDb};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use tempfile::TempDir;

/// The default recording endpoint: an archive reth serving full-history
/// `eth_getLogs` + historical `eth_call`/`eth_getStorageAt`. `--node`
/// overrides it; `DEGENBOT_RPC_HTTP_CHAINID_1` overrides the default too (the
/// configured-node resolution), so the recorder is not pinned to one host.
const DEFAULT_NODE: &str = "http://localhost:8545/";
/// The chain every window records (mainnet).
const CHAIN_ID: i64 = 1;
/// The provenance source string written into every cassette (fixed so
/// regeneration is byte-identical; the recording node is the public gateway).
const PROVENANCE_SOURCE: &str = "reth/v2.7.0-3d592ec/x86_64-unknown-linux-gnu";
/// The market name the seed stamps (the on-chain `getMarketId()` return).
const MARKET_NAME: &str = "Aave Ethereum Market";
/// The Aave V3 Ethereum bootstrap block (the market row's seed stamp).
const BOOTSTRAP_BLOCK: i64 = 16_291_070;
/// The `PoolAddressesProvider` (the substrate resolver's entry point).
const POOL_ADDRESS_PROVIDER: &str = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
/// The chain's GHO token (the reserve whose underlying it is carries the
/// GHO-vToken FK link).
const GHO_TOKEN: &str = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";

/// The tables one Aave chunk apply touches, in the dump's fixed
/// (alphabetical) order.
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

/// One recording window: the market cursor, the loop chunk size, and the
/// run's inclusive end block.
struct Window {
    name: &'static str,
    /// The seeded `last_update_block`; the run starts at `cursor + 1`.
    cursor: u64,
    chunk_size: u64,
    to_block: u64,
}

/// The five wave-4 windows, in the recording order (cheapest first).
const WINDOWS: &[Window] = &[
    Window {
        name: "w4_aave_pre_upgrade_control",
        cursor: 22_839_352,
        chunk_size: 5,
        to_block: 22_839_361,
    },
    Window {
        name: "w4_aave_atoken_upgrade_in_chunk",
        cursor: 18_870_590,
        chunk_size: 5,
        to_block: 18_870_599,
    },
    Window {
        name: "w4_aave_upgrade_plus_same_block_config",
        cursor: 23_088_580,
        chunk_size: 5,
        to_block: 23_088_589,
    },
    Window {
        name: "w4_aave_multi_upgrade_one_chunk",
        cursor: 24_247_923,
        chunk_size: 5,
        to_block: 24_247_931,
    },
    Window {
        name: "w4_aave_gho_deprecation_at_chunk_boundary",
        cursor: 22_839_357,
        chunk_size: 5,
        to_block: 22_839_366,
    },
];

/// One window's generation products.
struct WindowArtifacts {
    name: &'static str,
    requests: usize,
    cassette: Vec<u8>,
    ledger: String,
    dump: String,
    /// The seed manifest: the substrate + cursor the offline replay seeds
    /// from. Without it the replay cannot reconstruct the exact harness DB
    /// (block-resolved contract addresses + the chain-current reserve set).
    seed: String,
}

/// Seed the harness DB the chunk loop bootstraps from: the market row (at the
/// bootstrap stamp), the `POOL_ADDRESS_PROVIDER` row, the warm-boot contract
/// rows with their chain-resolved revisions, the cursor, and the chain-current
/// reserve set (each reserve's erc20 rows + `aave_v3_assets` row). The GHO
/// reserve carries the GHO-vToken FK link, so the discount pre-pass resolves
/// the GHO vToken. Returns `(db path, market id)`.
fn seed_db(
    dir: &Path,
    file_name: &str,
    substrate: &degenbot::aave::updater::aave_fetch::AaveSubstrate,
    cursor: u64,
) -> (PathBuf, i64) {
    let path = dir.join(file_name);
    let (db, _state) = DegenbotDb::open_for_writes(&path).expect("seed db opens");
    let gho: Address = GHO_TOKEN.parse().expect("GHO literal");
    let market_id = {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (chain_id, name, active, last_update_block) \
             VALUES (?1, ?2, 1, ?3)",
            rusqlite::params![CHAIN_ID, MARKET_NAME, BOOTSTRAP_BLOCK],
        )
        .expect("market row inserts");
        let market_id = conn.last_insert_rowid();
        DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            &conn,
            market_id,
            "POOL_ADDRESS_PROVIDER",
            POOL_ADDRESS_PROVIDER,
            None,
        )
        .expect("AP row inserts");
        for (name, address, revision) in [
            ("POOL", substrate.pool, Some(substrate.pool_revision)),
            (
                "POOL_CONFIGURATOR",
                substrate.configurator,
                Some(substrate.configurator_revision),
            ),
            ("PRICE_ORACLE", substrate.price_oracle, None),
            ("POOL_DATA_PROVIDER", substrate.data_provider, None),
        ] {
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &conn,
                market_id,
                name,
                &address.to_checksum(None),
                revision.map(|r| i64::try_from(r).expect("revision fits i64")),
            )
            .expect("contract row inserts");
        }
        DegenbotDb::set_market_last_update_block_on_conn(
            &conn,
            market_id,
            i64::try_from(cursor).expect("cursor fits i64"),
        )
        .expect("cursor stamps");
        for reserve in &substrate.reserves {
            let underlying_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                CHAIN_ID,
                &reserve.underlying.to_checksum(None),
                None,
                None,
                None,
            )
            .expect("underlying row inserts");
            let a_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                CHAIN_ID,
                &reserve.a_token.to_checksum(None),
                None,
                None,
                None,
            )
            .expect("aToken row inserts");
            let v_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                CHAIN_ID,
                &reserve.v_token.to_checksum(None),
                None,
                None,
                None,
            )
            .expect("vToken row inserts");
            let gho_link = if reserve.underlying == gho {
                Some(
                    DegenbotDb::get_or_create_gho_token_on_conn(
                        &conn,
                        CHAIN_ID,
                        &gho.to_checksum(None),
                    )
                    .expect("gho row inserts"),
                )
            } else {
                None
            };
            DegenbotDb::apply_reserve_initialized_on_conn(
                &conn,
                market_id,
                underlying_id,
                a_token_id,
                // The chain's aToken/vToken revision at the window is
                // resolved by the config dispatch from the recorded
                // `Upgraded` answers; the seeded value is the pre-window era
                // revision every non-upgraded asset keeps.
                1,
                v_token_id,
                1,
                None,
                gho_link,
            )
            .expect("asset row inserts");
        }
        market_id
    };
    (path, market_id)
}

/// The recording pass: seed, resolve the substrate over a side channel, run
/// the REAL chunk loop over the recording transport, flush the canonical
/// cassette bytes, then replay those bytes through the ledger for the goldens.
fn record_window(node_uri: &str, window: &Window) -> WindowArtifacts {
    let ap: Address = POOL_ADDRESS_PROVIDER.parse().expect("AP literal");
    let runtime = degenbot::core::runtime::get_runtime();
    // Side-channel substrate resolution (NOT recorded): the chain-current
    // reserve set + the contract addresses + revisions.
    let substrate = runtime.block_on(async {
        let side = AlloyProvider::new(node_uri, 3)
            .await
            .expect("side-channel provider");
        resolve_aave_substrate(&side, ap, Some(window.cursor))
            .await
            .expect("substrate resolution")
    });
    assert!(
        !substrate.reserves.is_empty(),
        "{}: the chain-current reserve set must be non-empty",
        window.name
    );

    let recorder = runtime
        .block_on(RecordingTransport::connect(node_uri))
        .expect("recording transport");
    let dir = TempDir::new().expect("temp dir");
    let (db_path, market_id) = seed_db(dir.path(), "record.db", &substrate, window.cursor);
    let report = run_aave_update(
        &db_path,
        CHAIN_ID,
        market_id,
        Some(window.to_block),
        window.chunk_size,
        recorder.as_alloy_provider(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        false,
        None,
        false,
        None,
    )
    .unwrap_or_else(|e| panic!("{}: recording run must commit cleanly: {e}", window.name));
    println!(
        "  {}: {} events, {} chunks",
        window.name, report.total_events_applied, report.chunks_committed
    );
    let requests = recorder.recorded_len();
    let cassette = recorder.cassette(
        u64::try_from(CHAIN_ID).expect("chain id fits u64"),
        CassetteProvenance {
            source: PROVENANCE_SOURCE.to_string(),
            // Fixed epoch timestamp: the cassette is committed, so the
            // capture time must not vary between regenerations.
            recorded_at: rfc3339_utc(0),
            span: CassetteSpan {
                from_block: window.cursor + 1,
                to_block: window.to_block,
            },
        },
    );
    let cassette_bytes = cassette.canonical_bytes().expect("canonical bytes");

    // The golden pass: replay the cassette bytes through the statement ledger.
    let parsed = Cassette::from_json_bytes(&cassette_bytes).expect("reparse");
    let replay = CassetteReplayTransport::new(parsed).as_alloy_provider();
    let dir2 = TempDir::new().expect("golden temp dir");
    let (db2, market_id2) = seed_db(dir2.path(), "golden.db", &substrate, window.cursor);
    let (ledger, _state) = LedgerDb::open_for_writes(&db2).expect("ledger opens");
    run_aave_update_on_db(
        ledger.db(),
        CHAIN_ID,
        market_id2,
        Some(window.to_block),
        window.chunk_size,
        replay,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        false,
        None,
        false,
        None,
    )
    .unwrap_or_else(|e| panic!("{}: golden replay must commit cleanly: {e}", window.name));
    let records = ledger.records().expect("the capture session is armed");
    let ledger_json = ledger_golden_json(&records);
    let conn = ledger.db().lock();
    let dump_json = dump_tables_golden_json(&conn, DUMP_TABLES).expect("dump");
    drop(conn);

    WindowArtifacts {
        name: window.name,
        requests,
        cassette: cassette_bytes,
        ledger: ledger_json,
        dump: dump_json,
        seed: seed_manifest(&substrate, window),
    }
}

/// Serialize the seed manifest: the substrate (block-resolved contract
/// addresses + revisions + the reserve set) and the window's cursor/chunk/to
/// params. The offline replay reads it to rebuild the harness DB.
fn seed_manifest(
    substrate: &degenbot::aave::updater::aave_fetch::AaveSubstrate,
    window: &Window,
) -> String {
    let reserves: Vec<serde_json::Value> = substrate
        .reserves
        .iter()
        .map(|r| {
            serde_json::json!({
                "underlying": r.underlying.to_checksum(None),
                "a_token": r.a_token.to_checksum(None),
                "v_token": r.v_token.to_checksum(None),
            })
        })
        .collect();
    let value = serde_json::json!({
        "name": window.name,
        "cursor": window.cursor,
        "chunk_size": window.chunk_size,
        "to_block": window.to_block,
        "pool": substrate.pool.to_checksum(None),
        "configurator": substrate.configurator.to_checksum(None),
        "price_oracle": substrate.price_oracle.to_checksum(None),
        "data_provider": substrate.data_provider.to_checksum(None),
        "pool_revision": substrate.pool_revision,
        "configurator_revision": substrate.configurator_revision,
        "reserves": reserves,
    });
    let mut bytes = serde_json::to_vec_pretty(&value).expect("seed manifest serializes");
    bytes.push(b'\n');
    String::from_utf8(bytes).expect("seed manifest is utf8")
}

fn generate_all(node_uri: &str) -> Vec<WindowArtifacts> {
    WINDOWS.iter().map(|w| record_window(node_uri, w)).collect()
}

fn home() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..")
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let check = args.iter().any(|a| a == "--check");
    let node_uri = args.iter().position(|a| a == "--node").map_or_else(
        || {
            std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
                .unwrap_or_else(|_| DEFAULT_NODE.to_string())
        },
        |i| args[i + 1].clone(),
    );
    println!("wave-4 aave upgrade-span recorder (node {node_uri})");

    println!("pass 1 ...");
    let pass1 = generate_all(&node_uri);
    println!("pass 2 (determinism gate) ...");
    let pass2 = generate_all(&node_uri);
    for (a, b) in pass1.iter().zip(&pass2) {
        assert_eq!(a.name, b.name);
        assert_eq!(
            a.cassette, b.cassette,
            "{}: cassette not byte-identical across passes",
            a.name
        );
        assert_eq!(
            a.ledger, b.ledger,
            "{}: statement ledger not byte-identical",
            a.name
        );
        assert_eq!(a.dump, b.dump, "{}: db dump not byte-identical", a.name);
    }
    println!("determinism: two passes byte-identical across all windows");
    for a in &pass1 {
        println!(
            "  {}\n    cassette {} bytes, {} requests\n    ledger {} bytes\n    dump {} bytes",
            a.name,
            a.cassette.len(),
            a.requests,
            a.ledger.len(),
            a.dump.len(),
        );
    }

    let cassette_home = home().join("tests/fixtures/cassettes/wave4");
    let golden_home = home().join("tests/fixtures/sql_goldens/wave4");
    if check {
        let mut drifted = 0usize;
        for a in &pass1 {
            for (path, produced) in [
                (
                    cassette_home.join(format!("{}.json", a.name)),
                    a.cassette.as_slice(),
                ),
                (
                    golden_home.join(format!("{}.statement-ledger.json", a.name)),
                    a.ledger.as_bytes(),
                ),
                (
                    golden_home.join(format!("{}.db-dump.json", a.name)),
                    a.dump.as_bytes(),
                ),
                (
                    golden_home.join(format!("{}.seed.json", a.name)),
                    a.seed.as_bytes(),
                ),
            ] {
                match std::fs::read(&path) {
                    Ok(committed) if committed == produced => {}
                    Ok(_) => {
                        eprintln!("DRIFT: {}", path.display());
                        drifted += 1;
                    }
                    Err(e) => {
                        eprintln!("MISSING: {} ({e})", path.display());
                        drifted += 1;
                    }
                }
            }
        }
        if drifted == 0 {
            println!("--check: all committed wave-4 artifacts byte-identical");
            std::process::ExitCode::SUCCESS
        } else {
            eprintln!("--check: {drifted} artifact(s) drifted");
            std::process::ExitCode::FAILURE
        }
    } else {
        std::fs::create_dir_all(&cassette_home).expect("create cassette home");
        std::fs::create_dir_all(&golden_home).expect("create golden home");
        for a in &pass1 {
            std::fs::write(cassette_home.join(format!("{}.json", a.name)), &a.cassette)
                .expect("write cassette");
            std::fs::write(
                golden_home.join(format!("{}.statement-ledger.json", a.name)),
                &a.ledger,
            )
            .expect("write ledger golden");
            std::fs::write(
                golden_home.join(format!("{}.db-dump.json", a.name)),
                &a.dump,
            )
            .expect("write dump golden");
            std::fs::write(golden_home.join(format!("{}.seed.json", a.name)), &a.seed)
                .expect("write seed manifest");
        }
        println!(
            "wrote {} window(s) under {} + {}",
            pass1.len(),
            cassette_home.display(),
            golden_home.display()
        );
        std::process::ExitCode::SUCCESS
    }
}

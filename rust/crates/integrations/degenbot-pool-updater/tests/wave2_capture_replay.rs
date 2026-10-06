//! The wave-2 generated-capture replay gate : every capture the
//! node-free EVM-oracle generator manufactured
//! (`generate_evm_oracle_captures`) replays through the REAL `run_pool_update`
//! chunk loop and must reproduce BOTH committed SQL goldens byte-for-byte —
//! the same drift gate the live-recorded corpus runs under.
//!
//! The captures are execution products of the real canonical `UniswapV3Pool`
//! (deployed via the committed `V3CaptureHarness` artifact, drift-gated by
//! `tier3_harness_artifacts.rs`): real mint/burn/swap frames, real events,
//! real verification reads. The replay run re-issues the updater's exact RPC
//! surface per chunk (the chunk boundaries mirror the recording run's) and
//! the statement-ledger + DB-dump goldens pin the SQL trace and the end
//! state — including the branches the live corpus cannot schedule: the
//! drained-pool complement-delete and the zero-amount Mint skipped write.
//!
//! Scenario-specific outcome pins live at the bottom: they are the regression
//! captures' teeth (the 0x5a17f7ce-family bitmap counterexample, the
//! short-hex precision-rule round trip, the sign-extension surface).

#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use degenbot_db::sql_ledger::{dump_tables_golden_json, ledger_golden_json, LedgerDb};
use degenbot_pool_updater::{run_pool_update_on_db, NoProgress};
use degenbot_rpc::cassette::{verify_cassette_bytes, Cassette};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use tempfile::TempDir;

/// The capture harness's deterministic address — it IS the V3 factory role
/// (it deploys the real pool and announces it through the real `PoolCreated`
/// event), so the seeded exchange row's factory is this address. A pinned
/// independent literal: it changes only if the harness artifact is rebuilt.
const WAVE2_FACTORY: &str = "0xbd770416a3345f91e4b34576cb804a576fa48eb1";

/// One wave-2 scenario's replay shape (mirrors the generator's chunking).
struct Scenario {
    name: &'static str,
    chunks: &'static [(u64, u64)],
    verify_chunk: bool,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "w2_cl_cross_chunk_flip_flip_swap",
        chunks: &[(1000, 1001), (1002, 1003), (1004, 1004)],
        verify_chunk: true,
    },
    Scenario {
        name: "w2_bitmap_5a17f7ce_early_termination",
        chunks: &[(1000, 1004)],
        verify_chunk: true,
    },
    Scenario {
        name: "w2_short_hex_hand_rolled_logs",
        chunks: &[(1000, 1004)],
        verify_chunk: false,
    },
    Scenario {
        name: "w2_drained_pool_zero_mint",
        chunks: &[(1000, 1001), (1002, 1003), (1004, 1004)],
        verify_chunk: true,
    },
    Scenario {
        name: "w2_odd_spacing_negative_ticks_sign_extension",
        chunks: &[(1000, 1004)],
        verify_chunk: true,
    },
];

fn cassette_path(name: &str) -> String {
    format!(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../tests/fixtures/cassettes/wave2/{name}.json"
        ),
        name = name
    )
}

fn ledger_golden_path(name: &str) -> String {
    format!(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../tests/fixtures/sql_goldens/wave2/{name}.statement-ledger.json"
        ),
        name = name
    )
}

fn dump_golden_path(name: &str) -> String {
    format!(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../tests/fixtures/sql_goldens/wave2/{name}.db-dump.json"
        ),
        name = name
    )
}

/// The tables one pool-chunk apply touches (the replay suite's dump list).
const DUMP_TABLES: &[&str] = &[
    "erc20_tokens",
    "exchanges",
    "initialization_maps",
    "liquidity_positions",
    "managed_pool_initialization_maps",
    "managed_pool_liquidity_positions",
    "managed_pools",
    "pools",
    "uniswap_v2_pools",
    "uniswap_v3_pools",
    "uniswap_v4_pools",
];

/// A temp DB with the ACTIVE `uniswap_v3` exchange stamped at 999 (the
/// generator's seeding; the chunk runs advance the cursor from there).
fn seeded_db(dir: &Path, chain_id: i64, file_name: &str) -> std::path::PathBuf {
    let path = dir.join(file_name);
    let (db, _state) = degenbot_db::DegenbotDb::open_for_writes(&path).unwrap();
    let factory: alloy::primitives::Address = WAVE2_FACTORY.parse().unwrap();
    let exchange = db
        .upsert_exchange(chain_id, "uniswap_v3", factory, None)
        .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE exchanges SET active = 1, last_update_block = 999 WHERE id = ?1",
        rusqlite::params![exchange.id],
    )
    .unwrap();
    path
}

/// Replay one scenario's committed cassette through the chunk loop and gate
/// BOTH goldens byte-for-byte.
fn replay_and_gate(scenario: &Scenario, chain_id: i64) -> std::path::PathBuf {
    let bytes = std::fs::read(cassette_path(scenario.name))
        .unwrap_or_else(|e| panic!("{}: committed cassette must exist: {e}", scenario.name));
    verify_cassette_bytes(&bytes)
        .unwrap_or_else(|e| panic!("{}: drift gate RED: {e}", scenario.name));
    let cassette = Cassette::from_json_bytes(&bytes).expect("a valid v1 cassette");
    let provider = CassetteReplayTransport::new(cassette).as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let db = seeded_db(dir.path(), chain_id, "golden.db");
    let (ledger, _state) = LedgerDb::open_for_writes(&db).unwrap();
    for (from, to) in scenario.chunks {
        run_pool_update_on_db(
            ledger.db(),
            chain_id,
            Some(*to),
            to - from + 1,
            provider.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(NoProgress),
            scenario.verify_chunk,
            None,
            false,
        )
        .unwrap_or_else(|e| {
            panic!(
                "{}: the replay of chunk [{from}..{to}] must commit cleanly: {e}",
                scenario.name
            )
        });
    }

    let records = ledger.records().expect("the capture session is armed");
    let ledger_json = ledger_golden_json(&records);
    let committed_ledger = std::fs::read_to_string(ledger_golden_path(scenario.name))
        .unwrap_or_else(|e| panic!("{}: committed ledger golden must exist: {e}", scenario.name));
    assert_eq!(
        ledger_json, committed_ledger,
        "{}: statement-ledger drift — regenerate with REGENERATE_SQL_GOLDENS=1 only if \
         the chunk loop's SQL surface legitimately changed",
        scenario.name
    );

    let conn = ledger.db().lock();
    let dump_json = dump_tables_golden_json(&conn, DUMP_TABLES).expect("dump the touched tables");
    drop(conn);
    let committed_dump = std::fs::read_to_string(dump_golden_path(scenario.name))
        .unwrap_or_else(|e| panic!("{}: committed dump golden must exist: {e}", scenario.name));
    assert_eq!(
        dump_json, committed_dump,
        "{}: DB-dump drift — the end state diverges from the committed golden",
        scenario.name
    );
    dir.keep().join("golden.db")
}

fn row_count(db_path: &Path, table: &str) -> i64 {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// ONE serialized test: the statement-ledger session is a process-global
/// slot (only one `LedgerDb` may be armed at a time), so the scenario
/// replays — and the per-scenario outcome pins — run in sequence here.
#[test]
fn wave2_generated_captures_replay_to_the_committed_goldens() {
    for scenario in SCENARIOS {
        let kept = replay_and_gate(scenario, 1);

        match scenario.name {
            // (b) the 0x5a17f7ce-family counterexample: ALL boundary ticks of
            // the three positions (100-k, k = 1..=4 — the whole bitmap word 0)
            // must be tracked; a greedy word search that halts at the first
            // set bit loses three of them.
            "w2_bitmap_5a17f7ce_early_termination" => {
                assert_eq!(
                    row_count(&kept, "liquidity_positions"),
                    4,
                    "ticks 100, 200, 300, 400 (all p = k*100 in word 0) must all be tracked"
                );
                assert_eq!(
                    row_count(&kept, "initialization_maps"),
                    1,
                    "the whole OR-set lives in bitmap word 0"
                );
            }
            // (d) the delta-persist branches: the drained pool's live sets are
            // EMPTY (the complement-delete fired over a NON-empty drained set
            // — the branch a chunk-new pool can never produce) and the
            // zero-amount Mint wrote nothing.
            "w2_drained_pool_zero_mint" => {
                assert_eq!(
                    row_count(&kept, "liquidity_positions"),
                    0,
                    "the full burn drained the position — the delete-all branch ran"
                );
                assert_eq!(
                    row_count(&kept, "initialization_maps"),
                    0,
                    "the flip and its complement cancel — the map rows deleted"
                );
                assert_eq!(row_count(&kept, "pools"), 1, "the pool row itself persists");
            }
            // (c) the short-hex scenario records the hand-rolled Mint-shaped
            // log (real LOG opcodes, truncated data) and the apply must have
            // skipped it — no phantom pool — while the raw emitter logs ride
            // the cassette verbatim.
            "w2_short_hex_hand_rolled_logs" => {
                assert_eq!(
                    row_count(&kept, "pools"),
                    0,
                    "the truncated Mint log must decode-skip (no phantom pool)"
                );
                let bytes = std::fs::read(cassette_path(scenario.name)).unwrap();
                let cassette = Cassette::from_json_bytes(&bytes).unwrap();
                let raw_window = cassette
                    .entries
                    .iter()
                    .find(|(key, _)| key.contains("eth_getLogs") && key.contains("\"topics\":[]"))
                    .expect("the generator's raw no-topic log window is recorded");
                let response = serde_json::to_value(&raw_window.1.response).unwrap();
                let logs = response
                    .get("result")
                    .and_then(serde_json::Value::as_array)
                    .unwrap();
                let data_fields: Vec<&str> = logs
                    .iter()
                    .filter_map(|log| log.get("data").and_then(serde_json::Value::as_str))
                    .collect();
                for expected in ["0x00", "0x00000000", "0x01", "0x06fdde03", "0x0000"] {
                    assert!(
                        data_fields.contains(&expected),
                        "the non-minimal data field {expected:?} must round-trip byte-exactly, got {data_fields:?}"
                    );
                }
            }
            _ => {}
        }
    }
}

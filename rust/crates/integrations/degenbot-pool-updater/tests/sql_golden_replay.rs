//! Statement-ledger + canonical DB-dump goldens for the replayed pool chunk
//! (ADR-068 D3/D4; GLOSSARY "statement ledger").
//!
//! The workload is the SAME replayed run as `cassette_replay_run.rs`: the
//! real `run_pool_update` chunk loop over the committed cassette through the
//! cassette replay provider — zero network. Here the run's DB handle is the
//! statement-ledger wrapper ([`LedgerDb`]), so the goldens are its SQL trace
//! and post-run DB state:
//!
//! - **Ledger golden** — every statement one chunk apply runs, in order,
//!   normalized (whitespace collapsed, literals → `?`), with arg count and
//!   rows changed.
//! - **DB-dump golden** — the touched tables (pools + the v2/v3/v4 subclass
//!   tables + liquidity positions + initialization maps + the token rows the
//!   pool apply creates + the exchange rows carrying the chunk stamp), rows
//!   sorted, hex identifiers lowercased.
//!
//! **Drift gate (machine-emitted discipline, GLOSSARY "Verification idioms"):**
//! regenerate BOTH artifacts through the same wrapper/writer pipeline and
//! diff byte-identical against the committed files. The artifact home is the
//! repo-root corpus home's sibling directory `tests/fixtures/sql_goldens/` —
//! one home per artifact kind: `tests/fixtures/cassettes/` holds RPC
//! recordings; SQL/DB goldens are a distinct artifact kind with its own home.
//!
//! **Statement-count assertion:** [`EXPECTED_LEDGER_STATEMENTS`] is an
//! independent literal, so an N+1 regression (any added statement per chunk)
//! fails this plain `cargo test` run even before the byte-diff.
//!
//! **Negative probes (demonstrated, per artifact type):** mutate one committed
//! golden (drop a statement entry / flip one dump row) → this gate goes red;
//! see the task sign-off for the captured red output.
//!
//! **Regeneration:** `REGENERATE_SQL_GOLDENS=1 cargo test -p
//! degenbot-pool-updater --test sql_golden_replay` rewrites the committed
//! files through the same writer the gate compares with. Only run it when the
//! chunk loop's SQL surface legitimately changed.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::panic
)]

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use alloy::primitives::Address;
use degenbot_db::sql_ledger::{dump_tables_golden_json, ledger_golden_json, LedgerDb};
use degenbot_pool_updater::{run_pool_update_on_db, NoProgress};
use degenbot_rpc::cassette::Cassette;
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use tempfile::TempDir;

/// The committed golden capture the run replays (the corpus home's
/// `pool_update_chunk_26102622-26102626.json`).
const CASSETTE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/cassettes/pool_update_chunk_26102622-26102626.json"
);

/// The committed statement-ledger golden (SQL half of the golden capture).
const LEDGER_GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/sql_goldens/pool_update_chunk_26102622-26102626.statement-ledger.json"
);

/// The committed canonical DB-dump golden (post-run DB state).
const DUMP_GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/sql_goldens/pool_update_chunk_26102622-26102626.db-dump.json"
);

/// Independent literal: the exact number of statements one chunk apply runs
/// over this corpus. Any N+1-style regression (a query hoisted out of a loop
/// going back in, a per-row re-read) changes this and fails the plain test
/// run — before the byte-diff even consults the committed golden.
///
/// Perf B (23 → 21): the delta persist removed the two full-map complement
/// deletes (`DELETE … WHERE pool_id = ? AND tick NOT IN (?, ?)` and the
/// `initialization_maps` twin) — a chunk-new pool's drained set is empty, so
/// the delta path issues no delete at all (both removed statements carried
/// `rows_changed: 0` on this corpus). The two map READS changed SHAPE, not
/// count: the full-map SELECTs became the dirty-key IN-form
/// (`… WHERE pool_id = ? AND tick IN (?, ?)` — the Perf B read surface).
/// Classified in the task sign-off; the DB dump golden is byte-identical.
const EXPECTED_LEDGER_STATEMENTS: usize = 21;

/// The tables one pool-chunk apply touches, in the dump's fixed (alphabetical)
/// order. `exchanges` is the chunk's commit stamp; `erc20_tokens` the token
/// rows the pool upsert creates; the v4/managed tables stay in the list
/// (empty for this corpus) so a pool family accidentally writing them cannot
/// drift silently.
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

/// The Uniswap V3 factory the capture was recorded against (the recorder
/// example's documented `--factory` for this corpus file).
const V3_FACTORY: &str = "0x1F98431c8aD98523631AE4a59f267346ea31F984";

/// A temp DB with one ACTIVE `uniswap_v3` exchange stamped at `from - 1`, so
/// the run's fetch window is exactly the cassette's recorded span (mirrors
/// `cassette_replay_run.rs`'s seeding).
fn seeded_db(dir: &Path, chain_id: i64, from: u64, file_name: &str) -> std::path::PathBuf {
    let path = dir.join(file_name);
    let (db, _state) = degenbot_db::DegenbotDb::open_for_writes(&path).unwrap();
    let factory: Address = V3_FACTORY.parse().unwrap();
    let exchange = db
        .upsert_exchange(chain_id, "uniswap_v3", factory, None)
        .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE exchanges SET active = 1, last_update_block = ?1 WHERE id = ?2",
        rusqlite::params![i64::try_from(from - 1).unwrap(), exchange.id],
    )
    .unwrap();
    path
}

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

#[test]
fn pool_chunk_ledger_and_db_dump_match_the_committed_goldens() {
    let bytes = std::fs::read(CASSETTE_PATH).expect("the committed cassette must exist");
    let cassette = Cassette::from_json_bytes(&bytes).expect("a valid v1 cassette");
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let span = cassette.provenance.span;

    // D5 injection: the replay transport presents as a live AlloyProvider.
    let provider = CassetteReplayTransport::new(cassette).as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let path = seeded_db(dir.path(), chain_id, span.from_block, "golden.db");

    // The ledger wrapper IS the run's DB handle — the trace hooks ride the
    // connection the chunk loop actually uses (ADR-068 D3: the ledger is the
    // chunk apply's SQL trace, not a side-channel).
    let (ledger, _state) = LedgerDb::open_for_writes(&path).unwrap();
    let report = run_pool_update_on_db(
        ledger.db(),
        chain_id,
        Some(span.to_block),
        span.to_block - span.from_block + 1,
        provider,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        // The verification gate stays OFF (same posture as
        // `cassette_replay_run.rs` — the corpus records the fetch surface).
        false,
        None,
        false,
    )
    .expect("the replayed run must commit cleanly");
    assert_eq!(report.chunks_committed, 1, "one chunk apply");
    assert_eq!(report.total_pools_written, 1);

    // Gate 1: the statement-count assertion (independent literal — the plain
    // N+1 tripwire).
    let records = ledger.records().expect("the capture session is armed");
    assert_eq!(
        records.len(),
        EXPECTED_LEDGER_STATEMENTS,
        "statement count drifted — an N+1 regression added or removed a \
         statement per chunk apply"
    );

    // Gate 2: the ledger golden (byte-identical regen-and-diff).
    let ledger_json = ledger_golden_json(&records);
    check_or_regen("statement ledger", &ledger_json, LEDGER_GOLDEN_PATH);

    // Gate 3: the canonical DB-dump golden (byte-identical regen-and-diff)
    // over the SAME connection the run wrote through.
    let conn = ledger.db().lock();
    let dump_json = dump_tables_golden_json(&conn, DUMP_TABLES).expect("dump the touched tables");
    drop(conn);
    check_or_regen("db dump", &dump_json, DUMP_GOLDEN_PATH);
}

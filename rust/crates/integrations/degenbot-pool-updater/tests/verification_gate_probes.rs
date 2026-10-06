//! Verification-gate negative probes (ADR-068 D4; GLOSSARY "negative probe").
//!
//! The pre-commit on-chain-truth gate (`run_pool_update` with
//! `verify_chunk = true`) is the production coherence mechanism - and before
//! this suite it had NEVER been probed against known-bad state. Each probe
//! here deliberately faults a COMMITTED golden capture IN MEMORY (the
//! committed files stay untouched) and asserts the exact red the gate must
//! produce. A gate whose red path has not been demonstrated is not evidence.
//!
//! - **Probe A (the coherence gate):** mutate ONE recorded verification
//!   answer - a `ticks(int24)` liquidityGross word the gate reads back via
//!   Multicall3/degraded direct calls - in the
//!   `pool_verify_chunk_26102622-26102626` cassette. The replayed gated run
//!   MUST fail with `RunError::Verification`, the chunk MUST roll back (no
//!   pool rows, no liquidity rows), and `last_update_block` MUST stay
//!   `UNadvanced`. A lying chain answer cannot reach the DB.
//! - **Probe B:** drop one log entry from an in-memory copy of the
//!   `pool_update_chunk_26102622-26102626` cassette - the replayed run's
//!   outcome goes red (0 pools written vs the committed corpus's 1) and the
//!   regenerated DB dump diverges from the committed golden.
//! - **Probe C:** mutate one apply input (the recorded `PoolCreated` log's
//!   `fee` topic word) in memory - the run still commits (the decode maps
//!   the fee verbatim), but the regenerated DB dump goes red against the
//!   committed golden (`fee_token0`/`fee_token1` 3001 vs 3000).
//!
//! Each probe prints its observed red evidence to stderr (captured in the
//! task sign-off).

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
use degenbot_db::DegenbotDb;
use degenbot_pool_updater::{run_pool_update, NoProgress, RunError};
use degenbot_rpc::cassette::{entry_digest, verify_cassette_bytes, Cassette, CassetteResponse};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use tempfile::TempDir;

/// The verification-surface capture (recorded by
/// `record_updater_cassette --kind pool --verify`): the gated run's fetch
/// surface PLUS the gate's tick/bitmap reads. The aggregate3 Multicall3
/// attempt against this historical reth state reverted and the batch
/// degraded to individual `ticks()`/`tickBitmap()` calls - all recorded, so
/// the replay reproduces the exact read sequence the gate issued live.
const VERIFY_CASSETTE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/cassettes/pool_verify_chunk_26102622-26102626.json"
);

/// The pool-chunk corpus + its committed SQL goldens (probe B/C's target).
const POOL_CASSETTE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/cassettes/pool_update_chunk_26102622-26102626.json"
);
const DUMP_GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/sql_goldens/pool_update_chunk_26102622-26102626.db-dump.json"
);

/// The pinned span both cassettes cover.
const SPAN_FROM: u64 = 26_102_622;
const SPAN_TO: u64 = 26_102_626;

/// The pool the span's `PoolCreated` event created (the `RunError` names it
/// checksummed - an independent literal from the committed corpus).
const CREATED_POOL_CHECKSUMMED: &str = "0x1cD938dF5700D97B393dd9ad3bd5fF2Dd7fA8E13";

/// The `ticks(int24)` selector - the gate's per-tick read (Multicall3
/// degrades to direct calls against this historical state; the cassette
/// records both).
const TICKS_SELECTOR: &str = "0xf30dba93";

/// The Uniswap V3 factory the captures were recorded against.
const V3_FACTORY: &str = "0x1F98431c8aD98523631AE4a59f267346ea31F984";

/// The tables one pool-chunk apply touches (mirrors
/// `sql_golden_replay.rs`'s dump list).
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

/// A temp DB with one ACTIVE `uniswap_v3` exchange stamped at `from - 1`
/// (mirrors `cassette_replay_run.rs`'s seed).
fn seeded_db(dir: &Path, chain_id: i64, from: u64, file_name: &str) -> std::path::PathBuf {
    let path = dir.join(file_name);
    let (db, _state) = DegenbotDb::open_for_writes(&path).unwrap();
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

fn committed_last_update_block(path: &Path, chain_id: i64) -> i64 {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.query_row(
        "SELECT MAX(last_update_block) FROM exchanges WHERE chain_id = ?1 AND active = 1",
        [chain_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn committed_pool_count(path: &Path, chain_id: i64) -> i64 {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM pools WHERE chain = ?1",
        [chain_id],
        |row| row.get(0),
    )
    .unwrap()
}

/// Parse a cassette ledger key into `(method, params)`.
fn key_parts(key: &str) -> (String, serde_json::Value) {
    let parsed: serde_json::Value = serde_json::from_str(key).expect("a canonical ledger key");
    (
        parsed[0].as_str().expect("method string").to_string(),
        parsed[1].clone(),
    )
}

/// Rebuild a cassette with one entry's response replaced (the write digest is
/// recomputed so the in-memory cassette stays self-coherent; the replay
/// transport serves the mutated answer verbatim).
fn with_entry_response(
    cassette: &Cassette,
    target_key: &str,
    mutate: impl Fn(&CassetteResponse) -> CassetteResponse,
) -> Cassette {
    let mut entries = cassette.entries.clone();
    let entry = entries.get_mut(target_key).expect("target entry present");
    entry.response = mutate(&entry.response);
    entry.digest = entry_digest(&entry.response);
    Cassette {
        schema: cassette.schema.clone(),
        chain_id: cassette.chain_id,
        provenance: cassette.provenance.clone(),
        entries,
    }
}

/// The committed DB-dump golden as `(table -> (columns, rows))`.
fn committed_dump() -> std::collections::BTreeMap<String, (Vec<String>, Vec<serde_json::Value>)> {
    let raw = std::fs::read_to_string(DUMP_GOLDEN_PATH).expect("the committed dump golden");
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let mut out = std::collections::BTreeMap::new();
    for table in parsed["tables"].as_array().expect("tables array") {
        let name = table["table"].as_str().expect("table name").to_string();
        let columns = table["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().expect("column name").to_string())
            .collect();
        let rows = table["rows"].as_array().expect("rows").clone();
        out.insert(name, (columns, rows));
    }
    out
}

fn dump_tables(conn: &rusqlite::Connection) -> String {
    degenbot_db::sql_ledger::dump_tables_golden_json(conn, DUMP_TABLES).expect("dump")
}

fn table_rows<'a>(
    dump: &'a std::collections::BTreeMap<String, (Vec<String>, Vec<serde_json::Value>)>,
    table: &str,
) -> &'a Vec<serde_json::Value> {
    &dump.get(table).expect("table in dump").1
}

// ── Probe A: the coherence gate catches a lying chain answer ──────────────

#[test]
fn probe_a_mutated_verification_answer_rolls_the_chunk_back() {
    let bytes = std::fs::read(VERIFY_CASSETTE_PATH).expect("the committed cassette must exist");
    verify_cassette_bytes(&bytes).expect("the committed cassette must pass the drift gate");
    let cassette = Cassette::from_json_bytes(&bytes).unwrap();
    let chain_id = i64::try_from(cassette.chain_id).unwrap();

    // Locate ONE recorded verification answer - the first `ticks(int24)`
    // read (the gate's per-tick gross/net lookup) - and mutate its
    // liquidityGross word (the response's first 32-byte word) in memory.
    let ticks_key = cassette
        .entries
        .keys()
        .find(|key| {
            let (method, params) = key_parts(key);
            method == "eth_call"
                && params[0]["input"]
                    .as_str()
                    .is_some_and(|input| input.starts_with(TICKS_SELECTOR))
        })
        .expect("the cassette records the gate's ticks() reads")
        .clone();

    let mutated = with_entry_response(&cassette, &ticks_key, |response| {
        let CassetteResponse::Success { result } = response else {
            panic!("the ticks() entry replays a success");
        };
        let hex = result.as_str().expect("hex result");
        // gross = the first 32-byte word; flip its low nibble ('f' -> 'e'
        // for the recorded corpus answer 0x...15b143acff395a30a4f - any
        // other recorded value would differ by the same one-ulp shape).
        assert_eq!(&hex[64..66], "4f", "corpus-pinned gross low byte");
        let mutated_hex = format!("{}e{}", &hex[..65], &hex[66..]);
        CassetteResponse::Success {
            result: serde_json::Value::String(mutated_hex),
        }
    });
    assert_ne!(mutated.entries[&ticks_key], cassette.entries[&ticks_key]);

    // D5 injection: the mutated cassette answers the gated run.
    let provider = CassetteReplayTransport::new(mutated).as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let path = seeded_db(dir.path(), chain_id, SPAN_FROM, "probe-a.db");

    let err = run_pool_update(
        &path,
        chain_id,
        Some(SPAN_TO),
        SPAN_TO - SPAN_FROM + 1,
        provider,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        // THE GATE IS ON: the pre-commit on-chain-truth check.
        true,
        None,
        false,
    )
    .expect_err("a lying chain answer MUST fail the gated run");

    // (1) The exact red: RunError::Verification carrying the diverging pool,
    //     the verify-at block, and the named divergence.
    let RunError::Verification {
        pool,
        block_number,
        divergences,
    } = &err
    else {
        panic!("expected RunError::Verification, got {err:?}");
    };
    assert_eq!(pool, CREATED_POOL_CHECKSUMMED);
    assert_eq!(*block_number, SPAN_TO, "the gate reads at the chunk end");
    assert!(!divergences.is_empty(), "the divergence list names the lie");
    eprintln!("PROBE A red transcript: {err}");
    eprintln!("PROBE A divergences: {divergences:#?}");

    // (2) The chunk never reached the DB: since Perf A the verify RPC runs
    //     pre-transaction, so a RED never OPENS the chunk transaction (the
    //     pre-Perf-A shape was an open-transaction rollback - the observable
    //     contract is the same triple: error raised, zero rows, stamp
    //     unadvanced). Asserted via post-run state either way: no pool rows.
    assert_eq!(
        committed_pool_count(&path, chain_id),
        0,
        "the diverged chunk's writes must not be durable"
    );

    // (3) The stamp stayed UNadvanced (the restart invariant holds: the next
    //     run re-processes the same chunk).
    assert_eq!(
        committed_last_update_block(&path, chain_id),
        i64::try_from(SPAN_FROM - 1).unwrap(),
        "last_update_block must NOT advance through a verification rollback"
    );
}

// ── Probe B: a dropped log is a loud red in the run outcome + the dump ────

#[test]
fn probe_b_dropped_log_turns_the_run_outcome_and_dump_red() {
    let bytes = std::fs::read(POOL_CASSETTE_PATH).expect("the committed cassette must exist");
    verify_cassette_bytes(&bytes).expect("the committed cassette must pass the drift gate");
    let cassette = Cassette::from_json_bytes(&bytes).unwrap();
    let chain_id = i64::try_from(cassette.chain_id).unwrap();

    // The PoolCreated fetch entry (the getLogs keyed by the factory address).
    let created_key = cassette
        .entries
        .keys()
        .find(|key| {
            let (method, params) = key_parts(key);
            method == "eth_getLogs" && !params[0]["address"].is_null()
        })
        .expect("the cassette records the PoolCreated fetch")
        .clone();

    // Drop the span's single recorded log from the answer.
    let mutated = with_entry_response(&cassette, &created_key, |response| {
        let CassetteResponse::Success { result } = response else {
            panic!("the getLogs entry replays a success");
        };
        let logs = result.as_array().expect("log array");
        assert_eq!(logs.len(), 1, "the committed corpus carries one log");
        CassetteResponse::Success {
            result: serde_json::Value::Array(Vec::new()),
        }
    });

    let provider = CassetteReplayTransport::new(mutated).as_alloy_provider();
    let dir = TempDir::new().unwrap();
    let path = seeded_db(dir.path(), chain_id, SPAN_FROM, "probe-b.db");

    // Run outcome red: nothing decodes/applies, 0 pools written (vs the
    // committed corpus's 1 - an independent literal).
    let report = run_pool_update(
        &path,
        chain_id,
        Some(SPAN_TO),
        SPAN_TO - SPAN_FROM + 1,
        provider,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        false,
        None,
        false,
    )
    .expect("the run itself completes - the OUTCOME is the red");
    assert_eq!(
        report.total_pools_written, 0,
        "RED: the dropped log means the span's PoolCreated decode/apply \
         never runs (the committed corpus writes 1)"
    );
    eprintln!(
        "PROBE B red transcript: run over the log-dropped cassette wrote {} pools \
         (the committed golden corpus writes 1)",
        report.total_pools_written
    );

    // The regenerated dump diverges from the committed golden: the pool row
    // (+ its token rows) is missing entirely.
    let golden = committed_dump();
    let conn =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let produced_raw = dump_tables(&conn);
    let produced: serde_json::Value = serde_json::from_str(&produced_raw).unwrap();
    let produced_pools = produced["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["table"] == "pools")
        .map(|t| t["rows"].as_array().unwrap().len())
        .unwrap();
    let golden_pools = table_rows(&golden, "pools").len();
    assert_ne!(
        produced_raw,
        std::fs::read_to_string(DUMP_GOLDEN_PATH).unwrap(),
        "RED: the regenerated dump must diverge from the committed golden"
    );
    assert_eq!(produced_pools, 0, "the produced dump carries no pool row");
    assert_eq!(golden_pools, 1, "the committed dump golden carries one");
    eprintln!(
        "PROBE B dump diff: produced pools table has {produced_pools} rows, \
         the committed golden has {golden_pools}"
    );
}

// ── Probe C: a mutated apply input lands in the dump golden ───────────────

#[test]
fn probe_c_mutated_apply_input_turns_the_dump_golden_red() {
    let bytes = std::fs::read(POOL_CASSETTE_PATH).expect("the committed cassette must exist");
    verify_cassette_bytes(&bytes).expect("the committed cassette must pass the drift gate");
    let cassette = Cassette::from_json_bytes(&bytes).unwrap();
    let chain_id = i64::try_from(cassette.chain_id).unwrap();

    let created_key = cassette
        .entries
        .keys()
        .find(|key| {
            let (method, params) = key_parts(key);
            method == "eth_getLogs" && !params[0]["address"].is_null()
        })
        .expect("the cassette records the PoolCreated fetch")
        .clone();

    // Mutate ONE apply input: the PoolCreated log's `fee` topic word
    // (3000 = 0xbb8 -> 3001 = 0xbb9). The decode consumes it verbatim, so
    // the apply writes a DIFFERENT pool row. (The tickSpacing word is NOT
    // a legal mutation: the span's minted ticks are multiples of 60, and
    // flip_tick's documented Python-parity assert rejects the mismatch -
    // the fee is the clean apply-input knob.)
    let mutated = with_entry_response(&cassette, &created_key, |response| {
        let CassetteResponse::Success { result } = response else {
            panic!("the getLogs entry replays a success");
        };
        let mut log = result[0].clone();
        let fee_topic = log["topics"][3].as_str().expect("fee topic").to_string();
        assert_eq!(&fee_topic[64..], "b8", "corpus-pinned fee word");
        let mutated_topic = format!("{}9", &fee_topic[..65]);
        log["topics"][3] = serde_json::Value::String(mutated_topic);
        CassetteResponse::Success {
            result: serde_json::Value::Array(vec![log]),
        }
    });

    let provider = CassetteReplayTransport::new(mutated).as_alloy_provider();
    let dir = TempDir::new().unwrap();
    let path = seeded_db(dir.path(), chain_id, SPAN_FROM, "probe-c.db");

    // The run COMMITS (61 is a valid tick spacing) - the red is the dump.
    let report = run_pool_update(
        &path,
        chain_id,
        Some(SPAN_TO),
        SPAN_TO - SPAN_FROM + 1,
        provider,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        false,
        None,
        false,
    )
    .expect("the mutated input still decodes to a valid pool");
    assert_eq!(report.total_pools_written, 1);

    let conn =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let produced_raw = dump_tables(&conn);
    let committed_raw = std::fs::read_to_string(DUMP_GOLDEN_PATH).unwrap();
    assert_ne!(
        produced_raw, committed_raw,
        "RED: the mutated apply input must diverge the regenerated dump"
    );

    // Name the exact drift: the pool row's fee columns differ from the
    // committed golden.
    let golden = committed_dump();
    let find_field = |raw: &str, table: &str, column: &str| -> serde_json::Value {
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
        for t in parsed["tables"].as_array().unwrap() {
            if t["table"] == table {
                let columns: Vec<String> = t["columns"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| c.as_str().unwrap().to_string())
                    .collect();
                let idx = columns.iter().position(|c| c == column).unwrap();
                return t["rows"][0][idx].clone();
            }
        }
        panic!("table {table} missing");
    };
    let produced_fee = find_field(&produced_raw, "uniswap_v3_pools", "fee_token0");
    let golden_fee = {
        let (columns, rows) = golden.get("uniswap_v3_pools").expect("v3 table");
        let idx = columns.iter().position(|c| c == "fee_token0").unwrap();
        rows[0][idx].clone()
    };
    assert_ne!(
        produced_fee, golden_fee,
        "RED: the mutated fee changed the applied pool row"
    );
    eprintln!(
        "PROBE C dump diff: uniswap_v3_pools.fee_token0 produced {produced_fee}, \
         committed golden {golden_fee}"
    );
    let _ = table_rows(&golden, "pools").len();
}

//! The replay bench (GLOSSARY "replay bench", ADR-068 D6): the
//! per-chunk baseline measurement the perf program re-ranks against.
//!
//! One fixed workload per committed golden capture: the REAL chunk loop
//! (`run_pool_update_on_db` / `run_aave_update_on_db`) over the committed
//! cassette through the replay transport (D5 injection, zero network),
//! writing to a temp SQLite DB wrapped in the statement ledger ([`LedgerDb`]).
//! Per chunk it reports:
//!
//! - **RPC round trips + response bytes** — the ledger entries the transport
//!   served for the run ([`CassetteReplayTransport::served_snapshot`]).
//! - **SQL statement count + SQL µs** — the statement ledger's records and
//!   their summed profile times.
//! - **Stage wall times** — fetch / decode+compute / verify / apply, the
//!   plain `Instant` spans the run entries carry on their per-chunk progress
//!   reports (the telemetry chunk lands its own spans later).
//! - **Write-lock hold** — the span from `transaction()` open to
//!   commit/drop: the headline number for Perf A (hoist RPC out of the
//!   SQLite transaction).
//! - **chunks/sec** — from the median chunk wall time.
//!
//! A plain `harness = false` bench binary rather than a Criterion harness,
//! deliberately: the headline outputs are per-chunk COUNTERS (round trips,
//! bytes, statements) plus modest timing medians — small deterministic facts
//! a review quotes, not distributions for a statistics engine, and the
//! table must be one command's stdout (`just bench-updaters`), directly
//! quotable into the survey's Baseline section. Criterion's noise model adds
//! warm-up and iteration machinery without adding decision value here. The
//! binary self-checks the counters for iteration stability (they are
//! workload facts, not timings) and builds a fresh transport, temp DB, and
//! ledger per iteration so the measurement overhead is transparent.
//!
//! The bench's own correctness is the counter gates: the corpus outcomes it
//! re-asserts per iteration (one chunk, the recorded pool/event counts) are
//! the same literals the replay suites gate on.

#![expect(clippy::print_stdout, clippy::print_stderr)]
// reason: the printed table IS the bench's interface (the recorder example's
// precedent); the harness fails loudly on a broken corpus.
#![expect(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
// reason: bench harness — an unconstructible prerequisite (missing corpus,
// fixture gap) must abort the run, not report garbage numbers.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use degenbot_aave::updater::{
    run_aave_update_on_db, AaveChunkProgress, ProgressSink as AaveProgressSink,
};
use degenbot_db::sql_ledger::LedgerDb;
use degenbot_db::DegenbotDb;
use degenbot_pool_updater::{
    run_pool_update_on_db, ChunkProgress, ProgressSink as PoolProgressSink,
};
use degenbot_rpc::cassette::{verify_cassette_bytes, Cassette};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use tempfile::TempDir;

/// Warm-up iterations (dropped: page cache, allocator, the ledger's first-arm
/// cost) before the measured ones.
const WARMUP_RUNS: usize = 2;

/// Measured iterations per workload; the table reports the median.
const MEASURED_RUNS: usize = 9;

// ── the corpus (the committed seed cassettes; the same files the replay
//    suites gate on) ─────────────────────────────────────────────────────────

const POOL_UPDATE_CASSETTE: &str = "pool_update_chunk_26102622-26102626.json";
const POOL_VERIFY_CASSETTE: &str = "pool_verify_chunk_26102622-26102626.json";
const AAVE_UPDATE_CASSETTE: &str = "aave_update_chunk_26130440-26130445.json";

fn corpus_path(file: &str) -> String {
    format!(
        "{}/../../../../tests/fixtures/cassettes/{file}",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn load_cassette(file: &str) -> Cassette {
    let path = corpus_path(file);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("corpus {}: {e}", path));
    verify_cassette_bytes(&bytes).expect("the committed cassette must pass the drift gate");
    Cassette::from_json_bytes(&bytes).unwrap()
}

// ── seeding (mirrors the replay suites' harness DBs row for row) ────────────

/// The Uniswap V3 factory the pool captures were recorded against.
const V3_FACTORY: &str = "0x1F98431c8aD98523631AE4a59f267346ea31F984";

/// The recorded span's committed outcome literals (the replay suites' gates —
/// the bench re-asserts them so a corpus/loop drift cannot masquerade as a
/// timing change).
const EXPECTED_POOL_POOLS: usize = 1;
const EXPECTED_AAVE_EVENTS: usize = 20;

/// A temp DB with one ACTIVE `uniswap_v3` exchange stamped at `from - 1`
/// (mirrors `cassette_replay_run.rs`).
fn seed_pool_db(dir: &Path, chain_id: i64, from: u64) -> std::path::PathBuf {
    let path = dir.join("bench.db");
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

/// The Aave V3 Ethereum bootstrap block (`activate_aave_market`'s fresh-seed
/// stamp; mirrors `aave_cassette_replay.rs`).
const AAVE_BOOTSTRAP_BLOCK: i64 = 16_291_070;

const POOL_ADDRESS_PROVIDER: &str = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
const POOL: &str = "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2";
const POOL_REVISION: i64 = 11;
const POOL_CONFIGURATOR: &str = "0x64b761D848206f447Fe2dd461b0c635Ec39EbB27";
const CONFIGURATOR_REVISION: i64 = 8;
const PRICE_ORACLE: &str = "0x54586bE62E3c3580375aE3723C145253060Ca0C2";
const GHO: &str = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";
const GHO_NAME: &str = "Gho Token";
const GHO_SYMBOL: &str = "GHO";
const GHO_DECIMALS: i64 = 18;
const SEED_TOKEN_REVISION: i64 = 1;

/// The span's reserve assets as `(underlying, a_token, v_token)`
/// (address-ascending: USDC, WETH, USDT — mirrors `aave_cassette_replay.rs`).
const RESERVES: [(&str, &str, &str); 3] = [
    (
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        "0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c",
        "0x72E95b8931767C79bA4EeE721354d6E99a61D004",
    ),
    (
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        "0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8",
        "0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE",
    ),
    (
        "0xdAC17F958D2ee523a2206206994597C13D831ec7",
        "0x23878914EFE38d27C4D67Ab83ed1b93A74D4086a",
        "0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8",
    ),
];

/// Seed the harness DB the recorder's `--kind aave-run` flow builds (mirrors
/// `aave_cassette_replay.rs`'s `seeded_db` row for row).
fn seed_aave_db(dir: &Path) -> (std::path::PathBuf, i64) {
    let path = dir.join("bench.db");
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
            1,
            GHO,
            Some(GHO_NAME),
            Some(GHO_SYMBOL),
            Some(GHO_DECIMALS),
        )
        .unwrap();
        DegenbotDb::get_or_create_gho_token_on_conn(&conn, 1, GHO).unwrap();

        DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            &conn,
            market_id,
            "POOL",
            POOL,
            Some(POOL_REVISION),
        )
        .unwrap();
        DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            &conn,
            market_id,
            "POOL_CONFIGURATOR",
            POOL_CONFIGURATOR,
            Some(CONFIGURATOR_REVISION),
        )
        .unwrap();
        DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            &conn,
            market_id,
            "PRICE_ORACLE",
            PRICE_ORACLE,
            None,
        )
        .unwrap();

        let span_from = 26_130_440_u64;
        DegenbotDb::set_market_last_update_block_on_conn(
            &conn,
            market_id,
            i64::try_from(span_from - 1).unwrap(),
        )
        .unwrap();

        for (underlying, a_token, v_token) in RESERVES {
            let underlying_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn, 1, underlying, None, None, None,
            )
            .unwrap();
            let a_token_id =
                DegenbotDb::get_or_create_erc20_token_on_conn(&conn, 1, a_token, None, None, None)
                    .unwrap();
            let v_token_id =
                DegenbotDb::get_or_create_erc20_token_on_conn(&conn, 1, v_token, None, None, None)
                    .unwrap();
            DegenbotDb::apply_reserve_initialized_on_conn(
                &conn,
                market_id,
                underlying_id,
                a_token_id,
                SEED_TOKEN_REVISION,
                v_token_id,
                SEED_TOKEN_REVISION,
                None,
                None,
            )
            .unwrap();
        }
        market_id
    };
    (path, market_id)
}

// ── the progress collectors (the stage spans' ride out of the run) ──────────

/// Collects the per-chunk stage reports (the sink trait takes `&self`, so
/// the vec sits behind a `Mutex` — the replay suites' collector shape).
#[derive(Default)]
struct PoolCollector(Mutex<Vec<ChunkProgress>>);

impl PoolProgressSink for PoolCollector {
    fn report_chunk(&self, progress: &ChunkProgress) {
        self.0.lock().unwrap().push(*progress);
    }
}

#[derive(Default)]
struct AaveCollector(Mutex<Vec<AaveChunkProgress>>);

impl AaveProgressSink for AaveCollector {
    fn report_chunk(&self, progress: &AaveChunkProgress) {
        self.0.lock().unwrap().push(progress.clone());
    }
}

/// One workload's per-iteration stage sums (over the run's chunks — the
/// corpora are single-chunk).
#[derive(Debug, Clone, Copy, Default)]
struct StageTimes {
    fetch: Duration,
    decode_compute: Duration,
    verify: Duration,
    apply: Duration,
    write_lock_hold: Duration,
    /// The whole run wall time (one chunk: also the chunk wall time).
    total: Duration,
    /// Sum of the statement ledger's profile µs (the SQL-time measure).
    sql_us: u64,
}

/// The deterministic counters + the median timings of one workload.
struct Measurement {
    label: &'static str,
    round_trips: u64,
    response_bytes: u64,
    statements: usize,
    median: StageTimes,
}

/// The median of one stage across the measured samples (re-sorts per stage;
/// the sample count is tiny).
fn median_duration(samples: &mut [StageTimes], pick: fn(&StageTimes) -> Duration) -> Duration {
    samples.sort_by_key(pick);
    pick(&samples[samples.len() / 2])
}

fn median(samples: &mut Vec<StageTimes>) -> StageTimes {
    let mut out = StageTimes::default();
    out.fetch = median_duration(samples, |s| s.fetch);
    out.decode_compute = median_duration(samples, |s| s.decode_compute);
    out.verify = median_duration(samples, |s| s.verify);
    out.apply = median_duration(samples, |s| s.apply);
    out.write_lock_hold = median_duration(samples, |s| s.write_lock_hold);
    out.total = median_duration(samples, |s| s.total);
    let mut sql: Vec<u64> = samples.iter().map(|s| s.sql_us).collect();
    sql.sort_unstable();
    out.sql_us = sql[sql.len() / 2];
    out
}

fn pool_stage_sums(chunks: &[ChunkProgress], total: Duration, sql_us: u64) -> StageTimes {
    let mut t = StageTimes {
        total,
        sql_us,
        ..StageTimes::default()
    };
    for c in chunks {
        t.fetch += c.fetch_time;
        t.decode_compute += c.decode_compute_time;
        t.verify += c.verify_time;
        t.apply += c.apply_time;
        t.write_lock_hold += c.write_lock_hold_time;
    }
    t
}

fn aave_stage_sums(chunks: &[AaveChunkProgress], total: Duration, sql_us: u64) -> StageTimes {
    let mut t = StageTimes {
        total,
        sql_us,
        ..StageTimes::default()
    };
    for c in chunks {
        t.fetch += c.fetch_time;
        t.decode_compute += c.decode_compute_time;
        t.verify += c.verify_time;
        t.apply += c.apply_time;
        t.write_lock_hold += c.write_lock_hold_time;
    }
    t
}

/// One measured iteration of the pool workload: fresh transport + temp DB +
/// ledger, the real chunk loop, the counters off the transport/ledger.
fn pool_iteration(
    cassette: &Cassette,
    chain_id: i64,
    from_block: u64,
    to_block: u64,
    verify_chunk: bool,
) -> (u64, u64, usize, StageTimes) {
    let transport = CassetteReplayTransport::new(cassette.clone());
    let provider = transport.as_alloy_provider();
    let dir = TempDir::new().unwrap();
    let path = seed_pool_db(dir.path(), chain_id, from_block);

    let collector = Arc::new(PoolCollector::default());
    let started = Instant::now();
    let (report, statements, sql_us) = {
        // The ledger wrapper IS the run's DB handle (ADR-068 D3); scoped so
        // its Drop disarms the process-global capture session before the
        // next iteration arms it again.
        let (ledger, _state) = LedgerDb::open_for_writes(&path).unwrap();
        let report = run_pool_update_on_db(
            ledger.db(),
            chain_id,
            Some(to_block),
            to_block - from_block + 1,
            provider,
            Arc::new(AtomicBool::new(false)),
            collector.clone(),
            verify_chunk,
            None,
            false,
        )
        .unwrap_or_else(|e| panic!("the replayed pool run must commit cleanly: {e}"));
        let records = ledger.records().expect("the capture session is armed");
        let sql_us = records.iter().map(|r| r.profile_us).sum::<u64>();
        (report, records.len(), sql_us)
    };
    let total = started.elapsed();
    assert_eq!(
        report.chunks_committed, 1,
        "the whole recorded span is one chunk"
    );
    assert_eq!(
        report.total_pools_written, EXPECTED_POOL_POOLS,
        "the corpus outcome drifted — this is a fixture problem, not a timing one"
    );
    let served = transport.served_snapshot();
    assert_eq!(
        served.requests, served.served,
        "every request must be a served ledger entry — a fixture gap is loud"
    );
    let chunks = collector.0.lock().unwrap().clone();
    (
        served.served,
        served.response_bytes,
        statements,
        pool_stage_sums(&chunks, total, sql_us),
    )
}

fn measure_pool(label: &'static str, cassette_file: &str, verify_chunk: bool) -> Measurement {
    let cassette = load_cassette(cassette_file);
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let span_from = cassette.provenance.span.from_block;
    let span_to = cassette.provenance.span.to_block;

    let mut round_trips: Option<u64> = None;
    let mut response_bytes: Option<u64> = None;
    let mut statements: Option<usize> = None;
    let mut samples: Vec<StageTimes> = Vec::new();

    for iteration in 0..(WARMUP_RUNS + MEASURED_RUNS) {
        let (rt, bytes, stmts, stages) =
            pool_iteration(&cassette, chain_id, span_from, span_to, verify_chunk);
        // The counters are workload facts, not timings: every iteration must
        // agree, or the harness is measuring noise.
        match (round_trips, response_bytes, statements) {
            (Some(p), Some(b), Some(s)) => {
                assert_eq!(p, rt, "round trips must be iteration-stable");
                assert_eq!(b, bytes, "response bytes must be iteration-stable");
                assert_eq!(s, stmts, "statement count must be iteration-stable");
            }
            _ => {
                round_trips = Some(rt);
                response_bytes = Some(bytes);
                statements = Some(stmts);
            }
        }
        if iteration >= WARMUP_RUNS {
            samples.push(stages);
        }
    }

    Measurement {
        label,
        round_trips: round_trips.unwrap(),
        response_bytes: response_bytes.unwrap(),
        statements: statements.unwrap(),
        median: median(&mut samples),
    }
}

/// One measured iteration of the Aave workload.
fn aave_iteration(
    cassette: &Cassette,
    chain_id: i64,
    from_block: u64,
    to_block: u64,
) -> (u64, u64, usize, StageTimes) {
    let transport = CassetteReplayTransport::new(cassette.clone());
    let provider = transport.as_alloy_provider();
    let dir = TempDir::new().unwrap();
    let (path, market_id) = seed_aave_db(dir.path());

    let collector = Arc::new(AaveCollector::default());
    let started = Instant::now();
    let (report, statements, sql_us) = {
        let (ledger, _state) = LedgerDb::open_for_writes(&path).unwrap();
        let report = run_aave_update_on_db(
            ledger.db(),
            chain_id,
            market_id,
            Some(to_block),
            to_block - from_block + 1,
            provider,
            Arc::new(AtomicBool::new(false)),
            collector.clone(),
            false,
            None,
            false,
            None,
        )
        .unwrap_or_else(|e| panic!("the replayed aave run must commit cleanly: {e}"));
        let records = ledger.records().expect("the capture session is armed");
        let sql_us = records.iter().map(|r| r.profile_us).sum::<u64>();
        (report, records.len(), sql_us)
    };
    let total = started.elapsed();
    assert_eq!(
        report.chunks_committed, 1,
        "the whole recorded span is one chunk"
    );
    assert_eq!(
        report.total_events_applied, EXPECTED_AAVE_EVENTS,
        "the corpus outcome drifted — this is a fixture problem, not a timing one"
    );
    let served = transport.served_snapshot();
    assert_eq!(
        served.requests, served.served,
        "every request must be a served ledger entry — a fixture gap is loud"
    );
    let chunks = collector.0.lock().unwrap().clone();
    (
        served.served,
        served.response_bytes,
        statements,
        aave_stage_sums(&chunks, total, sql_us),
    )
}

fn measure_aave(label: &'static str, cassette_file: &str) -> Measurement {
    let cassette = load_cassette(cassette_file);
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let span_from = cassette.provenance.span.from_block;
    let span_to = cassette.provenance.span.to_block;

    let mut round_trips: Option<u64> = None;
    let mut response_bytes: Option<u64> = None;
    let mut statements: Option<usize> = None;
    let mut samples: Vec<StageTimes> = Vec::new();

    for iteration in 0..(WARMUP_RUNS + MEASURED_RUNS) {
        let (rt, bytes, stmts, stages) = aave_iteration(&cassette, chain_id, span_from, span_to);
        match (round_trips, response_bytes, statements) {
            (Some(p), Some(b), Some(s)) => {
                assert_eq!(p, rt, "round trips must be iteration-stable");
                assert_eq!(b, bytes, "response bytes must be iteration-stable");
                assert_eq!(s, stmts, "statement count must be iteration-stable");
            }
            _ => {
                round_trips = Some(rt);
                response_bytes = Some(bytes);
                statements = Some(stmts);
            }
        }
        if iteration >= WARMUP_RUNS {
            samples.push(stages);
        }
    }

    Measurement {
        label,
        round_trips: round_trips.unwrap(),
        response_bytes: response_bytes.unwrap(),
        statements: statements.unwrap(),
        median: median(&mut samples),
    }
}

fn us(d: Duration) -> u128 {
    d.as_nanos() / 1_000
}

fn main() {
    println!(
        "replay bench (GLOSSARY \"replay bench\") — per-chunk baseline, median of {MEASURED_RUNS} runs ({WARMUP_RUNS} warmup)"
    );
    println!(
        "machine: this devcontainer; golden captures reth-recorded (each corpus file's provenance)"
    );
    println!();

    let measurements = vec![
        measure_pool(
            "pool_update_chunk_26102622-26102626 (verify gate OFF)",
            POOL_UPDATE_CASSETTE,
            false,
        ),
        measure_pool(
            "pool_verify_chunk_26102622-26102626 (verify gate ON)",
            POOL_VERIFY_CASSETTE,
            true,
        ),
        measure_aave("aave_update_chunk_26130440-26130445", AAVE_UPDATE_CASSETTE),
    ];

    println!(
        "{:<46} {:>4} {:>11} {:>5} {:>9} {:>9} {:>9} {:>9} {:>9} {:>12} {:>10} {:>10}",
        "capture",
        "rt",
        "resp bytes",
        "stmts",
        "sql µs",
        "fetch µs",
        "dc+cmp µs",
        "verify µs",
        "apply µs",
        "lock-hold µs",
        "chunk µs",
        "chunks/sec"
    );
    for m in &measurements {
        let chunk_us = us(m.median.total);
        let chunks_per_sec = if chunk_us > 0 {
            1_000_000_u128 / chunk_us
        } else {
            0
        };
        println!(
            "{:<46} {:>4} {:>11} {:>5} {:>9} {:>9} {:>9} {:>9} {:>9} {:>12} {:>10} {:>10}",
            m.label,
            m.round_trips,
            m.response_bytes,
            m.statements,
            m.median.sql_us,
            us(m.median.fetch),
            us(m.median.decode_compute),
            us(m.median.verify),
            us(m.median.apply),
            us(m.median.write_lock_hold),
            chunk_us,
            chunks_per_sec,
        );
    }
    println!();
    println!(
        "stages: fetch = RPC log fetches; dc+cmp = in-transaction decode+compute (pool: full-map read+compute; aave: per-tx discount pre-pass + config-dispatch reads — both under the write lock); verify = pre-commit on-chain gate (0 when off); apply = remaining in-transaction SQL; lock-hold = transaction() open → commit/drop."
    );
    println!(
        "sql µs = the statement ledger's summed profile times — SQLite's legacy CurrentTimeInt64 profile path quantizes each statement to whole milliseconds, so treat sql µs as a coarse floor (the Instant stage spans carry the fine timing)."
    );
    println!("one command reproduces: just bench-updaters");
}

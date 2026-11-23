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
//! The bench's own correctness is the counter gates: every iteration asserts
//! its RPC round trips, served bytes, statement count, and applied
//! deltas/events against the committed replay-surface authority
//! (`tests/fixtures/replay_surfaces/manifest.json`) — the same entry the
//! replay suites assert, so a bench row and a test literal cannot drift into
//! two independently maintained numbers.

#![expect(clippy::print_stdout)]
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
    run_aave_update_on_db, AaveChunkProgress, ProgressSink as AaveProgressSink, RunSpec,
    VerifyPolicy,
};
use degenbot_db::sql_ledger::LedgerDb;
use degenbot_db::DegenbotDb;
use degenbot_pool_updater::{
    run_pool_update_on_db, ChunkProgress, ProgressSink as PoolProgressSink,
};
use degenbot_rpc::cassette::{verify_cassette_bytes, Cassette};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use degenbot_rpc::replay_surface::{
    ReplayCapture, ReplaySurface, AAVE_CONFIG_CAPTURE, AAVE_UPDATE_CAPTURE, POOL_UPDATE_CAPTURE,
};
use tempfile::TempDir;

/// Warm-up iterations (dropped: page cache, allocator, the ledger's first-arm
/// cost) before the measured ones.
const WARMUP_RUNS: usize = 2;

/// Measured iterations per workload; the table reports the median.
const MEASURED_RUNS: usize = 9;

// ── the corpus (the committed seed cassettes; the same files the replay
//    suites gate on) ─────────────────────────────────────────────────────────

/// The workspace nests every crate manifest four levels under the repository
/// root.
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..")
}

/// The committed replay-surface authority: the one reviewed home of every
/// counter this table reports.
fn committed_surface() -> ReplaySurface {
    ReplaySurface::load_committed(&repo_root())
        .expect("the committed replay-surface authority must load")
}

fn surface_capture<'a>(surface: &'a ReplaySurface, id: &str) -> &'a ReplayCapture {
    surface
        .capture(id)
        .unwrap_or_else(|error| panic!("the replay-surface authority: {error}"))
}

fn load_cassette(capture: &ReplayCapture) -> Cassette {
    let path = repo_root().join(&capture.cassette);
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("capture {}: cassette {}: {e}", capture.id, path.display()));
    verify_cassette_bytes(&bytes).expect("the committed cassette must pass the drift gate");
    Cassette::from_json_bytes(&bytes).unwrap()
}

// ── seeding (mirrors the replay suites' harness DBs row for row) ────────────

/// The Uniswap V3 factory the pool captures were recorded against.
const V3_FACTORY: &str = "0x1F98431c8aD98523631AE4a59f267346ea31F984";

/// The seed corpus's single Mint touches the created pool's two boundary
/// ticks (the ledger's positions upsert writes 2 rows). A bench-table
/// denominator, not a replay expectation: the authority carries the applied
/// deltas, the per-tick columns divide them by this denominator.
const SEED_TOUCHED_TICKS: usize = 2;

// ── the workloads (what the bench measures, in table order) ────────────────

/// One pool workload's fixed configuration: which capture's cassette the
/// transport replays and which seed builds the harness database. The iteration
/// loop varies nothing else; the spec pins the measurement's shape, and the
/// authority pins every counter the iteration re-asserts.
struct PoolWorkload {
    /// The report table's row label (printed verbatim).
    label: &'static str,
    /// The authority's capture id (its cassette and expectations).
    capture: &'static str,
    /// The harness database seeder (`seed_pool_db`).
    seed_db: fn(&Path, i64, u64) -> std::path::PathBuf,
    /// Event-touched ticks on the capture's in-scope pool (the per-tick
    /// columns' denominator).
    touched_ticks: usize,
}

/// The corpus pool update — the production shape (the pre-commit gate runs on
/// the chunk like every other run).
const POOL_UPDATE_WORKLOAD: PoolWorkload = PoolWorkload {
    label: POOL_UPDATE_CAPTURE,
    capture: POOL_UPDATE_CAPTURE,
    seed_db: seed_pool_db,
    touched_ticks: SEED_TOUCHED_TICKS,
};

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

/// The config-era span's reserve assets as `(underlying, a_token, v_token)`,
/// in the recorder's candidate order (address-ascending by underlying: GHO,
/// WETH, USDT — mirrors `aave_config_cassette_replay.rs`). The GHO reserve
/// seeds the GHO-vToken FK link (the recorder's `gho_link`), which is what
/// makes the per-tx `build_discount_snapshot` resolve the GHO vToken at all.
const CONFIG_RESERVES: [(&str, &str, &str); 3] = [
    (
        "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f",
        "0x00907f9921424583e7ffBfEdf84F92B7B2Be4977",
        "0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B",
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

/// The config-era span's `--from` (the harness cursor stamp).
const AAVE_CONFIG_SPAN_FROM: u64 = 19_091_050;

/// Seed the config-era harness DB (mirrors `seed_aave_db` row for row except
/// the span stamp, the reserve set, and the GHO-vToken FK link — the
/// `aave_config_cassette_replay.rs` seed).
fn seed_aave_config_db(dir: &Path) -> (std::path::PathBuf, i64) {
    let path = dir.join("bench-config.db");
    let (db, _state) = DegenbotDb::open_for_writes(&path).unwrap();
    let market_id = {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (chain_id, name, active, last_update_block)              VALUES (?1, ?2, 1, ?3)",
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
        let gho_token_id = DegenbotDb::get_or_create_gho_token_on_conn(&conn, 1, GHO).unwrap();

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

        DegenbotDb::set_market_last_update_block_on_conn(
            &conn,
            market_id,
            i64::try_from(AAVE_CONFIG_SPAN_FROM - 1).unwrap(),
        )
        .unwrap();

        for (underlying, a_token, v_token) in CONFIG_RESERVES {
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
            let gho_link = if underlying == GHO {
                Some(gho_token_id)
            } else {
                None
            };
            DegenbotDb::apply_reserve_initialized_on_conn(
                &conn,
                market_id,
                underlying_id,
                a_token_id,
                SEED_TOKEN_REVISION,
                v_token_id,
                SEED_TOKEN_REVISION,
                None,
                gho_link,
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
    /// The ledger's total bind-parameter count per chunk (the statements'
    /// SIZE — the O(map)-vs-O(dirty) signature the row count hides).
    bind_args: u64,
    /// Touched ticks on the capture's in-scope pool (the per-tick metrics'
    /// denominator; 0 = not applicable, printed as `-`).
    touched_ticks: usize,
    median: StageTimes,
}

/// The median of one stage across the measured samples (re-sorts per stage;
/// the sample count is tiny).
fn median_duration(samples: &mut [StageTimes], pick: fn(&StageTimes) -> Duration) -> Duration {
    samples.sort_by_key(pick);
    pick(&samples[samples.len() / 2])
}

fn median(samples: &mut [StageTimes]) -> StageTimes {
    let mut sql: Vec<u64> = samples.iter().map(|s| s.sql_us).collect();
    sql.sort_unstable();
    StageTimes {
        fetch: median_duration(samples, |s| s.fetch),
        decode_compute: median_duration(samples, |s| s.decode_compute),
        verify: median_duration(samples, |s| s.verify),
        apply: median_duration(samples, |s| s.apply),
        write_lock_hold: median_duration(samples, |s| s.write_lock_hold),
        total: median_duration(samples, |s| s.total),
        sql_us: sql[sql.len() / 2],
    }
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
/// ledger, the real chunk loop, the counters off the transport/ledger. The
/// workload spec selects the harness seed; the capture carries every counter
/// this iteration re-asserts against the replay-surface authority.
fn pool_iteration(
    cassette: &Cassette,
    capture: &ReplayCapture,
    chain_id: i64,
    from_block: u64,
    to_block: u64,
    workload: &PoolWorkload,
) -> (u64, u64, usize, u64, StageTimes) {
    let transport = CassetteReplayTransport::new(cassette.clone());
    let provider = transport.as_alloy_provider();
    let dir = TempDir::new().unwrap();
    let path = (workload.seed_db)(dir.path(), chain_id, from_block);

    let collector = Arc::new(PoolCollector::default());
    let started = Instant::now();
    let (report, statements, sql_us, bind_args) = {
        // The ledger wrapper IS the run's DB handle (ADR-068 D3); scoped so
        // its Drop disarms the process-global capture session before the
        // next iteration arms it again.
        let (ledger, _state) = LedgerDb::open_for_writes(&path).unwrap();
        let report = run_pool_update_on_db(
            ledger.db(),
            chain_id,
            Some(to_block),
            to_block - from_block + 1,
            10_000,
            provider,
            Arc::new(AtomicBool::new(false)),
            collector.clone(),
            None,
            false,
        )
        .unwrap_or_else(|e| panic!("the replayed pool run must commit cleanly: {e}"));
        let records = ledger.records().expect("the capture session is armed");
        let sql_us = records.iter().map(|r| r.profile_us).sum::<u64>();
        // The ledger's total bind-parameter count — the per-statement SIZE
        // the statement count hides. The full-map persist binds
        // `4 × live_ticks + 1` params on its complement deletes + upserts;
        // the delta persist binds `4 × dirty`. This is the O(map) → O(dirty)
        // shape the dense capture exists to show.
        let bind_args = records
            .iter()
            .map(|r| u64::try_from(r.arg_count).unwrap_or(0))
            .sum::<u64>();
        (report, records.len(), sql_us, bind_args)
    };
    let total = started.elapsed();
    assert_eq!(
        report.chunks_committed, 1,
        "the whole recorded span is one chunk"
    );
    let applied = capture.pool_applied().unwrap_or_else(|| {
        panic!(
            "capture {}: a pool workload needs the authority's pool applied counts",
            capture.id
        )
    });
    assert_eq!(
        report.total_pools_written, applied.pools_written,
        "the corpus outcome drifted from the replay-surface authority — this is a \
         fixture/loop problem, not a timing one"
    );
    assert_eq!(
        report.total_liquidity_applies, applied.liquidity_applies,
        "the corpus outcome drifted from the replay-surface authority — this is a \
         fixture/loop problem, not a timing one"
    );
    let served = transport.served_snapshot();
    assert_eq!(
        served.requests, served.served,
        "every request must be a served ledger entry — a fixture gap is loud"
    );
    assert_eq!(
        served.served, capture.rpc.round_trips,
        "RPC round trips drifted from the replay-surface authority"
    );
    assert_eq!(
        served.response_bytes, capture.rpc.response_bytes,
        "served response bytes drifted from the replay-surface authority"
    );
    assert_eq!(
        statements, capture.sql.statements,
        "the statement count drifted from the replay-surface authority"
    );
    let chunks = collector.0.lock().unwrap().clone();
    (
        served.served,
        served.response_bytes,
        statements,
        bind_args,
        pool_stage_sums(&chunks, total, sql_us),
    )
}

fn measure_pool(surface: &ReplaySurface, workload: &PoolWorkload) -> Measurement {
    let capture = surface_capture(surface, workload.capture);
    let cassette = load_cassette(capture);
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let span_from = capture.span.from_block;
    let span_to = capture.span.to_block;

    // Every iteration asserts its counters against the authority, so the
    // counters are workload facts by construction (a nondeterministic counter
    // fails on the iteration that disagrees), and the first iteration's values
    // are the table's.
    let mut counters: Option<(u64, u64, usize, u64)> = None;
    let mut samples: Vec<StageTimes> = Vec::new();

    for iteration in 0..(WARMUP_RUNS + MEASURED_RUNS) {
        let (rt, bytes, stmts, args, stages) =
            pool_iteration(&cassette, capture, chain_id, span_from, span_to, workload);
        counters = Some((rt, bytes, stmts, args));
        if iteration >= WARMUP_RUNS {
            samples.push(stages);
        }
    }

    let (round_trips, response_bytes, statements, bind_args) = counters.unwrap();
    Measurement {
        label: workload.label,
        round_trips,
        response_bytes,
        statements,
        bind_args,
        touched_ticks: workload.touched_ticks,
        median: median(&mut samples),
    }
}

/// One measured iteration of the Aave workload.
fn aave_iteration(
    cassette: &Cassette,
    capture: &ReplayCapture,
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
            RunSpec {
                chain_id,
                market_id,
                to_block: Some(to_block),
                chunk_size: to_block - from_block + 1,
                max_block_range: 10_000,
                provider,
                cancel: Arc::new(AtomicBool::new(false)),
                progress: collector.clone(),
                verify: VerifyPolicy::default(),
                max_chunks: None,
            },
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
    let events = capture.aave_applied().unwrap_or_else(|| {
        panic!(
            "capture {}: an aave workload needs the authority's applied events",
            capture.id
        )
    });
    assert_eq!(
        report.total_events_applied, events.events_applied,
        "the corpus outcome drifted from the replay-surface authority — this is a \
         fixture/loop problem, not a timing one"
    );
    let served = transport.served_snapshot();
    assert_eq!(
        served.requests, served.served,
        "every request must be a served ledger entry — a fixture gap is loud"
    );
    assert_eq!(
        served.served, capture.rpc.round_trips,
        "RPC round trips drifted from the replay-surface authority"
    );
    assert_eq!(
        served.response_bytes, capture.rpc.response_bytes,
        "served response bytes drifted from the replay-surface authority"
    );
    assert_eq!(
        statements, capture.sql.statements,
        "the statement count drifted from the replay-surface authority"
    );
    let chunks = collector.0.lock().unwrap().clone();
    (
        served.served,
        served.response_bytes,
        statements,
        aave_stage_sums(&chunks, total, sql_us),
    )
}

/// One measured iteration of the config-era Aave workload (the
/// `aave_config_iteration` twin of [`aave_iteration`]: the config-era seed).
/// The capture is the pre-lock fact-phase corpus: the GHO-discount era span
/// 19,091,050..=19,091,059 — THREE variable-debt GHO `Burn`s by the SAME user
/// in three different txs (the first takes the discount `eth_call`, the two
/// later ones the DB-cache path the ops parser's first-tx writes created).
/// The authority's round-trip count IS the cross-tx fact-dependence gate: a
/// re-issued `getDiscountPercent` for the later same-user txs would push the
/// run above it.
fn aave_config_iteration(
    cassette: &Cassette,
    capture: &ReplayCapture,
    chain_id: i64,
    from_block: u64,
    to_block: u64,
) -> (u64, u64, usize, StageTimes) {
    let transport = CassetteReplayTransport::new(cassette.clone());
    let provider = transport.as_alloy_provider();
    let dir = TempDir::new().unwrap();
    let (path, market_id) = seed_aave_config_db(dir.path());

    let collector = Arc::new(AaveCollector::default());
    let started = Instant::now();
    let (report, statements, sql_us) = {
        let (ledger, _state) = LedgerDb::open_for_writes(&path).unwrap();
        let report = run_aave_update_on_db(
            ledger.db(),
            RunSpec {
                chain_id,
                market_id,
                to_block: Some(to_block),
                chunk_size: to_block - from_block + 1,
                max_block_range: 10_000,
                provider,
                cancel: Arc::new(AtomicBool::new(false)),
                progress: collector.clone(),
                verify: VerifyPolicy::default(),
                max_chunks: None,
            },
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
    let events = capture.aave_applied().unwrap_or_else(|| {
        panic!(
            "capture {}: an aave workload needs the authority's applied events",
            capture.id
        )
    });
    assert_eq!(
        report.total_events_applied, events.events_applied,
        "the corpus outcome drifted from the replay-surface authority — this is a \
         fixture/loop problem, not a timing one"
    );
    let served = transport.served_snapshot();
    assert_eq!(
        served.requests, served.served,
        "every request must be a served ledger entry — a fixture gap is loud"
    );
    assert_eq!(
        served.served, capture.rpc.round_trips,
        "the config-era RPC surface drifted from the replay-surface authority — a \
         re-issued getDiscountPercent for the later same-user txs (a broken cross-tx \
         fact phase) would show here"
    );
    assert_eq!(
        served.response_bytes, capture.rpc.response_bytes,
        "served response bytes drifted from the replay-surface authority"
    );
    assert_eq!(
        statements, capture.sql.statements,
        "the statement count drifted from the replay-surface authority"
    );
    let chunks = collector.0.lock().unwrap().clone();
    (
        served.served,
        served.response_bytes,
        statements,
        aave_stage_sums(&chunks, total, sql_us),
    )
}

/// One Aave measured-iteration entry point (the [`pool_iteration`] shape for
/// the pool workloads: fresh transport, harness seed, ledger, the real
/// chunk loop).
type AaveIteration = fn(&Cassette, &ReplayCapture, i64, u64, u64) -> (u64, u64, usize, StageTimes);

fn measure_aave(
    surface: &ReplaySurface,
    label: &'static str,
    capture_id: &str,
    iteration_fn: AaveIteration,
) -> Measurement {
    let capture = surface_capture(surface, capture_id);
    let cassette = load_cassette(capture);
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let span_from = capture.span.from_block;
    let span_to = capture.span.to_block;

    let mut counters: Option<(u64, u64, usize)> = None;
    let mut samples: Vec<StageTimes> = Vec::new();

    for iteration in 0..(WARMUP_RUNS + MEASURED_RUNS) {
        let (rt, bytes, stmts, stages) =
            iteration_fn(&cassette, capture, chain_id, span_from, span_to);
        counters = Some((rt, bytes, stmts));
        if iteration >= WARMUP_RUNS {
            samples.push(stages);
        }
    }

    let (round_trips, response_bytes, statements) = counters.unwrap();
    Measurement {
        label,
        round_trips,
        response_bytes,
        statements,
        bind_args: 0,
        touched_ticks: 0,
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

    let surface = committed_surface();
    let measurements = vec![
        measure_pool(&surface, &POOL_UPDATE_WORKLOAD),
        measure_aave(
            &surface,
            AAVE_UPDATE_CAPTURE,
            AAVE_UPDATE_CAPTURE,
            aave_iteration,
        ),
        measure_aave(
            &surface,
            "aave_config_chunk_19091050-19091059 (discount-path RPC)",
            AAVE_CONFIG_CAPTURE,
            aave_config_iteration,
        ),
    ];
    print_table(&measurements);
    print_legend();
}

/// The report table: one header row plus one row per measurement, the stdout
/// interface the survey's Baseline section quotes verbatim.
fn print_table(measurements: &[Measurement]) {
    println!(
        "{:<46} {:>4} {:>11} {:>5} {:>8} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9} {:>12} {:>10} {:>10} {:>8}",
        "capture",
        "rt",
        "resp bytes",
        "stmts",
        "stmts/",
        "args/",
        "bind args",
        "sql µs",
        "fetch µs",
        "dc+cmp µs",
        "verify µs",
        "apply µs",
        "lock-hold µs",
        "chunk µs",
        "chunks/sec",
    );
    for m in measurements {
        let chunk_us = us(m.median.total);
        // The per-touched-tick shape columns (Perf B's headline): statements
        // per touched tick, and ledger bind args per touched tick. `-` when
        // the workload has no touched-tick denominator (aave).
        let (stmts_per_tick, args_per_tick) = if m.touched_ticks > 0 {
            let per = |v: u64| v / u64::try_from(m.touched_ticks).unwrap_or(1);
            (
                per(u64::try_from(m.statements).unwrap_or(0)).to_string(),
                per(m.bind_args).to_string(),
            )
        } else {
            ("-".to_string(), "-".to_string())
        };
        let chunks_per_sec = 1_000_000_u128.checked_div(chunk_us).unwrap_or(0);
        println!(
            "{:<46} {:>4} {:>11} {:>5} {:>8} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9} {:>12} {:>10} {:>10} {:>8}",
            m.label,
            m.round_trips,
            m.response_bytes,
            m.statements,
            stmts_per_tick,
            args_per_tick,
            m.bind_args,
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
}

/// The footnotes under the table: what each stage column carries, the ledger
/// timing caveat, the per-tick columns' meaning, and the reproduce command.
fn print_legend() {
    println!(
        "stages: fetch = RPC log fetches; dc+cmp = in-transaction decode+compute (pool: map read+compute; aave: per-tx discount pre-pass + config-dispatch reads — both under the write lock); verify = pre-commit on-chain gate (0 when off); apply = remaining in-transaction SQL; lock-hold = transaction() open → commit/drop."
    );
    println!(
        "sql µs = the statement ledger's summed profile times — SQLite's legacy CurrentTimeInt64 profile path quantizes each statement to whole milliseconds, so treat sql µs as a coarse floor (the Instant stage spans carry the fine timing)."
    );
    println!(
        "stmts/tick + args/tick = the ledger's statement count and total bind parameters per event-touched tick on the capture's in-scope pool (the O(map) vs O(dirty) shape; the dense capture's deep map makes the per-statement SIZE visible: the full-map persist binds 4×live-tick params, the delta persist 4×dirty)."
    );
    println!("one command reproduces: just bench-updaters");
}

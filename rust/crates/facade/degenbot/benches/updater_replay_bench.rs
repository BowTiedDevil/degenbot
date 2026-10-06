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
const POOL_DENSE_CASSETTE: &str = "pool_dense_update_26131653-26133246.json";
const AAVE_UPDATE_CASSETTE: &str = "aave_update_chunk_26130440-26130445.json";

/// The dense-pool capture (Perf B's measurement corpus): the USDC/WETH 0.05%
/// V3 pool `0x88e6A0c2…` over blocks 26131653..=26133246 — 27 `Mint`/`Burn`
/// events on that pool touching 26 distinct ticks, the whole-chain scan
/// carrying 485 logs from 116 emitters. The pool is DEEP: the harness seeds
/// its base map (2048 in-range positions + the full-range mint's two extreme
/// ticks — a deterministic synthetic at the pool's real-order depth; the
/// values are harness inputs exactly like the Aave substrate seed, and the
/// dense posture runs gate-OFF so nothing verifies them against chain). The
/// SQL shape the Perf B delta targets is the map DEPTH, not the values.
const DENSE_POOL_ADDRESS: &str = "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640";
const DENSE_TOKEN0: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"; // USDC
const DENSE_TOKEN1: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"; // WETH
/// The synthetic base map: 2048 in-range positions, tick `189310 + 10*k` —
/// multiples of the pool's spacing 10 covering every non-extreme event tick.
const DENSE_SEEDED_TICKS: usize = 2048;
const DENSE_FIRST_SEEDED_TICK: i64 = 189_310;
/// The full-range mint's extreme ticks (the recorded span carries a
/// `-887270..887270` mint) — seeded so their words exist in the base.
const DENSE_EXTREME_TICKS: [i64; 2] = [-887_270, 887_270];

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
/// The dense span's `PoolCreated` events for the canonical V3 factory
/// (recorded pass; the replay commits exactly these pool rows).
const EXPECTED_DENSE_POOLS: usize = 6;
/// The dense span's in-scope liquidity applies: the seeded USDC/WETH row
/// PLUS the six pools the chunk's own `PoolCreated` upserts create (new pools
/// mint immediately — each takes the fused in-transaction path). The other
/// ~110 emitters are unknown → scope-skipped without a fetch.
const EXPECTED_DENSE_LIQUIDITY_APPLIES: usize = 7;
/// Distinct ticks the dense span's 27 USDC/WETH events touch (2 slots × 27
/// events, deduped — derived from the committed cassette's logs; the
/// per-touched-tick metric's denominator).
const EXPECTED_DENSE_TOUCHED_TICKS: usize = 26;
/// The seed corpus's single Mint touches the created pool's two boundary
/// ticks (the ledger's positions upsert writes 2 rows).
const EXPECTED_SEED_TOUCHED_TICKS: usize = 2;
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

/// The DENSE pool harness DB: the seed rows above PLUS the USDC/WETH 0.05%
/// pool row and its deep synthetic base map (2048 in-range positions across
/// their bitmap words + the two full-range extreme ticks). Seeding rides a
/// plain handle BEFORE the ledger arms, so the run's statement ledger holds
/// only the chunk loop's SQL (the seed is harness input, not workload).
///
/// The seeded values are deterministic and burn-safe: gross is `2^100` (any
/// recorded burn keeps `gross + delta >= 0`), net alternates sign so both
/// tick orientations exist. The bitmap words are computed with the canonical
/// EVM formula (`compressed = tick / spacing; word = compressed >> 8; bit =
/// compressed.rem_euclid(256)` — `degenbot-math`'s
/// `get_tick_word_and_bit_position`), so the apply's bit flips land on real
/// base words rather than seeding fresh ones.
fn seed_dense_pool_db(dir: &Path, chain_id: i64, from: u64) -> std::path::PathBuf {
    use alloy::primitives::U256;
    use degenbot::math::cl::liquidity_mapping::get_tick_word_and_bit_position;

    let path = dir.join("bench-dense.db");
    let (db, _state) = DegenbotDb::open_for_writes(&path).unwrap();
    let factory: Address = V3_FACTORY.parse().unwrap();
    let exchange = db
        .upsert_exchange(chain_id, "uniswap_v3", factory, None)
        .unwrap();
    let pool_address: Address = DENSE_POOL_ADDRESS.parse().unwrap();
    let token0: Address = DENSE_TOKEN0.parse().unwrap();
    let token1: Address = DENSE_TOKEN1.parse().unwrap();
    db.upsert_v3_pools(
        chain_id,
        "uniswap_v3",
        exchange.id,
        1_000_000,
        &[degenbot::db::V3PoolRowInput {
            address: pool_address,
            token0_address: token0,
            token1_address: token1,
            fee: 500,
            tick_spacing: 10,
        }],
    )
    .unwrap();
    let pool_id: i64 = {
        let conn = db.lock();
        conn.query_row(
            "SELECT id FROM pools WHERE address = ?1 AND chain = ?2",
            rusqlite::params![pool_address.to_checksum(None), chain_id],
            |r| r.get(0),
        )
        .unwrap()
    };

    let mut ticks: Vec<i64> = (0..i64::try_from(DENSE_SEEDED_TICKS).unwrap())
        .map(|k| DENSE_FIRST_SEEDED_TICK + 10 * k)
        .collect();
    ticks.extend_from_slice(&DENSE_EXTREME_TICKS);
    ticks.sort_unstable();
    ticks.dedup();

    // The deterministic base values (burn-safe gross, alternating net sign).
    // Each seeded tick sets its bit in the accumulated word bitmap — the same
    // flip the apply's `flip_tick` would produce — so the base words are real.
    let gross: U256 = U256::from(1u8) << 100;
    let mut word_bits: std::collections::BTreeMap<i32, U256> = std::collections::BTreeMap::new();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute("BEGIN", []).unwrap();
    {
        let mut stmt = conn
            .prepare(
                "INSERT INTO liquidity_positions (pool_id, tick, liquidity_net, liquidity_gross) \
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .unwrap();
        for (k, &tick) in ticks.iter().enumerate() {
            let net: i128 = if k % 2 == 0 {
                1i128 << 100
            } else {
                -(1i128 << 100)
            };
            stmt.execute(rusqlite::params![
                pool_id,
                i32::try_from(tick).unwrap(),
                net.to_string(),
                gross.to_string()
            ])
            .unwrap();
            let (word, bit) = get_tick_word_and_bit_position(i32::try_from(tick).unwrap(), 10);
            let bit_mask = U256::from(1u8) << u64::from(bit);
            *word_bits.entry(word).or_insert(U256::ZERO) |= bit_mask;
        }
    }
    {
        let mut stmt = conn
            .prepare("INSERT INTO initialization_maps (pool_id, word, bitmap) VALUES (?1, ?2, ?3)")
            .unwrap();
        for (word, bitmap) in &word_bits {
            stmt.execute(rusqlite::params![pool_id, *word, bitmap.to_string()])
                .unwrap();
        }
    }
    conn.execute("COMMIT", []).unwrap();
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
/// `seed_db` selects the harness DB (the seed corpus's exchange-only seed or
/// the dense capture's deep-map seed); `expected_pools`/`expected_applies`
/// are the corpus outcome literals the iteration re-asserts.
fn pool_iteration(
    cassette: &Cassette,
    chain_id: i64,
    from_block: u64,
    to_block: u64,
    verify_chunk: bool,
    seed_db: fn(&Path, i64, u64) -> std::path::PathBuf,
    expected_pools: usize,
    expected_applies: usize,
) -> (u64, u64, usize, u64, StageTimes) {
    let transport = CassetteReplayTransport::new(cassette.clone());
    let provider = transport.as_alloy_provider();
    let dir = TempDir::new().unwrap();
    let path = seed_db(dir.path(), chain_id, from_block);

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
    assert_eq!(
        report.total_pools_written, expected_pools,
        "the corpus outcome drifted — this is a fixture problem, not a timing one"
    );
    if expected_applies > 0 {
        assert_eq!(
            report.total_liquidity_applies, expected_applies,
            "the corpus outcome drifted — this is a fixture problem, not a timing one"
        );
    }
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
        bind_args,
        pool_stage_sums(&chunks, total, sql_us),
    )
}

#[expect(clippy::too_many_arguments)]
fn measure_pool(
    label: &'static str,
    cassette_file: &str,
    verify_chunk: bool,
    seed_db: fn(&Path, i64, u64) -> std::path::PathBuf,
    expected_pools: usize,
    expected_applies: usize,
    touched_ticks: usize,
) -> Measurement {
    let cassette = load_cassette(cassette_file);
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let span_from = cassette.provenance.span.from_block;
    let span_to = cassette.provenance.span.to_block;

    let mut round_trips: Option<u64> = None;
    let mut response_bytes: Option<u64> = None;
    let mut statements: Option<usize> = None;
    let mut bind_args: Option<u64> = None;
    let mut samples: Vec<StageTimes> = Vec::new();

    for iteration in 0..(WARMUP_RUNS + MEASURED_RUNS) {
        let (rt, bytes, stmts, args, stages) = pool_iteration(
            &cassette,
            chain_id,
            span_from,
            span_to,
            verify_chunk,
            seed_db,
            expected_pools,
            expected_applies,
        );
        // The counters are workload facts, not timings: every iteration must
        // agree, or the harness is measuring noise.
        match (round_trips, response_bytes, statements, bind_args) {
            (Some(p), Some(b), Some(s), Some(a)) => {
                assert_eq!(p, rt, "round trips must be iteration-stable");
                assert_eq!(b, bytes, "response bytes must be iteration-stable");
                assert_eq!(s, stmts, "statement count must be iteration-stable");
                assert_eq!(a, args, "bind-arg count must be iteration-stable");
            }
            _ => {
                round_trips = Some(rt);
                response_bytes = Some(bytes);
                statements = Some(stmts);
                bind_args = Some(args);
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
        bind_args: bind_args.unwrap(),
        touched_ticks,
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
    let mut bind_args: Option<u64> = None;
    let mut samples: Vec<StageTimes> = Vec::new();

    for iteration in 0..(WARMUP_RUNS + MEASURED_RUNS) {
        let (rt, bytes, stmts, stages) = aave_iteration(&cassette, chain_id, span_from, span_to);
        match (round_trips, response_bytes, statements, bind_args) {
            (Some(p), Some(b), Some(s), Some(a)) => {
                assert_eq!(p, rt, "round trips must be iteration-stable");
                assert_eq!(b, bytes, "response bytes must be iteration-stable");
                assert_eq!(s, stmts, "statement count must be iteration-stable");
                assert_eq!(a, 0, "aave workload tracks no bind-arg counter");
            }
            _ => {
                round_trips = Some(rt);
                response_bytes = Some(bytes);
                statements = Some(stmts);
                bind_args = Some(0);
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
        bind_args: bind_args.unwrap(),
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

    let measurements = vec![
        measure_pool(
            "pool_update_chunk_26102622-26102626 (verify gate OFF)",
            POOL_UPDATE_CASSETTE,
            false,
            seed_pool_db,
            EXPECTED_POOL_POOLS,
            0,
            EXPECTED_SEED_TOUCHED_TICKS,
        ),
        measure_pool(
            "pool_verify_chunk_26102622-26102626 (verify gate ON)",
            POOL_VERIFY_CASSETTE,
            true,
            seed_pool_db,
            EXPECTED_POOL_POOLS,
            0,
            EXPECTED_SEED_TOUCHED_TICKS,
        ),
        measure_pool(
            "pool_dense_update_26131653-26133246 (gate OFF, deep map)",
            POOL_DENSE_CASSETTE,
            false,
            seed_dense_pool_db,
            EXPECTED_DENSE_POOLS,
            EXPECTED_DENSE_LIQUIDITY_APPLIES,
            EXPECTED_DENSE_TOUCHED_TICKS,
        ),
        measure_aave("aave_update_chunk_26130440-26130445", AAVE_UPDATE_CASSETTE),
    ];

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
    for m in &measurements {
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
        let chunks_per_sec = if chunk_us > 0 {
            1_000_000_u128 / chunk_us
        } else {
            0
        };
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

//! Offline end-to-end replay of the Aave chunk loop over its committed golden
//! capture (ADR-068).
//!
//! The committed cassette (the corpus home's
//! `tests/fixtures/cassettes/aave_update_chunk_26130440-26130445.json`) was
//! recorded by the `record_updater_cassette` fixture-tools binary
//! (`--kind aave-run`) against the live node: the REAL `activate_aave_market`
//! seed + the REAL `run_aave_update`
//! chunk loop over the `RecordingTransport`, so the ledger carries everything
//! the loop actually asked - the full 7-pass chunk fetch (address provider,
//! configurator, pool, oracle, scaled tokens, discount config) plus the
//! ancillary `eth_chainId`/`eth_blockNumber` round trips - NOT just the
//! Pool-contract pass the older `aave_market_chunk` seed captured.
//!
//! The seeded temp DB mirrors the recorder's harness substrate row-for-row:
//! `activate_aave_market`'s one-shot seed (market row at the bootstrap block,
//! the `POOL_ADDRESS_PROVIDER` contract, the GHO token + bare `aave_gho_tokens`
//! row) plus the loop's warm-boot substrate (`POOL/POOL_CONFIGURATOR` with
//! chain-verified revisions, `PRICE_ORACLE`, and the span's reserve assets).
//! The run must commit the span, advance `last_update_block`, report the
//! `AaveChunkProgress` shape, and a restarted run must be a no-op (the
//! restart invariant over the replayed surface).
//!
//! The SQL half (statement ledger + canonical DB dump goldens, the
//! statement-count N+1 tripwire) rides the same replay through
//! [`LedgerDb`] - the drift gate is byte-identical regeneration through the
//! same writer (`REGENERATE_SQL_GOLDENS=1`), and each probe of the gate is a
//! deliberate, documented fault injection.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use alloy::primitives::{keccak256, B256};
use degenbot_aave::updater::{
    AaveChunkProgress, AaveUpdateReport, CanonicalDelta, NoProgress, ProgressSink, RunError,
    RunSpec, VerifyPolicy,
};
use degenbot_aave::AaveMarketWriter;
use degenbot_db::sql_ledger::{
    dump_tables_golden_json, ledger_golden_json, restore_aave_seed_json, LedgerDb,
};
use degenbot_db::DegenbotDb;
use degenbot_rpc::cassette::{
    entry_digest, entry_key, verify_cassette_bytes, Cassette, CassetteEntry, CassetteResponse,
};
use degenbot_rpc::cassette_replay::{CassetteReplayTransport, ServedSnapshot};
use degenbot_rpc::chain_evidence::{ChainEvidenceError, HeaderRefusal, ProjectionRefusal};
use degenbot_rpc::replay_surface::{ReplayCapture, ReplaySurface, AAVE_UPDATE_CAPTURE};
use serde_json::{json, Value};
use tempfile::TempDir;

/// The workspace nests every crate manifest four levels under the repository
/// root.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..")
}

/// The capture's entry in the committed replay-surface authority
/// (`tests/fixtures/replay_surfaces/manifest.json`): the one reviewed home of
/// the RPC counters, the SQL statement count, and the applied events the
/// replay must reproduce — shared with `updater_replay_bench`.
fn surface_capture() -> ReplayCapture {
    surface_capture_of(AAVE_UPDATE_CAPTURE)
}

/// One governed capture's entry in the committed replay-surface authority.
fn surface_capture_of(id: &str) -> ReplayCapture {
    let surface = ReplaySurface::load_committed(&repo_root())
        .expect("the committed replay-surface authority must load");
    surface
        .capture(id)
        .unwrap_or_else(|error| panic!("the authority governs capture {id}: {error}"))
        .clone()
}

/// The committed statement-ledger golden (SQL half of the golden capture).
const LEDGER_GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/sql_goldens/aave_update_chunk_26130440-26130445.statement-ledger.json"
);

/// The committed canonical DB-dump golden (post-run DB state).
const DUMP_GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/sql_goldens/aave_update_chunk_26130440-26130445.db-dump.json"
);

/// The pinned span (the recorder's `--from/--to`).
const SPAN_FROM: u64 = 26_130_440;
const SPAN_TO: u64 = 26_130_445;

/// The statement-ledger session is ONE process-global slot in degenbot-db, and
/// cargo runs this binary's tests as threads of one process, so the two
/// ledger-arming gates hold this guard for their whole session instead of
/// racing into `LedgerArmed`. A panic inside one gate poisons the mutex; the
/// other still runs rather than failing for the first one's abort.
fn ledger_session() -> std::sync::MutexGuard<'static, ()> {
    static LEDGER_SESSION: Mutex<()> = Mutex::new(());
    LEDGER_SESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The GHO first-mint capture's id in the committed replay-surface authority.
const AAVE_FIRST_MINT_CAPTURE: &str = "aave_gho_first_mint_17699253";

/// The first-mint capture's committed replay seed (the recorder's
/// `--seed-out`): the warm-boot substrate rows a replay restores.
const FIRST_MINT_SEED_GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/cassettes/aave_gho_first_mint_17699253.seed.json"
);

/// The first-mint capture's committed canonical delta stream.
const FIRST_MINT_DELTA_GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/sql_goldens/aave_gho_first_mint_17699253.delta-stream.json"
);

/// The first-mint capture's archive proof: the capture parent's position and
/// GHO-discount pre-state, read from the node the cassette was recorded against.
const FIRST_MINT_EVIDENCE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/aave_receipts/aave_gho_first_mint_17699253.evidence.json"
);

/// The first-mint capture's one-block span and the parent its seed reads.
const FIRST_MINT_BLOCK: u64 = 17_699_253;
const FIRST_MINT_PARENT: u64 = 17_699_252;

/// stkAAVE — the discount token the refresh's parent-block `balanceOf` targets.
const STK_AAVE: &str = "0x4da27a545c0c5B758a6BA100e3a049001de870f5";

/// The GHO reserve's vToken: the debt leg whose `scaledBalanceOf` is the zero
/// pre-state, and the emitter that answers both discount getters and its own
/// `DEBT_TOKEN_REVISION`.
const GHO_VTOKEN: &str = "0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B";

/// The GHO reserve's aToken: the collateral leg whose `ATOKEN_REVISION` the
/// capture seeds.
const GHO_ATOKEN: &str = "0x00907f9921424583e7ffBfEdf84F92B7B2Be4977";

/// `balanceOf(address)`'s selector (the one read the refresh pays for).
const BALANCE_OF_SELECTOR: &str = "0x70a08231";

/// The chunk's acquisition-surface composition (ADR-105 D2/D5), pinned
/// per method below:
///
/// - `eth_chainId` 1 (the terminal anchor's endpoint check) + 1 (the binding the
///   candidate-header reads inherit),
/// - `eth_getBlockByNumber` 1 (the terminal anchor) + 3 (the span's OTHER
///   candidate blocks: 26130441/2/4; 26130445 is the terminal anchor already read),
/// - `eth_getLogs` 6 (this span dispatches no config events and no GHO-discount
///   path, so no supplemental pass and no `eth_call`),
/// - `eth_getTransactionReceipt` 7 (one per candidate transaction),
/// - `debug_traceTransaction` 0 (`EXPECTED_TRACE_REQUESTS`: every candidate here is
///   receipt-only, so the run pays for no call trace).
///
/// Independent literal: the candidate transactions the span's filtered
/// acquisition names - one complete receipt each (ADR-105 D2). Its share of the
/// authority's round-trip count, so a receipt the run stops paying for is a
/// visible count change rather than a silent savings.
const EXPECTED_CANDIDATE_TRANSACTIONS: u64 = 7;

/// Independent literal: the block-header reads the run issues for the candidate
/// transactions' blocks. The terminal anchor is reused for its own height, so
/// this is the span's four candidate blocks minus the pinned terminal.
const EXPECTED_BLOCK_HEADER_READS: u64 = 4;

/// Independent literal: the call-trace acquisitions. Zero on this corpus - no
/// transaction in the span carries a `LiquidationCall`, so every candidate is
/// receipt-only and issues no trace request.
const EXPECTED_TRACE_REQUESTS: u64 = 0;

/// The Aave V3 Ethereum bootstrap block (`activate_aave_market`'s fresh-seed
/// stamp; the substrate constant mirrors `updater/run/activate.rs`).
const AAVE_BOOTSTRAP_BLOCK: i64 = 16_291_070;

// ── seed literals ─────────────────────────────────────────────────────────
//
// Chain-verified against reth/v2.7.0-3d592ec while recording (the
// PoolAddressesProvider getters, `getReserveData` token pairs, `symbol()`
// sanity checks, `POOL_REVISION()`/`CONFIGURATOR_REVISION()`); the two
// deployment addresses corroborate `degenbot-cli-core`'s `AAVE_DEPLOYMENTS`.
// The recorder seeds exactly these rows via the same substrate fns.

const POOL_ADDRESS_PROVIDER: &str = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
const POOL: &str = "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2";
const POOL_REVISION: i64 = 11;
const POOL_CONFIGURATOR: &str = "0x64b761D848206f447Fe2dd461b0c635Ec39EbB27";
const CONFIGURATOR_REVISION: i64 = 8;
const PRICE_ORACLE: &str = "0x54586bE62E3c3580375aE3723C145253060Ca0C2";

/// The market's GHO token (`activate_aave_market`'s RPC-fetched metadata
/// pins the erc20 row the dump golden carries).
const GHO: &str = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";
const GHO_NAME: &str = "Gho Token";
const GHO_SYMBOL: &str = "GHO";
const GHO_DECIMALS: i64 = 18;

/// The span's reserve assets as `(underlying, a_token, v_token)`, in the
/// recorder's candidate order (address-ascending: USDC, WETH, USDT). The
/// aToken/vToken set IS the run's scaled-token OR-set - the seed must match
/// the recorder's exactly or the replayed pass-5 getLogs filter misses its
/// recorded entry (a loud fixture gap, by design).
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

/// Seeded aToken/vToken revision (inert for this span - no `Upgraded` config
/// event dispatches and no GHO-discount path - but the columns are NOT NULL).
/// Mirrors the recorder's `AAVE_SEED_TOKEN_REVISION`.
const SEED_TOKEN_REVISION: i64 = 1;

/// The tables one Aave chunk apply touches, in the dump's fixed
/// (alphabetical) order. The empty-but-listed tables (asset configs, eMode
/// categories) stay so a config-dispatch path accidentally writing them
/// cannot drift silently - the same posture as the pool goldens' V4 tables.
const DUMP_TABLES: &[&str] = &[
    "aave_gho_tokens",
    "aave_v3_asset_configs",
    "aave_v3_assets",
    "aave_v3_checkpoint_generations",
    "aave_v3_checkpoint_positions",
    "aave_v3_collateral_positions",
    "aave_v3_contracts",
    "aave_v3_debt_positions",
    "aave_v3_emode_categories",
    "aave_v3_generations",
    "aave_v3_markets",
    "aave_v3_projection_checkpoints",
    "aave_v3_projection_cursors",
    "aave_v3_user_collateral_configs",
    "aave_v3_users",
    "erc20_tokens",
];

/// Seed the harness DB the recorder's `--kind aave-run` flow builds: (1)
/// `activate_aave_market`'s fresh-seed substrate (market row at the bootstrap
/// block, `POOL_ADDRESS_PROVIDER` contract, GHO erc20 row with metadata, bare
/// `aave_gho_tokens` row), (2) the warm-boot contract rows with their
/// chain-verified revisions, (3) the span cursor (`SPAN_FROM - 1`), (4) the
/// span's reserve assets via the `ReserveInitialized` substrate. Row ids and
/// insertion order mirror the recorder exactly (the dump golden carries ids).
fn seeded_db(dir: &Path, file_name: &str) -> (PathBuf, i64) {
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

        // (1) activate's substrate.
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

        // (2) the warm-boot contract rows.
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

        // (3) the span cursor.
        DegenbotDb::set_market_last_update_block_on_conn(
            &conn,
            market_id,
            i64::try_from(SPAN_FROM - 1).unwrap(),
        )
        .unwrap();

        // (4) the span's reserve assets.
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

/// The committed market cursor (the restart invariant's observable).
fn market_last_update_block(path: &Path, market_id: i64) -> Option<i64> {
    let (db, _state) = DegenbotDb::open_for_writes(path).unwrap();
    db.fetch_aave_market_row(market_id)
        .unwrap()
        .expect("market row present")
        .last_update_block
}

/// Collects the per-chunk [`AaveChunkProgress`] reports (the shape the
/// suite asserts).
#[derive(Default)]
struct ProgressCollector {
    chunks: Mutex<Vec<AaveChunkProgress>>,
}

impl ProgressSink for ProgressCollector {
    fn report_chunk(&self, progress: &AaveChunkProgress) {
        self.chunks.lock().unwrap().push(progress.clone());
    }
}

/// The committed canonical delta stream (ADR-105 D7, D13): every delta the
/// recorded span reduces, encoded with its exact position, generation and
/// effect. The stream is the vocabulary a journal persists, so it is pinned
/// byte-for-byte: nothing chunk-scoped participates, and a semantic change to
/// an effect's identity must show up here rather than only in row values.
const DELTA_STREAM_GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/sql_goldens/aave_update_chunk_26130440-26130445.delta-stream.json"
);

#[test]
fn aave_chunk_delta_stream_matches_the_committed_golden() {
    let cassette = committed_cassette();
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let provider = CassetteReplayTransport::new(cassette).as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let (path, market_id) = seeded_db(dir.path(), "delta-stream.db");
    let sink = Arc::new(ProgressCollector::default());
    AaveMarketWriter::acquire(&path, market_id)
        .unwrap()
        .update(RunSpec {
            chain_id,
            market_id,
            to_block: Some(SPAN_TO),
            chunk_size: SPAN_TO - SPAN_FROM + 1,
            max_block_range: 10_000,
            provider,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: sink.clone(),
            verify: VerifyPolicy::default(),
            max_chunks: None,
        })
        .expect("the replayed run must commit cleanly");

    let chunks = sink.chunks.lock().unwrap().clone();
    let mut encoded: Vec<String> = Vec::new();
    for chunk in &chunks {
        for delta in &chunk.deltas {
            encoded.push(delta.encode());
        }
    }
    assert!(
        !encoded.is_empty(),
        "the recorded span reduces at least one canonical delta"
    );
    let produced = format!("{}.\n", encoded.join("\n"));
    check_or_regen("canonical delta stream", &produced, DELTA_STREAM_GOLDEN);
}

/// The first-mint capture's restored substrate: the committed seed's rows,
/// restored through the case-preserving codec into a fresh harness DB. A replay
/// starts from exactly the rows the recording read — no in-code literals here.
fn restored_seed_db(seed_path: &str, dir: &Path, file_name: &str) -> (PathBuf, i64) {
    let seed = std::fs::read_to_string(seed_path)
        .unwrap_or_else(|e| panic!("the committed seed {seed_path} must exist: {e}"));
    let path = dir.join(file_name);
    let (db, _state) = DegenbotDb::open_for_writes(&path).unwrap();
    let market_id = {
        {
            let conn = db.lock();
            restore_aave_seed_json(&conn, &seed)
                .expect("the committed seed restores through the case-preserving codec");
        }
        let markets = db.fetch_active_aave_markets().unwrap();
        let [market] = markets.as_slice() else {
            panic!(
                "the committed seed carries one active market, got {}",
                markets.len()
            );
        };
        market.id
    };
    (path, market_id)
}

/// The header hash a capture records for `number`.
fn recorded_block_hash(cassette: &Cassette, number: u64) -> String {
    let key = entry_key("eth_getBlockByNumber", &json!([number.to_string(), false]));
    let entry = cassette
        .entries
        .get(&key)
        .unwrap_or_else(|| panic!("the capture records the header at {number}"));
    let CassetteResponse::Success { result } = &entry.response else {
        panic!("the recorded header read at {number} is a success");
    };
    result["hash"]
        .as_str()
        .expect("the recorded header carries its hash")
        .to_string()
}

/// The chunk reports' canonical delta stream, encoded the golden's way.
fn delta_stream(chunks: &[AaveChunkProgress]) -> String {
    let encoded: Vec<String> = chunks
        .iter()
        .flat_map(|chunk| chunk.deltas.iter().map(CanonicalDelta::encode))
        .collect();
    format!("{}.\n", encoded.join("\n"))
}

/// The first-mint capture's refresh surface: exactly ONE `balanceOf` call, to
/// stkAAVE, pinned to the capture parent's block hash. The refresh reads the
/// stkAAVE balance at the block BEFORE the borrow, so a second read — or a read
/// at the span block — is a different discount seed, not a cheaper one.
#[test]
fn first_mint_cassette_pays_for_one_parent_block_stk_aave_balance_read() {
    let capture = surface_capture_of(AAVE_FIRST_MINT_CAPTURE);
    let cassette = capture_cassette(&capture);
    assert_eq!(cassette.provenance.span.from_block, FIRST_MINT_BLOCK);
    assert_eq!(cassette.provenance.span.to_block, FIRST_MINT_BLOCK);

    let balance_reads: Vec<&String> = cassette
        .entries
        .keys()
        .filter(|key| key.contains(BALANCE_OF_SELECTOR))
        .collect();
    assert_eq!(
        balance_reads.len(),
        1,
        "the refresh pays for exactly one balanceOf read, got {balance_reads:?}"
    );
    let key = balance_reads[0];
    assert!(
        key.starts_with("[\"eth_call\""),
        "the one balance read is an eth_call: {key}"
    );
    assert!(
        key.contains(&STK_AAVE.to_lowercase()),
        "the one balance read targets stkAAVE: {key}"
    );
    let parent_hash = recorded_block_hash(&cassette, FIRST_MINT_PARENT).to_lowercase();
    assert!(
        key.contains(&parent_hash),
        "the one balance read is pinned to the capture parent's block hash {parent_hash}: {key}"
    );
    let CassetteResponse::Success { result } = &cassette.entries[key].response else {
        panic!("the recorded balanceOf answers with a success");
    };
    let zero_word = format!("0x{}", "0".repeat(64));
    assert_eq!(
        result.as_str(),
        Some(zero_word.as_str()),
        "the archive's pre-block stkAAVE balance is zero: the debt position is created in-span"
    );
}

/// The first-mint capture's committed delta stream: the real borrow block
/// replays from its committed seed over its committed cassette and reduces the
/// committed canonical deltas byte-for-byte. The stream is where the GHO
/// discount reasoning is pinned — after the refresh signal, its parent-block
/// observation then the recomputed discount — and where the reserve
/// configuration delta and the borrow's debt movement prove the same chunk
/// carries both families. A bare refresh signal (no observation, no refresh)
/// fails here rather than passing as a cheaper shape.
#[test]
#[expect(clippy::too_many_lines)]
fn first_mint_delta_stream_matches_the_committed_golden() {
    let capture = surface_capture_of(AAVE_FIRST_MINT_CAPTURE);
    let events = capture
        .aave_applied()
        .expect("the authority states the first-mint capture's applied events")
        .events_applied;
    let cassette = capture_cassette(&capture);
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let transport = CassetteReplayTransport::new(cassette);
    let provider = transport.as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let (path, market_id) = restored_seed_db(FIRST_MINT_SEED_GOLDEN, dir.path(), "first-mint.db");
    // The ledger wrapper IS the run's DB handle, so the statement count the
    // authority states rides the same replayed chunk (ADR-068 D3).
    let _ledger_session = ledger_session();
    let (ledger, _state) = LedgerDb::open_for_writes(&path).unwrap();
    let sink = Arc::new(ProgressCollector::default());
    let report = AaveMarketWriter::acquire(&path, market_id)
        .unwrap()
        .update_through(
            ledger.db(),
            RunSpec {
                chain_id,
                market_id,
                to_block: Some(FIRST_MINT_BLOCK),
                chunk_size: FIRST_MINT_BLOCK - FIRST_MINT_PARENT,
                max_block_range: 10_000,
                provider,
                cancel: Arc::new(AtomicBool::new(false)),
                progress: sink.clone(),
                verify: VerifyPolicy::default(),
                max_chunks: None,
            },
        )
        .expect("the replayed first mint must commit cleanly");
    assert_eq!(report.chunks_committed, 1);
    assert_eq!(report.total_events_applied, events);

    let records = ledger.records().expect("the capture session is armed");
    assert_eq!(
        records.len(),
        capture.sql.statements,
        "statement count drifted - an N+1 regression added or removed a statement per chunk apply"
    );

    // The reserve the run resolved is the row the seed restored: its
    // case-preserving address is what the replay's lookup matched, so a codec
    // that lowercased the seed could not have served this chunk.
    {
        let conn = ledger.db().lock();
        let underlying: String = conn
            .query_row("SELECT address FROM erc20_tokens WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("the restored seed carries the reserve's erc20 row");
        assert_eq!(
            underlying, GHO,
            "the restored reserve keeps the checksummed address the recording names"
        );
    }

    let served = transport.served_snapshot();
    assert_eq!(
        served.requests, served.served,
        "every request must be a served ledger entry - a fixture gap is a loud error, not a silent miss"
    );
    assert_eq!(
        served.served, capture.rpc.round_trips,
        "RPC round-trip count drifted - the first-mint fetch surface added or removed a read"
    );
    assert_eq!(
        served.response_bytes, capture.rpc.response_bytes,
        "served response bytes drifted - the first-mint corpus's recorded answers changed shape"
    );

    let chunks = sink.chunks.lock().unwrap().clone();
    let produced = delta_stream(&chunks);
    let lines: Vec<&str> = produced.lines().collect();
    let position = |needle: &str| {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("the first-mint stream carries {needle}:\n{produced}"))
    };
    let refresh = position("effect=gho-refresh-discount{");
    assert_eq!(
        position("effect=stk-aave-balance-observed{"),
        refresh + 1,
        "the balance observation is the refresh signal's next delta:\n{produced}"
    );
    assert_eq!(
        position("effect=gho-discount-refreshed{"),
        refresh + 2,
        "the recomputed discount follows its observation:\n{produced}"
    );
    assert_eq!(
        produced
            .matches("effect=stk-aave-balance-observed{")
            .count(),
        1,
        "one refresh signal, one observation:\n{produced}"
    );
    let reserve_config = position("effect=reserve-data-updated{");
    assert!(
        reserve_config < refresh,
        "the reserve configuration delta precedes the refresh:\n{produced}"
    );
    let borrow = position("effect=scaled-balance{aave_v3_debt_positions:");
    assert!(
        borrow < refresh,
        "the borrow's debt movement precedes the refresh its mint signal triggers:\n{produced}"
    );

    check_or_regen(
        "first-mint canonical delta stream",
        &produced,
        FIRST_MINT_DELTA_GOLDEN,
    );
}

/// The archive proof the capture is seeded from: every proof call is the
/// capture parent's, its selector is keccak of the getter it names, and its
/// answer is the value the committed seed carries. `scaledBalanceOf` is the zero
/// pre-state that makes the in-span position creation truthful. The proof is an
/// independent read: it never derives from the recording it corroborates.
#[test]
#[expect(clippy::too_many_lines)]
fn first_mint_evidence_pins_the_capture_parent_pre_state_the_seed_carries() {
    let evidence: Value = serde_json::from_str(
        &std::fs::read_to_string(FIRST_MINT_EVIDENCE).expect("the committed evidence must exist"),
    )
    .expect("the evidence is a JSON document");
    let seed: Value = serde_json::from_str(
        &std::fs::read_to_string(FIRST_MINT_SEED_GOLDEN).expect("the committed seed must exist"),
    )
    .expect("the seed is a JSON document");

    assert_eq!(
        evidence["schema"],
        json!("degenbot.aave_capture_evidence/v1")
    );
    assert_eq!(
        evidence["provenance"]["capture_parent"],
        json!(FIRST_MINT_PARENT)
    );
    assert_eq!(
        evidence["provenance"]["capture_block"],
        json!(FIRST_MINT_BLOCK)
    );
    let capture = surface_capture_of(AAVE_FIRST_MINT_CAPTURE);
    let cassette = capture_cassette(&capture);
    assert_eq!(
        evidence["provenance"]["capture_parent_hash"],
        json!(recorded_block_hash(&cassette, FIRST_MINT_PARENT)),
        "the evidence names the parent header the cassette records"
    );
    assert_eq!(
        evidence["provenance"]["capture_block_hash"],
        json!(recorded_block_hash(&cassette, FIRST_MINT_BLOCK)),
        "the evidence names the span header the cassette records"
    );

    let calls = evidence["calls"]
        .as_array()
        .expect("the evidence carries its proof calls");
    let call = |name: &str| {
        calls
            .iter()
            .find(|call| call["name"] == json!(name))
            .unwrap_or_else(|| panic!("the evidence proves {name}"))
    };
    for call in calls {
        assert_eq!(
            call["block"],
            json!(FIRST_MINT_PARENT),
            "every proof call reads the capture parent"
        );
        let signature = call["signature"]
            .as_str()
            .expect("a proof call names its getter");
        let selector = call["selector"]
            .as_str()
            .expect("a proof call carries its selector");
        assert_eq!(
            selector,
            format!(
                "0x{}",
                alloy::hex::encode(&keccak256(signature.as_bytes()).0[..4])
            ),
            "the selector IS the named getter's: {signature}"
        );
    }

    let seed_cell = |table: &str, column: &str| -> Value {
        let table_doc = seed["tables"]
            .as_array()
            .expect("the seed carries its tables")
            .iter()
            .find(|doc| doc["table"] == json!(table))
            .unwrap_or_else(|| panic!("the seed carries {table}"));
        let columns = table_doc["columns"]
            .as_array()
            .expect("the table names its columns");
        let index = columns
            .iter()
            .position(|name| name == &json!(column))
            .unwrap_or_else(|| panic!("{table} carries {column}"));
        table_doc["rows"]
            .as_array()
            .expect("the table carries rows")[0][index]
            .clone()
    };

    let erc20_address = |id: u64| -> String {
        let table_doc = seed["tables"]
            .as_array()
            .expect("the seed carries its tables")
            .iter()
            .find(|doc| doc["table"] == json!("erc20_tokens"))
            .unwrap_or_else(|| panic!("the seed carries erc20_tokens"));
        let columns = table_doc["columns"]
            .as_array()
            .expect("the table names its columns");
        let column = |name: &str| {
            columns
                .iter()
                .position(|entry| entry == &json!(name))
                .unwrap_or_else(|| panic!("erc20_tokens carries {name}"))
        };
        let (id_index, address_index) = (column("id"), column("address"));
        table_doc["rows"]
            .as_array()
            .expect("the table carries rows")
            .iter()
            .find(|row| row[id_index].as_u64() == Some(id))
            .unwrap_or_else(|| panic!("the seed carries the erc20 row {id}"))[address_index]
            .as_str()
            .expect("an erc20 row's address is a string")
            .to_string()
    };
    // The proof's targets are the reserve's OWN scaled tokens, taken from the
    // seed's FK graph rather than restated: a substituted target cannot agree
    // with both the seed and the evidence.
    let debt_token_address = erc20_address(
        seed_cell("aave_gho_tokens", "v_token_id")
            .as_u64()
            .expect("the seed's GHO vToken id is an integer"),
    );
    let collateral_token_address = erc20_address(
        seed_cell("aave_v3_assets", "a_token_id")
            .as_u64()
            .expect("the seed's aToken id is an integer"),
    );
    assert_eq!(
        debt_token_address, GHO_VTOKEN,
        "the seed's GHO vToken is the capture's debt token"
    );
    assert_eq!(
        collateral_token_address, GHO_ATOKEN,
        "the seed's GHO aToken is the capture's collateral token"
    );
    for name in [
        "scaledBalanceOf",
        "getDiscountToken",
        "getDiscountRateStrategy",
        "DEBT_TOKEN_REVISION",
    ] {
        assert_eq!(
            call(name)["target"],
            json!(debt_token_address),
            "the evidence reads {name} on the GHO vToken the seed seeds"
        );
    }
    assert_eq!(
        call("ATOKEN_REVISION")["target"],
        json!(collateral_token_address),
        "the evidence reads ATOKEN_REVISION on the GHO aToken the seed seeds"
    );

    let zero_word = format!("0x{}", "0".repeat(64));
    assert_eq!(
        call("scaledBalanceOf")["returned"],
        json!(zero_word),
        "the debt leg has no scaled balance before the borrow"
    );
    assert_eq!(
        call("getDiscountToken")["returned"],
        json!(format!(
            "0x{}{}",
            "0".repeat(24),
            STK_AAVE[2..].to_lowercase()
        )),
        "the discount token is the stkAAVE the seed seeds"
    );
    assert_eq!(
        call("getDiscountToken")["returned"],
        json!(format!(
            "0x{}{}",
            "0".repeat(24),
            seed_cell("aave_gho_tokens", "v_gho_discount_token")
                .as_str()
                .expect("the seed's discount token is an address")[2..]
                .to_lowercase()
        )),
        "the proof and the seed name one discount token"
    );
    assert_eq!(
        call("getDiscountRateStrategy")["returned"],
        json!(format!(
            "0x{}{}",
            "0".repeat(24),
            seed_cell("aave_gho_tokens", "v_gho_discount_rate_strategy")
                .as_str()
                .expect("the seed's rate strategy is an address")[2..]
                .to_lowercase()
        )),
        "the proof and the seed name one discount rate strategy"
    );
    let revision = |column: &str| {
        seed_cell("aave_v3_assets", column)
            .as_u64()
            .unwrap_or_else(|| panic!("the seed's {column} is an integer"))
    };
    assert_eq!(
        call("ATOKEN_REVISION")["returned"],
        json!(format!("0x{:064x}", revision("a_token_revision"))),
        "the reserve's aToken revision is the seeded one"
    );
    assert_eq!(
        call("DEBT_TOKEN_REVISION")["returned"],
        json!(format!("0x{:064x}", revision("v_token_revision"))),
        "the reserve's debt-token revision is the seeded one"
    );
}

fn committed_cassette() -> Cassette {
    capture_cassette(&surface_capture())
}

/// One governed capture's committed cassette package.
fn capture_cassette(capture: &ReplayCapture) -> Cassette {
    let path = repo_root().join(&capture.cassette);
    let bytes = degenbot_rpc::cassette::read_artifact(&path)
        .unwrap_or_else(|e| panic!("capture {}: cassette {}: {e}", capture.id, path.display()));
    verify_cassette_bytes(&bytes).expect("the committed cassette must pass the drift gate");
    Cassette::from_json_bytes(&bytes).unwrap()
}

#[test]
#[expect(clippy::too_many_lines)]
fn aave_chunk_replay_commits_the_recorded_span_offline_and_restarts_clean() {
    let capture = surface_capture();
    let events = capture
        .aave_applied()
        .expect("the authority states the aave capture's applied events")
        .events_applied;
    let cassette = committed_cassette();
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let span = cassette.provenance.span;
    assert_eq!(span.from_block, SPAN_FROM);
    assert_eq!(span.to_block, SPAN_TO);

    // D5 injection: the replay transport presents as a live AlloyProvider -
    // the chunk loop runs unchanged, its answers come only from the ledger
    // (no socket exists to dial). The handle stays here so the run's
    // serving stats (the RPC round-trip gate below) are readable.
    let transport = CassetteReplayTransport::new(cassette);
    let provider = transport.as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let (path, market_id) = seeded_db(dir.path(), "replay.db");

    // One chunk covering the whole recorded span (chunk_size > span width).
    let sink = Arc::new(ProgressCollector::default());
    let report = AaveMarketWriter::acquire(&path, market_id)
        .unwrap()
        .update(RunSpec {
            chain_id,
            market_id,
            to_block: Some(SPAN_TO),
            chunk_size: SPAN_TO - SPAN_FROM + 1,
            max_block_range: 10_000,
            provider: provider.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: sink.clone(),
            verify: VerifyPolicy::default(),
            max_chunks: None,
        })
        .expect("the replayed run must commit cleanly");

    assert_eq!(report.chain_id, chain_id);
    assert_eq!(report.market_id, market_id);
    assert_eq!(report.from_block, SPAN_FROM, "the run starts at the pin");
    assert_eq!(report.to_block, SPAN_TO, "the run advances to the pin");
    assert_eq!(
        report.chunks_committed, 1,
        "the whole recorded span is one chunk"
    );
    assert_eq!(
        report.total_events_applied, events,
        "the apply wrote exactly the recorded span's events (the authority's reviewed count)"
    );
    assert_eq!(
        market_last_update_block(&path, market_id),
        Some(i64::try_from(SPAN_TO).unwrap()),
        "the stamp advanced to the span end (the restart cursor)"
    );

    // The AaveChunkProgress report shape: one committed final chunk spanning
    // the pin, carrying the applied-event count and the touched users.
    let chunks = sink.chunks.lock().unwrap().clone();
    assert_eq!(chunks.len(), 1, "one chunk report");
    let progress = &chunks[0];
    assert_eq!(progress.chain_id, chain_id);
    assert_eq!(progress.market_id, market_id);
    assert_eq!(progress.chunk_start, SPAN_FROM);
    assert_eq!(progress.chunk_end, SPAN_TO);
    assert_eq!(progress.events_applied, events);
    assert!(progress.committed, "the chunk committed");
    assert!(progress.is_final, "the chunk is the run's final chunk");
    assert!(
        !progress.touched_user_addresses.is_empty(),
        "the chunk's logs touched users"
    );
    assert_eq!(
        progress.rpc_round_trips_in_lock, 0,
        "resolution runs before the write transaction opens: no provider round trip may \
         land inside the lock"
    );

    // Gate: the RPC round-trip drift tripwire. The chunk's fetch surface
    // serves EXACTLY the committed corpus's entries - every request the run
    // issued is a served ledger entry (a fixture gap would be a loud error
    // mid-run, so `requests == served` here), and the count/bytes are the
    // independent literals above. A shaping regression (an added pass, a
    // dropped scan, a re-recorded corpus) fails this plain assertion.
    let served = transport.served_snapshot();
    assert_eq!(
        served.requests, served.served,
        "every request must be a served ledger entry - a fixture gap is a loud error, not a silent miss"
    );
    assert_eq!(
        served.served, capture.rpc.round_trips,
        "RPC round-trip count drifted - the chunk's fetch surface added or removed a pass"
    );
    assert_eq!(
        served.response_bytes, capture.rpc.response_bytes,
        "served response bytes drifted - the corpus's recorded answers changed shape"
    );
    // The acquisition shape, per method: one complete receipt per candidate
    // transaction, one header per candidate block (terminal reused), and NO call
    // trace on a receipt-only corpus (ADR-105 D2: tracing every transaction is
    // not required, and a receipt-only operation must not pay for one).
    assert_eq!(
        method_served(&served, "eth_getTransactionReceipt"),
        EXPECTED_CANDIDATE_TRANSACTIONS,
        "every candidate transaction is receipt-bound before its parse"
    );
    assert_eq!(
        method_served(&served, "eth_getBlockByNumber"),
        EXPECTED_BLOCK_HEADER_READS,
        "one header per candidate block, with the terminal anchor reused"
    );
    assert_eq!(
        method_served(&served, "debug_traceTransaction"),
        EXPECTED_TRACE_REQUESTS,
        "a receipt-only corpus must issue no trace request"
    );

    // Restart no-op: the committed stamp roots the second run's cursor past
    // the pin, so it commits nothing and advances nothing.
    let restart = AaveMarketWriter::acquire(&path, market_id)
        .unwrap()
        .update(RunSpec {
            chain_id,
            market_id,
            to_block: Some(SPAN_TO),
            chunk_size: SPAN_TO - SPAN_FROM + 1,
            max_block_range: 10_000,
            provider,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(NoProgress),
            verify: VerifyPolicy::default(),
            max_chunks: None,
        })
        .expect("the restarted run must be a clean no-op");
    assert_eq!(restart.chunks_committed, 0, "nothing left to commit");
    assert_eq!(restart.total_events_applied, 0);
    assert_eq!(
        market_last_update_block(&path, market_id),
        Some(i64::try_from(SPAN_TO).unwrap()),
        "the stamp did not move past the pin"
    );

    // The no-op restart's serving stats: the recovery fence's ONE header read
    // and nothing else. The restart invariant's RPC face is now "no chunk
    // evidence", not "no read at all": a committed cursor can be on an
    // abandoned branch, and the head's silence is exactly the case a reorg
    // below the cursor produces (ADR-105 D9).
    let after_restart = transport.served_snapshot();
    assert_eq!(
        after_restart.served - served.served,
        1,
        "the restart serves the one header the recovery fence needs"
    );
}

/// The checkpoint a replayed chunk commits agrees with the exact cursor it
/// commits, and the journal is complete whether or not a checkpoint was
/// scheduled (ADR-105 D8, D9).
///
/// The checkpoint's cursor identity is not an argument the driver supplies: the
/// unit writer derives it from the row it just wrote, so the two can only agree.
/// This gate holds the driver to that, over the real corpus: the span's one unit
/// journals every delta, advances the exact cursor to the span end, and
/// checkpoints exactly that position.
#[test]
fn the_replayed_chunk_checkpoints_the_cursor_it_commits() {
    let cassette = committed_cassette();
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let provider = CassetteReplayTransport::new(cassette).as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let (path, market_id) = seeded_db(dir.path(), "checkpoint.db");
    AaveMarketWriter::acquire(&path, market_id)
        .unwrap()
        .update(RunSpec {
            chain_id,
            market_id,
            to_block: Some(SPAN_TO),
            chunk_size: SPAN_TO - SPAN_FROM + 1,
            max_block_range: 10_000,
            provider,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(NoProgress),
            verify: VerifyPolicy::default(),
            max_chunks: None,
        })
        .expect("the replayed run must commit cleanly");

    let (db, _state) = DegenbotDb::open_for_writes(&path).unwrap();
    let conn = db.lock();
    let cursor = degenbot_db::read_projection_cursor_on_conn(&conn, market_id)
        .expect("the cursor read")
        .expect("the run committed an exact cursor");
    let checkpoint = degenbot_db::read_latest_checkpoint_on_conn(&conn, market_id)
        .expect("the checkpoint read")
        .expect("the run's advancing unit checkpointed its cursor");

    assert_eq!(checkpoint.journal_sequence, cursor.journal_sequence);
    assert_eq!(
        checkpoint.position, cursor.position,
        "one cursor, one position"
    );
    assert_eq!(checkpoint.range_end_block, cursor.range_end_block);
    assert_eq!(
        checkpoint.encoding_version,
        degenbot_aave::projection::CHECKPOINT_ENCODING_VERSION,
        "the checkpoint states the payload version its vocabulary owns"
    );

    let journal = degenbot_db::read_journal_on_conn(&conn, market_id).expect("the journal read");
    assert_eq!(
        i64::try_from(journal.len()).unwrap(),
        cursor.journal_sequence,
        "the journal is complete on its own: it needs no checkpoint to be whole"
    );
    assert_eq!(
        journal[usize::try_from(cursor.journal_sequence).unwrap() - 1].position,
        checkpoint.position
    );
    assert!(
        !checkpoint.generations.is_empty(),
        "the checkpoint states the generations the projection read under"
    );

    let settled: i64 = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM aave_v3_collateral_positions p \
                     JOIN aave_v3_users u ON u.id = p.user_id WHERE u.market_id = ?1) \
                  + (SELECT COUNT(*) FROM aave_v3_debt_positions p \
                     JOIN aave_v3_users u ON u.id = p.user_id WHERE u.market_id = ?1)",
            rusqlite::params![market_id],
            |row| row.get(0),
        )
        .expect("the settled position count");
    assert_eq!(
        i64::try_from(
            checkpoint
                .positions
                .iter()
                .filter(|state| !state.balance.is_zero())
                .count()
        )
        .unwrap(),
        settled,
        "every settled position row of the market travels in the checkpoint"
    );
    assert!(
        checkpoint
            .positions
            .iter()
            .any(|state| state.balance.is_zero()),
        "the view's settled-zero positions travel too: the chunk cleanup deleted their rows          and a retained delta may move them again"
    );
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
fn aave_chunk_ledger_and_db_dump_match_the_committed_goldens() {
    let capture = surface_capture();
    let events = capture
        .aave_applied()
        .expect("the authority states the aave capture's applied events")
        .events_applied;
    let cassette = committed_cassette();
    let chain_id = i64::try_from(cassette.chain_id).unwrap();

    // D5 injection: the replay transport presents as a live AlloyProvider.
    let provider = CassetteReplayTransport::new(cassette).as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let (path, market_id) = seeded_db(dir.path(), "golden.db");

    // The ledger wrapper IS the run's DB handle - the trace hooks ride the
    // connection the chunk loop actually uses (ADR-068 D3).
    let _ledger_session = ledger_session();
    let (ledger, _state) = LedgerDb::open_for_writes(&path).unwrap();
    let report = AaveMarketWriter::acquire(&path, market_id)
        .unwrap()
        .update_through(
            ledger.db(),
            RunSpec {
                chain_id,
                market_id,
                to_block: Some(SPAN_TO),
                chunk_size: SPAN_TO - SPAN_FROM + 1,
                max_block_range: 10_000,
                provider,
                cancel: Arc::new(AtomicBool::new(false)),
                progress: Arc::new(NoProgress),
                verify: VerifyPolicy::default(),
                max_chunks: None,
            },
        )
        .expect("the replayed run must commit cleanly");
    assert_eq!(report.chunks_committed, 1);
    assert_eq!(report.total_events_applied, events);

    // Gate 1: the statement-count assertion (the authority's reviewed literal -
    // the plain N+1 tripwire).
    let records = ledger.records().expect("the capture session is armed");
    eprintln!("aave chunk statement count: {}", records.len());
    assert_eq!(
        records.len(),
        capture.sql.statements,
        "statement count drifted - an N+1 regression added or removed a \
         statement per chunk apply"
    );

    // Gate 2: the ledger golden (byte-identical regen-and-diff).
    let ledger_json = ledger_golden_json(&records);
    check_or_regen("statement ledger", &ledger_json, LEDGER_GOLDEN_PATH);

    // Gate 3: the canonical DB-dump golden over the SAME connection the run
    // wrote through.
    let conn = ledger.db().lock();
    let dump_json = dump_tables_golden_json(&conn, DUMP_TABLES).expect("dump the touched tables");
    drop(conn);
    check_or_regen("db dump", &dump_json, DUMP_GOLDEN_PATH);
}

// ── the run's terminal anchor (ADR-105 D1/D2) ──────────────────────────────
//
// Both arms of the run's terminal selection — the chain tip and an explicit
// to_block — resolve ONE exact ChainAnchor through the same acquisition
// interface the live node serves, and the anchor is carried into the chunk
// rather than discarded back to a number. The recorded captures are the corpus
// itself: only the evidence under test is rewritten.

/// One recorded answer in the recorder's canonical form, digested the way the
/// recorder writes it.
fn recorded_entry(result: Value) -> CassetteEntry {
    let response = CassetteResponse::Success { result };
    CassetteEntry {
        digest: entry_digest(&response),
        response,
    }
}

fn header_key(number: u64) -> String {
    entry_key(
        "eth_getBlockByNumber",
        &json!([format!("0x{number:x}"), false]),
    )
}

fn chain_key() -> String {
    entry_key("eth_chainId", &Value::Null)
}

fn head_key() -> String {
    entry_key("eth_blockNumber", &Value::Null)
}

/// The committed capture with its recorded head number rewritten to the pinned
/// span end, so the tip arm resolves the same terminal height the pin names.
fn tip_pinned_cassette() -> Cassette {
    let mut cassette = committed_cassette();
    cassette.entries.insert(
        head_key(),
        recorded_entry(Value::String(SPAN_TO.to_string())),
    );
    cassette
}

/// The terminal anchor the committed capture recorded, read back through the
/// cassette's own header evidence.
fn recorded_terminal_anchor() -> degenbot_core::canonical_head::ChainAnchor {
    committed_cassette()
        .recorded_headers()
        .expect("the committed capture carries its recorded headers")
        .into_iter()
        .find(|anchor| anchor.number() == SPAN_TO)
        .expect("the capture records the block its span closes on")
}

/// The per-method served count the transport reported for `method` (zero when
/// the run never asked for it).
fn method_served(served: &ServedSnapshot, method: &str) -> u64 {
    served
        .per_method
        .get(method)
        .map_or(0, |entry| entry.served)
}

/// One replayed chunk's outcome, serving stats, and the cursor the run left
/// behind (the restart-invariant state a refusal must not move).
struct ReplayedChunk {
    result: Result<AaveUpdateReport, RunError>,
    served: ServedSnapshot,
    last_update_block: Option<i64>,
}

/// Run one replayed chunk over a cassette and report its outcome, the serving
/// stats, and the market cursor afterwards.
fn run_recorded_state(cassette: &Cassette, to_block: Option<u64>) -> ReplayedChunk {
    let transport = CassetteReplayTransport::new(cassette.clone());
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let dir = TempDir::new().unwrap();
    let (path, market_id) = seeded_db(dir.path(), "terminal.db");
    let result = AaveMarketWriter::acquire(&path, market_id)
        .unwrap()
        .update(RunSpec {
            chain_id,
            market_id,
            to_block,
            chunk_size: SPAN_TO - SPAN_FROM + 1,
            max_block_range: 10_000,
            provider: transport.as_alloy_provider(),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(NoProgress),
            verify: VerifyPolicy::default(),
            max_chunks: None,
        });
    ReplayedChunk {
        result,
        served: transport.served_snapshot(),
        last_update_block: market_last_update_block(&path, market_id),
    }
}

/// Run one replayed chunk over a cassette, returning the run's outcome and the
/// transport's serving stats.
fn run_recorded(
    cassette: &Cassette,
    to_block: Option<u64>,
) -> (Result<AaveUpdateReport, RunError>, ServedSnapshot) {
    let run = run_recorded_state(cassette, to_block);
    (run.result, run.served)
}

/// The cassette's candidate transactions, in the ledger's own order: every
/// transaction a recorded `eth_getLogs` answer carries a log for.
fn candidate_transactions(cassette: &Cassette) -> Vec<B256> {
    let mut found: Vec<B256> = Vec::new();
    for entry in cassette.entries.values() {
        let CassetteResponse::Success { result } = &entry.response else {
            continue;
        };
        let Some(logs) = result.as_array() else {
            continue;
        };
        for log in logs {
            if log.get("logIndex").is_none() {
                continue;
            }
            let hash = log["transactionHash"]
                .as_str()
                .and_then(|raw| raw.parse::<B256>().ok())
                .unwrap_or_else(|| panic!("a recorded log names its transaction"));
            if !found.contains(&hash) {
                found.push(hash);
            }
        }
    }
    found
}

/// The block-wide log indices of `tx`'s FILTERED logs (the acquisition's input),
/// in the ledger's own order.
fn filtered_log_indices(cassette: &Cassette, tx: B256) -> Vec<String> {
    let mut indices = Vec::new();
    for entry in cassette.entries.values() {
        let CassetteResponse::Success { result } = &entry.response else {
            continue;
        };
        let Some(logs) = result.as_array() else {
            continue;
        };
        for log in logs {
            if log["transactionHash"]
                .as_str()
                .and_then(|raw| raw.parse::<B256>().ok())
                != Some(tx)
            {
                continue;
            }
            if let Some(index) = log["logIndex"].as_str() {
                indices.push(index.to_string());
            }
        }
    }
    indices.sort_by_key(|index| index.parse::<u64>().unwrap_or_default());
    indices.dedup();
    indices
}

fn receipt_key(tx: B256) -> String {
    entry_key("eth_getTransactionReceipt", &json!([format!("{tx}")]))
}

fn trace_key(tx: B256) -> String {
    entry_key(
        "debug_traceTransaction",
        &json!([
            format!("{tx}"),
            { "tracer": "callTracer", "tracerConfig": { "withLog": true } }
        ]),
    )
}

/// The recorded receipt answer of `tx`, or a loud failure: the corpus must
/// carry one complete receipt per candidate transaction.
fn recorded_receipt(cassette: &Cassette, tx: B256) -> Value {
    let entry = cassette
        .entries
        .get(&receipt_key(tx))
        .unwrap_or_else(|| panic!("the corpus records a complete receipt for {tx}"));
    let CassetteResponse::Success { result } = &entry.response else {
        panic!("the recorded receipt for {tx} is an answer");
    };
    result.clone()
}

// ── candidate-transaction evidence (ADR-105 D2/D5) ─────────────────────────
//
// Filtered acquisition names the chunk's candidate transactions; each one is
// receipt-bound before its parse, and the binding runs OUTSIDE the chunk's
// transaction. A receipt the node cannot serve, or one whose content contradicts
// the filtered logs, refuses the chunk before any write, so the cursor cannot
// advance on evidence the parse would have read.

#[test]
fn a_candidate_transaction_without_its_receipt_refuses_before_the_chunk_transaction() {
    let mut cassette = committed_cassette();
    let candidates = candidate_transactions(&cassette);
    assert_eq!(
        u64::try_from(candidates.len()).unwrap(),
        EXPECTED_CANDIDATE_TRANSACTIONS,
        "the corpus names the span's candidate transactions"
    );
    let missing = candidates[0];
    cassette.entries.remove(&receipt_key(missing));

    let run = run_recorded_state(&cassette, Some(SPAN_TO));

    let error = run
        .result
        .expect_err("a candidate transaction with no complete receipt cannot be parsed");
    assert!(
        matches!(error, RunError::Evidence(_)),
        "the refusal is the evidence class, got {error:?}"
    );
    assert!(
        run.served.per_method.contains_key("eth_getLogs"),
        "the chunk was fetched, so the refusal came after acquisition began"
    );
    assert_eq!(
        run.last_update_block,
        Some(i64::try_from(SPAN_FROM).unwrap() - 1),
        "no receipt acquisition failure may advance the cursor"
    );

    // Exact restoration: the refusal wrote nothing, so the intact corpus commits
    // the same chunk unchanged.
    let intact = run_recorded_state(&committed_cassette(), Some(SPAN_TO));
    intact.result.expect("the intact corpus still commits");
    assert_eq!(
        intact.last_update_block,
        Some(i64::try_from(SPAN_TO).unwrap()),
        "the canonical replay still advances to the span end"
    );
}

#[test]
fn a_foreign_receipt_refuses_before_the_chunk_transaction() {
    let mut cassette = committed_cassette();
    let candidates = candidate_transactions(&cassette);
    let (asked, other) = (candidates[0], candidates[1]);
    let mut foreign = recorded_receipt(&cassette, other);
    foreign["transactionHash"] = json!(format!("{asked}"));
    cassette
        .entries
        .insert(receipt_key(asked), recorded_entry(foreign));

    let run = run_recorded_state(&cassette, Some(SPAN_TO));

    let error = run
        .result
        .expect_err("another transaction's receipt is not this transaction's evidence");
    assert!(
        matches!(error, RunError::Evidence(_)),
        "a foreign receipt is an evidence refusal, got {error:?}"
    );
    assert_eq!(
        run.last_update_block,
        Some(i64::try_from(SPAN_FROM).unwrap() - 1),
        "a foreign receipt may not advance the cursor"
    );
}

#[test]
fn a_mutated_receipt_log_refuses_before_the_chunk_transaction() {
    let mut cassette = committed_cassette();
    let candidate = candidate_transactions(&cassette)[0];
    let filtered = filtered_log_indices(&cassette, candidate);
    let mut receipt = recorded_receipt(&cassette, candidate);
    let logs = receipt["logs"]
        .as_array_mut()
        .expect("a recorded receipt carries its logs");
    let position = logs
        .iter()
        .position(|log| log["logIndex"].as_str() == Some(filtered[0].as_str()))
        .expect("the filtered log is a log of the complete receipt");
    logs[position]["data"] = json!("0xdeadbeef");

    cassette
        .entries
        .insert(receipt_key(candidate), recorded_entry(receipt));

    let run = run_recorded_state(&cassette, Some(SPAN_TO));

    let error = run
        .result
        .expect_err("a receipt whose log content contradicts the filtered log is not evidence");
    assert!(
        matches!(error, RunError::Evidence(_)),
        "a mutated receipt is an evidence refusal, got {error:?}"
    );
    assert_eq!(
        run.last_update_block,
        Some(i64::try_from(SPAN_FROM).unwrap() - 1),
        "mutated evidence may not advance the cursor"
    );
}

/// The committed corpus must not carry a trace answer the run never asks for:
/// on a receipt-only span the evidence is the receipt alone, and a recorded call
/// tree would be fixture weight no consumer reads (ADR-105 D2).
#[test]
fn a_receipt_only_corpus_records_no_trace_answer() {
    let cassette = committed_cassette();

    for candidate in candidate_transactions(&cassette) {
        assert!(
            !cassette.entries.contains_key(&trace_key(candidate)),
            "the receipt-only span records no call trace for {candidate}"
        );
    }
}

#[test]
fn tip_and_explicit_runs_resolve_the_same_terminal_anchor() {
    let events = surface_capture()
        .aave_applied()
        .expect("the authority states the aave capture's applied events")
        .events_applied;
    let pinned = recorded_terminal_anchor();
    let (explicit, explicit_served) = run_recorded(&committed_cassette(), Some(SPAN_TO));
    let explicit = explicit.expect("the pinned replay must commit");
    let (tip, tip_served) = run_recorded(&tip_pinned_cassette(), None);
    let tip = tip.expect("the tip replay must commit");

    assert_eq!(
        explicit.terminal,
        Some(pinned),
        "the pinned run states the anchor its terminal header names"
    );
    assert_eq!(
        tip.terminal, explicit.terminal,
        "the tip arm resolves through the SAME terminal path as a pin"
    );
    assert_eq!(explicit.to_block, SPAN_TO);
    assert_eq!(tip.to_block, explicit.to_block);
    assert_eq!(explicit.total_events_applied, events);
    assert_eq!(
        tip.total_events_applied, explicit.total_events_applied,
        "the anchor did not change what the chunk applied"
    );

    let header_reads = |served: &ServedSnapshot| {
        served
            .per_method
            .get("eth_getBlockByNumber")
            .map(|method| method.served)
    };
    assert_eq!(
        header_reads(&explicit_served),
        Some(EXPECTED_BLOCK_HEADER_READS)
    );
    assert_eq!(
        header_reads(&tip_served),
        Some(EXPECTED_BLOCK_HEADER_READS),
        "the tip arm resolves its terminal header and the candidate blocks' the same way"
    );
    assert!(
        !explicit_served.per_method.contains_key("eth_blockNumber"),
        "a pinned run never asks the chain for its tip"
    );
    assert_eq!(
        tip_served
            .per_method
            .get("eth_blockNumber")
            .map(|method| method.served),
        Some(1),
        "the tip arm resolves its height from the node's head"
    );
}

#[test]
fn a_missing_terminal_header_refuses_before_any_chunk_work() {
    let mut cassette = committed_cassette();
    cassette.entries.remove(&header_key(SPAN_TO));

    let (result, served) = run_recorded(&cassette, Some(SPAN_TO));

    let error = result.expect_err("a terminal with no recorded header cannot anchor the run");
    assert!(
        matches!(error, RunError::TerminalAnchor(_)),
        "the refusal is the terminal-anchor class, got {error:?}"
    );
    assert!(
        !served.per_method.contains_key("eth_getLogs"),
        "no chunk was fetched: the run refused before chunk processing"
    );
}

#[test]
fn a_recorded_null_terminal_header_is_a_typed_missing_header() {
    let mut cassette = committed_cassette();
    cassette
        .entries
        .insert(header_key(SPAN_TO), recorded_entry(Value::Null));

    let (result, served) = run_recorded(&cassette, Some(SPAN_TO));

    let error = result.expect_err("a node with no complete header names no terminal");
    assert!(
        matches!(
            error,
            RunError::TerminalAnchor(ChainEvidenceError::Header(HeaderRefusal::MissingHeader {
                number: SPAN_TO
            }))
        ),
        "a null header is a typed missing-header refusal, got {error:?}"
    );
    assert!(!served.per_method.contains_key("eth_getLogs"));
}

#[test]
fn a_cross_chain_terminal_header_refuses_before_any_chunk_work() {
    let mut cassette = committed_cassette();
    cassette
        .entries
        .insert(chain_key(), recorded_entry(json!("0x2")));

    let (result, served) = run_recorded(&cassette, Some(SPAN_TO));

    let error = result.expect_err("a terminal block on another chain is not this market's");
    assert!(
        matches!(
            error,
            RunError::TerminalAnchor(ChainEvidenceError::CrossChainTerminal {
                number: SPAN_TO,
                expected: 1,
                served: 2,
            })
        ),
        "a cross-chain terminal is a typed refusal, got {error:?}"
    );
    assert!(!served.per_method.contains_key("eth_getLogs"));
}

#[test]
fn a_same_height_substitution_after_resolution_refuses_the_chunk() {
    let recorded = recorded_terminal_anchor();
    let mut cassette = committed_cassette();
    cassette.entries.insert(
        header_key(SPAN_TO),
        recorded_entry(json!({
            "number": format!("{SPAN_TO}"),
            "hash": format!("{}", B256::repeat_byte(0xb0)),
            "parentHash": format!("{}", recorded.parent_hash()),
        })),
    );

    let (result, served) = run_recorded(&cassette, Some(SPAN_TO));

    let error = result.expect_err("a same-height substitution cannot pass as the anchor's block");
    assert!(
        matches!(
            error,
            RunError::TerminalEvidence(ProjectionRefusal::LogBranchMismatch {
                number: SPAN_TO,
                ..
            })
        ),
        "the anchored height's logs refuse the substituted block, got {error:?}"
    );
    assert!(
        served.per_method.contains_key("eth_getLogs"),
        "the chunk was fetched and then refused before its transaction"
    );
}

/// The lock contract, asserted rather than inferred from a bench (ADR-105 D3):
/// every provider round trip the chunk needs is resolved BEFORE the write
/// transaction opens, so the transaction samples the process-global counter
/// unchanged. The replayed span is a full chunk - receipts, headers, per-tx
/// compute and the configuration family - not an empty one.
#[test]
fn aave_update_chunk_holds_no_rpc_round_trip_inside_the_write_lock() {
    let cassette = committed_cassette();
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let provider = CassetteReplayTransport::new(cassette).as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let (path, market_id) = seeded_db(dir.path(), "no-rpc-in-lock.db");
    let sink = Arc::new(ProgressCollector::default());
    AaveMarketWriter::acquire(&path, market_id)
        .unwrap()
        .update(RunSpec {
            chain_id,
            market_id,
            to_block: Some(SPAN_TO),
            chunk_size: SPAN_TO - SPAN_FROM + 1,
            max_block_range: 10_000,
            provider,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: sink.clone(),
            verify: VerifyPolicy::default(),
            max_chunks: None,
        })
        .expect("the replayed run must commit cleanly");

    let chunks = sink.chunks.lock().unwrap().clone();
    assert!(!chunks.is_empty(), "the run reports its chunks");
    assert!(
        chunks.iter().all(|c| c.committed),
        "every gate chunk must be a committed one - a rolled-back chunk resolves nothing"
    );
    assert!(
        chunks.iter().map(|c| c.events_applied).sum::<usize>() > 0,
        "the gate must cover a chunk that applies events, not an empty one"
    );
    for chunk in &chunks {
        assert_eq!(
            chunk.rpc_round_trips_in_lock, 0,
            "chunk {}..{} took {} provider round trip(s) inside the write lock",
            chunk.chunk_start, chunk.chunk_end, chunk.rpc_round_trips_in_lock
        );
    }
}

/// Identity creation runs during resolution, OUTSIDE the chunk transaction
/// (ADR-105 D4): the users, positions and token/asset rows the deltas reference
/// are committed before the transaction opens, so a chunk that rolls back
/// leaves them behind while the cursor stays put (a later unit's apply may
/// still create a row it needs, and that one rolls back with the chunk). A
/// retry must converge rather than fork - the retry's identities and delta
/// stream are a first-attempt run's, the final state is that run's, and no
/// duplicate rows appear.
///
/// The rollback is driven through the real driver: the pre-commit verification
/// asks for balances the receipt-only corpus never recorded, the provider
/// refuses, and the chunk's transaction (its deltas, cleanup and stamp) reverts
/// after resolution already settled every identity.
#[test]
fn a_rolled_back_chunk_retries_on_identical_identities_deltas_and_state() {
    let chain_id = i64::try_from(committed_cassette().chain_id).unwrap();
    let dir = TempDir::new().unwrap();

    let (path, market_id) = seeded_db(dir.path(), "retry.db");
    let (failed, failed_chunks) = run_span(
        &path,
        market_id,
        chain_id,
        VerifyPolicy {
            verify_chunk: true,
            ..VerifyPolicy::default()
        },
    );
    assert!(
        failed.is_err(),
        "the pre-commit verify reads a balance the corpus never recorded: the chunk must roll back"
    );
    assert!(
        failed_chunks.is_empty(),
        "the chain-truth acquisition refuses before the chunk's transaction opens, so the \
         chunk never writes: no rollback and no report - got {failed_chunks:?}"
    );
    assert_eq!(
        market_last_update_block(&path, market_id),
        Some(i64::try_from(SPAN_FROM - 1).unwrap()),
        "a rolled-back chunk must not move the cursor off the seeded pre-span position"
    );
    let leftover = identity_ids(&path);
    assert!(
        !leftover.is_empty(),
        "resolution created the identity rows before the transaction opened"
    );

    let (retried, retried_chunks) = run_span(&path, market_id, chain_id, VerifyPolicy::default());
    retried.expect("the retry commits over the same span");

    // The retried chunk is the canonical one: the same delta stream and the same
    // final state the committed goldens pin. A forked identity space (duplicate
    // users/positions/tokens, an advanced rowid) fails both byte-diffs.
    let produced_deltas = format!("{}.\n", delta_encodings(&retried_chunks).join("\n"));
    check_or_regen(
        "canonical delta stream",
        &produced_deltas,
        DELTA_STREAM_GOLDEN,
    );
    check_or_regen("db dump", &db_snapshot(&path), DUMP_GOLDEN_PATH);

    // The identities resolution fixed before the rollback are the retried run's
    // own: no user, token, asset or contract row was re-created under a new id.
    // (Positions may legitimately disappear: the retry's commit prunes the
    // zero-balance rows the rollback had restored.)
    let retried = identity_ids(&path);
    for table in &[
        "aave_v3_markets",
        "aave_v3_contracts",
        "erc20_tokens",
        "aave_v3_assets",
        "aave_v3_users",
    ] {
        let before = leftover
            .iter()
            .find(|(name, _)| name == table)
            .map(|(_, ids)| ids.clone())
            .unwrap_or_default();
        let after = retried
            .iter()
            .find(|(name, _)| name == table)
            .map(|(_, ids)| ids.clone())
            .unwrap_or_default();
        assert_eq!(
            before, after,
            "{table}: the retry must reuse the ids resolution already fixed"
        );
    }
}

/// Run the committed span over a fresh replay transport and return the chunk
/// reports (the deltas + the in-lock counters the gates read).
fn run_span(
    path: &Path,
    market_id: i64,
    chain_id: i64,
    verify: VerifyPolicy,
) -> (Result<AaveUpdateReport, RunError>, Vec<AaveChunkProgress>) {
    let provider = CassetteReplayTransport::new(committed_cassette()).as_alloy_provider();
    let sink = Arc::new(ProgressCollector::default());
    let outcome = AaveMarketWriter::acquire(path, market_id)
        .unwrap()
        .update(RunSpec {
            chain_id,
            market_id,
            to_block: Some(SPAN_TO),
            chunk_size: SPAN_TO - SPAN_FROM + 1,
            max_block_range: 10_000,
            provider,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: sink.clone(),
            verify,
            max_chunks: None,
        });
    let chunks = sink.chunks.lock().unwrap().clone();
    (outcome, chunks)
}

/// The identity tables' primary keys, in a stable order.
fn identity_ids(path: &Path) -> Vec<(String, Vec<i64>)> {
    const IDENTITY_TABLES: &[&str] = &[
        "aave_v3_markets",
        "aave_v3_contracts",
        "erc20_tokens",
        "aave_v3_assets",
        "aave_v3_users",
        "aave_v3_collateral_positions",
        "aave_v3_debt_positions",
    ];
    let (db, _state) = DegenbotDb::open_for_writes(path).unwrap();
    let conn = db.lock();
    IDENTITY_TABLES
        .iter()
        .map(|table| {
            let mut statement = conn
                .prepare(&format!("SELECT id FROM {table} ORDER BY id"))
                .unwrap();
            let ids = statement
                .query_map([], |row| row.get::<_, i64>(0))
                .unwrap()
                .collect::<Result<Vec<i64>, _>>()
                .unwrap();
            ((*table).to_string(), ids)
        })
        .collect()
}

/// The chunk reports' canonical delta stream, encoded the golden's way.
fn delta_encodings(chunks: &[AaveChunkProgress]) -> Vec<String> {
    chunks
        .iter()
        .flat_map(|chunk| {
            chunk
                .deltas
                .iter()
                .map(degenbot_aave::projection::CanonicalDelta::encode)
        })
        .collect()
}

/// The semantic database snapshot (the committed dump golden's encoder).
fn db_snapshot(path: &Path) -> String {
    let (db, _state) = DegenbotDb::open_for_writes(path).unwrap();
    let conn = db.lock();
    dump_tables_golden_json(&conn, DUMP_TABLES).expect("dump the identity and state tables")
}

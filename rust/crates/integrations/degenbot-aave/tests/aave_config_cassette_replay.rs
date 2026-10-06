//! Offline end-to-end replay of the Aave chunk loop over the CONFIG-ERA
//! golden capture (ADR-068; the pre-lock fact-phase design corpus).
//!
//! The committed cassette
//! (`tests/fixtures/cassettes/aave_config_chunk_19091050-19091059.json`) was
//! recorded by `record_updater_cassette --kind aave-run` against a live reth
//! archive node over mainnet blocks 19,091,050-19,091,059 — the GHO-discount
//! era (the GHO vToken's revision was 1, the discount mechanism live, four
//! months after the discount config landed at block 17,699,249). The span is
//! the corpus's config-heavy + discount-activity answer to the seed corpus's
//! corpus-blindness: its Pool pass carries GHO/WETH/USDT operation events,
//! and its span carries THREE variable-debt GHO `Burn`s by the SAME user in
//! THREE different transactions (blocks 19,091,050 / 19,091,053 /
//! 19,091,057) — the exact fact-dependence shape the pre-lock fact-phase
//! design must preserve:
//!
//! tx N (block 19,091,050): the user is NOT in the harness DB → the discount
//! pre-pass takes path #2 (`getDiscountPercent(user)` `eth_call` at the tx's
//! block — RECORDED, the cassette's single `eth_call`), the ops parser then
//! creates the user + the GHO debt position in the chunk apply, and the
//! post-apply GHO-discount refresh reads the just-written debt balance.
//! tx N+1 / N+2 (blocks 19,091,053 / 19,091,057): the SAME user burns again —
//! the pre-pass now takes path #1 (the DB-cache read of the row the ops
//! parser created in tx N) and issues NO RPC. A pre-lock fact phase that
//! computed the discount facts against the COMMITTED DB (without replaying
//! tx N's apply) would re-issue `getDiscountPercent` for tx N+1/N+2 — a
//! different RPC stream and a different fact source. The replay's
//! round-trip gate (7) IS that assertion: exactly one discount `eth_call`
//! serves the whole chunk.
//!
//! The SQL half (statement ledger + canonical DB dump goldens, the
//! statement-count N+1 tripwire) rides the same replay through
//! [`LedgerDb`] — the drift gate is byte-identical regeneration through the
//! same writer (`REGENERATE_SQL_GOLDENS=1`).
//!
//! The seeded temp DB mirrors the recorder's harness substrate row-for-row:
//! `activate_aave_market`'s one-shot seed (market row at the bootstrap block,
//! the `POOL_ADDRESS_PROVIDER` contract, the GHO token + bare `aave_gho_tokens`
//! row) plus the loop's warm-boot substrate (`POOL/POOL_CONFIGURATOR` with
//! chain-verified revisions, `PRICE_ORACLE`, and the span's reserve assets —
//! GHO, WETH, USDT in the recorder's candidate-ascending order). The run must
//! commit the span, advance `last_update_block`, report the
//! `AaveChunkProgress` shape, and a restarted run must be a no-op.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use degenbot_aave::updater::{
    run_aave_update, run_aave_update_on_db, AaveChunkProgress, NoProgress, ProgressSink,
};
use degenbot_db::sql_ledger::{dump_tables_golden_json, ledger_golden_json, LedgerDb};
use degenbot_db::DegenbotDb;
use degenbot_rpc::cassette::{verify_cassette_bytes, Cassette};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use tempfile::TempDir;

/// The committed config-era capture (machine-emitted, drift-gated).
const CASSETTE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/cassettes/aave_config_chunk_19091050-19091059.json"
);

/// The committed statement-ledger golden (SQL half of the golden capture).
const LEDGER_GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/sql_goldens/aave_config_chunk_19091050-19091059.statement-ledger.json"
);

/// The committed canonical DB-dump golden (post-run DB state).
const DUMP_GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../tests/fixtures/sql_goldens/aave_config_chunk_19091050-19091059.db-dump.json"
);

/// The pinned span (the recorder's `--from/--to`).
const SPAN_FROM: u64 = 19_091_050;
const SPAN_TO: u64 = 19_091_059;

/// Independent literal: the exact number of `AaveChunkEvent`s the recorded
/// span's chunk apply wrote (the recorder run's report; measured, not
/// assumed). A fetch/decode/apply regression that adds or drops applied
/// events fails here before the byte-diff.
const EXPECTED_APPLIED_EVENTS: usize = 32;

/// Independent literal: the exact number of statements one chunk apply runs
/// over this corpus - the N+1 tripwire (a query hoisted out of the per-tx
/// loop going back in, a per-row re-read, an unbatched revision call's
/// re-read all change this count and fail the plain run before the
/// byte-diff consults the committed golden). Measured on this corpus with
/// the Perf-C substrate in place; the discount-path `eth_call` adds no
/// statements, so the delta against the seed corpus's 52 is purely the
/// span's event mix (GHO debt burns + position/user creation + the
/// USDT/WETH operation events).
const EXPECTED_LEDGER_STATEMENTS: usize = 60;

/// Independent literal: the exact number of ledger entries the replayed
/// chunk serves from the cassette per run - the RPC round-trip count of the
/// chunk's fetch surface PLUS the discount pre-pass's single
/// `getDiscountPercent` `eth_call` (6 getLogs passes + 1 `eth_call`; the run
/// resolves its tip from the pin, so no `eth_chainId`/`eth_blockNumber` -
/// those recorded entries belong to the recorder's own run shape).
///
/// THE CROSS-TX FACT-DEPENDENCE GATE: exactly ONE discount `eth_call` serves
/// the whole chunk. The first GHO burn's user is absent from the harness DB
/// (path #2 - the RPC); the SAME user's two later burns read the row the
/// ops parser created in the first tx (path #1 - the DB cache, no RPC). A
/// pre-lock fact phase that computed the facts against the committed DB
/// would re-issue the call for the later txs and push this count to 9.
const EXPECTED_RPC_ROUND_TRIPS: u64 = 7;

/// Independent literal: the serialized payload bytes those served answers
/// carried (the recorded `result` members in wire form). Ties the suite's
/// expectation to this corpus file: a re-recorded cassette whose answers
/// carry different traffic fails here too.
const EXPECTED_RPC_RESPONSE_BYTES: u64 = 34815;

/// The Aave V3 Ethereum bootstrap block (`activate_aave_market`'s fresh-seed
/// stamp; the substrate constant mirrors `updater/run/activate.rs`).
const AAVE_BOOTSTRAP_BLOCK: i64 = 16_291_070;

// ── seed literals ─────────────────────────────────────────────────────────
//
// Chain-verified against the recording node (reth/v2.7.0-3d592ec via
// eth-mainnet.public.blastapi.io) at record time: the PoolAddressesProvider
// getters, `getReserveData` token pairs, `POOL_REVISION()` /
// `CONFIGURATOR_REVISION()`, and the GHO metadata calls. The recorder seeds
// exactly these rows via the same substrate fns.

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
/// recorder's candidate order (address-ascending by underlying: GHO, WETH,
/// USDT — the reserves the span's Pool-log topics touch). The aToken/vToken
/// set IS the run's scaled-token OR-set — the seed must match the recorder's
/// exactly or the replayed pass-5 getLogs filter misses its recorded entry
/// (a loud fixture gap, by design).
const RESERVES: [(&str, &str, &str); 3] = [
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

/// Seeded aToken/vToken revision (mirrors the recorder's
/// `AAVE_SEED_TOKEN_REVISION`). Era-accurate for this span: the GHO vToken's
/// on-chain revision was 1 here (the discount mechanism live — the recorded
/// `getDiscountPercent` call proves the pre-pass took the rev<4 path), and
/// no `Upgraded` config event dispatches inside the span, so the seeded
/// value is the revision every per-tx revision read sees.
const SEED_TOKEN_REVISION: i64 = 1;

/// The getDiscountPercent selector (keccak("getDiscountPercent(address)")[0..4])
/// — the recorded ``eth_call``'s calldata prefix. The cross-tx gate greps the
/// committed cassette's ledger keys for exactly ONE call carrying it.
const GET_DISCOUNT_PERCENT_SELECTOR: &str = "0x6c53272b";

/// The span's repeat-burn user (the same address in all three GHO debt
/// burns: blocks 19,091,050 / 19,091,053 / 19,091,057) — bare hex, the form
/// the recorded ``eth_call``'s calldata embeds (the 32-byte word's low 20 bytes).
const REPEAT_BURN_USER: &str = "c008e6479a83be6a6c49d95c2029a6064136688";

/// The tables one Aave chunk apply touches, in the dump's fixed
/// (alphabetical) order. The empty-but-listed tables (asset configs, eMode
/// categories) stay so a config-dispatch path accidentally writing them
/// cannot drift silently — the same posture as the pool goldens' V4 tables.
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

/// Seed the harness DB the recorder's `--kind aave-run` flow builds: (1)
/// `activate_aave_market`'s fresh-seed substrate (market row at the bootstrap
/// block, `POOL_ADDRESS_PROVIDER` contract, GHO erc20 row with metadata, bare
/// `aave_gho_tokens` row), (2) the warm-boot contract rows with their
/// chain-verified revisions, (3) the span cursor (`SPAN_FROM - 1`), (4) the
/// span's reserve assets via the `ReserveInitialized` substrate — the GHO
/// reserve LAST-write carries the GHO-vToken FK link (the recorder's
/// `gho_link`), which is what makes the per-tx `build_discount_snapshot`
/// resolve the GHO vToken at all. Row ids and insertion order mirror the
/// recorder exactly (the dump golden carries ids).
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

        // (4) the span's reserve assets (the GHO reserve carries the gho_link).
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
            let gho_link = if underlying == GHO {
                Some(DegenbotDb::get_or_create_gho_token_on_conn(&conn, 1, GHO).unwrap())
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

fn committed_cassette() -> Cassette {
    let bytes = std::fs::read(CASSETTE_PATH).expect("the committed cassette must exist");
    verify_cassette_bytes(&bytes).expect("the committed cassette must pass the drift gate");
    Cassette::from_json_bytes(&bytes).unwrap()
}

/// The committed corpus's discount-RPC shape (the cross-tx overlay case,
/// recorded): exactly ONE `eth_call` in the whole cassette, carrying the
/// `getDiscountPercent` selector for the repeat-burn user, pinned at the
/// FIRST burn tx's block. The two later same-user burns recorded NO call —
/// they took the DB-cache path against the row the ops parser created.
fn committed_discount_calls(cassette: &Cassette) -> Vec<String> {
    cassette
        .entries
        .keys()
        .filter(|k| k.contains("\"eth_call\""))
        .filter(|k| k.contains(GET_DISCOUNT_PERCENT_SELECTOR))
        .filter(|k| k.contains(REPEAT_BURN_USER))
        .cloned()
        .collect()
}

#[test]
fn aave_config_chunk_replay_commits_the_recorded_span_offline_and_restarts_clean() {
    let cassette = committed_cassette();
    let chain_id = i64::try_from(cassette.chain_id).unwrap();
    let span = cassette.provenance.span;
    assert_eq!(span.from_block, SPAN_FROM);
    assert_eq!(span.to_block, SPAN_TO);

    // The cross-tx overlay case, as recorded: ONE getDiscountPercent call,
    // pinned at the first burn tx's block, for the repeat-burn user. The
    // two later same-user txs issued no discount RPC (DB-cache path) — the
    // recorded negative space this suite's round-trip gate preserves.
    let discount_calls = committed_discount_calls(&cassette);
    assert_eq!(
        discount_calls.len(),
        1,
        "the committed corpus must carry exactly one getDiscountPercent call \
         (the first burn's path-#2 RPC); got {discount_calls:?}"
    );
    assert!(
        discount_calls[0].contains(&format!("\"{SPAN_FROM}\"")),
        "the discount call must be pinned at the first burn tx's block: {}",
        discount_calls[0]
    );

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
    let report = run_aave_update(
        &path,
        chain_id,
        market_id,
        Some(SPAN_TO),
        SPAN_TO - SPAN_FROM + 1,
        provider.clone(),
        Arc::new(AtomicBool::new(false)),
        sink.clone(),
        false,
        None,
        false,
        None,
    )
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
        report.total_events_applied, EXPECTED_APPLIED_EVENTS,
        "the apply wrote exactly the recorded span's events"
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
    assert_eq!(progress.events_applied, EXPECTED_APPLIED_EVENTS);
    assert!(progress.committed, "the chunk committed");
    assert!(progress.is_final, "the chunk is the run's final chunk");
    assert!(
        !progress.touched_user_addresses.is_empty(),
        "the chunk's logs touched users"
    );

    // Gate: the RPC round-trip drift tripwire. The chunk's serving surface
    // is EXACTLY the committed corpus minus the recorder's two ancillary
    // round trips: 6 getLogs passes + the ONE discount `eth_call`. THE
    // CROSS-TX GATE: the later same-user burn txs must take the DB-cache
    // path (the ops parser created the user in tx N) — a regression that
    // re-issues getDiscountPercent for them pushes this to 9.
    let served = transport.served_snapshot();
    assert_eq!(
        served.requests, served.served,
        "every request must be a served ledger entry - a fixture gap is a loud error, not a silent miss"
    );
    assert_eq!(
        served.served, EXPECTED_RPC_ROUND_TRIPS,
        "RPC round-trip count drifted - the chunk's fetch+discount surface added or removed a call"
    );
    assert_eq!(
        served.response_bytes, EXPECTED_RPC_RESPONSE_BYTES,
        "served response bytes drifted - the corpus's recorded answers changed shape"
    );

    assert_restart_no_op(
        &path,
        chain_id,
        market_id,
        provider,
        &transport,
        served.served,
    );
}

/// The restart no-op: the committed stamp roots the second run's cursor past
/// the pin, so it commits nothing, advances nothing, and serves nothing (the
/// restart invariant's RPC face — the cursor check is pure SQL).
fn assert_restart_no_op(
    path: &Path,
    chain_id: i64,
    market_id: i64,
    provider: degenbot_rpc::provider::AlloyProvider,
    transport: &CassetteReplayTransport,
    served_before: u64,
) {
    let restart = run_aave_update(
        path,
        chain_id,
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
    .expect("the restarted run must be a clean no-op");
    assert_eq!(restart.chunks_committed, 0, "nothing left to commit");
    assert_eq!(restart.total_events_applied, 0);
    assert_eq!(
        market_last_update_block(path, market_id),
        Some(i64::try_from(SPAN_TO).unwrap()),
        "the stamp did not move past the pin"
    );

    // The no-op restart's serving stats: zero round trips - the restart
    // invariant's RPC face (the cursor check is pure SQL).
    let after_restart = transport.served_snapshot();
    assert_eq!(
        after_restart.served - served_before,
        0,
        "the restart no-op must serve nothing"
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
fn aave_config_chunk_ledger_and_db_dump_match_the_committed_goldens() {
    let cassette = committed_cassette();
    let chain_id = i64::try_from(cassette.chain_id).unwrap();

    // D5 injection: the replay transport presents as a live AlloyProvider.
    let provider = CassetteReplayTransport::new(cassette).as_alloy_provider();

    let dir = TempDir::new().unwrap();
    let (path, market_id) = seeded_db(dir.path(), "golden.db");

    // The ledger wrapper IS the run's DB handle - the trace hooks ride the
    // connection the chunk loop actually uses (ADR-068 D3).
    let (ledger, _state) = LedgerDb::open_for_writes(&path).unwrap();
    let report = run_aave_update_on_db(
        ledger.db(),
        chain_id,
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
    .expect("the replayed run must commit cleanly");
    assert_eq!(report.chunks_committed, 1);
    assert_eq!(report.total_events_applied, EXPECTED_APPLIED_EVENTS);

    // Gate 1: the statement-count assertion (independent literal - the plain
    // N+1 tripwire).
    let records = ledger.records().expect("the capture session is armed");
    eprintln!("aave config chunk statement count: {}", records.len());
    assert_eq!(
        records.len(),
        EXPECTED_LEDGER_STATEMENTS,
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

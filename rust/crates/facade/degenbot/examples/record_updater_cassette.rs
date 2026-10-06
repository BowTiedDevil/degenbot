//! Record a golden-capture cassette (ADR-068 D1/D2) for one updater chunk
//! span against the configured live node, or run the drift gate.
//!
//! The recorder wraps the live endpoint in the recording transport
//! (`degenbot-rpc`'s `cassette` module) and drives the updaters' real fetch
//! surface — one pool-updater chunk (the `PoolCreated` fetch plus the
//! whole-chain V3 liquidity scan) or one Aave market chunk — plus the
//! ancillary `eth_chainId`/`eth_blockNumber` round trips. The
//! cassette is flushed with the recorder's canonical writer; the same writer
//! is what the drift gate (`--check`, and the `degenbot-rpc` corpus test)
//! re-runs, so regeneration is byte-identical.
//!
//! Config resolution follows ADR-062 exactly: the node URI comes from
//! `--node` / `DEGENBOT_RPC_*` / the `nodes.*` tables — nothing is hardcoded.
//!
//! Usage:
//!
//!     cargo run --locked --manifest-path rust/Cargo.toml -p degenbot \
//!         --example record_updater_cassette -- --check <cassette.json>...
//!
//!     cargo run --locked --manifest-path rust/Cargo.toml -p degenbot \
//!         --example record_updater_cassette -- --kind pool --family v3 \
//!         --factory 0x1F98431c8aD98523631AE4a59f267346ea31F984 \
//!         --from 26102622 --to 26102626 \
//!         --out tests/fixtures/cassettes/pool_update_chunk_26102622-26102626.json
//!
//!     ... --kind aave --pool 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 \
//!         --from 26130440 --to 26130445 --out tests/fixtures/cassettes/…
//!
//! Two extended capture modes (the replay-suite chunk):
//!
//! - `--kind pool … --verify` records the verification surface: instead of the
//!   bare fetch passes, the REAL `run_pool_update` chunk loop runs with the
//!   pre-commit verification gate ON (`verify_chunk = true`) over a freshly
//!   seeded temp DB, and the cassette captures everything the run asks — the
//!   fetch surface PLUS the gate's Multicall3 aggregate3 tick/bitmap reads at
//!   the chunk end. Those gate entries are what the negative probes mutate.
//!
//! - `--kind aave-run` records the full Aave chunk loop: the market is seeded
//!   via the real `activate_aave_market` plus the loop's warm-boot substrate
//!   (`POOL/POOL_CONFIGURATOR/PRICE_ORACLE` contract rows + the span's reserve
//!   assets resolved from chain), then the REAL `run_aave_update` runs over
//!   the recording transport — every getLogs pass and per-tx config-dispatch
//!   call the loop issues is recorded (the old `--kind aave` seed captured
//!   only the Pool-contract pass).

// Run-once diagnostic example: stdout/stderr reports ARE its interface, its
// prose names CLI flags and updaters that pedantry would backtick, and the
// record flow reads clearer as one body than behind extraction helpers.
#![expect(clippy::print_stdout, clippy::print_stderr, clippy::too_many_lines)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use degenbot::aave::updater::aave_fetch::fetch_pool_logs;
use degenbot::aave::updater::{activate_aave_market, run_aave_update};
use degenbot::db::DegenbotDb;
use degenbot::pool_updater::fetch::{
    fetch_pool_created_logs, fetch_v3_liquidity_logs_grouped, PoolFamily,
};
use degenbot::pool_updater::{run_pool_update, NoProgress as PoolNoProgress};
use degenbot::rpc::cassette::{
    rfc3339_utc, verify_cassette_bytes, CassetteProvenance, CassetteSpan, RecordingTransport,
};
use degenbot::rpc::provider::{AlloyProvider, LogFetcher};

/// Seeded aToken/vToken revision for the harness-substrate `aave_v3_assets`
/// rows. Inert for the pinned span (no `Upgraded` config event dispatches and
/// no GHO-discount path runs), but the columns are NOT NULL — the value pins
/// the seed so the recorder and replay-suite DBs stay identical.
const AAVE_SEED_TOKEN_REVISION: i64 = 1;

/// Raw CLI flags for record mode, validated into a [`RecordSpec`].
#[derive(Default)]
struct RecordArgs {
    kind_raw: String,
    family_raw: String,
    factory_raw: String,
    pool_raw: String,
    address_provider_raw: String,
    gho_raw: String,
    verify: bool,
    from_block: Option<u64>,
    to_block: Option<u64>,
    out: Option<PathBuf>,
    node: Option<String>,
}

struct RecordSpec {
    kind: Kind,
    from_block: u64,
    to_block: u64,
    out: PathBuf,
    node: Option<String>,
}

enum Kind {
    Pool {
        factory: alloy::primitives::Address,
        family: PoolFamily,
        /// `--verify`: record the verification surface — the REAL
        /// `run_pool_update` chunk loop with the pre-commit gate ON over a
        /// seeded temp DB (the negative-probe corpus).
        verify: bool,
    },
    Aave {
        pool: alloy::primitives::Address,
    },
    /// `--kind aave-run`: the full Aave chunk-loop capture (the real
    /// `run_aave_update` over the recording transport).
    AaveRun {
        address_provider: alloy::primitives::Address,
        gho: alloy::primitives::Address,
    },
}

fn parse_addr(label: &str, raw: &str) -> Result<alloy::primitives::Address, String> {
    raw.parse()
        .map_err(|e| format!("{label} {raw:?} is not an address: {e}"))
}

fn parse_family(raw: &str) -> Result<PoolFamily, String> {
    match raw {
        "v2" => Ok(PoolFamily::V2),
        "aerodrome-v2" => Ok(PoolFamily::AerodromeV2),
        "v3" => Ok(PoolFamily::V3),
        "v4" => Ok(PoolFamily::V4),
        other => Err(format!(
            "--family {other:?} (expected v2 | aerodrome-v2 | v3 | v4)"
        )),
    }
}

fn parse_block(label: &str, raw: &str) -> Result<u64, String> {
    raw.parse::<u64>()
        .map_err(|e| format!("{label} {raw:?} is not a decimal block number: {e}"))
}

fn usage() -> String {
    "usage: record_updater_cassette --check <cassette.json>...\n \
       or:  record_updater_cassette --kind pool --family v2|aerodrome-v2|v3|v4 \
       --factory <addr> --from <n> --to <n> --out <path>\n \
       or:  record_updater_cassette --kind aave --pool <addr> \
       --from <n> --to <n> --out <path>\n \
       or:  record_updater_cassette --kind aave-run --address-provider <addr> \
       --gho <addr> --from <n> --to <n> --out <path>"
        .to_string()
}

fn parse_args() -> Result<Either, String> {
    let mut check_files = Vec::new();
    let mut args = RecordArgs::default();
    let mut rest = std::env::args().skip(1);
    while let Some(arg) = rest.next() {
        let mut value = |flag: &str| {
            rest.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match arg.as_str() {
            "--check" => check_files.push(PathBuf::from(value("--check")?)),
            "--kind" => args.kind_raw = value("--kind")?,
            "--family" => args.family_raw = value("--family")?,
            "--factory" => args.factory_raw = value("--factory")?,
            "--pool" => args.pool_raw = value("--pool")?,
            "--from" => args.from_block = Some(parse_block("--from", &value("--from")?)?),
            "--to" => args.to_block = Some(parse_block("--to", &value("--to")?)?),
            "--out" => args.out = Some(PathBuf::from(value("--out")?)),
            "--node" => args.node = Some(value("--node")?),
            "--verify" => args.verify = true,
            "--address-provider" => {
                args.address_provider_raw = value("--address-provider")?;
            }
            "--gho" => args.gho_raw = value("--gho")?,
            flag if flag.starts_with('-') => {
                return Err(format!("unknown flag {flag:?}\n{}", usage()));
            }
            // Positional files after --check name the cassettes to gate.
            _ if !check_files.is_empty() => check_files.push(PathBuf::from(arg)),
            _ => {
                return Err(format!(
                    "unexpected positional argument {arg:?}\n{}",
                    usage()
                ));
            }
        }
    }
    if !check_files.is_empty() {
        return Ok(Either::Check(check_files));
    }

    let from_block = args.from_block.ok_or("--from <n> is required")?;
    let to_block = args.to_block.ok_or("--to <n> is required")?;
    if from_block > to_block {
        return Err(format!("--from {from_block} is above --to {to_block}"));
    }
    let out = args.out.ok_or("--out <path> is required")?;
    let kind = match args.kind_raw.as_str() {
        "pool" => Kind::Pool {
            factory: parse_addr("--factory", &args.factory_raw)?,
            family: parse_family(&args.family_raw)?,
            verify: args.verify,
        },
        "aave" => Kind::Aave {
            pool: parse_addr("--pool", &args.pool_raw)?,
        },
        "aave-run" => Kind::AaveRun {
            address_provider: parse_addr("--address-provider", &args.address_provider_raw)?,
            gho: parse_addr("--gho", &args.gho_raw)?,
        },
        other => {
            return Err(format!(
                "--kind {other:?} (expected pool | aave | aave-run)"
            ));
        }
    };
    Ok(Either::Record(RecordSpec {
        kind,
        from_block,
        to_block,
        out,
        node: args.node,
    }))
}

enum Either {
    Check(Vec<PathBuf>),
    Record(RecordSpec),
}

fn run_check(files: &[PathBuf]) -> ExitCode {
    let mut red = 0;
    for path in files {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                println!("DRIFT GATE RED {}: read failed: {e}", path.display());
                red += 1;
                continue;
            }
        };
        match verify_cassette_bytes(&bytes) {
            Ok(()) => println!(
                "drift gate green {} ({} bytes, byte-identical canonical regeneration)",
                path.display(),
                bytes.len()
            ),
            Err(reason) => {
                println!("DRIFT GATE RED {}: {reason}", path.display());
                red += 1;
            }
        }
    }
    if red == 0 {
        ExitCode::SUCCESS
    } else {
        println!("{red} cassette(s) RED");
        ExitCode::from(1)
    }
}

/// The recording node's identity string (`web3_clientVersion`) for the
/// cassette provenance, probed on a side channel (a plain provider) so it
/// does not enter the recorded ledger.
async fn probe_source(node_uri: &str) -> String {
    match AlloyProvider::new(node_uri, 3).await {
        Ok(provider) => provider
            .make_request("web3_clientVersion", serde_json::json!([]))
            .await
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string()),
        Err(_) => "unknown".to_string(),
    }
}

/// Seed the pool-update harness DB the replay suites use: one ACTIVE
/// `uniswap_v3` exchange stamped at `from - 1`, so the run's fetch window is
/// exactly the pinned span. Mirrors the `degenbot-pool-updater` replay
/// suites' `seeded_db` (same rows, same stamp).
fn seed_pool_update_db(
    database_path: &Path,
    chain_id: i64,
    factory: alloy::primitives::Address,
    from_block: u64,
) -> Result<(), String> {
    let (db, _state) =
        DegenbotDb::open_for_writes(database_path).map_err(|e| format!("db open: {e}"))?;
    let exchange = db
        .upsert_exchange(chain_id, "uniswap_v3", factory, None)
        .map_err(|e| format!("upsert_exchange: {e}"))?;
    let stamp = from_block
        .checked_sub(1)
        .and_then(|b| i64::try_from(b).ok())
        .ok_or("--from must be >= 1 for the pool-verify capture")?;
    let conn = rusqlite::Connection::open(database_path).map_err(|e| format!("db open: {e}"))?;
    conn.execute(
        "UPDATE exchanges SET active = 1, last_update_block = ?1 WHERE id = ?2",
        rusqlite::params![stamp, exchange.id],
    )
    .map_err(|e| format!("exchange stamp: {e}"))?;
    Ok(())
}

/// `run_pool_update` on a plain OS thread - the run entry blocks on the
/// shared runtime and MUST NOT run inside any tokio context (this example's
/// `#[tokio::main]` is one), so the blocking call hops to a bare thread.
fn run_pool_update_off_runtime(
    database_path: PathBuf,
    chain_id: i64,
    to_block: u64,
    chunk_size: u64,
    provider: AlloyProvider,
    verify_chunk: bool,
) -> Result<degenbot::pool_updater::UpdateReport, String> {
    std::thread::spawn(move || {
        run_pool_update(
            &database_path,
            chain_id,
            Some(to_block),
            chunk_size,
            provider,
            Arc::new(AtomicBool::new(false)),
            Arc::new(PoolNoProgress),
            verify_chunk,
            None,
            false,
        )
    })
    .join()
    .map_err(|_| "run_pool_update worker thread panicked".to_string())?
    .map_err(|e| format!("run_pool_update: {e}"))
}

/// The Aave warm-boot substrate the chunk loop's seeded DB carries: the
/// contract rows `build_fetch_spec` requires + the span's reserve assets.
struct AaveSubstrate {
    pool: alloy::primitives::Address,
    configurator: alloy::primitives::Address,
    price_oracle: alloy::primitives::Address,
    /// `POOL_REVISION()` - the ops parser reads the POOL contract row's
    /// revision for the scaled-amount tolerance gate (a NULL revision
    /// hard-errors the parse).
    pool_revision: u64,
    /// `CONFIGURATOR_REVISION()` - seeded for row fidelity with the loop's
    /// own cold-boot substrate.
    configurator_revision: u64,
    /// `(underlying, a_token, v_token)` per reserve the span's Pool logs touch.
    reserves: Vec<(
        alloy::primitives::Address,
        alloy::primitives::Address,
        alloy::primitives::Address,
    )>,
}

/// Resolve the warm-boot substrate over the side-channel provider (NOT the
/// recording seam - these are harness inputs, not the run's RPC surface):
/// the Pool/Configurator/PriceOracle addresses from the `PoolAddressProvider`,
/// then the reserve set from the span's Pool logs (candidate addresses are
/// the zero-padded topic words; `getReserveData` returns a zeroed struct for
/// non-reserves, which filters users and unrelated addresses out), each with
/// its aToken/vToken pair.
async fn resolve_aave_substrate(
    side_channel: &AlloyProvider,
    address_provider: alloy::primitives::Address,
    from_block: u64,
    to_block: u64,
) -> Result<AaveSubstrate, String> {
    let pool = call_address(side_channel, address_provider, "getPool()").await?;
    let configurator =
        call_address(side_channel, address_provider, "getPoolConfigurator()").await?;
    let price_oracle = call_address(side_channel, address_provider, "getPriceOracle()").await?;
    let pool_revision = call_revision(side_channel, pool, "POOL_REVISION()").await?;
    let configurator_revision =
        call_revision(side_channel, configurator, "CONFIGURATOR_REVISION()").await?;

    let fetcher = LogFetcher::new(Arc::new(side_channel.clone()), 2000);
    let pool_logs = fetcher
        .fetch_logs_chunked(
            from_block,
            to_block,
            Some(vec![pool.to_checksum(None)]),
            None,
        )
        .await
        .map_err(|e| format!("span pool logs: {e}"))?;
    let mut candidates: Vec<alloy::primitives::Address> = Vec::new();
    for log in &pool_logs {
        for topic in log.topics().iter().skip(1) {
            candidates.push(alloy::primitives::Address::from_word(*topic));
        }
    }
    candidates.sort();
    candidates.dedup();

    let mut reserves = Vec::new();
    for candidate in candidates {
        if let Some((a_token, v_token)) = reserve_tokens(side_channel, pool, candidate).await? {
            reserves.push((candidate, a_token, v_token));
        }
    }
    if reserves.is_empty() {
        return Err("no market reserves found in the span's Pool logs".to_string());
    }
    Ok(AaveSubstrate {
        pool,
        configurator,
        price_oracle,
        pool_revision,
        configurator_revision,
        reserves,
    })
}

/// `eth_call` a no-arg view returning an `address` (the low 20 bytes of word
/// 0). The 4-byte selector is computed from the signature - no hand-pinned
/// constants.
async fn call_address(
    provider: &AlloyProvider,
    target: alloy::primitives::Address,
    signature: &str,
) -> Result<alloy::primitives::Address, String> {
    let selector = &alloy::primitives::keccak256(signature.as_bytes()).0[0..4];
    let mut calldata = Vec::with_capacity(4);
    calldata.extend_from_slice(selector);
    let ret = provider
        .eth_call(&target, calldata.into(), None)
        .await
        .map_err(|e| format!("{signature}: {e}"))?;
    if ret.len() < 32 {
        return Err(format!("{signature}: short return ({} bytes)", ret.len()));
    }
    Ok(alloy::primitives::Address::from_slice(&ret[12..32]))
}

/// `eth_call` a no-arg view returning a uint revision (word 0).
async fn call_revision(
    provider: &AlloyProvider,
    target: alloy::primitives::Address,
    signature: &str,
) -> Result<u64, String> {
    let selector = &alloy::primitives::keccak256(signature.as_bytes()).0[0..4];
    let mut calldata = Vec::with_capacity(4);
    calldata.extend_from_slice(selector);
    let ret = provider
        .eth_call(&target, calldata.into(), None)
        .await
        .map_err(|e| format!("{signature}: {e}"))?;
    if ret.len() < 32 {
        return Err(format!("{signature}: short return ({} bytes)", ret.len()));
    }
    let mut word = [0u8; 8];
    word.copy_from_slice(&ret[24..32]);
    Ok(u64::from_be_bytes(word))
}

/// `getReserveData(address)` -> the aToken/vToken pair (the address words of
/// the Aave 3.2+ return shape, verified against the live Pool's symbols), or
/// `None` when the candidate is not a market reserve (the Pool returns a
/// zeroed struct for unknown assets).
async fn reserve_tokens(
    provider: &AlloyProvider,
    pool: alloy::primitives::Address,
    candidate: alloy::primitives::Address,
) -> Result<Option<(alloy::primitives::Address, alloy::primitives::Address)>, String> {
    let selector = &alloy::primitives::keccak256("getReserveData(address)".as_bytes()).0[0..4];
    let mut calldata = Vec::with_capacity(36);
    calldata.extend_from_slice(selector);
    calldata.extend_from_slice(&[0u8; 12]);
    calldata.extend_from_slice(candidate.as_slice());
    let ret = provider
        .eth_call(&pool, calldata.into(), None)
        .await
        .map_err(|e| format!("getReserveData({candidate}): {e}"))?;
    if ret.len() < 11 * 32 {
        return Err(format!(
            "getReserveData({candidate}): short return ({} bytes)",
            ret.len()
        ));
    }
    let a_token = alloy::primitives::Address::from_slice(&ret[8 * 32 + 12..9 * 32]);
    let v_token = alloy::primitives::Address::from_slice(&ret[10 * 32 + 12..11 * 32]);
    if a_token.is_zero() || v_token.is_zero() {
        return Ok(None);
    }
    Ok(Some((a_token, v_token)))
}

/// Seed the chunk loop's warm-boot substrate in ONE writeable handle: the
/// contract rows (POOL / `POOL_CONFIGURATOR` / `PRICE_ORACLE` - the
/// `POOL_ADDRESS_PROVIDER` row + the bare GHO substrate `activate_aave_market`
/// already wrote), the run's span cursor (`from - 1`), and the span's reserve
/// assets (erc20 rows + `aave_v3_assets` rows via the `ReserveInitialized`
/// substrate the loop's own dispatch uses). The replay suite seeds the same
/// rows from the same literals.
fn seed_aave_substrate(
    database_path: &Path,
    chain_id: i64,
    market_id: i64,
    substrate: &AaveSubstrate,
    gho: alloy::primitives::Address,
    from_block: u64,
) -> Result<(), String> {
    let (db, _state) =
        DegenbotDb::open_for_writes(database_path).map_err(|e| format!("db open: {e}"))?;
    let conn = db.lock();
    for (name, address, revision) in [
        ("POOL", substrate.pool, Some(substrate.pool_revision)),
        (
            "POOL_CONFIGURATOR",
            substrate.configurator,
            Some(substrate.configurator_revision),
        ),
        ("PRICE_ORACLE", substrate.price_oracle, None),
    ] {
        DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            &conn,
            market_id,
            name,
            &address.to_checksum(None),
            revision
                .map(i64::try_from)
                .transpose()
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("contract row {name}: {e}"))?;
    }
    let stamp = from_block
        .checked_sub(1)
        .and_then(|b| i64::try_from(b).ok())
        .ok_or("--from must be >= 1 for the aave-run capture")?;
    DegenbotDb::set_market_last_update_block_on_conn(&conn, market_id, stamp)
        .map_err(|e| format!("market stamp: {e}"))?;
    for (underlying, a_token, v_token) in &substrate.reserves {
        let underlying_id = DegenbotDb::get_or_create_erc20_token_on_conn(
            &conn,
            chain_id,
            &underlying.to_checksum(None),
            None,
            None,
            None,
        )
        .map_err(|e| format!("erc20 row {underlying}: {e}"))?;
        let a_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
            &conn,
            chain_id,
            &a_token.to_checksum(None),
            None,
            None,
            None,
        )
        .map_err(|e| format!("erc20 row {a_token}: {e}"))?;
        let v_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
            &conn,
            chain_id,
            &v_token.to_checksum(None),
            None,
            None,
            None,
        )
        .map_err(|e| format!("erc20 row {v_token}: {e}"))?;
        let gho_link = if *underlying == gho {
            Some(
                DegenbotDb::get_or_create_gho_token_on_conn(
                    &conn,
                    chain_id,
                    &gho.to_checksum(None),
                )
                .map_err(|e| format!("gho row: {e}"))?,
            )
        } else {
            None
        };
        DegenbotDb::apply_reserve_initialized_on_conn(
            &conn,
            market_id,
            underlying_id,
            a_token_id,
            AAVE_SEED_TOKEN_REVISION,
            v_token_id,
            AAVE_SEED_TOKEN_REVISION,
            None,
            gho_link,
        )
        .map_err(|e| format!("asset row {underlying}: {e}"))?;
    }
    Ok(())
}

/// The `--kind aave-run` flow: seed the market via the REAL
/// `activate_aave_market`, resolve + seed the warm-boot substrate over a
/// side-channel provider, then run the REAL `run_aave_update` over the
/// recording transport - all on ONE plain OS thread (the run entries block on
/// the shared runtime and MUST NOT run inside this example's `#[tokio::main]`
/// context). Returns `(market_id, events applied)`.
#[expect(clippy::unused_async)] // awaits live inside the worker thread's block_on block
async fn record_aave_run_chunk(
    chain_id: u64,
    node_uri: &str,
    recorder: Arc<RecordingTransport>,
    address_provider: alloy::primitives::Address,
    gho: alloy::primitives::Address,
    from_block: u64,
    to_block: u64,
) -> Result<(i64, usize), String> {
    let node_uri = node_uri.to_string();
    let chain_id_i64 = i64::try_from(chain_id).map_err(|e| format!("chain id {chain_id}: {e}"))?;
    let handle = std::thread::spawn(move || -> Result<(i64, usize), String> {
        let dir = tempfile::TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
        let db_path = dir.path().join("aave-run.db");

        // (1) The real one-shot market seed (its RPC half blocks on the shared
        //     runtime internally - legal on this plain thread).
        let activated = activate_aave_market(
            &db_path,
            chain_id_i64,
            &address_provider.to_checksum(None),
            &gho.to_checksum(None),
            &node_uri,
        )
        .map_err(|e| format!("activate_aave_market: {e}"))?;
        let market_id = activated.market_id;

        // (2) Warm-boot substrate resolution over a side-channel provider
        //     (unrecorded - harness inputs, not the run's RPC surface), under
        //     ONE block_on on the shared runtime (this thread carries no
        //     ambient tokio context, so the block_on is legal).
        let substrate = degenbot::core::runtime::get_runtime().block_on(async {
            let side_channel = AlloyProvider::new(&node_uri, 3)
                .await
                .map_err(|e| format!("side-channel provider: {e}"))?;
            let substrate =
                resolve_aave_substrate(&side_channel, address_provider, from_block, to_block)
                    .await?;
            Ok::<AaveSubstrate, String>(substrate)
        })?;

        // (3) The substrate rows (the loop's warm boot then finds POOL +
        //     POOL_CONFIGURATOR + the span's assets, and the cursor pins the
        //     run's fetch window to the recorded span).
        seed_aave_substrate(
            &db_path,
            chain_id_i64,
            market_id,
            &substrate,
            gho,
            from_block,
        )?;

        // (4) The REAL chunk loop over the recording transport.
        let report = run_aave_update(
            &db_path,
            chain_id_i64,
            market_id,
            Some(to_block),
            to_block - from_block + 1,
            recorder.as_alloy_provider(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(degenbot::aave::updater::NoProgress),
            false,
            None,
            false,
            None,
        )
        .map_err(|e| format!("run_aave_update: {e}"))?;
        Ok((market_id, report.total_events_applied))
    });
    handle
        .join()
        .map_err(|_| "aave-run worker thread panicked".to_string())?
}

async fn record(args: RecordSpec) -> Result<usize, String> {
    let cfg =
        degenbot::config::loader::load_process_config().map_err(|e| format!("config: {e}"))?;
    let chain_id = degenbot::config::resolvers::resolve_chain_id(&cfg, None)
        .map_err(|e| format!("chain id: {e}"))?
        .value;
    // The recorder's transport is HTTP-only; route `--node` into the
    // matching config slot so a WS-defaulting config can be pointed at an
    // http endpoint for the capture.
    let overrides = match args.node.as_deref() {
        Some(uri) if uri.starts_with("http") => {
            degenbot::config::resolvers::NodeOverrides::new().with_http(uri)
        }
        Some(uri) if uri.starts_with("ws") => {
            degenbot::config::resolvers::NodeOverrides::new().with_ws(uri)
        }
        Some(uri) if uri.starts_with("ipc") => {
            degenbot::config::resolvers::NodeOverrides::new().with_ipc(uri)
        }
        Some(uri) => return Err(format!("--node {uri:?} has no http/ws/ipc scheme")),
        None => degenbot::config::resolvers::NodeOverrides::new(),
    };
    let node_uri =
        degenbot::config::resolvers::resolve_node_request_uri(&cfg, chain_id, &overrides)
            .map_err(|e| format!("node uri: {e}"))?
            .value;
    let source = probe_source(&node_uri).await;

    // Recording seam: a real AlloyProvider over the recording transport —
    // the updaters' fetch surface runs unchanged through it.
    let recorder = Arc::new(
        RecordingTransport::connect(&node_uri)
            .await
            .map_err(|e| format!("recording transport: {e}"))?,
    );
    let provider = recorder.as_alloy_provider();

    // Ancillary round trips — recorded with everything else.
    let chain_id = provider
        .get_chain_id()
        .await
        .map_err(|e| format!("eth_chainId: {e}"))?;
    let head = provider
        .get_block_number()
        .await
        .map_err(|e| format!("eth_blockNumber: {e}"))?;
    if args.to_block > head {
        return Err(format!(
            "pinned span ends at {} but the node head is {head}",
            args.to_block
        ));
    }
    let chain_id_i64 = i64::try_from(chain_id).map_err(|e| format!("chain id {chain_id}: {e}"))?;

    let fetcher = LogFetcher::new(Arc::new(provider), 2000);
    let decoded = match &args.kind {
        Kind::Pool {
            factory,
            family,
            verify: false,
        } => {
            let events = fetch_pool_created_logs(
                &fetcher,
                args.from_block,
                args.to_block,
                *factory,
                *family,
            )
            .await
            .map_err(|e| format!("fetch_pool_created_logs: {e}"))?;
            // The chunk loop's OTHER fetch surface: the whole-chain V3
            // Mint/Burn liquidity scan over the same span (run_pool_update
            // issues it unconditionally per chunk). Recording it here is what
            // makes the committed corpus cover the run's full fetch surface
            // for an offline replay.
            let v3_liquidity = fetch_v3_liquidity_logs_grouped(
                &fetcher,
                args.from_block,
                args.to_block,
                None, // whole-chain (the run's V3 scan is unfiltered)
            )
            .await
            .map_err(|e| format!("fetch_v3_liquidity_logs_grouped: {e}"))?;
            let liquidity_events: usize = v3_liquidity.values().map(Vec::len).sum();
            events.len() + liquidity_events
        }
        Kind::Pool {
            factory,
            family: _,
            verify: true,
        } => {
            // The verification-surface capture: seed the pool-update harness
            // DB (one ACTIVE uniswap_v3 exchange stamped at from-1 - the same
            // rows the replay suites seed) and run the REAL chunk loop with
            // the pre-commit verification gate ON. Everything the run asks -
            // the fetch passes PLUS the gate Multicall3 tick/bitmap reads -
            // lands in the recording ledger.
            let dir = tempfile::TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
            let db_path = dir.path().join("pool-verify.db");
            seed_pool_update_db(&db_path, chain_id_i64, *factory, args.from_block)?;
            let report = run_pool_update_off_runtime(
                db_path,
                chain_id_i64,
                args.to_block,
                args.to_block - args.from_block + 1,
                recorder.as_alloy_provider(),
                true,
            )?;
            report.total_pools_written + report.total_liquidity_applies
        }
        Kind::Aave { pool } => {
            let logs = fetch_pool_logs(&fetcher, args.from_block, args.to_block, *pool)
                .await
                .map_err(|e| format!("fetch_pool_logs: {e}"))?;
            logs.len()
        }
        Kind::AaveRun {
            address_provider,
            gho,
        } => {
            // The full Aave chunk-loop capture: seed the market via the real
            // `activate_aave_market` + the loop warm-boot substrate, then run
            // the REAL `run_aave_update` over the recording transport.
            // Everything the loop asks - the chunk fetch passes + any per-tx
            // config-dispatch RPC - lands in the recording ledger.
            let (_market_id, events_applied) = record_aave_run_chunk(
                chain_id,
                &node_uri,
                recorder.clone(),
                *address_provider,
                *gho,
                args.from_block,
                args.to_block,
            )
            .await?;
            events_applied
        }
    };

    let recorded_at = rfc3339_utc(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("system clock: {e}"))?
            .as_secs(),
    );
    let cassette = recorder.cassette(
        chain_id,
        CassetteProvenance {
            source: source.clone(),
            recorded_at,
            span: CassetteSpan {
                from_block: args.from_block,
                to_block: args.to_block,
            },
        },
    );
    let entries = cassette.entries.len();
    let bytes = cassette
        .canonical_bytes()
        .map_err(|e| format!("canonical flush: {e}"))?;
    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(&args.out, &bytes).map_err(|e| format!("write {}: {e}", args.out.display()))?;

    println!(
        "recorded cassette {} ({} entries, {decoded} fetched events, chain {chain_id}, \
         span {}..={}, source {source:?})",
        args.out.display(),
        entries,
        args.from_block,
        args.to_block,
    );
    Ok(entries)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::from(2);
        }
    };
    match args {
        Either::Check(files) => run_check(&files),
        Either::Record(args) => match record(args).await {
            Ok(_) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("error: {msg}");
                ExitCode::FAILURE
            }
        },
    }
}

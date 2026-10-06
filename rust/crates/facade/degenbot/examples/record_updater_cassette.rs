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

// Run-once diagnostic example: stdout/stderr reports ARE its interface, its
// prose names CLI flags and updaters that pedantry would backtick, and the
// record flow reads clearer as one body than behind extraction helpers.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::doc_markdown,
    clippy::too_many_lines
)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use degenbot::aave::updater::aave_fetch::fetch_pool_logs;
use degenbot::pool_updater::fetch::{
    fetch_pool_created_logs, fetch_v3_liquidity_logs_grouped, PoolFamily,
};
use degenbot::rpc::cassette::{
    rfc3339_utc, verify_cassette_bytes, CassetteProvenance, CassetteSpan, RecordingTransport,
};
use degenbot::rpc::provider::{AlloyProvider, LogFetcher};

/// Raw CLI flags for record mode, validated into a [`RecordSpec`].
struct RecordArgs {
    kind_raw: String,
    family_raw: String,
    factory_raw: String,
    pool_raw: String,
    from_block: Option<u64>,
    to_block: Option<u64>,
    out: Option<PathBuf>,
    node: Option<String>,
}

impl Default for RecordArgs {
    fn default() -> Self {
        Self {
            kind_raw: String::new(),
            family_raw: String::new(),
            factory_raw: String::new(),
            pool_raw: String::new(),
            from_block: None,
            to_block: None,
            out: None,
            node: None,
        }
    }
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
    },
    Aave {
        pool: alloy::primitives::Address,
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
       --from <n> --to <n> --out <path>"
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
            flag if flag.starts_with('-') => {
                return Err(format!("unknown flag {flag:?}\n{}", usage()));
            }
            // Positional files after --check name the cassettes to gate.
            _ if !check_files.is_empty() => check_files.push(PathBuf::from(arg)),
            _ => {
                return Err(format!(
                    "unexpected positional argument {arg:?}\n{}",
                    usage()
                ))
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
        },
        "aave" => Kind::Aave {
            pool: parse_addr("--pool", &args.pool_raw)?,
        },
        other => return Err(format!("--kind {other:?} (expected pool | aave)")),
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

    let fetcher = LogFetcher::new(Arc::new(provider), 2000);
    let decoded = match &args.kind {
        Kind::Pool { factory, family } => {
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
        Kind::Aave { pool } => {
            let logs = fetch_pool_logs(&fetcher, args.from_block, args.to_block, *pool)
                .await
                .map_err(|e| format!("fetch_pool_logs: {e}"))?;
            logs.len()
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

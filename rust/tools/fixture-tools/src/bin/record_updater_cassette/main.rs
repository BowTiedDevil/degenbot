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
//! # Home
//!
//! This program lives in `degenbot-fixture-tools` (`src/bin/<name>/main.rs`), not in the
//! facade's `examples/`: it is fixture tooling, not a consumer starting point (the epic's
//! taxonomy). Nothing about the recording changed with that move — the pool / Aave /
//! `--verify` / `aave-run` kinds, the real chunk loops, the recorder writer and its
//! canonical bytes, the configured-node resolution, and the argv surface all travelled
//! with the source; only the invocation did. It needs NO feature lane: it never reads the
//! statement ledger, so it builds under this package's default features (the four
//! SQL-ledger binaries keep their own `required-features`). It also resolves no fixture
//! home itself — every cassette path is an argv path (`--out`, `--check`) — so the move
//! needed no repository-root re-anchor and no `paths` call.
//!
//! Config resolution follows ADR-062 exactly: the node URI comes from
//! `--node` / `DEGENBOT_RPC_*` / the `nodes.*` tables — nothing is hardcoded.
//!
//! Usage:
//!
//!     cargo run --locked --manifest-path rust/Cargo.toml \
//!         -p degenbot-fixture-tools --bin record_updater_cassette \
//!         -- --check <cassette.json.zst>...
//!
//!     cargo run --locked --manifest-path rust/Cargo.toml \
//!         -p degenbot-fixture-tools --bin record_updater_cassette \
//!         -- --kind pool \
//!         --factory 0x1F98431c8aD98523631AE4a59f267346ea31F984 \
//!         --from 26102622 --to 26102626 \
//!         --out tests/fixtures/cassettes/pool_update_chunk_26102622-26102626.json
//!
//!     ... --kind aave --pool 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 \
//!         --from 26130440 --to 26130445 --out tests/fixtures/cassettes/…
//!
//! The `--kind pool` capture is the REAL `run_pool_update` chunk loop over a
//! freshly seeded temp DB, so the cassette holds everything the run asks: the
//! fetch surface PLUS the pre-commit gate's tick/bitmap reads at the chunk end
//! (the gate runs on every chunk). Those gate entries are what the negative
//! probes mutate.
//!
//! Extended capture modes (the replay-suite chunk):
//!
//! - `--kind aave-run` records the full Aave chunk loop. The market is activated
//!   from metadata fetched at the CAPTURE PARENT (`from - 1`) through
//!   `fetch_activation_metadata_at`, then `activate_aave_market_on_conn` seeds it —
//!   the same substrate fn production `activate_aave_market` calls, reached here
//!   because the recorder owns the anchor. The loop's warm-boot substrate
//!   (`POOL/POOL_CONFIGURATOR/PRICE_ORACLE` contract rows, the span's reserve
//!   assets, and the GHO discount token/rate strategy) is resolved over an
//!   UNRECORDED side channel, every getter pinned to that same parent block. The
//!   REAL market update (`AaveMarketWriter::update`) then runs over the
//!   recording transport — every
//!   getLogs pass and per-tx config-dispatch call the loop issues is recorded
//!   (the old `--kind aave` seed captured only the Pool-contract pass).
//!
//! `--seed-out <seed.json>` dumps that seeded substrate through degenbot-db's
//! case-preserving `degenbot.aave-seed/v1` codec, AFTER the substrate rows are
//! committed and BEFORE the recorded update writes anything: the dump is a
//! function of archive state at the capture parent alone, so a replay starts from
//! exactly the rows the recorded run read (checksummed addresses and all).
//!
//! The write-versus-check contract is unchanged. Record mode is the LIVE arm: it flushes
//! the cassette through the canonical writer and hands `--out` to the cassette module,
//! which writes the committed artifact as a `.json.zst` package (`write_artifact`).
//! `--check <cassette.json[.zst]>...` is fully OFFLINE and writes nothing: it decodes
//! each committed artifact, re-serializes it through that same writer, and re-checks
//! every entry digest, printing one verdict per file and exiting 1 on any drift.
//! `--check-seed <seed.json>...` is its seed twin, also fully offline: it decodes the
//! committed seed, restores it into a fresh harness DB through the same codec, and
//! requires the re-export to be byte-identical.
//!
//! `--cassette <cassette.json.zst> --delta <delta-stream.json>` extend that seed gate
//! with its REPLAY half, still fully offline: after the byte-identical restore, the
//! recorded run replays through the cassette replay transport and its canonical delta
//! stream must match the committed one byte for byte. Both are explicit argv paths and
//! accompany exactly one `--check-seed`; without them the gate stays the canonical-only
//! restore check, so no path is ever inferred from the seed's name or home.

// Run-once diagnostic tool: stdout/stderr reports ARE its interface, its
// prose names CLI flags and updaters that pedantry would backtick, and the
// record flow reads clearer as one body than behind extraction helpers.
#![expect(clippy::print_stdout, clippy::print_stderr, clippy::too_many_lines)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use degenbot_aave::updater::aave_fetch::fetch_pool_logs;
use degenbot_aave::updater::{
    activate_aave_market_on_conn, fetch_activation_metadata_at, AaveChunkProgress,
    ActivationMetadata, CanonicalDelta, ProgressSink, RunSpec, VerifyPolicy,
    ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
};
use degenbot_aave::AaveMarketWriter;
use degenbot_core::canonical_head::ChainAnchor;
use degenbot_db::sql_ledger::{dump_aave_seed_json, restore_aave_seed_json};
use degenbot_db::DegenbotDb;
use degenbot_pool_updater::{run_pool_update, NoProgress as PoolNoProgress};
use degenbot_rpc::cassette::{
    read_artifact, rfc3339_utc, verify_cassette_bytes, write_artifact, Cassette,
    CassetteProvenance, CassetteSpan, RecordingTransport,
};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use degenbot_rpc::chain_evidence::resolve_terminal_anchor;
use degenbot_rpc::provider::{AlloyProvider, LogFetcher};

/// Raw CLI flags for record mode, validated into a [`RecordSpec`].
#[derive(Default)]
struct RecordArgs {
    kind_raw: String,
    factory_raw: String,
    pool_raw: String,
    address_provider_raw: String,
    gho_raw: String,
    from_block: Option<u64>,
    to_block: Option<u64>,
    out: Option<PathBuf>,
    seed_out: Option<PathBuf>,
    node: Option<String>,
}

/// The explicit restore -> replay inputs of one seed's gate: the recorded
/// cassette the replay serves from, and the committed delta stream that replay
/// must reproduce byte-for-byte.
struct SeedReplay {
    cassette: PathBuf,
    delta: PathBuf,
}

struct RecordSpec {
    kind: Kind,
    from_block: u64,
    to_block: u64,
    out: PathBuf,
    /// Where the `aave-run` replay seed is written.
    seed_out: Option<PathBuf>,
    node: Option<String>,
}

enum Kind {
    Pool {
        factory: alloy::primitives::Address,
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

fn parse_block(label: &str, raw: &str) -> Result<u64, String> {
    raw.parse::<u64>()
        .map_err(|e| format!("{label} {raw:?} is not a decimal block number: {e}"))
}

fn usage() -> String {
    "usage: record_updater_cassette --check <cassette.json[.zst]>...\n \
       or:  record_updater_cassette --check-seed <seed.json>...\n \
       or:  record_updater_cassette --check-seed <seed.json> \
       --cassette <cassette.json.zst> --delta <delta-stream.json>\n \
       or:  record_updater_cassette --kind pool --factory <addr> \
       --from <n> --to <n> --out <path>\n \
       or:  record_updater_cassette --kind aave --pool <addr> \
       --from <n> --to <n> --out <path>\n \
       or:  record_updater_cassette --help\n \
       or:  record_updater_cassette --kind aave-run --address-provider <addr> \
       --gho <addr> --from <n> --to <n> --out <path> [--seed-out <seed.json>]"
        .to_string()
}

fn parse_args(flags: impl Iterator<Item = String>) -> Result<Either, String> {
    let mut check_files = Vec::new();
    let mut seed_check_files = Vec::new();
    let mut cassette = None;
    let mut delta = None;
    let mut args = RecordArgs::default();
    let mut rest = flags;
    while let Some(arg) = rest.next() {
        let mut value = |flag: &str| {
            rest.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match arg.as_str() {
            "--help" | "-h" => return Ok(Either::Help),
            "--check" => check_files.push(PathBuf::from(value("--check")?)),
            "--check-seed" => seed_check_files.push(PathBuf::from(value("--check-seed")?)),
            "--cassette" => cassette = Some(PathBuf::from(value("--cassette")?)),
            "--delta" => delta = Some(PathBuf::from(value("--delta")?)),
            "--kind" => args.kind_raw = value("--kind")?,
            "--factory" => args.factory_raw = value("--factory")?,
            "--pool" => args.pool_raw = value("--pool")?,
            "--from" => args.from_block = Some(parse_block("--from", &value("--from")?)?),
            "--to" => args.to_block = Some(parse_block("--to", &value("--to")?)?),
            "--out" => args.out = Some(PathBuf::from(value("--out")?)),
            "--seed-out" => args.seed_out = Some(PathBuf::from(value("--seed-out")?)),
            "--node" => args.node = Some(value("--node")?),
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
    if !check_files.is_empty() && !seed_check_files.is_empty() {
        return Err(format!(
            "--check names cassettes and --check-seed names seeds; pass one\n{}",
            usage()
        ));
    }
    let replay = match (cassette, delta) {
        (None, None) => None,
        (Some(cassette), Some(delta)) => Some(SeedReplay { cassette, delta }),
        (Some(_), None) => {
            return Err(format!(
                "--cassette replays one restored seed; pass its --delta too\n{}",
                usage()
            ));
        }
        (None, Some(_)) => {
            return Err(format!(
                "--delta commits one replay's delta stream; pass its --cassette too\n{}",
                usage()
            ));
        }
    };
    if replay.is_some() && seed_check_files.is_empty() {
        return Err(format!(
            "--cassette/--delta extend --check-seed: they name one seed's replay inputs\n{}",
            usage()
        ));
    }
    if replay.is_some() && seed_check_files.len() != 1 {
        return Err(format!(
            "--cassette/--delta pair ONE seed with one cassette and one delta stream, got {} seeds\n{}",
            seed_check_files.len(),
            usage()
        ));
    }
    if !check_files.is_empty() {
        return Ok(Either::Check(check_files));
    }
    // The offline seed gate answers before any record flag is validated: the
    // `just record-aave-cassette` wrapper supplies `--kind aave-run` on every
    // call, and `--check-seed` must still reach the gate.
    if !seed_check_files.is_empty() {
        return Ok(Either::CheckSeed {
            seeds: seed_check_files,
            replay,
        });
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
    // The seed is the `aave-run` substrate dump: the other kinds run no
    // activation and seed no market, so a `--seed-out` there would name an
    // artifact nothing writes.
    if args.seed_out.is_some() && !matches!(&kind, Kind::AaveRun { .. }) {
        return Err(format!("--seed-out requires --kind aave-run\n{}", usage()));
    }
    Ok(Either::Record(RecordSpec {
        kind,
        from_block,
        to_block,
        out,
        seed_out: args.seed_out,
        node: args.node,
    }))
}

enum Either {
    Check(Vec<PathBuf>),
    /// The offline seed gate: the canonical restore check for every named seed,
    /// plus the explicit restore -> replay -> committed-delta arm when the
    /// replay inputs were supplied.
    CheckSeed {
        seeds: Vec<PathBuf>,
        replay: Option<SeedReplay>,
    },
    Help,
    Record(RecordSpec),
}

fn run_check(files: &[PathBuf]) -> ExitCode {
    let mut red = 0;
    for path in files {
        let bytes = match read_artifact(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                println!("DRIFT GATE RED {}: read failed: {e}", path.display());
                red += 1;
                continue;
            }
        };
        match verify_cassette_bytes(&bytes) {
            Ok(()) => println!(
                "drift gate green {} ({} canonical bytes, byte-identical canonical regeneration)",
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

/// The seed drift gate's verdict for one committed seed: restore it into a fresh
/// harness DB through the codec, re-export, and require the bytes to match. Fully
/// offline — the seed is a dump of archive state, not a recording of it.
fn check_seed(path: &Path) -> Result<usize, String> {
    let committed = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let dir = tempfile::TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
    let (db, _state) = DegenbotDb::open_for_writes(&dir.path().join("seed-check.db"))
        .map_err(|e| format!("db open: {e}"))?;
    let conn = db.lock();
    restore_aave_seed_json(&conn, &committed).map_err(|e| format!("restore: {e}"))?;
    let reexport = dump_aave_seed_json(&conn).map_err(|e| format!("re-export: {e}"))?;
    if reexport != committed {
        return Err("restore -> re-export drifted from the committed bytes".to_string());
    }
    Ok(committed.len())
}

/// The canonical deltas a replayed run reported, in the order it reported them.
#[derive(Default)]
struct DeltaSink {
    deltas: Mutex<Vec<CanonicalDelta>>,
}

impl ProgressSink for DeltaSink {
    fn report_chunk(&self, progress: &AaveChunkProgress) {
        let mut deltas = self
            .deltas
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        deltas.extend(progress.deltas.iter().cloned());
    }
}

/// The seed gate's replay half for one committed seed: restore it
/// byte-identically, replay the named cassette, and require the run's canonical
/// delta stream to equal the named committed stream byte-for-byte. Every input
/// is an explicit argv path; the arm infers nothing from the seed's name.
fn check_seed_replay(path: &Path, replay: &SeedReplay) -> Result<String, String> {
    let committed = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let committed_deltas = std::fs::read_to_string(&replay.delta)
        .map_err(|e| format!("read {}: {e}", replay.delta.display()))?;

    let dir = tempfile::TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
    let db_path = dir.path().join("seed-replay.db");
    let (db, _state) =
        DegenbotDb::open_for_writes(&db_path).map_err(|e| format!("db open: {e}"))?;
    let market_id = {
        let conn = db.lock();
        restore_aave_seed_json(&conn, &committed).map_err(|e| format!("restore: {e}"))?;
        let reexport = dump_aave_seed_json(&conn).map_err(|e| format!("re-export: {e}"))?;
        if reexport != committed {
            return Err("restore -> re-export drifted from the committed bytes".to_string());
        }
        drop(conn);
        let markets = db
            .fetch_active_aave_markets()
            .map_err(|e| format!("market lookup: {e}"))?;
        let [market] = markets.as_slice() else {
            return Err(format!(
                "the restored seed carries {} active markets; the replay arm pairs one seed with one market",
                markets.len()
            ));
        };
        market.id
    };
    drop(db);

    let bytes = read_artifact(&replay.cassette)
        .map_err(|e| format!("cassette {}: {e}", replay.cassette.display()))?;
    verify_cassette_bytes(&bytes).map_err(|reason| format!("cassette drift: {reason}"))?;
    let cassette =
        Cassette::from_json_bytes(&bytes).map_err(|e| format!("cassette decode: {e}"))?;
    let chain_id = i64::try_from(cassette.chain_id).map_err(|e| format!("chain id: {e}"))?;
    let span = cassette.provenance.span;
    let transport = CassetteReplayTransport::new(cassette);

    let sink = Arc::new(DeltaSink::default());
    let collector = Arc::clone(&sink);
    std::thread::spawn(move || {
        let writer = AaveMarketWriter::acquire(&db_path, market_id)?;
        writer.update(RunSpec {
            chain_id,
            market_id,
            to_block: Some(span.to_block),
            chunk_size: span.to_block - span.from_block + 1,
            max_block_range: 10_000,
            provider: transport.as_alloy_provider(),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: collector,
            verify: VerifyPolicy::default(),
            max_chunks: None,
        })
    })
    .join()
    .map_err(|_| "replay worker thread panicked".to_string())?
    .map_err(|e| format!("replay: {e}"))?;

    let deltas = sink
        .deltas
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let encoded: Vec<String> = deltas.iter().map(CanonicalDelta::encode).collect();
    if encoded.is_empty() {
        return Err("the replayed span reduced no canonical delta".to_string());
    }
    let produced = format!("{}.\n", encoded.join("\n"));
    if produced != committed_deltas {
        return Err(format!(
            "restore -> replay drifted from the committed delta stream {}",
            replay.delta.display()
        ));
    }
    Ok(format!(
        "seed gate green {} ({bytes} canonical bytes): restore -> replay reproduced the {count} canonical deltas committed in {delta}",
        path.display(),
        bytes = committed.len(),
        count = encoded.len(),
        delta = replay.delta.display(),
    ))
}

fn run_check_seed(seeds: &[PathBuf], replay: Option<&SeedReplay>) -> ExitCode {
    let mut red = 0;
    for path in seeds {
        let verdict = match replay {
            Some(replay) => check_seed_replay(path, replay),
            None => check_seed(path).map(|bytes| {
                format!(
                    "seed gate green {} ({bytes} canonical bytes, byte-identical re-export after restore)",
                    path.display()
                )
            }),
        };
        match verdict {
            Ok(line) => println!("{line}"),
            Err(reason) => {
                println!("SEED GATE RED {}: {reason}", path.display());
                red += 1;
            }
        }
    }
    if red == 0 {
        ExitCode::SUCCESS
    } else {
        println!("{red} seed(s) RED");
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
) -> Result<degenbot_pool_updater::UpdateReport, String> {
    std::thread::spawn(move || {
        run_pool_update(
            &database_path,
            chain_id,
            Some(to_block),
            chunk_size,
            10_000,
            provider,
            Arc::new(AtomicBool::new(false)),
            Arc::new(PoolNoProgress),
            None,
            false,
        )
    })
    .join()
    .map_err(|_| "run_pool_update worker thread panicked".to_string())?
    .map_err(|e| format!("run_pool_update: {e}"))
}

/// The capture parent every seeded substrate read is pinned to: the block
/// before the recorded span, so the whole seed is a function of ONE archive
/// state.
fn seed_block(from_block: u64) -> Result<u64, String> {
    from_block
        .checked_sub(1)
        .ok_or_else(|| "--from must be >= 1: the seed reads at from - 1".to_string())
}

/// The pinned read view every seeded substrate fact comes from: one block fixed
/// at construction, one `eth_call` site that always names it. A getter cannot
/// read at the head because no seed read path exists without this view.
struct SeedReads<'a> {
    provider: &'a AlloyProvider,
    block: u64,
}

impl<'a> SeedReads<'a> {
    /// The capture-parent view over `provider` for a span starting at
    /// `from_block`.
    fn at(provider: &'a AlloyProvider, from_block: u64) -> Result<Self, String> {
        Ok(Self {
            provider,
            block: seed_block(from_block)?,
        })
    }

    async fn call(
        &self,
        target: alloy::primitives::Address,
        signature: &str,
        argument: Option<alloy::primitives::Address>,
    ) -> Result<alloy::primitives::Bytes, String> {
        let selector = &alloy::primitives::keccak256(signature.as_bytes()).0[0..4];
        let mut calldata = Vec::with_capacity(4 + 32);
        calldata.extend_from_slice(selector);
        if let Some(argument) = argument {
            calldata.extend_from_slice(&[0u8; 12]);
            calldata.extend_from_slice(argument.as_slice());
        }
        self.provider
            .eth_call(&target, calldata.into(), Some(self.block))
            .await
            .map_err(|e| format!("{signature} at block {}: {e}", self.block))
    }

    /// A no-arg view returning an `address` (the low 20 bytes of word 0).
    async fn address(
        &self,
        target: alloy::primitives::Address,
        signature: &str,
    ) -> Result<alloy::primitives::Address, String> {
        let ret = self.call(target, signature, None).await?;
        if ret.len() < 32 {
            return Err(format!("{signature}: short return ({} bytes)", ret.len()));
        }
        Ok(alloy::primitives::Address::from_slice(&ret[12..32]))
    }

    /// A no-arg view returning a uint (word 0), narrowed to the stored column.
    async fn word(
        &self,
        target: alloy::primitives::Address,
        signature: &str,
    ) -> Result<i64, String> {
        let ret = self.call(target, signature, None).await?;
        if ret.len() < 32 {
            return Err(format!("{signature}: short return ({} bytes)", ret.len()));
        }
        let mut word = [0u8; 8];
        word.copy_from_slice(&ret[24..32]);
        let value = u64::from_be_bytes(word);
        i64::try_from(value)
            .map_err(|_| format!("{signature}: {value} does not fit a signed 64-bit column"))
    }
}

/// The address-provider facts at the capture parent.
struct SeedContracts {
    pool: alloy::primitives::Address,
    configurator: alloy::primitives::Address,
    price_oracle: alloy::primitives::Address,
    /// `POOL_REVISION()` — the ops parser reads the POOL contract row's
    /// revision for the scaled-amount tolerance gate (a NULL revision
    /// hard-errors the parse).
    pool_revision: i64,
    /// `CONFIGURATOR_REVISION()` — seeded for row fidelity with the loop's own
    /// cold-boot substrate.
    configurator_revision: i64,
}

/// One seeded reserve: the underlying asset, its scaled tokens, and the
/// revisions those tokens answered at the capture parent.
struct SeededReserve {
    underlying: alloy::primitives::Address,
    a_token: alloy::primitives::Address,
    v_token: alloy::primitives::Address,
    a_token_revision: i64,
    v_token_revision: i64,
}

/// The Aave warm-boot substrate the chunk loop's seeded DB carries: the contract
/// rows `build_fetch_spec` requires, the span's reserve assets, and the GHO
/// discount facts a refresh resolves against.
struct AaveSubstrate {
    pool: alloy::primitives::Address,
    configurator: alloy::primitives::Address,
    price_oracle: alloy::primitives::Address,
    pool_revision: i64,
    configurator_revision: i64,
    discount_token: alloy::primitives::Address,
    discount_rate_strategy: alloy::primitives::Address,
    reserves: Vec<SeededReserve>,
}

async fn read_seed_contracts(
    reads: &SeedReads<'_>,
    address_provider: alloy::primitives::Address,
) -> Result<SeedContracts, String> {
    let pool = reads.address(address_provider, "getPool()").await?;
    let configurator = reads
        .address(address_provider, "getPoolConfigurator()")
        .await?;
    let price_oracle = reads.address(address_provider, "getPriceOracle()").await?;
    let pool_revision = reads.word(pool, "POOL_REVISION()").await?;
    let configurator_revision = reads.word(configurator, "CONFIGURATOR_REVISION()").await?;
    Ok(SeedContracts {
        pool,
        configurator,
        price_oracle,
        pool_revision,
        configurator_revision,
    })
}

/// `getReserveData(address)` -> the aToken/vToken pair (the address words of the
/// Aave 3.2+ return shape, verified against the live Pool's symbols), or `None`
/// when the candidate is not a market reserve (the Pool returns a zeroed struct
/// for unknown assets).
async fn read_reserve_tokens(
    reads: &SeedReads<'_>,
    pool: alloy::primitives::Address,
    candidate: alloy::primitives::Address,
) -> Result<Option<(alloy::primitives::Address, alloy::primitives::Address)>, String> {
    let ret = reads
        .call(pool, "getReserveData(address)", Some(candidate))
        .await?;
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

/// The two scaled-token revisions at the capture parent, read per reserve: no
/// capture inherits a default for a fact the chain states.
async fn read_reserve_revisions(
    reads: &SeedReads<'_>,
    a_token: alloy::primitives::Address,
    v_token: alloy::primitives::Address,
) -> Result<(i64, i64), String> {
    Ok((
        reads.word(a_token, "ATOKEN_REVISION()").await?,
        reads.word(v_token, "DEBT_TOKEN_REVISION()").await?,
    ))
}

/// The GHO discount facts a refresh resolves against, read on the GHO vToken at
/// the capture parent.
async fn read_gho_discount(
    reads: &SeedReads<'_>,
    v_gho: alloy::primitives::Address,
) -> Result<(alloy::primitives::Address, alloy::primitives::Address), String> {
    Ok((
        reads.address(v_gho, "getDiscountToken()").await?,
        reads.address(v_gho, "getDiscountRateStrategy()").await?,
    ))
}

/// Resolve the warm-boot substrate on the UNRECORDED side channel — these are
/// harness inputs, not the run's RPC surface: every getter reads at the capture
/// parent, the reserve set comes from the span's Pool logs (candidate addresses
/// are the zero-padded topic words; `getReserveData` returns a zeroed struct for
/// non-reserves, which filters users and unrelated addresses out), and the GHO
/// reserve's vToken carries the discount facts.
async fn resolve_aave_substrate(
    reads: &SeedReads<'_>,
    fetcher: &LogFetcher,
    address_provider: alloy::primitives::Address,
    gho: alloy::primitives::Address,
    from_block: u64,
    to_block: u64,
) -> Result<AaveSubstrate, String> {
    let contracts = read_seed_contracts(reads, address_provider).await?;

    let pool_logs = fetcher
        .fetch_logs_chunked(
            from_block,
            to_block,
            Some(vec![contracts.pool.to_checksum(None)]),
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
    let mut gho_v_token = None;
    for candidate in candidates {
        let Some((a_token, v_token)) =
            read_reserve_tokens(reads, contracts.pool, candidate).await?
        else {
            continue;
        };
        let (a_token_revision, v_token_revision) =
            read_reserve_revisions(reads, a_token, v_token).await?;
        if candidate == gho {
            gho_v_token = Some(v_token);
        }
        reserves.push(SeededReserve {
            underlying: candidate,
            a_token,
            v_token,
            a_token_revision,
            v_token_revision,
        });
    }
    if reserves.is_empty() {
        return Err("no market reserves found in the span's Pool logs".to_string());
    }
    let v_gho = match gho_v_token {
        Some(v_gho) => v_gho,
        None => {
            read_reserve_tokens(reads, contracts.pool, gho)
                .await?
                .ok_or_else(|| format!("--gho {gho} is not a reserve of pool {}", contracts.pool))?
                .1
        }
    };
    let (discount_token, discount_rate_strategy) = read_gho_discount(reads, v_gho).await?;

    Ok(AaveSubstrate {
        pool: contracts.pool,
        configurator: contracts.configurator,
        price_oracle: contracts.price_oracle,
        pool_revision: contracts.pool_revision,
        configurator_revision: contracts.configurator_revision,
        discount_token,
        discount_rate_strategy,
        reserves,
    })
}

/// Seed the chunk loop's warm-boot substrate in ONE writeable handle: the
/// contract rows (POOL / `POOL_CONFIGURATOR` / `PRICE_ORACLE` — the
/// `POOL_ADDRESS_PROVIDER` row + the bare GHO substrate
/// `activate_aave_market_on_conn` already wrote), the run's span cursor
/// (`from - 1`), the span's reserve assets (erc20 rows + `aave_v3_assets` rows via
/// the `ReserveInitialized` substrate the loop's own dispatch uses), and the GHO
/// discount token / rate strategy the refresh resolves against. The replay suite
/// seeds the same rows from the committed seed.
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
            revision,
        )
        .map_err(|e| format!("contract row {name}: {e}"))?;
    }
    let stamp = from_block
        .checked_sub(1)
        .and_then(|b| i64::try_from(b).ok())
        .ok_or("--from must be >= 1 for the aave-run capture")?;
    DegenbotDb::set_market_last_update_block_on_conn(&conn, market_id, stamp)
        .map_err(|e| format!("market stamp: {e}"))?;
    for reserve in &substrate.reserves {
        let underlying = reserve.underlying;
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
            &reserve.a_token.to_checksum(None),
            None,
            None,
            None,
        )
        .map_err(|e| format!("erc20 row {}: {e}", reserve.a_token))?;
        let v_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
            &conn,
            chain_id,
            &reserve.v_token.to_checksum(None),
            None,
            None,
            None,
        )
        .map_err(|e| format!("erc20 row {}: {e}", reserve.v_token))?;
        let gho_link = if underlying == gho {
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
            reserve.a_token_revision,
            v_token_id,
            reserve.v_token_revision,
            None,
            gho_link,
        )
        .map_err(|e| format!("asset row {underlying}: {e}"))?;
    }
    let gho_token_id =
        DegenbotDb::get_or_create_gho_token_on_conn(&conn, chain_id, &gho.to_checksum(None))
            .map_err(|e| format!("gho row: {e}"))?;
    DegenbotDb::apply_gho_discount_rate_strategy_updated_on_conn(
        &conn,
        gho_token_id,
        Some(&substrate.discount_rate_strategy.to_checksum(None)),
    )
    .map_err(|e| format!("gho discount rate strategy: {e}"))?;
    DegenbotDb::apply_gho_discount_token_updated_on_conn(
        &conn,
        gho_token_id,
        Some(&substrate.discount_token.to_checksum(None)),
    )
    .map_err(|e| format!("gho discount token: {e}"))?;
    Ok(())
}

/// Dump the seeded substrate through the case-preserving seed codec: after the
/// substrate rows are committed and before the recorded update writes anything.
fn write_aave_seed(database_path: &Path, seed_out: &Path) -> Result<(), String> {
    let conn =
        rusqlite::Connection::open(database_path).map_err(|e| format!("seed db open: {e}"))?;
    let json = dump_aave_seed_json(&conn).map_err(|e| format!("seed dump: {e}"))?;
    drop(conn);
    // A bare filename's parent is empty — it names the working directory, which
    // exists; only a real directory component needs creating.
    if let Some(parent) = seed_out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(seed_out, json).map_err(|e| format!("write {}: {e}", seed_out.display()))?;
    println!("recorded seed {}", seed_out.display());
    Ok(())
}

/// Activate the market from metadata fetched at the capture parent, through the
/// same substrate fn production `activate_aave_market` calls: ONE transaction,
/// committed before the seed dump reads the rows back.
fn activate_market_at_capture_parent(
    database_path: &Path,
    chain_id: i64,
    metadata: &ActivationMetadata,
    address_provider: alloy::primitives::Address,
    gho: alloy::primitives::Address,
) -> Result<i64, String> {
    let (db, _state) =
        DegenbotDb::open_for_writes(database_path).map_err(|e| format!("db open: {e}"))?;
    let mut guard = db.lock();
    let tx = guard
        .transaction()
        .map_err(|e| format!("activation transaction: {e}"))?;
    let market_id = activate_aave_market_on_conn(
        &tx,
        chain_id,
        &metadata.market_name,
        &address_provider.to_checksum(None),
        &gho.to_checksum(None),
        metadata.gho_name.as_deref(),
        metadata.gho_symbol.as_deref(),
        Some(metadata.gho_decimals),
        ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
    )
    .map_err(|e| format!("activate_aave_market_on_conn: {e}"))?;
    tx.commit().map_err(|e| format!("activation commit: {e}"))?;
    Ok(market_id)
}

/// The `--kind aave-run` flow: activate the market from metadata fetched at the
/// capture parent, resolve + seed the warm-boot substrate at that same block,
/// dump the seed, then run the REAL `run_aave_update` over the recording
/// transport - all on ONE plain OS thread (the run entries block on the shared
/// runtime and MUST NOT run inside this example's `#[tokio::main]` context).
/// Returns `(market_id, events applied)`.
#[expect(clippy::unused_async)] // awaits live inside the worker thread's block_on block
#[expect(clippy::too_many_arguments)] // the run's pinned inputs, one per fact read
async fn record_aave_run_chunk(
    chain_id: u64,
    node_uri: &str,
    recorder: Arc<RecordingTransport>,
    address_provider: alloy::primitives::Address,
    gho: alloy::primitives::Address,
    from_block: u64,
    to_block: u64,
    seed_out: Option<PathBuf>,
) -> Result<(i64, usize), String> {
    let parent_block = seed_block(from_block)?;
    let node_uri = node_uri.to_string();
    let chain_id_i64 = i64::try_from(chain_id).map_err(|e| format!("chain id {chain_id}: {e}"))?;
    let handle = std::thread::spawn(move || -> Result<(i64, usize), String> {
        let dir = tempfile::TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
        let db_path = dir.path().join("aave-run.db");

        // (1) The activation's identity facts at the CAPTURE PARENT: ONE exact
        //     anchor at `from - 1` on the unrecorded side channel (a harness
        //     input, not the run's RPC surface), then the same `_at` fetch
        //     production activation delegates to. The seed transaction is the
        //     production substrate fn, so a recording cannot activate a market
        //     production would not.
        let side_channel = degenbot_core::runtime::get_runtime()
            .block_on(AlloyProvider::new(&node_uri, 3))
            .map_err(|e| format!("side-channel provider: {e}"))?;
        let anchor: ChainAnchor = degenbot_core::runtime::get_runtime()
            .block_on(resolve_terminal_anchor(
                &side_channel,
                chain_id,
                Some(parent_block),
            ))
            .map_err(|e| format!("capture-parent anchor {parent_block}: {e}"))?;
        let metadata: ActivationMetadata = degenbot_core::runtime::get_runtime()
            .block_on(fetch_activation_metadata_at(
                &side_channel,
                anchor,
                &address_provider,
                &gho,
            ))
            .map_err(|e| format!("activation metadata at {parent_block}: {e}"))?;
        let market_id = activate_market_at_capture_parent(
            &db_path,
            chain_id_i64,
            &metadata,
            address_provider,
            gho,
        )?;

        // (2) Warm-boot substrate resolution at the same parent block, under ONE
        //     block_on on the shared runtime (this thread carries no ambient
        //     tokio context, so the block_on is legal).
        let substrate = degenbot_core::runtime::get_runtime().block_on(async {
            let reads = SeedReads::at(&side_channel, from_block)?;
            let fetcher = LogFetcher::new(Arc::new(side_channel.clone()), 2000);
            resolve_aave_substrate(
                &reads,
                &fetcher,
                address_provider,
                gho,
                from_block,
                to_block,
            )
            .await
        })?;

        // (3) The substrate rows (the loop's warm boot then finds POOL +
        //     POOL_CONFIGURATOR + the span's assets, the cursor pins the run's
        //     fetch window to the recorded span, and the discount facts let a
        //     refresh resolve), then the seed dump — the rows a replay restores.
        seed_aave_substrate(
            &db_path,
            chain_id_i64,
            market_id,
            &substrate,
            gho,
            from_block,
        )?;
        if let Some(seed_out) = seed_out.as_deref() {
            write_aave_seed(&db_path, seed_out)?;
        }

        // (4) The REAL chunk loop over the recording transport.
        let report = AaveMarketWriter::acquire(&db_path, market_id)
            .map_err(|e| format!("market writer: {e}"))?
            .update(RunSpec {
                chain_id: chain_id_i64,
                market_id,
                to_block: Some(to_block),
                chunk_size: to_block - from_block + 1,
                max_block_range: 10_000,
                provider: recorder.as_alloy_provider(),
                cancel: Arc::new(AtomicBool::new(false)),
                progress: Arc::new(degenbot_aave::updater::NoProgress),
                verify: VerifyPolicy::default(),
                max_chunks: None,
            })
            .map_err(|e| format!("run_aave_update: {e}"))?;
        Ok((market_id, report.total_events_applied))
    });
    handle
        .join()
        .map_err(|_| "aave-run worker thread panicked".to_string())?
}

async fn record(args: RecordSpec) -> Result<usize, String> {
    let cfg = degenbot_config::loader::load_process_config().map_err(|e| format!("config: {e}"))?;
    let chain_id = degenbot_config::resolvers::resolve_chain_id(&cfg, None)
        .map_err(|e| format!("chain id: {e}"))?
        .value;
    // The recorder's transport is HTTP-only; route `--node` into the
    // matching config slot so a WS-defaulting config can be pointed at an
    // http endpoint for the capture.
    let overrides = match args.node.as_deref() {
        Some(uri) if uri.starts_with("http") => {
            degenbot_config::resolvers::NodeOverrides::new().with_http(uri)
        }
        Some(uri) if uri.starts_with("ws") => {
            degenbot_config::resolvers::NodeOverrides::new().with_ws(uri)
        }
        Some(uri) if uri.starts_with("ipc") => {
            degenbot_config::resolvers::NodeOverrides::new().with_ipc(uri)
        }
        Some(uri) => return Err(format!("--node {uri:?} has no http/ws/ipc scheme")),
        None => degenbot_config::resolvers::NodeOverrides::new(),
    };
    let node_uri = degenbot_config::resolvers::resolve_node_request_uri(&cfg, chain_id, &overrides)
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
        Kind::Pool { factory } => {
            // The pool capture: seed the pool-update harness DB (one ACTIVE
            // uniswap_v3 exchange stamped at from-1 - the same rows the replay
            // suites seed) and run the REAL chunk loop. Everything the run
            // asks lands in the recording ledger: the fetch passes PLUS the
            // pre-commit verification gate's per-pool tick/bitmap reads (the
            // gate runs on every chunk - it is not an option).
            let dir = tempfile::TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
            let db_path = dir.path().join("pool-update.db");
            seed_pool_update_db(&db_path, chain_id_i64, *factory, args.from_block)?;
            let report = run_pool_update_off_runtime(
                db_path,
                chain_id_i64,
                args.to_block,
                args.to_block - args.from_block + 1,
                recorder.as_alloy_provider(),
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
            // The full Aave chunk-loop capture: activate + seed the market from
            // facts read at the capture parent, dump the seed when asked, then
            // run the REAL `run_aave_update` over the recording transport.
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
                args.seed_out.clone(),
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
    let package = write_artifact(&args.out, &bytes)
        .map_err(|e| format!("write {}: {e}", args.out.display()))?;

    println!(
        "recorded cassette {} ({} entries, {decoded} fetched events, chain {chain_id}, \
         span {}..={}, source {source:?})",
        package.display(),
        entries,
        args.from_block,
        args.to_block,
    );
    Ok(entries)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::from(2);
        }
    };
    match args {
        Either::Help => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        Either::Check(files) => run_check(&files),
        Either::CheckSeed { seeds, replay } => run_check_seed(&seeds, replay.as_ref()),
        Either::Record(args) => match record(args).await {
            Ok(_) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("error: {msg}");
                ExitCode::FAILURE
            }
        },
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use alloy::primitives::{address, keccak256, Address};
    use degenbot_rpc::offline::OfflineProvider;

    const ADDRESS_PROVIDER: Address = address!("2f39d218133afab8f2b819b1066c7e434ad94e9e");
    const POOL: Address = address!("87870bca3f3fd6335c3f4ce8392d69350b4fa4e2");
    const CONFIGURATOR: Address = address!("64b761d848206f447fe2dd461b0c635ec39ebb27");
    const PRICE_ORACLE: Address = address!("54586b645bb2ef2c5a2e48b3b0a5f1f6ba6e1f3c");
    const GHO: Address = address!("40d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f");
    const A_TOKEN: Address = address!("b8f0f1b8f0f1b8f0f1b8f0f1b8f0f1b8f0f1b8f0");
    const V_TOKEN: Address = address!("c1d0e2c1d0e2c1d0e2c1d0e2c1d0e2c1d0e2c1d0");
    const STK_AAVE: Address = address!("4da27a545c0c5b758a6ba100e3a049001de870f5");
    const STRATEGY: Address = address!("4c38ec4d1d2068540dfc11dfa4de41f733ddf812");

    const PARENT: u64 = 17_699_252;
    const FROM: u64 = PARENT + 1;

    fn selector(signature: &str) -> String {
        let hash = keccak256(signature.as_bytes());
        format!("0x{}", alloy::hex::encode(&hash.0[..4]))
    }

    fn address_word(address: Address) -> String {
        format!(
            "{}{}",
            "0".repeat(24),
            alloy::hex::encode(address.as_slice())
        )
    }

    fn uint_word(value: u64) -> String {
        format!("{value:064x}")
    }

    /// One recorded `eth_call` key, exactly as the offline provider derives it.
    fn call_key(target: Address, signature: &str, argument: Option<Address>) -> String {
        let mut data = selector(signature);
        if let Some(argument) = argument {
            data.push_str(&address_word(argument));
        }
        format!("0x{}:{data}", alloy::hex::encode(target.as_slice()))
    }

    /// Every seeded substrate getter's answer, recorded at the capture parent.
    fn seed_answers() -> Vec<(String, String)> {
        let mut reserve_data = vec![uint_word(0); 11];
        reserve_data[8] = address_word(A_TOKEN);
        reserve_data[10] = address_word(V_TOKEN);
        vec![
            (
                call_key(ADDRESS_PROVIDER, "getPool()", None),
                address_word(POOL),
            ),
            (
                call_key(ADDRESS_PROVIDER, "getPoolConfigurator()", None),
                address_word(CONFIGURATOR),
            ),
            (
                call_key(ADDRESS_PROVIDER, "getPriceOracle()", None),
                address_word(PRICE_ORACLE),
            ),
            (call_key(POOL, "POOL_REVISION()", None), uint_word(9)),
            (
                call_key(CONFIGURATOR, "CONFIGURATOR_REVISION()", None),
                uint_word(3),
            ),
            (call_key(A_TOKEN, "ATOKEN_REVISION()", None), uint_word(1)),
            (
                call_key(V_TOKEN, "DEBT_TOKEN_REVISION()", None),
                uint_word(1),
            ),
            (
                call_key(V_TOKEN, "getDiscountToken()", None),
                address_word(STK_AAVE),
            ),
            (
                call_key(V_TOKEN, "getDiscountRateStrategy()", None),
                address_word(STRATEGY),
            ),
            (
                call_key(POOL, "getReserveData(address)", Some(GHO)),
                reserve_data.concat(),
            ),
        ]
    }

    /// A chain whose parent block answers every seed getter and whose span block
    /// answers nothing — so a read that leaves the parent finds no answer, and
    /// "latest" (the span block) is not a fallback either.
    fn recorded_seed_corpus() -> String {
        let calls: serde_json::Map<String, serde_json::Value> = seed_answers()
            .into_iter()
            .map(|(key, answer)| (key, serde_json::Value::from(answer)))
            .collect();
        serde_json::json!({
            "chain_id": 1,
            "blocks": {
                PARENT.to_string(): { "calls": calls },
                FROM.to_string(): {},
            },
        })
        .to_string()
    }

    fn seed_provider() -> AlloyProvider {
        OfflineProvider::from_json_str(&recorded_seed_corpus())
            .unwrap()
            .as_alloy_provider()
    }

    #[test]
    fn the_seed_block_is_the_capture_parent() {
        assert_eq!(seed_block(FROM).unwrap(), PARENT);
        assert!(
            seed_block(0).is_err(),
            "a span starting at genesis has no capture parent"
        );
    }

    #[test]
    fn every_seed_getter_reads_at_the_capture_parent() {
        let provider = seed_provider();
        degenbot_core::runtime::get_runtime().block_on(async {
            let reads = SeedReads::at(&provider, FROM).unwrap();
            assert_eq!(reads.block, PARENT);

            let contracts = read_seed_contracts(&reads, ADDRESS_PROVIDER)
                .await
                .expect("the parent answers every contract getter");
            assert_eq!(contracts.pool, POOL);
            assert_eq!(contracts.configurator, CONFIGURATOR);
            assert_eq!(contracts.price_oracle, PRICE_ORACLE);
            assert_eq!(contracts.pool_revision, 9);
            assert_eq!(contracts.configurator_revision, 3);

            let (a_token, v_token) = read_reserve_tokens(&reads, POOL, GHO)
                .await
                .expect("the parent answers getReserveData")
                .expect("GHO is a market reserve");
            assert_eq!((a_token, v_token), (A_TOKEN, V_TOKEN));
            assert_eq!(
                read_reserve_revisions(&reads, a_token, v_token)
                    .await
                    .expect("the parent answers both revisions"),
                (1, 1)
            );
            assert_eq!(
                read_gho_discount(&reads, v_token)
                    .await
                    .expect("the parent answers both discount getters"),
                (STK_AAVE, STRATEGY)
            );
        });
    }

    #[test]
    fn a_seed_read_off_the_capture_parent_finds_nothing() {
        let provider = seed_provider();
        degenbot_core::runtime::get_runtime().block_on(async {
            let late = SeedReads {
                provider: &provider,
                block: FROM,
            };
            let Err(error) = read_seed_contracts(&late, ADDRESS_PROVIDER).await else {
                panic!("the span block records no seed getter");
            };
            assert!(
                error.contains("getPool()"),
                "the refusal names the getter that was not served: {error}"
            );
        });
    }

    fn argv<'a>(items: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
        items.iter().map(|item| (*item).to_string())
    }

    #[test]
    fn the_recording_lane_takes_a_seed_out_path() {
        let parsed = parse_args(argv(&[
            "--kind",
            "aave-run",
            "--address-provider",
            "0x2f39d218133afab8f2b819b1066c7e434ad94e9e",
            "--gho",
            "0x40d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f",
            "--from",
            "17699253",
            "--to",
            "17699253",
            "--out",
            "cassette.json.zst",
            "--seed-out",
            "seed.json",
        ]))
        .unwrap();
        let Either::Record(spec) = parsed else {
            panic!("record mode expected");
        };
        assert_eq!(spec.seed_out.as_deref(), Some(Path::new("seed.json")));
    }

    #[test]
    fn the_wrapper_seed_check_reaches_the_offline_gate() {
        let parsed =
            parse_args(argv(&["--kind", "aave-run", "--check-seed", "seed.json"])).unwrap();
        let Either::CheckSeed { seeds, replay } = parsed else {
            panic!("seed check mode expected");
        };
        assert_eq!(seeds, vec![PathBuf::from("seed.json")]);
        assert!(
            replay.is_none(),
            "without explicit replay inputs the gate stays the canonical-only restore check"
        );
    }

    #[test]
    fn the_replay_inputs_are_explicit_argv_paths() {
        let parsed = parse_args(argv(&[
            "--kind",
            "aave-run",
            "--check-seed",
            "seed.json",
            "--cassette",
            "capture.json.zst",
            "--delta",
            "capture.delta-stream.json",
        ]))
        .unwrap();
        let Either::CheckSeed { seeds, replay } = parsed else {
            panic!("seed check mode expected");
        };
        assert_eq!(seeds, vec![PathBuf::from("seed.json")]);
        let replay = replay.expect("the explicit replay inputs reach the gate");
        assert_eq!(replay.cassette, PathBuf::from("capture.json.zst"));
        assert_eq!(replay.delta, PathBuf::from("capture.delta-stream.json"));
    }

    /// The replay arm pairs ONE seed with ONE cassette and ONE delta stream, so
    /// every shape that could not name those inputs is refused before any DB or
    /// file is touched.
    #[test]
    fn incomplete_or_ambiguous_replay_inputs_are_refused() {
        let only_cassette = parse_args(argv(&[
            "--check-seed",
            "seed.json",
            "--cassette",
            "capture.json.zst",
        ]));
        let Err(error) = only_cassette else {
            panic!("a cassette without its delta stream names no committed artifact");
        };
        assert!(error.contains("pass its --delta too"), "{error}");

        let only_delta = parse_args(argv(&[
            "--check-seed",
            "seed.json",
            "--delta",
            "capture.delta-stream.json",
        ]));
        let Err(error) = only_delta else {
            panic!("a delta stream without its cassette replays nothing");
        };
        assert!(error.contains("pass its --cassette too"), "{error}");

        let no_seed = parse_args(argv(&[
            "--delta",
            "capture.delta-stream.json",
            "--cassette",
            "capture.json.zst",
        ]));
        let Err(error) = no_seed else {
            panic!("the replay arm extends a seed check");
        };
        assert!(error.contains("extend --check-seed"), "{error}");

        let two_seeds = parse_args(argv(&[
            "--check-seed",
            "first.json",
            "--check-seed",
            "second.json",
            "--cassette",
            "capture.json.zst",
            "--delta",
            "capture.delta-stream.json",
        ]));
        let Err(error) = two_seeds else {
            panic!("one replay cannot serve two seeds");
        };
        assert!(error.contains("pair ONE seed"), "{error}");
    }

    /// A harness DB carrying one activated market — the substrate a seed dumps,
    /// built without any RPC.
    fn activated_seed_db(path: &Path) {
        let (db, _state) = DegenbotDb::open_for_writes(path).unwrap();
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        activate_aave_market_on_conn(
            &tx,
            1,
            "Aave Ethereum Market",
            "0x2f39d218133afab8f2b819b1066c7e434ad94e9e",
            "0x40d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f",
            Some("Gho Token"),
            Some("GHO"),
            Some(18),
            ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
        )
        .unwrap();
        tx.commit().unwrap();
    }

    /// The offline seed gate end to end, on the PRODUCTION schema: a seed dumped
    /// from a locally activated market restores byte-identically, and a drifted
    /// file is refused.
    #[test]
    fn the_seed_gate_accepts_a_restored_seed_and_refuses_drift() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("seed-source.db");
        activated_seed_db(&db_path);
        let seed_path = dir.path().join("seed.json");
        write_aave_seed(&db_path, &seed_path).unwrap();
        let committed = std::fs::read_to_string(&seed_path).unwrap();
        assert_eq!(
            check_seed(&seed_path).unwrap(),
            committed.len(),
            "a dumped seed restores and re-exports byte-identically"
        );

        let drift_path = dir.path().join("drift.json");
        std::fs::write(&drift_path, committed.trim_end()).unwrap();
        assert!(
            check_seed(&drift_path).is_err(),
            "a seed whose bytes drift from its canonical re-export fails the gate"
        );
    }

    /// `--seed-out` names the `aave-run` substrate dump, so a kind that seeds
    /// nothing must refuse it at argument validation instead of ignoring it.
    #[test]
    fn a_seed_out_outside_the_aave_run_kind_is_refused() {
        let parsed = parse_args(argv(&[
            "--kind",
            "pool",
            "--factory",
            "0x1f98431c8ad98523631ae4a59f267346ea31f984",
            "--from",
            "17699253",
            "--to",
            "17699253",
            "--out",
            "cassette.json.zst",
            "--seed-out",
            "seed.json",
        ]));
        let Err(error) = parsed else {
            panic!("--seed-out is the aave-run seed");
        };
        assert!(
            error.contains("--seed-out requires --kind aave-run"),
            "the refusal names the required kind: {error}"
        );
    }

    /// A bare relative `--seed-out` filename names the working directory — which
    /// exists — so the writer must not try to create an empty parent. The probe
    /// uses a uniquely named file beside the test process and never moves the
    /// process-global working directory.
    #[test]
    fn a_bare_seed_filename_writes_without_creating_a_parent() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("seed-source.db");
        activated_seed_db(&db_path);

        let bare = PathBuf::from(format!("aave-seed-probe-{}.json", std::process::id()));
        assert!(
            bare.parent().is_some_and(Path::is_empty),
            "a bare relative filename carries an empty parent"
        );
        write_aave_seed(&db_path, &bare).unwrap();
        let written = std::fs::read_to_string(&bare);
        let _ = std::fs::remove_file(&bare);
        let written = written.expect("the bare seed path is written beside the process");
        assert!(
            written.contains("degenbot.aave-seed/v1"),
            "a bare filename writes the seed itself"
        );
    }

    #[test]
    fn usage_names_every_recording_lane() {
        let usage = usage();
        for flag in [
            "--kind aave-run",
            "--seed-out",
            "--check-seed",
            "--cassette",
            "--delta",
            "--check <cassette",
        ] {
            assert!(usage.contains(flag), "usage must name {flag}");
        }
    }
}

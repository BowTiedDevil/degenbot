// Slice-1 driver has no panic/unwrap surfaces yet; keep only the lint
// expectation the operator binary actually triggers (stdout diagnostics),
// mirroring rust/crates/degenbot/examples/standalone_consumer.rs.
#![expect(
    clippy::print_stdout,
    reason = "standalone-driver binary whose boot/config/ledger diagnostics are read by the operator"
)]

//! Rust-owned settlement-arbitrage bot — the pure-Rust parity twin of the
//! Python driver (`examples/eth_settlement_arbitrage_v2_v3_v4_rust.py` +
//! `src/degenbot/runner/*`).
//!
//! This binary is the end-to-end consumer-parity test of epic RGZG4S
//! (`docs/architecture/rust-settlement-bot-parity.md`): it depends ONLY on
//! the umbrella `degenbot` crate (the `cargo add degenbot` experience), so
//! every surface the Python driver reaches but a Rust consumer cannot is a
//! compile wall here — recorded as a numbered gap in the ledger.
//!
//! Slice 1 (this file) covers ledger rows 1–5 (CLI, driver config,
//! RPC-URI cascade, DB path, snapshot-load boot slice) and advertises the
//! gap-gated rows 6–20 in its boot report. The engine handshake
//! (subscribe → S → resume+auto-backfill → consume → register paths) is
//! gated on **Gap G1** (ergo task 5XOGRK): the settlement engine is
//! `pub(crate)` in `degenbot-bot`, so a pure-Rust consumer cannot subscribe,
//! resume, register paths, or consume `ResultBatch` today.
//!
//! Parity sources (constants + error semantics mirrored byte-for-byte):
//!   - `src/degenbot/runner/cli.py`       — CLI flags
//!   - `src/degenbot/runner/config.py`    — `ArbitrageConfig.from_env`
//!   - `src/degenbot/config.py`           — `resolve_rpc_uris` cascade + db path

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use degenbot::core::address_utils::to_checksum_address_str;

/// The settled arbitration chain for this driver (Ethereum mainnet),
/// mirroring `ArbitrageConfig.from_env(chain_id=1)`.
const CHAIN_ID: u64 = 1;

// ── Driver-config defaults (mirrored from runner/config.py) ───────────────

const MIN_PROFIT_NET: u64 = 1;
const FEE_HISTORY_WINDOW: u64 = 10;
const FEE_PERCENTILES: [u64; 2] = [10, 50];
const TARGET_PROFIT_RATIO: f64 = 1.25;
const BLOCKS_BEFORE_NONCE_EXPIRES: u64 = 5;
const MAX_SIMULATE_CONCURRENT: u64 = 50;
const AGE_DECAY_CONSTANT: f64 = 0.25;
const MIN_PRIORITY_FEE_PERCENTILE: u64 = 10;
const MAX_PRIORITY_FEE_PERCENTILE: u64 = 50;
const PATH_SUPPRESS_THRESHOLD: u64 = 10;
const PATH_SUPPRESS_RETRY_INTERVAL: u64 = 100;
const VERIFICATION_RETRY_MAX_ATTEMPTS: u64 = 4;
const VERIFICATION_RETRY_BASE_DELAY: f64 = 0.5;
const VERIFICATION_RETRY_MAX_DELAY: f64 = 4.0;
const VERIFICATION_RETRY_JITTER: f64 = 0.5;

/// `ETH_MAINNET_ALLOWED_TOKENS` (runner/config.py) — checksummed, lowercase-
/// compared by the path predicate (row 13; full predicate lands with G3,
/// ergo task XFEJUG). Kept here so slice 1 reports the same value the
/// Python config would.
const ALLOWED_INTERMEDIATE_TOKENS: [&str; 11] = [
    "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", // USDC
    "0xdAC17F958D2ee523a2206206994597C13D831ec7", // USDT
    "0x6B175474E89094C44Da98b954EedeAC495271d0F", // DAI
    "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599", // WBTC
    "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984", // UNI
    "0x514910771AF9Ca656af840dff83E8264EcF986CA", // LINK
    "0x6B3595068778DD592e39A122f4f5a5cF09C90fE2", // SUSHI
    "0xD533a949740bb3306d119CC777fa900bA034cd52", // CRV
    "0xc00e94Cb662C3520282E6f5717214004A7f26888", // COMP
    "0x0bc529c00C6401aEF6D220BE8C6Ea1667F6Ad93e", // YFI
    "0x7D1AfA7B718fb893dB30A3aBc0Cfc608AaCfeBB0", // MATIC/POL
];

const DEFAULT_EXECUTOR_ADDRESS: &str = "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5";
const DEFAULT_INJECTED_ADDRESS: &str = "0x0D6d4c3cF3BD3b769De1821f2BE0d7d99913E4F1";
const DEFAULT_EXECUTOR_OWNER: &str = "0x9C56a29c7231974c269E24F9FB3c29203039089E";

/// Dry-run operator placeholders (runner/config.py): Anvil account-0 — a
/// valid secp256k1 key that never signs; the not-yet-wired submit leaf (G4,
/// ergo L4E7RI) must keep the same never-sign guarantee.
const DRY_RUN_OPERATOR_PRIVATE_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const DRY_RUN_OPERATOR_ADDRESS: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

/// `degenbot.constants.ZERO_ADDRESS`.
const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

// ── CLI (mirrors runner/cli.py::build_arbitrage_arg_parser) ───────────────

struct Cli {
    live: bool,
    permutation: Option<String>,
    node_http: Option<String>,
    node_ws: Option<String>,
    operator_socket: Option<String>,
}

const USAGE: &str = "usage: settlement-bot [--live] [--permutation V2-V3-V4] \
[--node-http URL] [--node-ws URL] [--operator-socket PATH]";

fn parse_cli(args: &[String]) -> Result<Cli, String> {
    let mut cli = Cli {
        live: false,
        permutation: None,
        node_http: None,
        node_ws: None,
        operator_socket: None,
    };
    let mut i = 0;
    while i < args.len() {
        let (flag, inline_value) = match args[i].split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (args[i].as_str(), None),
        };
        let take_value = |i: &mut usize| -> Result<String, String> {
            if let Some(v) = inline_value {
                return Ok(v);
            }
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match flag {
            "--live" => cli.live = true,
            "--permutation" => cli.permutation = Some(take_value(&mut i)?),
            "--node-http" => cli.node_http = Some(take_value(&mut i)?),
            "--node-ws" => cli.node_ws = Some(take_value(&mut i)?),
            "--operator-socket" => cli.operator_socket = Some(take_value(&mut i)?),
            "--help" | "-h" => {
                println!("{USAGE}");
                return Err("help".to_string());
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
        i += 1;
    }
    Ok(cli)
}

// ── Dotenv (mirrors dotenv.dotenv_values("examples/mainnet.env")) ─────────

fn read_dotenv(path: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        // Python's dotenv_values on a missing file yields an empty mapping;
        // the cascade then falls through to defaults/env exactly as here.
        return BTreeMap::new();
    };
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            map.insert(k.trim().to_string(), v.to_string());
        }
    }
    map
}

// ── Verification retry policy (mirrors arbitrage/verification_retry.py) ───

#[derive(Clone, Debug)]
struct VerificationRetryPolicy {
    max_attempts: u64,
    base_delay: f64,
    max_delay: f64,
    jitter: f64,
}

// ── Driver config (mirrors runner/config.py::ArbitrageConfig) ─────────────

struct SettlementBotConfig {
    operator_address: String,
    operator_private_key: String,
    node_http: String,
    node_ws: String,
    executor_address: String,
    executor_owner: String,
    inject_executor_code: bool,
    injected_address: String,
    min_profit_net: u64,
    fee_history_window: u64,
    fee_percentiles: [u64; 2],
    target_profit_ratio: f64,
    blocks_before_nonce_expires: u64,
    max_simulate_concurrent: u64,
    age_decay_constant: f64,
    min_priority_fee_percentile: u64,
    max_priority_fee_percentile: u64,
    path_suppress_threshold: u64,
    path_suppress_retry_interval: u64,
    allowed_intermediate_tokens: BTreeSet<String>,
    permutation_filter: Option<String>,
    verification_retry_policy: VerificationRetryPolicy,
    dry_run: bool,
    executor_runtime: Option<String>,
}

fn checksum_or_empty(addr: &str) -> Result<String, String> {
    if addr.is_empty() {
        return Ok(String::new());
    }
    to_checksum_address_str(addr).map_err(|e| format!("invalid address {addr}: {e}"))
}

fn parse_u64_env(raw: Option<&String>, default: u64, suffix: &str) -> Result<u64, String> {
    match raw {
        None => Ok(default),
        Some(v) if v.is_empty() => Ok(default),
        Some(v) => v
            .parse::<u64>()
            .map_err(|_| format!("VERIFICATION_RETRY_{suffix} must be an integer, got {v:?}")),
    }
}

fn parse_f64_env(raw: Option<&String>, default: f64, suffix: &str) -> Result<f64, String> {
    match raw {
        None => Ok(default),
        Some(v) if v.is_empty() => Ok(default),
        Some(v) => v
            .parse::<f64>()
            .map_err(|_| format!("VERIFICATION_RETRY_{suffix} must be a float, got {v:?}")),
    }
}

/// Read the operator config file (`~/.config/degenbot/config.toml`) the way
/// the Python cascade's `config.toml` layer does; a missing/unparseable file
/// yields `None` (that layer then simply contributes nothing).
fn read_config_toml() -> Option<toml::Table> {
    let home = std::env::var("HOME").ok()?;
    let path = PathBuf::from(home).join(".config/degenbot/config.toml");
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

fn config_toml_entry<'a>(
    toml_value: Option<&'a toml::Table>,
    section: &str,
    key: &str,
) -> Option<&'a str> {
    toml_value?.get(section)?.get(key)?.as_str()
}

/// Mirror `resolve_rpc_uris(chain_id, cli_http, cli_ws)`:
/// CLI > OS env `DEGENBOT_RPC_{HTTP,WS}_CHAINID_1` > config.toml
/// `rpc[1]`/`ws[1]` > error. **No localhost default** — a chain with no
/// configured endpoint in any layer is a hard error (Python raises
/// `RpcNotConfiguredError`; this driver reports + exits, see `main`).
fn cascade_rpc_uri(
    kind: &str,
    cli: Option<&String>,
    toml_section: &str,
    config_toml: Option<&toml::Table>,
) -> Result<String, String> {
    if let Some(v) = cli.filter(|v| !v.is_empty()) {
        return Ok(v.clone());
    }
    let env_name = format!("DEGENBOT_RPC_{kind}_CHAINID_{CHAIN_ID}");
    if let Ok(v) = std::env::var(&env_name) {
        if !v.is_empty() {
            return Ok(v);
        }
    }
    let chain_key = CHAIN_ID.to_string();
    if let Some(v) = config_toml_entry(config_toml, toml_section, &chain_key) {
        if !v.is_empty() {
            return Ok(v.to_string());
        }
    }
    Err(format!(
        "no {kind} RPC endpoint configured for chain {CHAIN_ID} (cascade: \
         --node-{kind_lower} > {env_name} > config.toml {toml_section}[{CHAIN_ID}]); \
         no localhost default",
        kind_lower = kind.to_lowercase(),
    ))
}

/// Mirror `_make_arbitrage_config`'s db path: config.toml `database.path`
/// else `~/.config/degenbot/degenbot.db`. `DEGENBOT_FIXTURE_DB` overrides,
/// the same CI test seam `standalone_consumer.rs` uses.
fn resolve_db_path(config_toml: Option<&toml::Table>) -> PathBuf {
    if let Ok(fixture) = std::env::var("DEGENBOT_FIXTURE_DB") {
        if !fixture.is_empty() {
            return PathBuf::from(fixture);
        }
    }
    if let Some(v) = config_toml_entry(config_toml, "database", "path") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config/degenbot/degenbot.db")
}

impl SettlementBotConfig {
    /// Mirror of `ArbitrageConfig.from_env(env, live=..., permutation=...)`:
    /// same defaults, same fail-fast error strings, same cascade order.
    #[expect(
        clippy::too_many_lines,
        reason = "linear env-parse mirror of ArbitrageConfig.from_env; splitting obscures the field-by-field cascade"
    )]
    fn from_env(env: &BTreeMap<String, String>, cli: &Cli) -> Result<Self, String> {
        // ── Operator ──
        let operator_raw = env.get("OPERATOR_ADDRESS").cloned().unwrap_or_default();
        let mut operator_address = if operator_raw.is_empty() {
            String::new()
        } else {
            checksum_or_empty(&operator_raw)?
        };
        let mut operator_private_key = env.get("OPERATOR_PRIVATE_KEY").cloned().unwrap_or_default();
        if cli.live {
            if operator_address.is_empty() || operator_private_key.is_empty() {
                return Err(
                    "OPERATOR_ADDRESS and OPERATOR_PRIVATE_KEY must be set in mainnet.env \
                     for live mode"
                        .to_string(),
                );
            }
        } else {
            if operator_address.is_empty() {
                operator_address = DRY_RUN_OPERATOR_ADDRESS.to_string();
            }
            if operator_private_key.is_empty() {
                operator_private_key = DRY_RUN_OPERATOR_PRIVATE_KEY.to_string();
            }
        }

        // ── Node URLs (standard cascade) ──
        let config_toml = read_config_toml();
        let node_http =
            cascade_rpc_uri("HTTP", cli.node_http.as_ref(), "rpc", config_toml.as_ref())?;
        let node_ws = cascade_rpc_uri("WS", cli.node_ws.as_ref(), "ws", config_toml.as_ref())?;

        // ── Executor ──
        let mut executor_address = checksum_or_empty(
            env.get("EXECUTOR_CONTRACT_ADDRESS")
                .map_or(DEFAULT_EXECUTOR_ADDRESS, String::as_str),
        )?;
        if executor_address == ZERO_ADDRESS {
            return Err("EXECUTOR_CONTRACT_ADDRESS is the zero address".to_string());
        }
        let inject_executor_code =
            env.get("INJECT_EXECUTOR_CODE").map_or("1", String::as_str) == "1";
        let injected_address = checksum_or_empty(
            env.get("INJECTED_EXECUTOR_ADDRESS")
                .map_or(DEFAULT_INJECTED_ADDRESS, String::as_str),
        )?;
        let executor_owner = checksum_or_empty(
            env.get("EXECUTOR_OWNER_ADDRESS")
                .map_or(DEFAULT_EXECUTOR_OWNER, String::as_str),
        )?;
        if inject_executor_code {
            executor_address.clone_from(&injected_address);
        }
        let executor_runtime = env
            .get("EXECUTOR_RUNTIME")
            .filter(|v| !v.is_empty())
            .cloned();

        let verification_retry_policy = VerificationRetryPolicy {
            max_attempts: parse_u64_env(
                env.get("VERIFICATION_RETRY_MAX_ATTEMPTS"),
                VERIFICATION_RETRY_MAX_ATTEMPTS,
                "MAX_ATTEMPTS",
            )?,
            base_delay: parse_f64_env(
                env.get("VERIFICATION_RETRY_BASE_DELAY"),
                VERIFICATION_RETRY_BASE_DELAY,
                "BASE_DELAY",
            )?,
            max_delay: parse_f64_env(
                env.get("VERIFICATION_RETRY_MAX_DELAY"),
                VERIFICATION_RETRY_MAX_DELAY,
                "MAX_DELAY",
            )?,
            jitter: parse_f64_env(
                env.get("VERIFICATION_RETRY_JITTER"),
                VERIFICATION_RETRY_JITTER,
                "JITTER",
            )?,
        };

        Ok(Self {
            operator_address,
            operator_private_key,
            node_http,
            node_ws,
            executor_address,
            executor_owner,
            inject_executor_code,
            injected_address,
            min_profit_net: MIN_PROFIT_NET,
            fee_history_window: FEE_HISTORY_WINDOW,
            fee_percentiles: FEE_PERCENTILES,
            target_profit_ratio: TARGET_PROFIT_RATIO,
            blocks_before_nonce_expires: BLOCKS_BEFORE_NONCE_EXPIRES,
            max_simulate_concurrent: MAX_SIMULATE_CONCURRENT,
            age_decay_constant: AGE_DECAY_CONSTANT,
            min_priority_fee_percentile: MIN_PRIORITY_FEE_PERCENTILE,
            max_priority_fee_percentile: MAX_PRIORITY_FEE_PERCENTILE,
            path_suppress_threshold: PATH_SUPPRESS_THRESHOLD,
            path_suppress_retry_interval: PATH_SUPPRESS_RETRY_INTERVAL,
            allowed_intermediate_tokens: ALLOWED_INTERMEDIATE_TOKENS
                .iter()
                .map(ToString::to_string)
                .collect(),
            permutation_filter: cli.permutation.clone(),
            verification_retry_policy,
            dry_run: !cli.live,
            executor_runtime,
        })
    }
}

// ── Parity ledger boot report ─────────────────────────────────────────────

/// Machine-checkable parity ledger rows for the boot report. The RSP-8
/// running gate (ergo 23DLCY) diffs these lines between the Python and Rust
/// drivers; `grep "^parity-ledger"` is the extraction contract.
fn print_parity_ledger(snapshot_seed_block: Option<u64>) {
    let s = snapshot_seed_block.map_or("None".to_string(), |s| s.to_string());
    #[rustfmt::skip]
    let rows: &[(&str, &str, &str)] = &[
        ("01-cli", "REACHABLE", "parse_cli (driver-local, mirrors runner/cli.py)"),
        ("02-driver-config", "REACHABLE", "from_env (mirrors runner/config.py)"),
        ("03-rpc-cascade", "REACHABLE", "cascade_rpc_uri (mirrors resolve_rpc_uris)"),
        ("04-db-path", "REACHABLE", "resolve_db_path (+DEGENBOT_FIXTURE_DB seam)"),
        ("05-snapshot-load", "REACHABLE", "SnapshotDb::open + Bot::load_snapshot_from_db"),
        ("06-engine-subscribe-resume", "BLOCKED-G1(ergo=5XOGRK)", "ArbitrageEngine is pub(crate); BlockPump needs the engine as Arc<dyn StageHandlers>"),
        ("07-result-batch-stream", "BLOCKED-G1(ergo=5XOGRK)", "ResultBatch public; no public producer"),
        ("08-register-and-solve-path", "BLOCKED-G1(ergo=5XOGRK)", "crate-private engine method"),
        ("09-verify-lifecycles", "PARTIAL(ergo=XFEJUG)", "run_v3/v4_registration_lifecycle exported; engine-side lifecycle + VerifyClaims G1/G3"),
        ("10-pool-construction", "REACHABLE", "probe_pool_type + build_v2/v3/v4/... (umbrella)"),
        ("11-discovery-db-enumeration", "PARTIAL(ergo=YFIOSF)", "SQLAlchemy enumeration has no verified degenbot-db twin"),
        ("12-path-discovery-batching", "PARTIAL(ergo=XFEJUG)", "PathGraph reachable; async batched find_paths wrapper Python-side"),
        ("13-path-policy", "DRIVER-POLICY", "allowlist mirrored in SettlementBotConfig"),
        ("14-in-process-sim", "REACHABLE", "simulate_in_process_with_db + SimulateContext"),
        ("15-dispatch-selection", "REACHABLE", "degenbot::arbitrage::dispatch_profitable_results"),
        ("16-sim-fanout-submitter", "DRIVER-POLICY-BLOCKED-G1(ergo=L4E7RI)", "tokio pipeline; feeds from blocked row 7"),
        ("17-fee-determination", "PARTIAL(ergo=L4E7RI)", "eip_1559 reachable; eth_feeHistory reachability unverified"),
        ("18-live-submission", "PARTIAL(ergo=L4E7RI)", "degenbot-submission reachable; TxSigner reachability unverified"),
        ("19-session-watch", "DRIVER-POLICY(ergo=KPLWUM)", "tokio watchdog, not yet wired"),
        ("20-operator-channel", "DRIVER-POLICY(ergo=KPLWUM)", "Unix socket accepted by CLI; server not yet wired"),
    ];
    for (row, status, note) in rows {
        println!("parity-ledger row={row} status={status} note={note}");
    }
    println!("parity-ledger snapshot-seed-block S={s}");
}

// ── main ──────────────────────────────────────────────────────────────────

fn main() -> ExitCode {
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            // The Python driver logs + returns (exit 0) on config failure;
            // this driver deliberately returns failure so the RSP-8 gate can
            // fail loudly. Recorded divergence vs runner/config.py callers.
            println!("[degenbot-settlement-bot] ERROR: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = parse_cli(&args)?;

    // The Python example reads `examples/mainnet.env` from the repo root;
    // CARGO_MANIFEST_DIR is rust/examples/settlement_bot, so ../../../mainnet.env
    // is that same file.
    let env_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../mainnet.env");
    let dotenv = read_dotenv(&env_path);
    if let Some(p) = &cli.permutation {
        println!("[startup] Permutation filter from CLI: {p}");
    }
    if cli.live {
        println!("\n*** LIVE MODE — BOT WILL SUBMIT REAL TRANSACTIONS (once G4 lands) ***\n");
    }

    let cfg = SettlementBotConfig::from_env(&dotenv, &cli)?;
    // Full (secret-masked) config dump — this line IS the read of every
    // driver-config field, so nothing in the struct is dead state.
    println!(
        "[degenbot-settlement-bot] rust-owned settlement bot (dry_run={}) ready; \
        operator={} operator_key=set(len={}) http={} ws={}",
        cfg.dry_run,
        cfg.operator_address,
        cfg.operator_private_key.len(),
        cfg.node_http,
        cfg.node_ws,
    );
    println!(
        "[config] executor={} owner={} inject_code={} injected={} executor_runtime={:?}",
        cfg.executor_address,
        cfg.executor_owner,
        cfg.inject_executor_code,
        cfg.injected_address,
        cfg.executor_runtime,
    );
    println!(
        "[config] min_profit_net={} fee_history_window={} fee_percentiles={:?} \
        target_profit_ratio={} nonce_expires_blocks={} max_sim_concurrent={} \
        age_decay={} priority_fee_percentiles=[{},{}] path_suppress=[{},{}] \
        allowed_intermediate_tokens={} permutation={:?}",
        cfg.min_profit_net,
        cfg.fee_history_window,
        cfg.fee_percentiles,
        cfg.target_profit_ratio,
        cfg.blocks_before_nonce_expires,
        cfg.max_simulate_concurrent,
        cfg.age_decay_constant,
        cfg.min_priority_fee_percentile,
        cfg.max_priority_fee_percentile,
        cfg.path_suppress_threshold,
        cfg.path_suppress_retry_interval,
        cfg.allowed_intermediate_tokens.len(),
        cfg.permutation_filter,
    );
    println!(
        "[config] verification_retry: max_attempts={} base_delay={} max_delay={} jitter={}",
        cfg.verification_retry_policy.max_attempts,
        cfg.verification_retry_policy.base_delay,
        cfg.verification_retry_policy.max_delay,
        cfg.verification_retry_policy.jitter,
    );

    // ── Boot slice (ledger row 5): DB snapshot load → seed block S ──
    // The Python path loads the DB snapshot eagerly inside `Bot.__init__`
    // and stashes S on the shared state before `engine.subscribe(...)`.
    let db_path = resolve_db_path(read_config_toml().as_ref());
    let (snap, _schema) = degenbot::db::snapshot_db::SnapshotDb::open(&db_path)
        .map_err(|e| format!("open snapshot DB {}: {e}", db_path.display()))?;
    let bot = degenbot::bot_core::Bot::new(CHAIN_ID);
    // SnapshotDb holds the deferred read tx; it implements TickMapDb directly
    // (degenbot_db::snapshot::TickMapDb is the trait the bot reads S through).
    bot.load_snapshot_from_db(&snap, CHAIN_ID)
        .map_err(|e| format!("load_snapshot_from_db on {}: {e}", db_path.display()))?;
    let seed_block = bot
        .state_arc()
        .read_at(degenbot::bot_core::state_lock::LockSite::Core)
        .snapshot_seed_block();
    println!(
        "[boot] snapshot loaded from {} (chain {CHAIN_ID}) → S={:?}",
        db_path.display(),
        seed_block
    );

    print_parity_ledger(seed_block);

    if cli.operator_socket.is_some() {
        println!("[operator] --operator-socket accepted; channel not yet wired (G5, ergo KPLWUM)");
    }
    // Gap G1 wall (ergo 5XOGRK): the next phases — engine.subscribe(ws),
    // resume()+auto-backfill, register_and_solve_path, ResultBatch
    // consumption, dispatch — have no public Rust entry points today
    // (ArbitrageEngine is `pub(crate)` in degenbot-bot). This binary exits
    // here until the facade lands; each new slice will retract this tail.
    println!(
        "[gap G1] engine handshake not wired (ergo 5XOGRK): subscribe → resume+backfill → \
         consume → dispatch all require the crate-private ArbitrageEngine. Exiting after boot."
    );
    Ok(())
}

// Slice-1 driver has no panic/unwrap surfaces yet; keep only the lint
// expectation the operator binary actually triggers (stdout diagnostics),
// mirroring rust/crates/degenbot/examples/standalone_consumer.rs.
#![expect(
    clippy::print_stdout,
    reason = "standalone-driver binary whose boot/config/ledger diagnostics are read by the operator"
)]
#![expect(
    dead_code,
    reason = "the Gap-G3 pipeline modules ship the full driver stage surface (claims/retry/ledger               classification + the smoke-gated live registration arms); the default offline boot and               the #[cfg(test)] suite exercise different subsets of it"
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
//! Slice 1 covers ledger rows 1–5 (CLI, driver config, RPC-URI cascade, DB
//! path, snapshot-load boot slice). Rows 6–8 (the engine handshake, the
//! result-batch stream, and path registration) are driven through the public
//! `degenbot::EngineDriver` (ADR-050 / Gap G1,):
//! `EngineDriver::start` → `take_result_receiver` → `resume` (the driver owns
//! the `S+1..W` auto-backfill) → `stop`. The G3 registration pipeline lands
//! driver-side . Gap G4  adds the driver-side
//! `consume`/`dispatch`/`sim_submit`/`submission` modules mirroring rows
//! 15–18. Gap G5  adds `session_watch` (the typed end-state
//! verdict + heartbeat/stall watchdog over the consume loop) and
//! `operator_channel` (the `--operator-socket` JSON-lines channel; row 19/20).
//! The live handshake is gated behind `SMOKE_RPC_URL` so the example
//! stays CI-runnable; without it (or with `--smoke-offline`) it stops after the
//! parity-ledger print. Once live registration completes the arm enters the
//! RSP-10 run-until-shutdown phase (`run_loop`,) — the
//! `BotRunner.run` main-loop shape: consumer + watch + operator channel stay
//! alive until SIGINT or the bounded `DEGENBOT_SMOKE_MAX_SECS` window, with
//! per-block heartbeats for observation runs. `--operator-inert` (with `--operator-socket`) is the
//! documented RPC-free operator-serve mode for the wire integration check.
//!
//! Parity sources (constants + error semantics mirrored byte-for-byte):
//!   - `src/degenbot/runner/cli.py`       — CLI flags
//!   - `src/degenbot/runner/config.py`    — `ArbitrageConfig.from_env`
//!   - `src/degenbot/config.py`           — `resolve_rpc_uris` cascade + db path

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use degenbot::core::address_utils::to_checksum_address_str;
use degenbot::core::retry::RetryPolicy;

mod claims;
mod consume;
mod discovery;
mod dispatch;
mod ledger;
mod live;
mod operator_channel;
mod pipeline;
mod policy;
mod progress;
mod retry;
mod run_loop;
mod session_watch;
mod sim_submit;
mod submission;
mod telemetry;

use crate::discovery::{build_graph, DiscoveryParams, NATIVE_CURRENCY};
use crate::pipeline::{run_offline, RegistrationPipeline};
use crate::policy::{parse_permutation_filter, PathPolicy};
use degenbot::pathfinding::PoolKind;

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

/// `ETH_MAINNET_ALLOWED_TOKENS` (runner/config.py) — checksummed, lowercase-
/// compared by the path predicate (row 13; implemented by the `policy` module,
///). Kept here so the config dump reports the same value the
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

/// `_driver_constants.WETH_ADDRESS` (Ethereum mainnet wrapped native).
const WETH_ADDRESS: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";

/// `_driver_constants.ETH_MAINNET_ALLOWED_TOKENS` — the DISCOVERY allowlist
/// `build_paths.py::discovery_sweep` passes to `find_paths_async`.
///
/// NOTE: Python has two distinct sets. `config.py::_ALLOWED_INTERMEDIATE_TOKENS`
/// (11 tokens, mirrored by `ALLOWED_INTERMEDIATE_TOKENS` above) is the config
/// FIELD; the discovery path actually passes the 15-token
/// `_driver_constants.ETH_MAINNET_ALLOWED_TOKENS` (which includes WETH —
/// required, because `build_path_graph` intersects the candidate-token set
/// with it). The example mirrors the DISCOVERY set here so the graph filter is
/// faithful; the config field stays the 11-token list for row-2 parity.
const ETH_MAINNET_DISCOVERY_ALLOWED_TOKENS: [&str; 15] = [
    "0x163f8C2467924be0ae7B5347228CABF260318753", // WLD
    "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8", // PyUSD
    "0xB8c77482e45F1F44dE1745F52C74426C631bDD52", // BNB
    "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", // WETH
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
///) must keep the same never-sign guarantee.
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
    operator_inert: bool,
    smoke_offline: bool,
}

const USAGE: &str = "usage: settlement-bot [--live] [--permutation V2-V3-V4] \
[--node-http URL] [--node-ws URL] [--operator-socket PATH] [--operator-inert] \
[--smoke-offline]";

fn parse_cli(args: &[String]) -> Result<Cli, String> {
    let mut cli = Cli {
        live: false,
        permutation: None,
        node_http: None,
        node_ws: None,
        operator_socket: None,
        operator_inert: false,
        smoke_offline: false,
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
            "--operator-inert" => cli.operator_inert = true,
            "--smoke-offline" => cli.smoke_offline = true,
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
    verification_retry_policy: RetryPolicy,
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

/// Read the operator config file (`$XDG_CONFIG_HOME` when absolute, else
/// `$HOME/.config`) `/degenbot/config.toml` the way the Python cascade's
/// `config.toml` layer does; a missing/unparseable file yields `None` (that
/// layer then simply contributes nothing).
fn read_config_toml() -> Option<toml::Table> {
    let base = match std::env::var("XDG_CONFIG_HOME") {
        Ok(xdg) if !xdg.is_empty() && PathBuf::from(&xdg).is_absolute() => PathBuf::from(xdg),
        _ => PathBuf::from(std::env::var("HOME").ok()?).join(".config"),
    };
    let path = base.join("degenbot/config.toml");
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
/// else `<state_home>/degenbot/db/degenbot.db` (`$XDG_STATE_HOME` when
/// absolute, else `$HOME/.local/state`). `DEGENBOT_FIXTURE_DB` overrides, the
/// same CI test seam `standalone_consumer.rs` uses.
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
    let state_home = match std::env::var("XDG_STATE_HOME") {
        Ok(xdg) if !xdg.is_empty() && PathBuf::from(&xdg).is_absolute() => PathBuf::from(xdg),
        _ => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state"),
    };
    state_home.join("degenbot/db/degenbot.db")
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
        // Injection stance resolves from one precedence chain (env > dotenv
        // > deployed-default false), mirroring the Python runner's
        // from_env: the bare dotenv/OsEnv name with the old implicit-true
        // default used to overlap a second import-time read, and divergence
        // silently vetoed live submission. A bare INJECT_EXECUTOR_CODE in the
        // process environment is refused rather than re-admitted.
        let inject_executor_code = if std::env::var_os("INJECT_EXECUTOR_CODE").is_some() {
            return Err(
                "INJECT_EXECUTOR_CODE is retired as an OS environment variable; set \
                 DEGENBOT_INJECT_EXECUTOR_CODE (typed key simulation.inject_executor_code)"
                    .to_string(),
            );
        } else if let Ok(typed) = std::env::var("DEGENBOT_INJECT_EXECUTOR_CODE") {
            matches!(typed.to_ascii_lowercase().as_str(), "1" | "true" | "on")
        } else {
            env.get("INJECT_EXECUTOR_CODE").map_or("0", String::as_str) == "1"
        };
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

        // Defaults come from the workspace-canonical policy type; the env
        // knobs override them field-by-field.
        let retry_defaults = RetryPolicy::verification_default();
        let verification_retry_policy = RetryPolicy {
            max_attempts: u32::try_from(parse_u64_env(
                env.get("VERIFICATION_RETRY_MAX_ATTEMPTS"),
                u64::from(retry_defaults.max_attempts),
                "MAX_ATTEMPTS",
            )?)
            .unwrap_or(u32::MAX),
            base_delay: parse_f64_env(
                env.get("VERIFICATION_RETRY_BASE_DELAY"),
                retry_defaults.base_delay,
                "BASE_DELAY",
            )?,
            max_delay: parse_f64_env(
                env.get("VERIFICATION_RETRY_MAX_DELAY"),
                retry_defaults.max_delay,
                "MAX_DELAY",
            )?,
            jitter: parse_f64_env(
                env.get("VERIFICATION_RETRY_JITTER"),
                retry_defaults.jitter,
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
/// running gate  diffs these lines between the Python and Rust
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
        ("06-engine-subscribe-resume", "REACHED-via-EngineDriver", "EngineDriver::start → subscribe → verify-config (stops pre-resume); resume owns the S+1..W auto-backfill via BlockPump::backfill_with_drain"),
        ("07-result-batch-stream", "REACHED-via-EngineDriver", "EngineDriver::take_result_receiver (attach pre-resume); ResultBatch over the existing unbounded channel"),
        ("08-register-and-solve-path", "REACHED-via-EngineDriver", "EngineDriver::register_and_solve_path delegates to EngineStages"),
        ("09-verify-lifecycles", "REACHABLE", "EngineDriver::run_v3/v4_registration_lifecycle(+_sync) expose the core lifecycles; claims.rs VerifyClaims (tokio at-most-once) + retry.rs + ledger.rs shipped by XFEJUG"),
        ("10-pool-construction", "REACHABLE", "probe_pool_type + build_v2/v3/v4/... (umbrella)"),
        ("11-discovery-db-enumeration", "REACHABLE", "degenbot::db::SnapshotDb::fetch_discovery_rows (degenbot-db::discovery_read) + tests/discovery_read_parity.rs"),
        ("12-path-discovery-batching", "REACHABLE", "discovery.rs: graph build over G2 rows + batched lazy OwnedPathFinder (batch_size<=1 per-path; one cooperative async hop per batch)"),
        ("13-path-policy", "DRIVER-POLICY", "policy.rs (hop bounds 2/3, allow/deny, duplicate-pool, permutation) + discovery allowlist graph filter; config.py 11-token + _driver_constants 15-token sets"),
        ("14-in-process-sim", "REACHABLE", "simulate_in_process_with_db + SimulateContext"),
        ("15-dispatch-selection", "REACHABLE", "degenbot::arbitrage::{dispatch_profitable_results,filter_thin_margin_results} + driver dispatch.rs plan_batch typed decisions (skip/suppressed/thin-margin/sim)"),
        ("16-sim-fanout-submitter", "DRIVER-POLICY", "sim_submit.rs: tokio Semaphore(max_simulate_concurrent) + single ordered FIFO submitter; consume.rs consumes the EngineDriver result stream (row 7); no core lift"),
        ("17-fee-determination", "REACHABLE", "degenbot::arbitrage::compute_priority_fee + degenbot::rpc::{fetch_priority_fee_percentiles,provider::AlloyProvider::eth_fee_history} + degenbot::submission::fetch_fee_history + degenbot_core::eip_1559::next_base_fee"),
        ("18-live-submission", "REACHABLE", "degenbot::submission::{TxSigner,dispatch_and_submit,monitor_pending_transaction,Dispatcher,PathSuppression}; submission.rs dry-run seam never signs"),
        ("19-session-watch", "DRIVER-POLICY", "session_watch.rs: typed SessionEndVerdict {PumpEnded,RegistrationFailed,WatchdogTripped} + Heartbeat/stall_watchdog observing the live consume loop (watch-as-observer, no core lift)"),
        ("20-operator-channel", "DRIVER-POLICY", "operator_channel.rs: tokio UnixListener JSON-lines add_path/discover/set+get_fleet_posture; fleet posture through degenbot::workers::posture::process (reachable via the umbrella)"),
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

#[expect(
    clippy::too_many_lines,
    reason = "linear boot + ledger + EngineDriver handshake driver; splitting obscures the phase order"
)]
fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = parse_cli(&args)?;

    // ── Telemetry boot prelude (Gap G6,) ──
    // The Python driver boots its subscriber / OTLP / metrics stack at `_ffi`
    // import; the standalone parity twin boots the same stack here, before any
    // driver diagnostic. Telemetry failure degrades loudly but never aborts the
    // boot (logging stays up, telemetry is optional — `telemetry` module docs).
    // The binding's `Drop` flushes + shuts the OTel provider and stops the
    // scrape server on every exit path (ADR-043 section 6).
    let _telemetry_boot = telemetry::init();

    // The Python example reads `examples/mainnet.env` from the repo root;
    // CARGO_MANIFEST_DIR is rust/examples/settlement_bot, so ../../../mainnet.env
    // is that same file.
    let env_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../mainnet.env");
    let dotenv = read_dotenv(&env_path);
    if let Some(p) = &cli.permutation {
        println!("[startup] Permutation filter from CLI: {p}");
    }
    if cli.live {
        println!("\n*** LIVE MODE — BOT WILL SUBMIT REAL TRANSACTIONS ***\n");
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
    degenbot::op_info!(domain = pump, dry_run = cfg.dry_run, "driver config ready");
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
    degenbot::op_info!(domain = state, seed_block = ?seed_block, "boot: snapshot loaded");

    // ── Candidate-pool discovery (ledger row 11): the read-only enumeration
    // `build_paths.py` performs over its SQLAlchemy ORM, now reachable through
    // the umbrella on the SAME held-tx snapshot handle (Gap G2,).
    // The call is the compile-time proof that a `cargo add degenbot` consumer
    // reaches `degenbot::db::SnapshotDb::fetch_discovery_rows`; the count is
    // the runtime witness.
    // `DEGENBOT_DISCOVERY_CHAIN_ID` is a test seam (the `DEGENBOT_FIXTURE_DB`
    // precedent): the parity fixture is chain 8453 while the driver's settled
    // chain is 1. Defaults to CHAIN_ID (the Python driver reads the chain from
    // the DB/bot, not an env var).
    let discovery_chain: i64 = match std::env::var("DEGENBOT_DISCOVERY_CHAIN_ID") {
        Ok(raw) if !raw.is_empty() => raw
            .parse::<i64>()
            .map_err(|e| format!("DEGENBOT_DISCOVERY_CHAIN_ID: {e}"))?,
        _ => i64::try_from(CHAIN_ID).map_err(|e| format!("chain id: {e}"))?,
    };
    let discovered = snap
        .fetch_discovery_rows(discovery_chain)
        .map_err(|e| format!("discovery enumeration from {}: {e}", db_path.display()))?;
    println!(
        "[boot] discovery enumerated {} candidate pools (chain {discovery_chain}, read-only, held-tx)",
        discovered.len()
    );
    degenbot::op_info!(
        domain = path,
        candidates = discovered.len(),
        chain_id = discovery_chain,
        "boot: discovery enumerated"
    );

    // ── G3 pipeline (ledger rows 9 + 12 + 13,) ──
    // 1. Permutation filter → per-depth pool-kind filter + requested kinds.
    let perms: BTreeSet<String> = cfg
        .permutation_filter
        .iter()
        .cloned()
        .collect::<BTreeSet<String>>();
    let permutation = parse_permutation_filter(&perms)?;
    let (pool_type_per_depth, requested_kinds) = permutation.as_ref().map_or_else(
        || (None, vec![PoolKind::V2, PoolKind::V3, PoolKind::V4]),
        |filter| (Some(filter.per_depth.clone()), filter.pool_kinds.clone()),
    );

    // 2. Graph build from the SAME held discovery rows (snapshot discipline).
    let allowed: BTreeSet<String> = ETH_MAINNET_DISCOVERY_ALLOWED_TOKENS
        .iter()
        .map(|t| t.to_lowercase())
        .collect();
    let built = build_graph(&discovered, &requested_kinds, Some(&allowed));
    println!(
        "[g3] graph built: {} nodes, {} candidate tokens, {} requested kinds {:?}",
        built.nodes.len(),
        built.candidate_tokens.len(),
        requested_kinds.len(),
        requested_kinds
    );
    degenbot::op_info!(
        domain = path,
        nodes = built.nodes.len(),
        candidate_tokens = built.candidate_tokens.len(),
        "boot: candidate graph built"
    );

    // 3. Start/end tokens (WETH + V4 native currency), resolved from the
    //    discovered token ids (mirrors `discovery_sweep`'s start/end lists).
    let weth_lower = WETH_ADDRESS.to_lowercase();
    let native_lower = NATIVE_CURRENCY.to_string();
    let mut boundary_tokens: Vec<u64> = Vec::new();
    for addr in [&weth_lower, &native_lower] {
        if let Some(id) = built.token_id_by_lower.get(addr) {
            if !boundary_tokens.contains(id) {
                boundary_tokens.push(*id);
            }
        }
    }

    // 4. Discovery params: min_depth 2 (find_paths default), max_depth 3
    //    (`discovery_sweep`), batch size from the typed config
    //    (`pathfinding.discovery_batch_size`, env DEGENBOT_DISCOVERY_BATCH_SIZE).
    let batch_size = degenbot::config::holder::config()
        .pathfinding
        .discovery_batch_size
        .max(1);
    let params = DiscoveryParams {
        start_tokens: boundary_tokens.clone(),
        end_tokens: boundary_tokens.clone(),
        min_depth: 2,
        max_depth: Some(3),
        pool_type_per_depth: pool_type_per_depth.clone(),
        batch_size,
    };

    // 5. Driver policy (row 13): hop bounds pinned to the discovery floor/cap,
    //    duplicate-pool guard on; the token allowlist is applied at the graph
    //    filter above (mirroring `find_paths_async`'s `allowed_intermediate_tokens`).
    let policy = PathPolicy {
        min_hops: 2,
        max_hops: 3,
        ..PathPolicy::default()
    };
    let retry_policy = cfg.verification_retry_policy;
    retry_policy.validate()?;
    let mut pipeline = RegistrationPipeline::new(policy, retry_policy);

    // 6. Offline-dry run: enumeration → directions → policy → candidate counts.
    let report = degenbot::runtime::get_runtime().block_on(run_offline(
        &mut pipeline,
        &built,
        &params,
        &weth_lower,
        &weth_lower,
    ));
    println!(
        "[g3] offline-dry pipeline: candidates={} path_count={} skips={} policy_rejected={} \
         direction_errors={} v4_hops={} dup={} cap={} batch_size={}",
        report.candidates,
        report.path_count,
        report.skip_count,
        report.policy_rejected,
        report.direction_errors,
        report.v4_pool_count,
        report.dup_count,
        report.capped,
        batch_size,
    );
    degenbot::op_info!(
        domain = solver,
        candidates = report.candidates,
        path_count = report.path_count,
        "g3: offline-dry pipeline complete"
    );
    if !report.skip_reasons.is_empty() {
        let breakdown: Vec<String> = report
            .skip_reasons
            .iter()
            .map(|(reason, count)| format!("{reason}={count}"))
            .collect();
        println!("[g3] skip reasons: {}", breakdown.join(", "));
    }

    print_parity_ledger(seed_block);

    // ── G5 operator channel (ledger row 20,) ──
    // A documented inert mode: serve the operator Unix socket WITHOUT RPC so
    // the wire contract is exercisable offline (the live arm needs
    // `SMOKE_RPC_URL`; this is the CI/integration test surface). The default
    // offline boot below is untouched unless `--operator-inert` is passed.
    if cli.operator_inert {
        let Some(socket_path) = cli.operator_socket.clone() else {
            return Err("--operator-inert requires --operator-socket PATH".to_string());
        };
        let ops: std::sync::Arc<dyn operator_channel::PathOps> =
            std::sync::Arc::new(operator_channel::PipelinePathOps::new(
                &discovered,
                &requested_kinds,
                &allowed,
                &params,
                pipeline.policy.clone(),
                pipeline.retry_policy,
                weth_lower.clone(),
                weth_lower.clone(),
            ));
        let runtime = degenbot::runtime::get_runtime();
        return runtime.block_on(async {
            let running = operator_channel::start_operator_server(Path::new(&socket_path), ops)?;
            println!(
                "[operator] inert channel listening on {socket_path} (no RPC; Ctrl-C to stop)"
            );
            tokio::signal::ctrl_c()
                .await
                .map_err(|e| format!("ctrl_c wait: {e}"))?;
            running.close().await;
            println!("[operator] operator channel closed; socket removed");
            Ok(())
        });
    }

    if cli.operator_socket.is_some() {
        println!(
            "[operator] --operator-socket accepted; served only in the live arm or with --operator-inert (offline boot exits as before)"
        );
    }

    // ── Engine handshake (ledger rows 6–8): the public Rust-native driver ──
    // Offline by default (CI-safe): with no `SMOKE_RPC_URL`, stop after the
    // ledger print. `SMOKE_RPC_URL` (a ws:// endpoint) opts into the live
    // ritual; `--smoke-offline` forces offline even when it is set.
    let smoke_rpc = std::env::var("SMOKE_RPC_URL").ok();
    if cli.smoke_offline || smoke_rpc.is_none() {
        println!(
            "[smoke-offline] engine handshake skipped: set SMOKE_RPC_URL='ws://...' to drive \
             EngineDriver::start → take_result_receiver → resume (--smoke-offline forces offline)"
        );
        return Ok(());
    }
    let ws = smoke_rpc.unwrap_or_else(|| cfg.node_ws.clone());
    let http = if cfg.node_http.is_empty() {
        ws.clone()
    } else {
        cfg.node_http.clone()
    };
    let bot = std::sync::Arc::new(bot);
    let driver = degenbot::EngineDriver::new(
        std::sync::Arc::clone(&bot),
        degenbot::config::holder::config_arc(),
    );
    // RSP-11 (KETJNN): bind the registered-path budget from DEGENBOT_MAX_PATHS
    // (default 100000; 0 = uncapped) onto the engine registry BEFORE the crawl,
    // mirroring build_paths.py's `engine.set_path_cap(MAX_REGISTERED_PATHS or None)`.
    let max_paths = progress::parse_max_paths(std::env::var("DEGENBOT_MAX_PATHS").ok().as_deref())?;
    driver.set_path_cap(max_paths);
    println!("[registration] path cap = {max_paths:?} (DEGENBOT_MAX_PATHS)");
    degenbot::op_info!(domain = path, path_cap = ?max_paths, "registration: path cap bound");
    // Attach the result consumer BEFORE resume — the BotRunner ordering
    // invariant (`BotRunner.run`: create the consumer, THEN resume). The
    // receiver is handed to the G4 consumer task (row 7 + row 16).
    let result_rx = driver
        .take_result_receiver()
        .ok_or_else(|| "EngineDriver result receiver already taken".to_string())?;
    let runtime = degenbot::runtime::get_runtime();
    let outcome = runtime.block_on(async {
        // G4 : the result-batch consumer runs concurrently with
        // the live registration arm; `driver.stop()` below closes the channel
        // so its pending `recv()` sees end-of-stream exactly once (ADR-050 D6).
        // G5 : the consumer beats a session-watch heartbeat per
        // batch and the watch aborts it (`WatchdogTripped`) if the loop stalls.
        let heartbeat = session_watch::Heartbeat::new();
        // RSP-10: the shared progress view the run-loop heartbeat reads.
        let progress = consume::SessionProgress::new();
        let consumer = tokio::spawn(consume::run_result_consumer_watched(
            result_rx,
            Some(heartbeat.clone()),
            Some(progress.clone()),
        ));
        let stall_after = std::time::Duration::from_millis(
            std::env::var("SETTLEMENT_STALL_WATCHDOG_MS")
                .ok()
                .and_then(|raw| raw.parse::<u64>().ok())
                .unwrap_or(120_000),
        );
        let watch_task = tokio::spawn(session_watch::supervise_consumer(
            consumer,
            heartbeat.clone(),
            stall_after,
        ));
        // G5 row 20: the operator Unix-socket channel (`--operator-socket`).
        let operator = match cli.operator_socket.as_deref() {
            Some(path) => {
                let ops: std::sync::Arc<dyn operator_channel::PathOps> =
                    std::sync::Arc::new(operator_channel::PipelinePathOps::new(
                        &discovered,
                        &requested_kinds,
                        &allowed,
                        &params,
                        pipeline.policy.clone(),
                        pipeline.retry_policy,
                        weth_lower.clone(),
                        weth_lower.clone(),
                    ));
                let running = operator_channel::start_operator_server(Path::new(path), ops)?;
                println!("[operator] listening on {path} (G5)");
                Some(running)
            }
            None => None,
        };
        // RSP-14: the V4 registration verify lifecycle needs a `StateView`
        // contract address (`RegistrationLifecycleError::MissingStateView`
        // otherwise). Python passes the chain deployment's address to
        // `EngineRegistry.start(..., verify_state_view=...)`
        // (`runner/bot_runner.py:470`); resolve the same fact from the
        // enumerated `pool_managers` rows (the column the V4 build path
        // already trusts) instead of hard-coding a chain constant.
        let verify_state_view =
            live::resolve_verify_state_view(&discovered).map(|address| format!("{address:#x}"));
        match verify_state_view.as_deref() {
            Some(view) => {
                println!("[registration] V4 verify state view = {view} (from pool_managers)");
                degenbot::op_info!(
                    domain = pump,
                    state_view = view,
                    "registration: V4 verify state view bound"
                );
            }
            None => println!(
                "[registration] no V4 StateView in the enumeration; the V4 verify lifecycle \
                 will refuse MissingStateView"
            ),
        }
        let w = driver
            .start(&http, &ws, verify_state_view.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        let phase_after_start = driver.current_phase();
        // resume is the single gate after which batches flow; the driver owns
        // the S+1..W auto-backfill before it spawns the live loop.
        driver.resume().await.map_err(|e| e.to_string())?;
        let phase_after_resume = driver.current_phase();

        // ── G3 live registration arm (gated by SMOKE_RPC_URL) ──
        // Per-candidate pool build through the core `ConstructionIo`, BotState
        // registration, the ADR-022 verify lifecycle under the tokio claim
        // table + retry dance, then `register_and_solve_path`.
        let provider = degenbot::rpc::provider::AlloyProvider::new(
            &http,
            degenbot::rpc::provider::DEFAULT_MAX_RETRIES,
        )
        .await
        .map_err(|e| format!("live construction provider from {http}: {e}"))?;
        let (live_db, _live_schema) = degenbot::db::DegenbotDb::open(&db_path)
            .map_err(|e| format!("open live construction DB {}: {e}", db_path.display()))?;
        let io = degenbot::bot_core::construction_io::ConstructionIo::new(
            std::sync::Arc::new(
                degenbot::bot_core::construction_io::DegenbotDbConstruction::new(live_db),
            ),
            std::sync::Arc::new(
                degenbot::bot_core::construction_io::AlloyRpcConstruction::new(provider),
            ),
        );
        let live_ctx = live::LiveContext {
            chain_id: CHAIN_ID,
            block: None,
            io: &io,
            db: Some(&snap),
        };
        // RSP-11 (KETJNN): the time-throttled registration-progress summary
        // (DEGENBOT_REG_PROGRESS_SECS, default 30s) keeps the crawl visible
        // even when path_count never crosses a 1000-boundary.
        let progress_secs = progress::parse_progress_secs(
            std::env::var("DEGENBOT_REG_PROGRESS_SECS").ok().as_deref(),
        )?;
        let mut reg_progress = progress::ProgressCadence::new(progress_secs);
        let live_report = live::run_live(
            &driver,
            &built,
            &discovered,
            &mut pipeline,
            &params,
            &weth_lower,
            &weth_lower,
            &live_ctx,
            &mut reg_progress,
        )
        .await;
        println!(
            "[g3-live] build+verify+register: path_count={} skips={} engine_rejects={} \
             register_fails={} v4_hops={} dup={} cap={}",
            live_report.path_count,
            live_report.skip_count,
            live_report.engine_reject_count,
            live_report.register_fail_count,
            live_report.v4_pool_count,
            live_report.dup_count,
            live_report.capped,
        );
        degenbot::op_info!(
            domain = path,
            path_count = live_report.path_count,
            skips = live_report.skip_count,
            engine_rejects = live_report.engine_reject_count,
            register_fails = live_report.register_fail_count,
            "g3: live registration arm complete"
        );

        // ── RSP-10 run-until-shutdown phase  ──
        // Registration is done; mirror `BotRunner.run`'s main loop and hold
        // the session (consumer + watch + operator channel) open until SIGINT
        // or the env-gated bounded observation window. `run_live` ran first,
        // exactly as before — this phase starts after it and is anchored to
        // its own window, independent of how long registration took.
        let max_secs = std::env::var("DEGENBOT_SMOKE_MAX_SECS")
            .ok()
            .filter(|v| !v.is_empty())
            .map(|v| {
                v.parse::<u64>()
                    .map_err(|_| format!("DEGENBOT_SMOKE_MAX_SECS must be an integer, got {v:?}"))
            })
            .transpose()?;
        let run_end = run_loop::run_session_loop(
            &run_loop::RunLoopConfig::new(max_secs),
            &progress,
            async {
                // SIGINT is the BotRunner Ctrl-C path; a handler-install
                // failure (no signal driver) must not abort the session, so
                // the result is deliberately discarded.
                let _ = tokio::signal::ctrl_c().await;
            },
            |hb| {
                println!(
                    "[session] heartbeat ticks={} batches_seen={} blocks_seen={} current_block={}",
                    hb.ticks, hb.batches_seen, hb.blocks_seen, hb.current_block
                );
                degenbot::op_info!(
                    domain = pump,
                    ticks = hb.ticks,
                    batches_seen = hb.batches_seen,
                    blocks_seen = hb.blocks_seen,
                    current_block = hb.current_block,
                    "session heartbeat"
                );
            },
        )
        .await;
        println!("[session] run loop ended: {run_end:?} (max_secs={max_secs:?})");
        degenbot::op_info!(domain = pump, end = ?run_end, "session run loop ended");
        Ok::<_, String>((
            watch_task,
            (w, phase_after_start, phase_after_resume),
            operator,
        ))
    })?;
    let (watch_task, (w, phase_after_start, phase_after_resume), operator) = outcome;
    // ADR-050 D6 teardown, centralized + unit-tested in `run_loop`: stop the
    // pump first (outside the block_on — its join parks on the shared
    // runtime; the result-channel close then lets the consumer observe the
    // single end-of-stream), then close the operator channel, then join the
    // session watch/consumer. The order is never reordered.
    let watch_outcome = run_loop::teardown_session(
        || driver.stop().map_err(|e| e.to_string()),
        || {
            if let Some(operator) = operator {
                runtime.block_on(operator.close());
                println!("[operator] operator channel closed; socket removed");
            }
            Ok(())
        },
        || {
            runtime
                .block_on(watch_task)
                .map_err(|e| format!("session watch join: {e}"))
        },
    )?;
    // G5 row 19: the session-watch verdict. `PumpEnded` is the graceful stop
    // path; the other two verdicts are loud failures.
    match watch_outcome.verdict {
        session_watch::SessionEndVerdict::PumpEnded => {}
        session_watch::SessionEndVerdict::WatchdogTripped => {
            return Err(
                "session watch: consume loop stalled and the watchdog aborted it (set \
                 SETTLEMENT_STALL_WATCHDOG_MS to tune)"
                    .to_string(),
            );
        }
        session_watch::SessionEndVerdict::RegistrationFailed => {
            return Err(format!(
                "session watch: registration failed: {}",
                watch_outcome
                    .registration_error
                    .as_deref()
                    .unwrap_or("unknown registration error")
            ));
        }
    }
    let (consumer_report, consumer_clock) = watch_outcome
        .consumer
        .ok_or_else(|| "session watch lost the consumer output".to_string())?
        .map_err(|e| format!("result consumer join: {e}"))?
        .map_err(|e| format!("result consumer: {e:?}"))?;
    println!(
        "[g4] result consumer: batches={} end_of_stream={} end={:?} clock={}",
        consumer_report.batches,
        consumer_report.end_of_stream,
        consumer_report.end,
        consumer_clock.current_block,
    );
    degenbot::op_info!(
        domain = pump,
        batches = consumer_report.batches,
        end_of_stream = consumer_report.end_of_stream,
        current_block = consumer_clock.current_block,
        "result consumer report"
    );
    println!(
        "[engine] EngineDriver handshake OK: W={w} phase_after_start={phase_after_start:?} \
         phase_after_resume={phase_after_resume:?}"
    );
    degenbot::op_info!(
        domain = pump,
        w,
        phase_after_start = ?phase_after_start,
        phase_after_resume = ?phase_after_resume,
        "engine handshake complete"
    );
    Ok(())
}

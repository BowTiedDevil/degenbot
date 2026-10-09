//! Integration tests for the `aave` command arms .
//!
//! Covers the active-market walk (`aave update`), the market row flips
//! (`activate`/`deactivate`), and the market/user scalar reads behind
//! `aave position show` — all against a temp DB, offline.
#![expect(clippy::unwrap_used, clippy::panic)]

use std::cell::RefCell;
use std::collections::BTreeMap;
// The `abi_string` helper writes hex through `fmt::Write`; `io::Write` (the
// stub's response body) is imported by name, so this one rides anonymously.
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use alloy::primitives::Address;
use degenbot_cli_core::{
    resolve_aave_deployment, run, AaveCommand, AaveReport, AaveUpdateOutcome, CliContext, CliError,
    Command, DeactivateOutcome, ExitCode, PromptPlan, Prompter,
};
use degenbot_config::MapEnv;
use degenbot_db::ops;
use tempfile::TempDir;

struct NoPrompt {
    calls: RefCell<usize>,
}

impl NoPrompt {
    fn new() -> Self {
        Self {
            calls: RefCell::new(0),
        }
    }
}

impl Prompter for NoPrompt {
    fn confirm(&self, _message: &str, _default: bool) -> bool {
        *self.calls.borrow_mut() += 1;
        false
    }
}

fn write_db(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("degenbot.db");
    ops::create_new_database(&path).unwrap();
    path
}

fn env_with_rpc_url(url: &str) -> MapEnv {
    let mut map = BTreeMap::new();
    map.insert("DEGENBOT_RPC_HTTP_CHAINID_1".to_string(), url.to_string());
    MapEnv::new(map)
}

fn env_with_rpc() -> MapEnv {
    env_with_rpc_url("http://127.0.0.1:1")
}

fn seed_market(path: &Path, id: i64, name: &str, active: bool, last_update_block: Option<i64>) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute(
        "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
         VALUES (?1, 1, ?2, ?3, ?4)",
        rusqlite::params![id, name, active, last_update_block],
    )
    .unwrap();
}

const USER_ADDRESS: &str = "0x1111111111111111111111111111111111111111";

fn checksum(raw: &str) -> String {
    raw.parse::<Address>().unwrap().to_checksum(None)
}

fn seed_user(path: &Path, id: i64, market_id: i64) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute(
        "INSERT INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, \
         stk_aave_balance, isolation_mode_collateral_asset_id, isolation_mode_debt) \
         VALUES (?1, ?2, ?3, 0, 0, NULL, NULL, '0')",
        rusqlite::params![id, market_id, checksum(USER_ADDRESS)],
    )
    .unwrap();
}

fn seed_positions(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute(
        "INSERT INTO erc20_tokens (id, chain, address, name, symbol, decimals) \
         VALUES (1, 1, '0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48', 'Usd Coin', 'USDC', 6)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, a_token_id, \
         a_token_revision, v_token_id, v_token_revision, e_mode_category_id, price_source, \
         last_update_block, liquidity_index, liquidity_rate, borrow_index, borrow_rate) \
         VALUES (1, 1, 1, 1, 0, 1, 0, NULL, NULL, NULL, '1', '0', '1', '0')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO aave_v3_collateral_positions (id, user_id, asset_id, balance, last_index) \
         VALUES (1, 1, 1, '1000', '1')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO aave_v3_debt_positions (id, user_id, asset_id, balance, last_index) \
         VALUES (1, 1, 1, '500', '1')",
        [],
    )
    .unwrap();
}

fn run_aave(command: AaveCommand, path: &Path, e: &MapEnv) -> degenbot_cli_core::CommandOutcome {
    let ctx = CliContext::new(e).with_database(path.display().to_string());
    let prompter = NoPrompt::new();
    run(&Command::Aave(command), &ctx, &prompter)
}

// ── a local activation-RPC stub ───────────────────────────────────────────

/// The checksummed address provider `resolve_aave_deployment(1)` names.
const POOL_ADDRESS_PROVIDER: &str = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";

/// The chain's GHO token address.
const GHO_TOKEN: &str = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";

/// The head block the activation metadata reads pin.
const HEAD_BLOCK: u64 = 16_291_071;

/// The no-arg selectors the activation RPC issues.
const GET_MARKET_ID_SELECTOR: &str = "0x568ef470";
const NAME_SELECTOR: &str = "0x06fdde03";
const SYMBOL_SELECTOR: &str = "0x95d89b41";
const DECIMALS_SELECTOR: &str = "0x313ce567";

/// ABI-encode a dynamic `string` return: the 0x20 offset word, the byte-length
/// word, then the bytes right-padded to a 32-byte multiple. Mirrors alloy's
/// `eth_abi` decode the module reads back.
fn abi_string(value: &str) -> String {
    let bytes = value.as_bytes();
    let len = bytes.len();
    let mut padded = bytes.to_vec();
    padded.resize(len + (32 - len % 32) % 32, 0);
    let mut out = format!("0x{:064x}{len:064x}", 32);
    for byte in &padded {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// A minimal one-request-per-connection JSON-RPC stub serving the activation
/// metadata the reset module reads through the CLI's provider: the head block,
/// `getMarketId()` naming `market_name`, and the GHO token's `name()` /
/// `symbol()` / `decimals()`. Every other call is a `-32601` refusal.
struct ResetRpcStub {
    url: String,
    shutdown: Arc<AtomicBool>,
}

impl ResetRpcStub {
    fn spawn(market_name: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let name = market_name.to_string();
        listener.set_nonblocking(true).unwrap();
        thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        let _ = answer_activation(&mut stream, &name);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self { url, shutdown }
    }
}

impl Drop for ResetRpcStub {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

fn answer_activation(stream: &mut TcpStream, market_name: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..]);
    let request: serde_json::Value =
        serde_json::from_str(body.trim()).unwrap_or(serde_json::Value::Null);
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or("");
    let result = match method {
        "eth_blockNumber" => Some(serde_json::json!(format!("0x{HEAD_BLOCK:x}"))),
        "eth_call" => {
            let call = &request["params"][0];
            let to = call["to"].as_str().unwrap_or("").to_ascii_lowercase();
            let input = call
                .get("input")
                .or_else(|| call.get("data"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            if to == POOL_ADDRESS_PROVIDER.to_ascii_lowercase()
                && input.starts_with(GET_MARKET_ID_SELECTOR)
            {
                Some(serde_json::json!(abi_string(market_name)))
            } else if to == GHO_TOKEN.to_ascii_lowercase() {
                if input.starts_with(NAME_SELECTOR) {
                    Some(serde_json::json!(abi_string("Gho Token")))
                } else if input.starts_with(SYMBOL_SELECTOR) {
                    Some(serde_json::json!(abi_string("GHO")))
                } else if input.starts_with(DECIMALS_SELECTOR) {
                    Some(serde_json::json!(format!("0x{:064x}", 18)))
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    };
    let response = match result {
        Some(result) => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
        None => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": "method not found"},
        }),
    };
    write_rpc_response(stream, &response)
}

fn write_rpc_response(stream: &mut TcpStream, response: &serde_json::Value) -> std::io::Result<()> {
    let body = response.to_string();
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len(),
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

// ── deployment resolution ─────────────────────────────────────────────────

#[test]
fn only_ethereum_mainnet_has_an_aave_deployment() {
    let deployment = resolve_aave_deployment(1).unwrap();
    assert_eq!(
        deployment.pool_address_provider.to_checksum(None),
        "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e"
    );
    assert_eq!(
        deployment.gho_token_address.to_checksum(None),
        "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f"
    );
    assert!(matches!(
        resolve_aave_deployment(8453),
        Err(CliError::UnknownDeployment { .. })
    ));
}

// ── deactivate ────────────────────────────────────────────────────────────

#[test]
fn deactivate_without_a_market_reports_no_entry() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let e = MapEnv::new(BTreeMap::new());
    let outcome = run_aave(
        AaveCommand::Deactivate {
            chain_id: 1,
            market_name: "Aave Ethereum Market".to_string(),
        },
        &db,
        &e,
    );
    assert_eq!(outcome.exit_code, ExitCode::Success);
    assert!(matches!(
        outcome.report(),
        Some(degenbot_cli_core::CommandReport::Aave(
            AaveReport::Deactivated {
                outcome: DeactivateOutcome::NoEntry,
                ..
            }
        ))
    ));
}

#[test]
fn deactivate_flips_active_and_reports_already_inactive_silently() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    seed_market(&db, 1, "Aave Ethereum Market", true, Some(100));
    let e = MapEnv::new(BTreeMap::new());
    let outcome = run_aave(
        AaveCommand::Deactivate {
            chain_id: 1,
            market_name: "Aave Ethereum Market".to_string(),
        },
        &db,
        &e,
    );
    assert_eq!(outcome.exit_code, ExitCode::Success);
    assert!(matches!(
        outcome.report(),
        Some(degenbot_cli_core::CommandReport::Aave(
            AaveReport::Deactivated {
                outcome: DeactivateOutcome::Deactivated,
                ..
            }
        ))
    ));
    let conn = rusqlite::Connection::open(&db).unwrap();
    let active: bool = conn
        .query_row("SELECT active FROM aave_v3_markets WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(!active);

    let again = run_aave(
        AaveCommand::Deactivate {
            chain_id: 1,
            market_name: "Aave Ethereum Market".to_string(),
        },
        &db,
        &e,
    );
    let report = again.report().unwrap();
    assert!(matches!(
        report,
        degenbot_cli_core::CommandReport::Aave(AaveReport::Deactivated {
            outcome: DeactivateOutcome::AlreadyDeactivated,
            ..
        })
    ));
    assert!(
        report.render_lines().is_empty(),
        "already-inactive is silent"
    );
}

// ── position show ─────────────────────────────────────────────────────────

#[test]
fn position_show_rejects_an_invalid_address() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let e = MapEnv::new(BTreeMap::new());
    let outcome = run_aave(
        AaveCommand::PositionShow {
            address: "nope".to_string(),
            market: "Aave Ethereum Market".to_string(),
            chain_id: 1,
        },
        &db,
        &e,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert_eq!(outcome.error().unwrap().message(), "Invalid address: nope");
}

#[test]
fn position_show_reports_missing_market_and_missing_user() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let e = MapEnv::new(BTreeMap::new());
    let no_market = run_aave(
        AaveCommand::PositionShow {
            address: USER_ADDRESS.to_string(),
            market: "Aave Ethereum Market".to_string(),
            chain_id: 1,
        },
        &db,
        &e,
    );
    assert!(matches!(
        no_market.report(),
        Some(degenbot_cli_core::CommandReport::Aave(
            AaveReport::PositionNoMarket { .. }
        ))
    ));

    seed_market(&db, 1, "Aave Ethereum Market", true, Some(100));
    let no_user = run_aave(
        AaveCommand::PositionShow {
            address: USER_ADDRESS.to_string(),
            market: "Aave Ethereum Market".to_string(),
            chain_id: 1,
        },
        &db,
        &e,
    );
    assert!(matches!(
        no_user.report(),
        Some(degenbot_cli_core::CommandReport::Aave(
            AaveReport::PositionNoUser { .. }
        ))
    ));
}

#[test]
fn position_show_reads_the_scalar_market_user_and_position_rows() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    seed_market(&db, 1, "Aave Ethereum Market", true, Some(100));
    seed_user(&db, 1, 1);
    seed_positions(&db);
    let e = MapEnv::new(BTreeMap::new());
    let outcome = run_aave(
        AaveCommand::PositionShow {
            address: USER_ADDRESS.to_string(),
            market: "Aave Ethereum Market".to_string(),
            chain_id: 1,
        },
        &db,
        &e,
    );
    assert_eq!(outcome.exit_code, ExitCode::Success);
    let Some(degenbot_cli_core::CommandReport::Aave(AaveReport::Position {
        user_address,
        collateral,
        debt,
        ..
    })) = outcome.report()
    else {
        panic!("expected Position, got {:?}", outcome.report());
    };
    assert_eq!(user_address, &checksum(USER_ADDRESS));
    assert_eq!(collateral.len(), 1);
    assert_eq!(collateral[0].symbol, "USDC");
    assert_eq!(collateral[0].balance.to_string(), "1000");
    assert_eq!(debt.len(), 1);
    assert_eq!(debt[0].balance.to_string(), "500");
}

// ── update ────────────────────────────────────────────────────────────────

#[test]
fn update_without_active_markets_is_a_typed_failure() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let e = env_with_rpc();
    let outcome = run_aave(update_command("latest", false), &db, &e);
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(
        outcome.error(),
        Some(CliError::NoActiveAaveMarkets)
    ));
}

#[test]
fn update_dry_run_previews_without_a_core_call() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    seed_market(&db, 1, "Aave Ethereum Market", true, Some(100));
    let e = env_with_rpc();
    let outcome = run_aave(update_command("latest", true), &db, &e);
    assert_eq!(outcome.exit_code, ExitCode::Success);
    let Some(degenbot_cli_core::CommandReport::Aave(AaveReport::Updated { entries })) =
        outcome.report()
    else {
        panic!("expected Updated, got {:?}", outcome.report());
    };
    assert_eq!(entries.len(), 1);
    assert!(matches!(
        entries[0].outcome,
        AaveUpdateOutcome::DryRun {
            last_update_block: 100,
            to_block: None
        }
    ));
    assert!(entries[0].market_name.contains("Aave Ethereum Market"));
}

#[test]
fn update_skips_a_market_needing_bootstrap() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    seed_market(&db, 1, "Aave Ethereum Market", true, None);
    let e = env_with_rpc();
    let outcome = run_aave(update_command("latest", false), &db, &e);
    assert_eq!(outcome.exit_code, ExitCode::Success);
    let Some(degenbot_cli_core::CommandReport::Aave(AaveReport::Updated { entries })) =
        outcome.report()
    else {
        panic!("expected Updated, got {:?}", outcome.report());
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].outcome, AaveUpdateOutcome::NeedsBootstrap);
}

// ── reset ─────────────────────────────────────────────────────────────────

const UNKNOWN_MARKET: &str = "No Such Aave Market";

#[test]
fn reset_refuses_an_unknown_market_without_touching_data() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    seed_market(&db, 1, "Aave Ethereum Market", true, Some(100));
    let e = MapEnv::new(BTreeMap::new());
    let outcome = run_aave(
        AaveCommand::Reset {
            chain_id: 1,
            market_name: Some(UNKNOWN_MARKET.to_string()),
            dry_run: true,
        },
        &db,
        &e,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(
        outcome.error(),
        Some(CliError::UnknownAaveMarket { .. })
    ));
}

#[test]
fn reset_dry_run_prints_the_plan_without_deleting() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    seed_market(&db, 1, "Aave Ethereum Market", true, Some(100));
    seed_user(&db, 1, 1);
    seed_positions(&db);
    let e = MapEnv::new(BTreeMap::new());
    let outcome = run_aave(
        AaveCommand::Reset {
            chain_id: 1,
            market_name: None,
            dry_run: true,
        },
        &db,
        &e,
    );
    assert_eq!(outcome.exit_code, ExitCode::Success);
    let Some(degenbot_cli_core::CommandReport::Aave(AaveReport::Reset {
        market_id,
        counts,
        dry_run,
        reinit,
        ..
    })) = outcome.report()
    else {
        panic!("expected Reset, got {:?}", outcome.report());
    };
    assert_eq!(*market_id, 1);
    assert!(*dry_run);
    assert!(reinit.is_none(), "a preview never re-initializes");
    let by_table = |t: &str| counts.iter().find(|c| c.table == t).unwrap().rows;
    assert_eq!(by_table("aave_v3_collateral_positions"), 1);
    assert_eq!(by_table("aave_v3_debt_positions"), 1);
    assert_eq!(by_table("aave_v3_users"), 1);
    // Nothing was deleted.
    let conn = rusqlite::Connection::open(&db).unwrap();
    let users: i64 = conn
        .query_row("SELECT COUNT(*) FROM aave_v3_users", [], |r| r.get(0))
        .unwrap();
    assert_eq!(users, 1);
    // The plan renders the counts on stdout.
    let lines = outcome.report().unwrap().render_lines();
    assert!(
        lines.iter().any(|l| l.contains("aave_v3_users: 1 row(s)")),
        "{lines:?}"
    );
    assert!(lines.iter().any(|l| l.contains("Dry run")), "{lines:?}");
}

#[test]
fn reset_reinits_through_the_activate_seam_without_an_update() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    seed_market(&db, 1, "Aave Ethereum Market", true, Some(26_130_445));
    seed_user(&db, 1, 1);
    seed_positions(&db);
    // The module's re-seed reads the on-chain market name through the
    // activation RPC, and the arm builds its provider from the configured RPC
    // URL — so the stub must answer it (a name that keyed another row would be
    // refused, and a dead endpoint would fail the arm).
    let stub = ResetRpcStub::spawn("Aave Ethereum Market");
    let e = env_with_rpc_url(&stub.url);
    let outcome = run_aave(
        AaveCommand::Reset {
            chain_id: 1,
            market_name: None,
            dry_run: false,
        },
        &db,
        &e,
    );
    assert_eq!(outcome.exit_code, ExitCode::Success);
    let Some(degenbot_cli_core::CommandReport::Aave(AaveReport::Reset {
        market_id,
        counts,
        dry_run,
        reinit,
        ..
    })) = outcome.report()
    else {
        panic!("expected Reset, got {:?}", outcome.report());
    };
    assert_eq!(*market_id, 1);
    assert!(!*dry_run);
    let reinit = reinit.as_ref().unwrap();
    assert!(reinit.market_activated);
    assert!(reinit.contract_row_present);
    assert_eq!(
        reinit.last_update_block,
        Some(16_291_070),
        "the cursor sits at the bootstrap block, not the pre-reset cursor"
    );
    // The purge removed every populated row.
    let by_table = |t: &str| counts.iter().find(|c| c.table == t).unwrap().rows;
    assert_eq!(by_table("aave_v3_collateral_positions"), 1);
    assert_eq!(by_table("aave_v3_debt_positions"), 1);
    assert_eq!(by_table("aave_v3_users"), 1);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let users: i64 = conn
        .query_row("SELECT COUNT(*) FROM aave_v3_users", [], |r| r.get(0))
        .unwrap();
    assert_eq!(users, 0, "the reset purged the users");
    let active: i64 = conn
        .query_row("SELECT active FROM aave_v3_markets WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(active, 1, "the re-init re-activated the market row");
    let stamp: Option<i64> = conn
        .query_row(
            "SELECT last_update_block FROM aave_v3_markets WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stamp, Some(16_291_070));
    let ap_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM aave_v3_contracts \
             WHERE market_id = 1 AND name = 'POOL_ADDRESS_PROVIDER'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ap_rows, 1, "the re-init ensured the address-provider row");
    // The report renders the activate-seam outcome, not an update entry.
    let lines = outcome.report().unwrap().render_lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("re-initialized via the activate seam")),
        "{lines:?}"
    );
}

fn update_command(to_block: &str, dry_run: bool) -> AaveCommand {
    AaveCommand::Update {
        chunk_size: 10_000,
        to_block: to_block.to_string(),
        verify_chunk: true,
        verify_all: false,
        verify_all_interval: 1_000_000,
        stop_after_one_chunk: false,
        dry_run,
        enable_backup: false,
    }
}

#[test]
fn prompt_plans_are_none_and_exit_codes_are_ported() {
    let e = MapEnv::new(BTreeMap::new());
    let ctx = CliContext::new(&e);
    assert_eq!(
        AaveCommand::PositionShow {
            address: "x".to_string(),
            market: "m".to_string(),
            chain_id: 1,
        }
        .prompt_plan(&ctx),
        PromptPlan::None
    );
    assert_eq!(
        AaveCommand::Activate { chain_id: 1 }.prompt_plan(&ctx),
        PromptPlan::None
    );
    assert_eq!(
        ExitCode::from(&CliError::NoActiveAaveMarkets),
        ExitCode::Failure
    );
}

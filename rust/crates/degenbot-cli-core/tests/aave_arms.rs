//! Integration tests for the `aave` command arms .
//!
//! Covers the active-market walk (`aave update`), the market row flips
//! (`activate`/`deactivate`), and the market/user scalar reads behind
//! `aave position show` — all against a temp DB, offline.
#![expect(clippy::unwrap_used, clippy::panic)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

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

fn env_with_rpc() -> MapEnv {
    let mut map = BTreeMap::new();
    map.insert(
        "DEGENBOT_RPC_HTTP_CHAINID_1".to_string(),
        "http://127.0.0.1:1".to_string(),
    );
    MapEnv::new(map)
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

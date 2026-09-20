//! Integration tests for the supported-exchange / supported-market auto-
//! registration: the console no longer requires the operator to CREATE rows.
//! Every supported (chain, DEX) pair + Aave market that is not found in the
//! DB is registered **inactive** at the write-open seams (fresh `database
//! reset`, `pool update`, `aave update`); activation is then a pure flag flip.

#![expect(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::Path;

use degenbot_cli_core::{
    ensure_supported_registrations, run, Command, DatabaseCommand, DatabaseReport,
    ExchangeCommand,
};
use degenbot_config::MapEnv;
use tempfile::TempDir;

struct NoPrompt;

impl degenbot_cli_core::Prompter for NoPrompt {
    fn confirm(&self, _message: &str, _default: bool) -> bool {
        false
    }
}

fn env() -> MapEnv {
    MapEnv::new(BTreeMap::new())
}

fn run_cmd(command: Command, path: &Path) -> degenbot_cli_core::CommandOutcome {
    run(
        &command,
        &degenbot_cli_core::CliContext::new(&env()).with_database(path.display().to_string()),
        &NoPrompt,
    )
}

fn exchange_row(path: &Path, chain_id: i64, name: &str) -> Option<(bool,)> {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(
        "SELECT active FROM exchanges WHERE chain_id = ?1 AND name = ?2",
        rusqlite::params![chain_id, name],
        |r| Ok((r.get(0)?,)),
    )
    .ok()
}

fn aave_row(path: &Path, chain_id: i64, name: &str) -> Option<(bool, Option<i64>)> {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(
        "SELECT active, last_update_block FROM aave_v3_markets WHERE chain_id = ?1 AND name = ?2",
        rusqlite::params![chain_id, name],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

fn exchange_count(path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row("SELECT COUNT(*) FROM exchanges", [], |r| r.get(0))
        .unwrap()
}

fn pool_manager_count(path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row("SELECT COUNT(*) FROM pool_managers", [], |r| r.get(0))
        .unwrap()
}

const RETIRED_PAIRS: usize = 17;

#[test]
fn database_reset_registers_supported_exchanges_and_market_inactive() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("degenbot.db");

    let outcome = run_cmd(Command::Database(DatabaseCommand::Reset { force: true }), &db);
    assert_eq!(outcome.exit_code, degenbot_cli_core::ExitCode::Success);
    assert!(matches!(
        outcome.report(),
        Some(degenbot_cli_core::CommandReport::Database(
            DatabaseReport::Reset { .. }
        ))
    ));

    // Every supported exchange pair is registered inactive...
    assert_eq!(
        exchange_count(&db),
        RETIRED_PAIRS as i64,
        "all 17 supported (chain, dex) pairs registered"
    );
    for (chain, name) in [
        (8453, "aerodrome_v2"),
        (1, "uniswap_v4"),
        (1, "pancakeswap_v3"),
    ] {
        let (active,) = exchange_row(&db, chain, name).expect("registered row");
        assert!(!active, "{name} must register inactive");
    }
    // ...including the V4 pool manager singletons.
    assert_eq!(pool_manager_count(&db), 2, "both V4 singletons registered");

    // The supported Aave market is registered inactive + unstamped (the
    // bootstrap stamp arrives with `aave activate`).
    let (active, stamp) = aave_row(&db, 1, "Aave Ethereum Market").unwrap();
    assert!(!active);
    assert_eq!(stamp, None, "bare registration carries no bootstrap stamp");

    // `exchange list` now reads them as inactive (not "not in database").
    let outcome = run_cmd(
        Command::Exchange(ExchangeCommand::List { chain: None }),
        &db,
    );
    let lines = outcome.report().unwrap().render_lines();
    assert!(lines
        .iter()
        .any(|l| l == "Aerodrome V2 on Base (chain ID 8453): inactive"));
    assert!(!lines.iter().any(|l| l.contains("not in database")));
}

#[test]
fn ensure_is_idempotent_and_activation_flips_without_creating() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("degenbot.db");

    // The ensure seam is callable directly (the update arms call it too).
    let first = ensure_supported_registrations(&db).unwrap();
    assert_eq!(first.exchanges, RETIRED_PAIRS);
    assert_eq!(first.aave_markets, 1);
    let second = ensure_supported_registrations(&db).unwrap();
    assert_eq!(second.exchanges, 0, "idempotent: nothing new registered");
    assert_eq!(second.aave_markets, 0, "idempotent: nothing new registered");

    // Activation on a pre-registered row is a pure flag flip.
    let outcome = run_cmd(
        Command::Exchange(ExchangeCommand::Activate {
            chain: "ethereum".to_string(),
            name: "uniswap_v2".to_string(),
        }),
        &db,
    );
    assert_eq!(outcome.exit_code, degenbot_cli_core::ExitCode::Success);
    let (active,) = exchange_row(&db, 1, "uniswap_v2").unwrap();
    assert!(active);
    assert_eq!(exchange_count(&db), RETIRED_PAIRS as i64, "no duplicate rows");
}

#[test]
fn registered_bare_market_has_no_contract_substrate() {
    // The auto-registered aave row is BARE: no POOL_ADDRESS_PROVIDER contract
    // row, no GHO rows, no bootstrap stamp. `aave activate` must COMPLETE it.
    // (The RPC-free substrate proof lives in the degenbot-aave activate
    // tests; this asserts the registration state end to end.)
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("degenbot.db");
    ensure_supported_registrations(&db).unwrap();

    let conn = rusqlite::Connection::open(&db).unwrap();
    let market_id: i64 = conn
        .query_row(
            "SELECT id FROM aave_v3_markets WHERE chain_id = 1 AND name = 'Aave Ethereum Market'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let contracts: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM aave_v3_contracts WHERE market_id = ?1",
            rusqlite::params![market_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        contracts, 0,
        "the bare registration carries no contract substrate"
    );
}

//! Integration tests for the `exchange` command arms (ergo FTVJ6L).
//!
//! Covers the data-driven collapse of the 34 Python click verbs: the coverage
//! test pins every retired `(chain, dex)` pair to a shipped registry record (the
//! Uniswap V4 singletons excepted, documented), and the arm tests exercise the
//! get-or-create / active-flip / V4 manager upsert against a temp DB.
#![expect(clippy::unwrap_used)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use degenbot_cli_core::{
    resolve_deployment, run, CliContext, CliError, Command, CommandReport, DeactivateOutcome,
    ExchangeCommand, ExchangeReport, ExitCode, PromptPlan, Prompter, RETIRED_EXCHANGES,
};
use degenbot_config::MapEnv;
use degenbot_db::ops;
use degenbot_uniswap::deployments;
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

fn env() -> MapEnv {
    MapEnv::new(BTreeMap::new())
}

fn write_db(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("degenbot.db");
    ops::create_new_database(&path).unwrap();
    path
}

fn run_exchange(
    command: ExchangeCommand,
    path: &Path,
) -> (degenbot_cli_core::CommandOutcome, NoPrompt) {
    let e = env();
    let ctx = CliContext::new(&e).with_database(path.display().to_string());
    let prompter = NoPrompt::new();
    let outcome = run(&Command::Exchange(command), &ctx, &prompter);
    (outcome, prompter)
}

fn exchange_row(path: &Path, chain_id: i64, name: &str) -> Option<(bool, String, Option<String>)> {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(
        "SELECT active, factory, deployer FROM exchanges WHERE chain_id = ?1 AND name = ?2",
        rusqlite::params![chain_id, name],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .ok()
}

fn pool_manager_count(path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row("SELECT COUNT(*) FROM pool_managers", [], |r| r.get(0))
        .unwrap()
}

// ── coverage: every Python-era verb resolves ──────────────────────────────

#[test]
fn every_retired_verb_pair_resolves_to_a_deployment() {
    // 17 command chains (10 Base + 7 Ethereum), each with activate + deactivate
    // handlers = the 34 retired Python verbs.
    assert_eq!(RETIRED_EXCHANGES.len(), 17);
    for entry in RETIRED_EXCHANGES {
        let resolved = resolve_deployment(entry.chain_id, entry.dex_slug).unwrap();
        assert_eq!(resolved.chain_id, entry.chain_id);
        assert_eq!(resolved.dex_slug, entry.dex_slug);
        assert_eq!(resolved.display_name, entry.display_name);
        match entry.source {
            degenbot_cli_core::exchange::DeploymentSource::Registry { factory } => {
                let record = deployments::lookup(entry.chain_id, factory).unwrap();
                assert_eq!(resolved.factory, record.factory);
                assert_eq!(resolved.deployer, record.deployer);
                assert!(resolved.pool_manager.is_none());
            }
            degenbot_cli_core::exchange::DeploymentSource::V4Singleton { .. } => {
                assert_eq!(resolved.dex_slug, "uniswap_v4");
                assert!(resolved.pool_manager.is_some());
            }
        }
    }
}

#[test]
fn only_the_uniswap_v4_singletons_lack_a_registry_record() {
    // The V4 pool_manager/state_view are the only literals without a factory
    // path in the registry (documented in the Python deployments module); every
    // other retired verb is registry-backed.
    let non_registry: Vec<(&str, &str)> = RETIRED_EXCHANGES
        .iter()
        .filter(|e| {
            !matches!(
                e.source,
                degenbot_cli_core::exchange::DeploymentSource::Registry { .. }
            )
        })
        .map(|e| (e.chain_slug, e.dex_slug))
        .collect();
    assert_eq!(
        non_registry,
        vec![("base", "uniswap_v4"), ("ethereum", "uniswap_v4")],
        "only the Uniswap V4 singletons may bypass the registry"
    );
}

#[test]
fn chain_selector_parses_slugs_and_ids() {
    assert_eq!(
        degenbot_cli_core::resolve_chain_selector("base").unwrap(),
        8453
    );
    assert_eq!(
        degenbot_cli_core::resolve_chain_selector("ethereum").unwrap(),
        1
    );
    assert_eq!(degenbot_cli_core::resolve_chain_selector("eth").unwrap(), 1);
    assert_eq!(degenbot_cli_core::resolve_chain_selector("1").unwrap(), 1);
    assert!(matches!(
        degenbot_cli_core::resolve_chain_selector("dogechain"),
        Err(CliError::UnknownChain { .. })
    ));
}

// ── activate / deactivate ─────────────────────────────────────────────────

#[test]
fn activate_creates_row_active_true_with_registry_identity() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let (outcome, prompter) = run_exchange(
        ExchangeCommand::Activate {
            chain: "base".to_string(),
            name: "aerodrome_v2".to_string(),
        },
        &db,
    );
    assert_eq!(outcome.exit_code, ExitCode::Success);
    assert_eq!(*prompter.calls.borrow(), 0, "exchange arms never prompt");
    assert!(matches!(
        outcome.report(),
        Some(CommandReport::Exchange(ExchangeReport::Activated {
            outcome: degenbot_cli_core::ActivateOutcome::Activated,
            ..
        }))
    ));
    let (active, factory, deployer) = exchange_row(&db, 8453, "aerodrome_v2").unwrap();
    assert!(active);
    assert_eq!(factory, "0x420DD381b31aEf6683db6B902084cB0FFECe40Da");
    assert_eq!(deployer, None);
    assert_eq!(
        pool_manager_count(&db),
        0,
        "no V4 manager for a V2 exchange"
    );
}

#[test]
fn activate_is_idempotent_and_reports_already_active() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let command = ExchangeCommand::Activate {
        chain: "base".to_string(),
        name: "uniswap_v3".to_string(),
    };
    let first = run_exchange(command.clone(), &db).0;
    assert_eq!(first.exit_code, ExitCode::Success);
    let second = run_exchange(command, &db).0;
    assert_eq!(second.exit_code, ExitCode::Success);
    assert!(matches!(
        second.report(),
        Some(CommandReport::Exchange(ExchangeReport::Activated {
            outcome: degenbot_cli_core::ActivateOutcome::AlreadyActive,
            ..
        }))
    ));
    assert_eq!(
        second.report().unwrap().render_lines(),
        vec!["Exchange is already activated.".to_string()]
    );
}

#[test]
fn deactivate_flips_and_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let activate = ExchangeCommand::Activate {
        chain: "base".to_string(),
        name: "sushiswap_v2".to_string(),
    };
    assert_eq!(run_exchange(activate, &db).0.exit_code, ExitCode::Success);

    let deactivate = ExchangeCommand::Deactivate {
        chain: "base".to_string(),
        name: "sushiswap_v2".to_string(),
    };
    let first = run_exchange(deactivate.clone(), &db).0;
    assert_eq!(first.exit_code, ExitCode::Success);
    assert!(matches!(
        first.report(),
        Some(CommandReport::Exchange(ExchangeReport::Deactivated {
            outcome: DeactivateOutcome::Deactivated,
            ..
        }))
    ));
    let (active, _, _) = exchange_row(&db, 8453, "sushiswap_v2").unwrap();
    assert!(!active);

    let second = run_exchange(deactivate, &db).0;
    assert!(matches!(
        second.report(),
        Some(CommandReport::Exchange(ExchangeReport::Deactivated {
            outcome: DeactivateOutcome::AlreadyDeactivated,
            ..
        }))
    ));
}

#[test]
fn deactivate_without_a_row_reports_no_entry() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let (outcome, _) = run_exchange(
        ExchangeCommand::Deactivate {
            chain: "ethereum".to_string(),
            name: "uniswap_v2".to_string(),
        },
        &db,
    );
    assert_eq!(outcome.exit_code, ExitCode::Success);
    assert!(matches!(
        outcome.report(),
        Some(CommandReport::Exchange(ExchangeReport::Deactivated {
            outcome: DeactivateOutcome::NoEntry,
            ..
        }))
    ));
    assert!(outcome.report().unwrap().render_lines()[0]
        .contains("The database has no entry for Uniswap V2 on Ethereum"));
}

#[test]
fn activate_uniswap_v4_also_upserts_the_pool_manager() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let (outcome, _) = run_exchange(
        ExchangeCommand::Activate {
            chain: "base".to_string(),
            name: "uniswap_v4".to_string(),
        },
        &db,
    );
    assert_eq!(outcome.exit_code, ExitCode::Success);
    assert_eq!(pool_manager_count(&db), 1);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let (kind, state_view): (String, Option<String>) = conn
        .query_row(
            "SELECT kind, state_view FROM pool_managers LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "uniswap_v4");
    assert_eq!(
        state_view.as_deref(),
        Some("0xA3c0c9b65baD0b08107Aa264b0f3dB444b867A71")
    );
}

#[test]
fn unknown_deployment_is_a_typed_failure() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let (outcome, _) = run_exchange(
        ExchangeCommand::Activate {
            chain: "base".to_string(),
            name: "curve_v1".to_string(),
        },
        &db,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(
        outcome.error(),
        Some(CliError::UnknownDeployment { .. })
    ));
}

#[test]
fn prompt_plans_are_none_and_exit_codes_are_ported() {
    let e = env();
    let ctx = CliContext::new(&e);
    assert_eq!(
        ExchangeCommand::Activate {
            chain: "base".to_string(),
            name: "uniswap_v2".to_string()
        }
        .prompt_plan(&ctx),
        PromptPlan::None
    );
    assert_eq!(
        ExchangeCommand::Deactivate {
            chain: "base".to_string(),
            name: "uniswap_v2".to_string()
        }
        .prompt_plan(&ctx),
        PromptPlan::None
    );
    assert_eq!(
        ExitCode::from(&CliError::UnknownChain {
            chain: "x".to_string()
        }),
        ExitCode::Failure
    );
}

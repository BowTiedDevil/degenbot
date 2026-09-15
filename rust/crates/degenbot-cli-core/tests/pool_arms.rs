//! Integration tests for the `pool` command arms (ergo FTVJ6L).
//!
//! The `--to-block` semantics are unit-tested purely (no RPC); the arm tests
//! cover the flag parse, the pre-RPC refusals, and the prompt/exit-code
//! surfaces.
#![expect(clippy::unwrap_used, clippy::panic)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use degenbot_cli_core::{
    parse_to_block, resolve_to_block, run, BlockTag, CliContext, CliError, Command, ExitCode,
    PoolCommand, PoolFamily, PromptPlan, Prompter, ToBlockSpec,
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
    map.insert("DEGENBOT_DEFAULT_CHAIN_ID".to_string(), "8453".to_string());
    map.insert(
        "DEGENBOT_RPC_HTTP_CHAINID_8453".to_string(),
        "http://127.0.0.1:1".to_string(),
    );
    MapEnv::new(map)
}

// ── --to-block resolution parity ──────────────────────────────────────────

#[test]
fn to_block_parses_tags_offsets_and_integers() {
    assert_eq!(parse_to_block("latest").unwrap(), ToBlockSpec::Tip);
    assert_eq!(parse_to_block("earliest").unwrap(), ToBlockSpec::Tip);
    assert_eq!(parse_to_block("pending").unwrap(), ToBlockSpec::Tip);
    assert_eq!(parse_to_block("safe").unwrap(), ToBlockSpec::Tip);
    assert_eq!(parse_to_block("finalized").unwrap(), ToBlockSpec::Tip);
    assert_eq!(parse_to_block("100").unwrap(), ToBlockSpec::Number(100));
    assert_eq!(
        parse_to_block("latest:-64").unwrap(),
        ToBlockSpec::TagOffset {
            tag: BlockTag::Latest,
            offset: -64
        }
    );
    assert_eq!(
        parse_to_block("safe:128").unwrap(),
        ToBlockSpec::TagOffset {
            tag: BlockTag::Safe,
            offset: 128
        }
    );
    // A bare tag with an explicit zero offset still means "chain tip".
    assert_eq!(parse_to_block("latest:0").unwrap(), ToBlockSpec::Tip);
    // Python `int(offset)` accepts surrounding whitespace / a sign.
    assert_eq!(
        parse_to_block("latest: 5 ").unwrap(),
        ToBlockSpec::TagOffset {
            tag: BlockTag::Latest,
            offset: 5
        }
    );
}

#[test]
fn to_block_rejects_malformed_identifiers() {
    for raw in ["foo", "foo:1", "latest:x", "0x10", "-5", ":5"] {
        let err = parse_to_block(raw).unwrap_err();
        assert!(
            matches!(err, CliError::InvalidBlockTag(_)),
            "{raw} must be rejected, got {err:?}"
        );
    }
    assert_eq!(
        parse_to_block("foo").unwrap_err().message(),
        "Invalid block tag: foo"
    );
}

#[test]
fn resolve_to_block_passes_numbers_and_tips_through_without_rpc() {
    // No RPC for integers or pure tags.
    assert_eq!(
        resolve_to_block(ToBlockSpec::Number(42), "http://127.0.0.1:1").unwrap(),
        Some(42)
    );
    assert_eq!(
        resolve_to_block(ToBlockSpec::Tip, "http://127.0.0.1:1").unwrap(),
        None
    );
}

// ── pool update arm ───────────────────────────────────────────────────────

#[test]
fn pool_update_rejects_a_malformed_to_block_before_any_rpc() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let e = env_with_rpc();
    let ctx = CliContext::new(&e).with_database(db.display().to_string());
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Pool(PoolCommand::Update {
            chunk_size: 10_000,
            to_block: "bogus".to_string(),
            verify_chunk: true,
            verify_all: false,
            verify_all_interval: 1_000_000,
        }),
        &ctx,
        &prompter,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(
        outcome.error(),
        Some(CliError::InvalidBlockTag(_))
    ));
    assert_eq!(*prompter.calls.borrow(), 0);
}

#[test]
fn pool_update_with_no_active_exchanges_is_a_noop_without_rpc() {
    // `run_pool_update` returns before it builds the provider when no exchange
    // is active, so this exercises the success path offline.
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let e = env_with_rpc();
    let ctx = CliContext::new(&e).with_database(db.display().to_string());
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Pool(PoolCommand::Update {
            chunk_size: 10_000,
            to_block: "latest".to_string(),
            verify_chunk: true,
            verify_all: false,
            verify_all_interval: 1_000_000,
        }),
        &ctx,
        &prompter,
    );
    assert_eq!(outcome.exit_code, ExitCode::Success);
    let Some(degenbot_cli_core::CommandReport::Pool(degenbot_cli_core::PoolReport::Updated {
        chunks_committed,
        ..
    })) = outcome.report()
    else {
        panic!("expected Updated, got {:?}", outcome.report());
    };
    assert_eq!(*chunks_committed, 0);
}

#[test]
fn pool_update_without_a_chain_id_is_a_config_failure() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let e = MapEnv::new(BTreeMap::new());
    let ctx = CliContext::new(&e).with_database(db.display().to_string());
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Pool(PoolCommand::Update {
            chunk_size: 10_000,
            to_block: "latest".to_string(),
            verify_chunk: true,
            verify_all: false,
            verify_all_interval: 1_000_000,
        }),
        &ctx,
        &prompter,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(outcome.error(), Some(CliError::Config(_))));
}

// ── pool verify arm (pre-RPC refusals) ────────────────────────────────────

#[test]
fn pool_verify_requires_a_pool_manager_for_v4() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let e = MapEnv::new(BTreeMap::new());
    let ctx = CliContext::new(&e).with_database(db.display().to_string());
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Pool(PoolCommand::Verify {
            rpc_url: "http://127.0.0.1:1".to_string(),
            chain_id: 8453,
            block_number: 1,
            pool: "0x0000000000000000000000000000000000000000000000000000000000000001".to_string(),
            family: PoolFamily::V4,
            pool_manager: None,
        }),
        &ctx,
        &prompter,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert_eq!(
        outcome.error().unwrap().message(),
        "--pool-manager is required for --family v4 (the PoolManager singleton)."
    );
}

#[test]
fn pool_verify_rejects_an_unparseable_v3_address() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let e = MapEnv::new(BTreeMap::new());
    let ctx = CliContext::new(&e).with_database(db.display().to_string());
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Pool(PoolCommand::Verify {
            rpc_url: "http://127.0.0.1:1".to_string(),
            chain_id: 8453,
            block_number: 1,
            pool: "not-an-address".to_string(),
            family: PoolFamily::V3,
            pool_manager: None,
        }),
        &ctx,
        &prompter,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(outcome.error(), Some(CliError::InvalidAddress(_))));
}

#[test]
fn pool_verify_unknown_v3_pool_is_a_typed_failure() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let e = MapEnv::new(BTreeMap::new());
    let ctx = CliContext::new(&e).with_database(db.display().to_string());
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Pool(PoolCommand::Verify {
            rpc_url: "http://127.0.0.1:1".to_string(),
            chain_id: 8453,
            block_number: 1,
            pool: "0x1111111111111111111111111111111111111111".to_string(),
            family: PoolFamily::V3,
            pool_manager: None,
        }),
        &ctx,
        &prompter,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(
        outcome.error(),
        Some(CliError::InvalidArgument(_))
    ));
}

#[test]
fn prompt_plans_are_none_for_both_pool_arms() {
    let e = MapEnv::new(BTreeMap::new());
    let ctx = CliContext::new(&e);
    assert_eq!(
        PoolCommand::Update {
            chunk_size: 10_000,
            to_block: "latest".to_string(),
            verify_chunk: true,
            verify_all: false,
            verify_all_interval: 1_000_000,
        }
        .prompt_plan(&ctx),
        PromptPlan::None
    );
    assert_eq!(
        PoolCommand::Verify {
            rpc_url: "x".to_string(),
            chain_id: 1,
            block_number: 1,
            pool: "x".to_string(),
            family: PoolFamily::V3,
            pool_manager: None,
        }
        .prompt_plan(&ctx),
        PromptPlan::None
    );
    assert_eq!(
        ExitCode::from(&CliError::InvalidBlockTag("x".to_string())),
        ExitCode::Failure
    );
}

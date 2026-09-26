//! Integration tests for the `pool` command arms .
//!
//! The `--to-block` semantics are unit-tested purely (no RPC); the arm tests
//! cover the flag parse, the pre-RPC refusals, and the prompt/exit-code
//! surfaces.
#![expect(clippy::unwrap_used, clippy::panic)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use degenbot_cli_core::{
    ensure_supported_registrations, parse_to_block, resolve_to_block, run, BlockTag, CliContext,
    CliError, Command, ExitCode, PoolCommand, PoolFamily, PromptPlan, Prompter, ToBlockSpec,
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

// ── pool update failure rendering ─────────────────────────────────────────

// The connection-class failure is injected honestly through the real seam:
// an unparseable RPC URL makes `AlloyProvider::new` return
// `ProviderError::ConnectionFailed` (the variant the transport's
// `BackendGone` maps onto) before any network is touched.
const DROP_ENDPOINT: &str = "http://[::1";

/// Register the supported chain-8453 pairs, activate them, and stage their
/// cursors so all three resume groups exist: the first row is already at the
/// requested target (current), most sit at block 50 (behind), and every third
/// remaining row was never updated. Returns the staged `(name, cursor)` pairs.
fn seed_active_exchanges(db_path: &Path, chain_id: i64) -> Vec<(String, Option<i64>)> {
    ensure_supported_registrations(db_path).unwrap(); // already registered; idempotent
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let rows: Vec<(i64, String)> = {
        let mut statement = conn
            .prepare("SELECT id, name FROM exchanges WHERE chain_id = ?1 ORDER BY id")
            .unwrap();
        let mapped = statement
            .query_map([chain_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        mapped.map(|row| row.unwrap()).collect()
    };
    let mut staged = Vec::with_capacity(rows.len());
    for (index, (id, name)) in rows.iter().enumerate() {
        let cursor = if index == 0 {
            Some(500)
        } else if index % 3 == 2 {
            None
        } else {
            Some(50)
        };
        conn.execute(
            "UPDATE exchanges SET active = 1, last_update_block = ?2 WHERE id = ?1",
            rusqlite::params![id, cursor],
        )
        .unwrap();
        staged.push((name.clone(), cursor));
    }
    staged
}

/// Run `pool update --to-block 100` against the drop endpoint (no network is
/// touched) on a temp database and return the rendered failure message.
fn drop_failure_message(db_path: &Path) -> String {
    let mut map = BTreeMap::new();
    map.insert("DEGENBOT_DEFAULT_CHAIN_ID".to_string(), "8453".to_string());
    map.insert(
        "DEGENBOT_RPC_HTTP_CHAINID_8453".to_string(),
        DROP_ENDPOINT.to_string(),
    );
    let e = MapEnv::new(map);
    let ctx = CliContext::new(&e).with_database(db_path.display().to_string());
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Pool(PoolCommand::Update {
            chunk_size: 100,
            to_block: "100".to_string(),
            verify_chunk: false,
            verify_all: false,
            verify_all_interval: 1_000_000,
        }),
        &ctx,
        &prompter,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(
        matches!(outcome.error(), Some(CliError::PoolUpdate(_))),
        "expected PoolUpdate, got {:?}",
        outcome.error()
    );
    outcome.error().unwrap().message()
}

#[test]
fn pool_update_transport_failure_names_endpoint_chain_and_range() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    let staged = seed_active_exchanges(&db, 8453);
    assert!(!staged.is_empty(), "the fixture must register exchanges");
    let message = drop_failure_message(&db);
    // The message never updated these rows — the fixture's own cursors are
    // still the truth this test pins.
    assert!(
        message.contains(&format!(
            "the RPC connection to {DROP_ENDPOINT} dropped mid-run"
        )),
        "endpoint + drop wording missing: {message}"
    );
    assert!(message.contains("8453"), "chain missing: {message}");
    // The never-updated exchanges put the intended start at block 1.
    assert!(message.contains("blocks 1-100"), "range missing: {message}");
    assert!(
        message.contains("chunks already committed are kept"),
        "{message}"
    );
    assert!(
        message.contains("Rerunning resumes from the recorded per-exchange cursors"),
        "{message}"
    );
    assert!(
        message.contains("underlying error:"),
        "the library text is kept as detail: {message}"
    );
}

#[test]
fn pool_update_failure_reports_per_exchange_resume_state() {
    let dir = TempDir::new().unwrap();
    let db = write_db(dir.path());
    seed_active_exchanges(&db, 8453);
    let message = drop_failure_message(&db);
    // Classify from the same read-only view the failure report uses, so the
    // assertions are independent of the staged fixture's id order.
    let conn = rusqlite::Connection::open(&db).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT name, last_update_block FROM exchanges \
             WHERE chain_id = 8453 AND active = 1 ORDER BY name",
        )
        .unwrap();
    let rows: Vec<(String, Option<i64>)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert!(!rows.is_empty(), "the fixture must register exchanges");
    let current: Vec<String> = rows
        .iter()
        .filter(|(_, cursor)| matches!(cursor, Some(block) if *block >= 100))
        .map(|(name, _)| name.clone())
        .collect();
    let behind: Vec<String> = rows
        .iter()
        .filter(|(_, cursor)| matches!(cursor, Some(block) if *block < 100))
        .map(|(name, cursor)| format!("{name} (block {})", cursor.unwrap()))
        .collect();
    let never: Vec<String> = rows
        .iter()
        .filter(|(_, cursor)| cursor.is_none())
        .map(|(name, _)| name.clone())
        .collect();
    if current.is_empty() {
        assert!(
            !message.contains("current at the requested target"),
            "{message}"
        );
    } else {
        assert!(
            message.contains(&format!(
                "current at the requested target: {}",
                current.join(", ")
            )),
            "{message}"
        );
    }
    assert!(
        message.contains(&format!(
            "behind (last committed block): {}",
            behind.join(", ")
        )),
        "{message}"
    );
    assert!(
        message.contains(&format!("never updated: {}", never.join(", "))),
        "{message}"
    );
}

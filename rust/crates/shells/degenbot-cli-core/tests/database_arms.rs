//! Integration tests for the `database` command arms .
//!
//! Fixture conventions mirror `degenbot-db`'s own tests: temp dirs, never committed
//! fixtures. States exercised: Rust-owned (fresh create), legacy-marker,
//! empty/fresh-standalone, and foreign.
#![expect(clippy::unwrap_used, clippy::panic)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use degenbot_cli_core::{
    database_backup_path, run, CliContext, CliError, Command, CommandOutcome, CommandReport,
    DatabaseCommand, DatabaseReport, DryRunKind, ExitCode, PromptPlan, Prompter,
};
use degenbot_config::MapEnv;
use degenbot_db::ops;
use degenbot_db::SchemaState;
use tempfile::TempDir;

/// A recording `Prompter`: returns `answer` and captures every ask.
struct RecordingPrompter {
    answer: bool,
    calls: RefCell<Vec<(String, bool)>>,
}

impl RecordingPrompter {
    fn new(answer: bool) -> Self {
        Self {
            answer,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(String, bool)> {
        self.calls.borrow().clone()
    }
}

impl Prompter for RecordingPrompter {
    fn confirm(&self, message: &str, default: bool) -> bool {
        self.calls.borrow_mut().push((message.to_string(), default));
        self.answer
    }
}

fn env() -> MapEnv {
    MapEnv::new(BTreeMap::new())
}

/// A Rust-owned DB (fresh create stamps the Rust schema).
fn rust_owned(dir: &Path) -> PathBuf {
    let path = dir.join("degenbot.db");
    ops::create_new_database(&path).unwrap();
    path
}

/// A legacy `alembic_version`-marked DB (the convertible/healable state).
fn legacy(dir: &Path) -> PathBuf {
    let path = rust_owned(dir);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP TABLE _degenbot_db_schema_version;\n\
         CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
         INSERT INTO alembic_version (version_num) VALUES ('e0aaad8ad486');",
    )
    .unwrap();
    drop(conn);
    path
}

/// A foreign `SQLite` file (tables, no legacy history).
fn foreign(dir: &Path) -> PathBuf {
    let path = dir.join("foreign.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE unrelated (x INTEGER);")
        .unwrap();
    drop(conn);
    path
}

/// An empty file (no tables, no legacy history).
fn empty(dir: &Path) -> PathBuf {
    let path = dir.join("empty.db");
    std::fs::write(&path, b"").unwrap();
    path
}

fn run_db(
    command: DatabaseCommand,
    path: &Path,
    prompter: &RecordingPrompter,
    env: &MapEnv,
) -> CommandOutcome {
    let ctx = CliContext::new(env).with_database(path.display().to_string());
    run(&Command::Database(command), &ctx, prompter)
}

fn inspect(path: &Path) -> SchemaState {
    ops::inspect_schema_state(path).unwrap()
}

// ── backup ────────────────────────────────────────────────────────────────

#[test]
fn backup_writes_bak_without_prompting_when_absent() {
    let dir = TempDir::new().unwrap();
    let db = rust_owned(dir.path());
    let prompter = RecordingPrompter::new(false);

    let outcome = run_db(DatabaseCommand::Backup, &db, &prompter, &env());

    assert_eq!(outcome.exit_code, ExitCode::Success);
    assert!(prompter.calls().is_empty(), "no prompt when target absent");
    let report = outcome.report().unwrap();
    let CommandReport::Database(DatabaseReport::BackedUp { source, backup }) = report else {
        panic!("expected BackedUp, got {report:?}");
    };
    assert_eq!(source, &db);
    assert_eq!(backup, &database_backup_path(&db));
    assert!(backup.exists());
    assert!(matches!(inspect(backup), SchemaState::RustOwned { .. }));
}

#[test]
fn backup_prompts_and_replaces_when_target_exists() {
    let dir = TempDir::new().unwrap();
    let db = rust_owned(dir.path());
    let backup = database_backup_path(&db);
    // A stale/garbage pre-existing target the operator must confirm replacing.
    std::fs::write(&backup, b"not a database").unwrap();

    let prompter = RecordingPrompter::new(true);
    let outcome = run_db(DatabaseCommand::Backup, &db, &prompter, &env());

    assert_eq!(outcome.exit_code, ExitCode::Success);
    let calls = prompter.calls();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].0.contains("An existing backup was found at"),
        "ported prompt text, got {}",
        calls[0].0
    );
    assert!(!calls[0].1, "default is false");
    // The garbage was replaced by a real backup.
    assert!(matches!(inspect(&backup), SchemaState::RustOwned { .. }));
}

#[test]
fn backup_declined_aborts_and_leaves_target_untouched() {
    let dir = TempDir::new().unwrap();
    let db = rust_owned(dir.path());
    let backup = database_backup_path(&db);
    std::fs::write(&backup, b"keep me").unwrap();

    let prompter = RecordingPrompter::new(false);
    let outcome = run_db(DatabaseCommand::Backup, &db, &prompter, &env());

    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(outcome.error(), Some(CliError::Aborted)));
    assert_eq!(std::fs::read(&backup).unwrap(), b"keep me");
}

// ── reset ─────────────────────────────────────────────────────────────────

#[test]
fn reset_prompts_unless_force() {
    let dir = TempDir::new().unwrap();
    let db = rust_owned(dir.path());

    let declined = RecordingPrompter::new(false);
    let outcome = run_db(
        DatabaseCommand::Reset { force: false },
        &db,
        &declined,
        &env(),
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(outcome.error(), Some(CliError::Aborted)));
    assert_eq!(declined.calls().len(), 1);
    assert!(db.exists(), "the DB survives a declined reset");

    let forced = RecordingPrompter::new(false);
    let outcome = run_db(DatabaseCommand::Reset { force: true }, &db, &forced, &env());
    assert_eq!(outcome.exit_code, ExitCode::Success);
    assert!(forced.calls().is_empty(), "--force never prompts");
    assert!(matches!(
        outcome.report(),
        Some(CommandReport::Database(DatabaseReport::Reset { .. }))
    ));
    assert!(matches!(inspect(&db), SchemaState::RustOwned { .. }));
}

#[test]
fn reset_creates_missing_parent_directories() {
    // The default DB state home (~/.local/state/degenbot/db/) does not exist
    // on a fresh install; reset must bootstrap the chain, not fail on the
    // open. (A rogue-session recovery ran into exactly this.)
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("missing/nested/dir/degenbot.db");

    let outcome = run_db(
        DatabaseCommand::Reset { force: true },
        &db,
        &RecordingPrompter::new(false),
        &env(),
    );

    assert_eq!(outcome.exit_code, ExitCode::Success);
    assert!(matches!(inspect(&db), SchemaState::RustOwned { .. }));
}

// ── compact ───────────────────────────────────────────────────────────────

#[test]
fn compact_succeeds_without_prompting() {
    let dir = TempDir::new().unwrap();
    let db = rust_owned(dir.path());
    let prompter = RecordingPrompter::new(false);

    let outcome = run_db(DatabaseCommand::Compact, &db, &prompter, &env());

    assert_eq!(outcome.exit_code, ExitCode::Success);
    assert!(prompter.calls().is_empty());
    assert!(matches!(
        outcome.report(),
        Some(CommandReport::Database(DatabaseReport::Compacted { .. }))
    ));
}

// ── inspect ───────────────────────────────────────────────────────────────

#[test]
fn inspect_reports_state_without_writing() {
    let dir = TempDir::new().unwrap();
    let db = legacy(dir.path());
    let before = std::fs::read(&db).unwrap();
    let prompter = RecordingPrompter::new(false);

    let outcome = run_db(DatabaseCommand::Inspect, &db, &prompter, &env());

    assert_eq!(outcome.exit_code, ExitCode::Success);
    assert!(prompter.calls().is_empty());
    let report = outcome.report().unwrap();
    let CommandReport::Database(DatabaseReport::Inspected { state, .. }) = report else {
        panic!("expected Inspected, got {report:?}");
    };
    assert_eq!(*state, SchemaState::LegacyAlembic);
    assert_eq!(
        std::fs::read(&db).unwrap(),
        before,
        "inspect writes nothing"
    );
    assert_eq!(
        report.render_lines(),
        vec!["Schema state: legacy_alembic.".to_string()]
    );
}

// ── cutover ───────────────────────────────────────────────────────────────

#[test]
fn cutover_converts_legacy_marker() {
    let dir = TempDir::new().unwrap();
    let db = legacy(dir.path());
    let prompter = RecordingPrompter::new(false);

    let outcome = run_db(
        DatabaseCommand::Cutover {
            dry_run: false,
            force: true,
        },
        &db,
        &prompter,
        &env(),
    );

    assert_eq!(outcome.exit_code, ExitCode::Success);
    assert!(prompter.calls().is_empty());
    let report = outcome.report().unwrap();
    let CommandReport::Database(DatabaseReport::Cutover {
        outcome: result, ..
    }) = report
    else {
        panic!("expected Cutover, got {report:?}");
    };
    assert_eq!(*result, degenbot_cli_core::CutoverOutcome::Converted);
    assert!(matches!(inspect(&db), SchemaState::RustOwned { .. }));

    // A second forced cutover is an idempotent no-op.
    let again = run_db(
        DatabaseCommand::Cutover {
            dry_run: false,
            force: true,
        },
        &db,
        &prompter,
        &env(),
    );
    let report = again.report().unwrap();
    let CommandReport::Database(DatabaseReport::Cutover {
        outcome: result, ..
    }) = report
    else {
        panic!("expected Cutover, got {report:?}");
    };
    assert_eq!(*result, degenbot_cli_core::CutoverOutcome::AlreadyRustOwned);
}

#[test]
fn cutover_prompts_unless_force_and_declining_aborts() {
    let dir = TempDir::new().unwrap();
    let db = legacy(dir.path());
    let prompter = RecordingPrompter::new(false);

    let outcome = run_db(
        DatabaseCommand::Cutover {
            dry_run: false,
            force: false,
        },
        &db,
        &prompter,
        &env(),
    );

    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(outcome.error(), Some(CliError::Aborted)));
    let calls = prompter.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].0.contains("ONE-WAY"), "ported prompt text");
    assert_eq!(inspect(&db), SchemaState::LegacyAlembic);
}

#[test]
fn cutover_dry_run_reports_and_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let db = legacy(dir.path());
    let before = std::fs::read(&db).unwrap();
    let prompter = RecordingPrompter::new(false);

    let outcome = run_db(
        DatabaseCommand::Cutover {
            dry_run: true,
            force: false,
        },
        &db,
        &prompter,
        &env(),
    );

    assert_eq!(outcome.exit_code, ExitCode::Success, "dry-run exits 0");
    assert!(prompter.calls().is_empty(), "dry-run never prompts");
    let report = outcome.report().unwrap();
    let CommandReport::Database(DatabaseReport::DryRun { kind, state, .. }) = report else {
        panic!("expected DryRun, got {report:?}");
    };
    assert_eq!(*kind, DryRunKind::Cutover);
    assert_eq!(*state, SchemaState::LegacyAlembic);
    assert_eq!(std::fs::read(&db).unwrap(), before);
    assert!(report.render_lines()[0].contains("Would cutover from Alembic to Rust ownership"));
}

#[test]
fn cutover_on_foreign_refuses() {
    let dir = TempDir::new().unwrap();
    let db = foreign(dir.path());
    let prompter = RecordingPrompter::new(true);

    let outcome = run_db(
        DatabaseCommand::Cutover {
            dry_run: false,
            force: true,
        },
        &db,
        &prompter,
        &env(),
    );

    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(outcome.error(), Some(CliError::DatabaseForeign)));
    assert!(prompter.calls().is_empty());
    assert_eq!(inspect(&db), SchemaState::Unrecognized);
}

#[test]
fn cutover_on_empty_refuses_nothing_to_do() {
    let dir = TempDir::new().unwrap();
    let db = empty(dir.path());
    let prompter = RecordingPrompter::new(true);

    let outcome = run_db(
        DatabaseCommand::Cutover {
            dry_run: false,
            force: true,
        },
        &db,
        &prompter,
        &env(),
    );

    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(
        outcome.error(),
        Some(CliError::DatabaseNothingToDo)
    ));
    assert!(prompter.calls().is_empty());
}

// ── heal ──────────────────────────────────────────────────────────────────

#[test]
fn heal_rebuilds_legacy_marker_to_rust_owned() {
    let dir = TempDir::new().unwrap();
    let db = legacy(dir.path());
    let prompter = RecordingPrompter::new(false);

    let outcome = run_db(
        DatabaseCommand::Heal {
            dry_run: false,
            force: true,
        },
        &db,
        &prompter,
        &env(),
    );

    assert_eq!(outcome.exit_code, ExitCode::Success);
    assert!(prompter.calls().is_empty());
    let report = outcome.report().unwrap();
    let CommandReport::Database(DatabaseReport::Healed { report, .. }) = report else {
        panic!("expected Healed, got {report:?}");
    };
    assert_eq!(report.old_state, SchemaState::LegacyAlembic);
    assert!(matches!(report.new_state, SchemaState::RustOwned { .. }));
    assert!(report.bak_path.exists(), "the old DB is preserved as .bak");
    assert!(matches!(inspect(&db), SchemaState::RustOwned { .. }));
}

#[test]
fn heal_accepts_legacy_but_refuses_foreign() {
    let dir = TempDir::new().unwrap();
    let legacy_db = legacy(dir.path());
    let prompter = RecordingPrompter::new(false);

    let outcome = run_db(
        DatabaseCommand::Heal {
            dry_run: false,
            force: true,
        },
        &legacy_db,
        &prompter,
        &env(),
    );

    assert_eq!(outcome.exit_code, ExitCode::Success, "heal accepts legacy");
    let report = outcome.report().unwrap();
    let CommandReport::Database(DatabaseReport::Healed { report, .. }) = report else {
        panic!("expected Healed, got {report:?}");
    };
    assert_eq!(report.old_state, SchemaState::LegacyAlembic);
    assert!(matches!(report.new_state, SchemaState::RustOwned { .. }));

    let foreign_db = foreign(dir.path());
    let outcome = run_db(
        DatabaseCommand::Heal {
            dry_run: false,
            force: true,
        },
        &foreign_db,
        &prompter,
        &env(),
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(outcome.error(), Some(CliError::DatabaseForeign)));
}

#[test]
fn heal_dry_run_reports_and_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let db = legacy(dir.path());
    let before = std::fs::read(&db).unwrap();
    let prompter = RecordingPrompter::new(false);

    let outcome = run_db(
        DatabaseCommand::Heal {
            dry_run: true,
            force: false,
        },
        &db,
        &prompter,
        &env(),
    );

    assert_eq!(outcome.exit_code, ExitCode::Success);
    assert!(prompter.calls().is_empty());
    let report = outcome.report().unwrap();
    let CommandReport::Database(DatabaseReport::DryRun { kind, .. }) = report else {
        panic!("expected DryRun, got {report:?}");
    };
    assert_eq!(*kind, DryRunKind::Heal);
    assert_eq!(std::fs::read(&db).unwrap(), before);
    assert!(report.render_lines()[0].contains("Would heal"));
}

#[test]
fn heal_prompts_unless_force() {
    let dir = TempDir::new().unwrap();
    let db = legacy(dir.path());
    let prompter = RecordingPrompter::new(false);

    let outcome = run_db(
        DatabaseCommand::Heal {
            dry_run: false,
            force: false,
        },
        &db,
        &prompter,
        &env(),
    );

    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(outcome.error(), Some(CliError::Aborted)));
    let calls = prompter.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].0.contains("out-of-place"));
    assert_eq!(inspect(&db), SchemaState::LegacyAlembic);
}

// ── upgrade (retired) ─────────────────────────────────────────────────────

#[test]
fn upgrade_is_retired_and_points_at_heal() {
    let dir = TempDir::new().unwrap();
    let db = rust_owned(dir.path());
    let prompter = RecordingPrompter::new(true);

    let outcome = run_db(
        DatabaseCommand::Upgrade { force: false },
        &db,
        &prompter,
        &env(),
    );

    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(
        outcome.error(),
        Some(CliError::DatabaseUpgradeRetired)
    ));
    assert!(prompter.calls().is_empty(), "retired: never prompts");
    let message = outcome.error().unwrap().message();
    assert!(message.contains("upgrades itself at open"));
    assert!(message.contains("degenbot database heal"));
    assert!(matches!(inspect(&db), SchemaState::RustOwned { .. }));
}

// ── exit-code + prompt-plan surfaces ──────────────────────────────────────

#[test]
fn boot_refused_maps_to_ex_config_78() {
    let err = CliError::BootRefused("cannot host the fleet".to_string());
    assert_eq!(ExitCode::from(&err), ExitCode::Config);
    assert_eq!(ExitCode::from(&err).code(), 78);
    assert_eq!(ExitCode::Success.code(), 0);
    assert_eq!(ExitCode::Failure.code(), 1);
}

#[test]
fn prompt_plan_matches_ported_policy() {
    let dir = TempDir::new().unwrap();
    let db = rust_owned(dir.path());
    let e = env();
    let ctx = CliContext::new(&e).with_database(db.display().to_string());

    assert_eq!(DatabaseCommand::Inspect.prompt_plan(&ctx), PromptPlan::None);
    assert_eq!(DatabaseCommand::Compact.prompt_plan(&ctx), PromptPlan::None);
    assert_eq!(
        DatabaseCommand::Reset { force: false }.prompt_plan(&ctx),
        PromptPlan::UnlessForce
    );
    assert_eq!(
        DatabaseCommand::Cutover {
            dry_run: false,
            force: false
        }
        .prompt_plan(&ctx),
        PromptPlan::UnlessForce
    );
    assert_eq!(
        DatabaseCommand::Heal {
            dry_run: false,
            force: false
        }
        .prompt_plan(&ctx),
        PromptPlan::UnlessForce
    );
    assert_eq!(
        DatabaseCommand::Backup.prompt_plan(&ctx),
        PromptPlan::OnCondition(false),
        "no existing target"
    );

    std::fs::write(database_backup_path(&db), b"x").unwrap();
    assert_eq!(
        DatabaseCommand::Backup.prompt_plan(&ctx),
        PromptPlan::OnCondition(true),
        "existing target flips the condition"
    );

    assert!(PromptPlan::UnlessForce.asks(false));
    assert!(!PromptPlan::UnlessForce.asks(true));
    assert!(!PromptPlan::None.asks(false));
    assert!(PromptPlan::OnCondition(true).asks(true));
}

#[test]
fn cli_context_resolves_override_env_and_default() {
    let override_env = env();
    let cli = CliContext::new(&override_env).with_database("/tmp/cli.db");
    assert_eq!(cli.database_path().value, PathBuf::from("/tmp/cli.db"));

    let mut map = BTreeMap::new();
    map.insert("DEGENBOT_DB_PATH".to_string(), "/tmp/env.db".to_string());
    let env_map = MapEnv::new(map);
    let ctx = CliContext::new(&env_map);
    assert_eq!(ctx.database_path().value, PathBuf::from("/tmp/env.db"));

    let empty_env = env();
    let default_ctx = CliContext::new(&empty_env);
    assert!(default_ctx
        .database_path()
        .value
        .ends_with(".local/state/degenbot/db/degenbot.db"));
}

#[test]
fn rust_owned_fixture_prompt_and_cutover_noop() {
    let dir = TempDir::new().unwrap();
    let db = rust_owned(dir.path());
    assert!(matches!(inspect(&db), SchemaState::RustOwned { .. }));
}

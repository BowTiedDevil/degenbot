//! The database command arms (ADR-051 D1/D4) — the template group.
//!
//! Ported verbatim from `src/degenbot/cli/database.py`: same dry-run text, same
//! confirmation conditions, same refusals. `database upgrade` is retired — the
//! database upgrades itself at open (ADR-052); `database heal` is the explicit
//! repair.
//!
//! These arms never call heal on another command's behalf; the auto-heal epic
//! (ergo `6ATMVN`) owns schema self-healing inside `ensure_schema`.

use std::path::{Path, PathBuf};

use degenbot_db::ops;
use degenbot_db::SchemaState;

use crate::command::DatabaseCommand;
use crate::context::CliContext;
use crate::error::CliError;
use crate::prompt::{PromptPlan, Prompter};
use crate::report::{CutoverOutcome, DatabaseReport, DryRunKind};

/// Execute a `database` command.
///
/// # Errors
///
/// [`CliError`] for a declined prompt, a schema refusal
/// ([`CliError::DatabaseForeign`] / [`CliError::DatabaseNothingToDo`]), the
/// retired `upgrade` command, or a degenbot-db op failure.
pub(crate) fn execute(
    command: &DatabaseCommand,
    ctx: &CliContext<'_>,
    prompter: &dyn Prompter,
) -> Result<DatabaseReport, CliError> {
    let path = ctx.database_path().value;
    let plan = command.prompt_plan(ctx);
    match command {
        DatabaseCommand::Backup => backup(&path, plan, prompter),
        DatabaseCommand::Reset { force } => reset(&path, plan, *force, prompter),
        DatabaseCommand::Upgrade { .. } => Err(CliError::DatabaseUpgradeRetired),
        DatabaseCommand::Compact => compact(&path),
        DatabaseCommand::Cutover { dry_run, force } => {
            cutover(&path, plan, *dry_run, *force, prompter)
        }
        DatabaseCommand::Heal { dry_run, force } => heal(&path, plan, *dry_run, *force, prompter),
        DatabaseCommand::Inspect => inspect(&path),
    }
}

/// `backup`: write the `.db.bak` sibling, confirming replacement when the target
/// already exists (the click handler's `BackupExists` policy).
fn backup(
    path: &Path,
    plan: PromptPlan,
    prompter: &dyn Prompter,
) -> Result<DatabaseReport, CliError> {
    let backup_path = database_backup_path(path);
    if plan.asks(false) {
        if !prompter.confirm(&backup_prompt(&backup_path), false) {
            return Err(CliError::Aborted);
        }
        remove_file_if_exists(&backup_path)?;
    }
    ops::backup_database(path, &backup_path)?;
    tracing::info!(
        source = %path.display(),
        backup = %backup_path.display(),
        "backed up SQLite database"
    );
    Ok(DatabaseReport::BackedUp {
        source: path.to_path_buf(),
        backup: backup_path,
    })
}

/// `reset`: remove and recreate the database (confirms unless `--force`).
fn reset(
    path: &Path,
    plan: PromptPlan,
    force: bool,
    prompter: &dyn Prompter,
) -> Result<DatabaseReport, CliError> {
    if plan.asks(force) && !prompter.confirm(&reset_prompt(path), false) {
        return Err(CliError::Aborted);
    }
    remove_file_if_exists(path)?;
    ops::create_new_database(path)?;
    tracing::info!(path = %path.display(), "initialized new SQLite database");
    Ok(DatabaseReport::Reset {
        path: path.to_path_buf(),
    })
}

/// `compact`: `VACUUM` the database; never prompts.
fn compact(path: &Path) -> Result<DatabaseReport, CliError> {
    ops::compact_database(path)?;
    tracing::info!(path = %path.display(), "compacted SQLite database");
    Ok(DatabaseReport::Compacted {
        path: path.to_path_buf(),
    })
}

/// `cutover`: the opt-in one-way ownership flip (ADR-010). Refuses foreign /
/// empty file DBs BEFORE any prompt; confirms unless `--force`.
fn cutover(
    path: &Path,
    plan: PromptPlan,
    dry_run: bool,
    force: bool,
    prompter: &dyn Prompter,
) -> Result<DatabaseReport, CliError> {
    let state = ops::inspect_schema_state(path)?;
    if dry_run {
        return Ok(DatabaseReport::DryRun {
            path: path.to_path_buf(),
            state,
            kind: DryRunKind::Cutover,
        });
    }
    match &state {
        SchemaState::Unrecognized => return Err(CliError::DatabaseForeign),
        SchemaState::FreshStandalone { .. } => return Err(CliError::DatabaseNothingToDo),
        SchemaState::LegacyAlembic | SchemaState::RustOwned { .. } => {}
    }
    if plan.asks(force) && !prompter.confirm(&cutover_prompt(path), false) {
        return Err(CliError::Aborted);
    }
    let outcome = if matches!(state, SchemaState::RustOwned { .. }) {
        CutoverOutcome::AlreadyRustOwned
    } else {
        CutoverOutcome::Converted
    };
    ops::convert_alembic_to_rust_owned(path)?;
    tracing::info!(path = %path.display(), "database schema ownership cutover");
    Ok(DatabaseReport::Cutover {
        path: path.to_path_buf(),
        state,
        outcome,
    })
}

/// `heal`: the out-of-place dump-and-restore rebuild (ADR-011). ACCEPTS a
/// legacy `alembic_version`-marked DB (unlike `cutover`); refuses a foreign
/// file; confirms unless `--force`.
fn heal(
    path: &Path,
    plan: PromptPlan,
    dry_run: bool,
    force: bool,
    prompter: &dyn Prompter,
) -> Result<DatabaseReport, CliError> {
    let state = ops::inspect_schema_state(path)?;
    if dry_run {
        return Ok(DatabaseReport::DryRun {
            path: path.to_path_buf(),
            state,
            kind: DryRunKind::Heal,
        });
    }
    if matches!(state, SchemaState::Unrecognized) {
        return Err(CliError::DatabaseForeign);
    }
    if plan.asks(force) && !prompter.confirm(&heal_prompt(path), false) {
        return Err(CliError::Aborted);
    }
    let report = ops::heal_database(path)?;
    tracing::info!(path = %path.display(), "healed SQLite database");
    Ok(DatabaseReport::Healed {
        path: path.to_path_buf(),
        report,
    })
}

/// `inspect`: read-only schema-state inspection; never writes.
fn inspect(path: &Path) -> Result<DatabaseReport, CliError> {
    let state = ops::inspect_schema_state(path)?;
    Ok(DatabaseReport::Inspected {
        path: path.to_path_buf(),
        state,
    })
}

/// The `.db.bak` sibling of `path`, mirroring Python's
/// `Path.with_suffix(".db.bak")`.
#[must_use]
pub fn database_backup_path(path: &Path) -> PathBuf {
    let mut backup = path.to_path_buf();
    backup.set_extension("db.bak");
    backup
}

fn backup_prompt(backup_path: &Path) -> String {
    format!(
        "An existing backup was found at {}. Do you want to replace it?",
        backup_path.display()
    )
}

fn reset_prompt(path: &Path) -> String {
    format!(
        "The existing database at {} will be removed and a new, empty database will be \
         created and initialized using the schema included in degenbot version {}. Do you \
         want to proceed?",
        path.display(),
        env!("CARGO_PKG_VERSION")
    )
}

fn cutover_prompt(path: &Path) -> String {
    format!(
        "The database at {} will be cut over from Alembic to Rust schema ownership. This is \
         ONE-WAY and cannot be undone. Proceed?",
        path.display()
    )
}

fn heal_prompt(path: &Path) -> String {
    format!(
        "The database at {} will be rebuilt out-of-place: a fresh Rust-schema DB is created, \
         all user rows are copied across, and the result is atomically swapped into place \
         (the old DB is preserved as *.bak). Proceed?",
        path.display()
    )
}

/// Remove `path` when present (Python's `unlink(missing_ok=True)`).
fn remove_file_if_exists(path: &Path) -> Result<(), CliError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(degenbot_db::DbError::Io(err).into()),
    }
}

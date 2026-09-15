//! Typed command reports (ADR-051 D1: execution returns typed results; the
//! facade renders them — Q1).

use std::path::PathBuf;

use degenbot_db::ops::HealReport;
use degenbot_db::SchemaState;

use crate::error::{CliError, ExitCode};

/// The typed result of one command execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandReport {
    /// A `database` command report.
    Database(DatabaseReport),
}

impl CommandReport {
    /// The operator-facing lines the argv facade renders.
    #[must_use]
    pub fn render_lines(&self) -> Vec<String> {
        match self {
            Self::Database(report) => report.render_lines(),
        }
    }
}

/// The typed result of the execution entry.
#[derive(Debug)]
pub struct CommandOutcome {
    result: Result<CommandReport, CliError>,
    /// The process exit code derived at the single `From<&CliError>` site.
    pub exit_code: ExitCode,
}

impl CommandOutcome {
    /// Bundle a result with its derived exit code.
    #[must_use]
    pub(crate) const fn new(result: Result<CommandReport, CliError>, exit_code: ExitCode) -> Self {
        Self { result, exit_code }
    }

    /// The typed report, when the command completed.
    #[must_use]
    pub fn report(&self) -> Option<&CommandReport> {
        self.result.as_ref().ok()
    }

    /// The typed failure, when the command did not complete.
    #[must_use]
    pub fn error(&self) -> Option<&CliError> {
        self.result.as_ref().err()
    }
}

/// Which dry-run arm produced a [`DatabaseReport::DryRun`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DryRunKind {
    /// `database cutover --dry-run`.
    Cutover,
    /// `database heal --dry-run`.
    Heal,
}

/// The outcome of a successful `database cutover` (pre-state derived).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutoverOutcome {
    /// The DB was Alembic-stamped and is now Rust-owned.
    Converted,
    /// The DB was already Rust-owned; the cutover was a no-op.
    AlreadyRustOwned,
}

/// The typed result of a `database` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseReport {
    /// `backup` completed.
    BackedUp {
        /// The source database path.
        source: PathBuf,
        /// The written backup path.
        backup: PathBuf,
    },
    /// `reset` recreated the database.
    Reset {
        /// The database path.
        path: PathBuf,
    },
    /// `compact` vacuumed the database.
    Compacted {
        /// The database path.
        path: PathBuf,
    },
    /// `inspect` read the schema state (writes nothing).
    Inspected {
        /// The database path.
        path: PathBuf,
        /// The observed schema state.
        state: SchemaState,
    },
    /// `cutover` flipped schema ownership.
    Cutover {
        /// The database path.
        path: PathBuf,
        /// The observed pre-cutover schema state.
        state: SchemaState,
        /// Whether the cutover converted or no-op'd.
        outcome: CutoverOutcome,
    },
    /// `heal` rebuilt the database out-of-place.
    Healed {
        /// The database path.
        path: PathBuf,
        /// The heal report (rows copied, `.bak` path, warnings).
        report: HealReport,
    },
    /// A `--dry-run` observed the schema state and wrote nothing.
    DryRun {
        /// The database path.
        path: PathBuf,
        /// The observed schema state.
        state: SchemaState,
        /// Which dry-run arm ran.
        kind: DryRunKind,
    },
}

impl DatabaseReport {
    /// The operator-facing lines for this report (the ported click `echo` text).
    #[must_use]
    pub fn render_lines(&self) -> Vec<String> {
        match self {
            Self::BackedUp { backup, .. } => {
                vec![format!("Backed up SQLite database to {}", backup.display())]
            }
            Self::Reset { path } => {
                vec![format!(
                    "Initialized new SQLite database at {}",
                    path.display()
                )]
            }
            Self::Compacted { path } => {
                vec![format!("Compacted SQLite database at {}", path.display())]
            }
            Self::Inspected { state, .. } => {
                vec![format!("Schema state: {}.", schema_state_label(state))]
            }
            Self::Cutover { path, outcome, .. } => match outcome {
                CutoverOutcome::Converted => vec![format!(
                    "Cut over database at {} from Alembic to Rust ownership (the \
                     `alembic_version` table was dropped and `_degenbot_db_schema_version` \
                     was stamped).",
                    path.display()
                )],
                CutoverOutcome::AlreadyRustOwned => {
                    vec!["The database is already Rust-owned; cutover was a no-op.".to_string()]
                }
            },
            Self::Healed { path, report } => healed_lines(path, report),
            Self::DryRun { state, kind, .. } => vec![match kind {
                DryRunKind::Cutover => cutover_dry_run_line(state),
                DryRunKind::Heal => heal_dry_run_line(state),
            }],
        }
    }
}

/// The ported `heal` success / no-op text.
fn healed_lines(path: &std::path::Path, report: &HealReport) -> Vec<String> {
    if matches!(report.old_state, SchemaState::RustOwned { .. }) {
        return vec![format!(
            "Database at {} is already Rust-owned; heal is a no-op (no copy, no .bak).",
            path.display()
        )];
    }
    let total_rows: u64 = report.rows_copied.values().sum();
    let n_tables = report.rows_copied.len();
    let mut lines = vec![format!(
        "Healed database at {}: {total_rows} rows across {n_tables} tables copied; old DB \
         preserved at {}; new state: {}.",
        path.display(),
        report.bak_path.display(),
        schema_state_label(&report.new_state)
    )];
    if !report.warnings.is_empty() {
        lines.push(format!("Warnings: {}", report.warnings.join("; ")));
    }
    lines
}

/// The ported `database cutover --dry-run` line for `state`.
#[must_use]
pub fn cutover_dry_run_line(state: &SchemaState) -> String {
    let label = schema_state_label(state);
    match state {
        SchemaState::AlembicCurrent => format!(
            "Schema state: {label}. Would cutover from Alembic to Rust ownership (drop \
             `alembic_version`, stamp `_degenbot_db_schema_version`)."
        ),
        SchemaState::RustOwned { .. } => {
            format!("Schema state: {label}. Already Rust-owned — cutover is a no-op.")
        }
        SchemaState::AlembicStale { .. } => format!(
            "Schema state: {label}. Schema is stale — run `degenbot database upgrade` first."
        ),
        SchemaState::FreshStandalone { .. } => {
            format!("Schema state: {label}. No Alembic history — nothing to cutover.")
        }
        SchemaState::Unrecognized => {
            format!("Schema state: {label}. Unrecognized database (foreign file).")
        }
    }
}

/// The ported `database heal --dry-run` line for `state`.
#[must_use]
pub fn heal_dry_run_line(state: &SchemaState) -> String {
    let label = schema_state_label(state);
    match state {
        SchemaState::AlembicCurrent => format!(
            "Schema state: {label}. Would heal: rebuild at the Rust head schema, copy all \
             rows, drop alembic_version, stamp _degenbot_db_schema_version, atomic-swap with \
             a *.bak backup."
        ),
        SchemaState::RustOwned { .. } => {
            format!("Schema state: {label}. Already Rust-owned — heal is a no-op.")
        }
        SchemaState::AlembicStale { .. } => format!(
            "Schema state: {label}. Schema is stale — heal can proceed (out-of-place rebuild \
             handles stale schemas) but consider `degenbot database upgrade` first for a \
             strictly in-place path."
        ),
        SchemaState::FreshStandalone { .. } => format!(
            "Schema state: {label}. Empty file — heal produces a fresh RustOwned DB (0 rows \
             copied)."
        ),
        SchemaState::Unrecognized => {
            format!(
                "Schema state: {label}. Unrecognized database (foreign file) — heal would \
                 refuse."
            )
        }
    }
}

/// The Python-compatible label for a [`SchemaState`] (mirrors the
/// `db_inspect_schema_state` return values).
#[must_use]
pub fn schema_state_label(state: &SchemaState) -> &'static str {
    match state {
        SchemaState::AlembicCurrent => "alembic_current",
        SchemaState::AlembicStale { .. } => "alembic_stale",
        SchemaState::FreshStandalone { .. } => "fresh_standalone",
        SchemaState::RustOwned { .. } => "rust_owned",
        SchemaState::Unrecognized => "unrecognized",
    }
}

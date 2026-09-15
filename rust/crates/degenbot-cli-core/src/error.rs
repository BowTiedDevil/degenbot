//! Typed console failures and the single [`CliError`] → [`ExitCode`] mapping
//! (ADR-051 D1).
//!
//! The workspace lint `exit = "deny"` forbids a library from aborting the host
//! process: `run` returns codes. `EX_CONFIG` (78, sysexits) is the typed fleet
//! boot refusal lifted out of `DegenbotCLI.invoke` (FF-T1, BPHR6F).

use std::fmt;

use degenbot_config::ConfigError;
use degenbot_db::DbError;

/// The process exit code a command run maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// `0`: the command completed (including a `--dry-run`, which writes nothing).
    Success,
    /// `1`: a typed command failure, including a declined confirmation (the click
    /// `Abort` arm).
    Failure,
    /// `78` (sysexits `EX_CONFIG`): the typed fleet boot refusal — the host cannot
    /// host the fleet configuration (FF-T1).
    Config,
}

impl ExitCode {
    /// The numeric process exit code.
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Success => 0,
            Self::Failure => 1,
            Self::Config => 78,
        }
    }
}

/// A typed console failure.
///
/// Every variant carries the data the argv facade needs to render the
/// operator-facing line through [`CliError::message`]; rendering itself is the
/// facade's job (ADR-051 Q1).
#[derive(Debug)]
pub enum CliError {
    /// The typed fleet boot refusal (FF-T1). Exits `EX_CONFIG` 78.
    BootRefused(String),
    /// The operator declined a confirmation prompt — the click `Abort` arm.
    Aborted,
    /// The `database upgrade` subcommand is retired: the database upgrades itself
    /// at open (ADR-052), and `database heal` is the explicit repair.
    DatabaseUpgradeRetired,
    /// The schema is stamped at a prior Alembic revision — the pointed refusal
    /// (run `degenbot database upgrade` first).
    DatabaseStale {
        /// The revision actually stamped in the database.
        head: String,
        /// The revision this binary expects.
        expected: String,
    },
    /// The file is a foreign `SQLite` database — the arm refuses to adopt it.
    DatabaseForeign,
    /// The schema state offers nothing for this arm (e.g. `cutover` on a DB with
    /// no Alembic history).
    DatabaseNothingToDo,
    /// Any other database failure (I/O, integrity, heal verification).
    Database(DbError),
    /// Driver-domain config resolution failed (ADR-051 D8).
    Config(ConfigError),
}

impl CliError {
    /// The operator-facing line for this failure.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::BootRefused(message) => message.clone(),
            Self::Aborted => "Aborted!".to_string(),
            Self::DatabaseUpgradeRetired => {
                "the database upgrades itself at open; for an explicit repair, run \
                 `degenbot database heal`"
                    .to_string()
            }
            Self::DatabaseStale { head, expected } => format!(
                "The database schema is stale (revision {head}; expected {expected}). Run \
                 `degenbot database upgrade`."
            ),
            Self::DatabaseForeign => {
                "The database is unrecognized (a foreign SQLite file); refused.".to_string()
            }
            Self::DatabaseNothingToDo => {
                "The database has no Alembic history; there is nothing to cut over.".to_string()
            }
            Self::Database(err) => err.to_string(),
            Self::Config(err) => err.to_string(),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            Self::Config(err) => Some(err),
            _ => None,
        }
    }
}

/// Map a database error onto its typed console failure.
///
/// Alembic-stale and foreign-file failures stay typed here — the facade must be
/// able to point the operator at the right remedy without string matching.
impl From<DbError> for CliError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::AlembicStale { head, expected } => Self::DatabaseStale { head, expected },
            DbError::UnrecognizedSchema => Self::DatabaseForeign,
            other => Self::Database(other),
        }
    }
}

/// THE one `CliError → ExitCode` mapping site (ADR-051 D1).
impl From<&CliError> for ExitCode {
    fn from(err: &CliError) -> Self {
        match err {
            // FF-T1: the typed fleet boot refusal is the lone `EX_CONFIG` arm.
            CliError::BootRefused(_) => Self::Config,
            CliError::Aborted
            | CliError::DatabaseUpgradeRetired
            | CliError::DatabaseStale { .. }
            | CliError::DatabaseForeign
            | CliError::DatabaseNothingToDo
            | CliError::Database(_)
            | CliError::Config(_) => Self::Failure,
        }
    }
}

impl From<CliError> for ExitCode {
    fn from(err: CliError) -> Self {
        Self::from(&err)
    }
}

//! Typed console failures and the single [`CliError`] → [`ExitCode`] mapping
//! (ADR-051 D1).
//!
//! The workspace lint `exit = "deny"` forbids a library from aborting the host
//! process: `run` returns codes. `EX_CONFIG` (78, sysexits) is the typed fleet
//! boot refusal lifted out of `DegenbotCLI.invoke` (FF-T1, BPHR6F).

use std::fmt;

use degenbot_aave::RunError as AaveRunError;
use degenbot_config::ConfigError;
use degenbot_db::DbError;
use degenbot_pool_updater::RunError as PoolRunError;

/// The process exit code a command run maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// `0`: the command completed (including a `--dry-run`, which writes nothing,
    /// and a cooperative `Cancelled` run, whose committed chunks stay durable).
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
    /// The file is a foreign `SQLite` database — the arm refuses to adopt it.
    DatabaseForeign,
    /// The schema state offers nothing for this arm (e.g. `cutover` on an empty
    /// file with no legacy history).
    DatabaseNothingToDo,
    /// Any other database failure (I/O, integrity, heal verification).
    Database(DbError),
    /// Driver-domain config resolution failed (ADR-051 D8).
    Config(ConfigError),
    /// An unknown chain selector (`--chain foo`): the console names chain slugs
    /// (`base`, `ethereum`) or numeric chain ids.
    UnknownChain {
        /// The rejected selector, verbatim.
        chain: String,
    },
    /// The deployments registry has no record for the resolved
    /// `(chain_id, name)` pair.
    UnknownDeployment {
        /// The resolved chain id.
        chain_id: u64,
        /// The DEX name slug (the `--name` value).
        name: String,
    },
    /// A malformed block identifier — the exact `Invalid block tag: {tag}`
    /// refusal the Python `_resolve_to_block` raises.
    InvalidBlockTag(String),
    /// The RPC read that resolves a `tag:offset` block identifier failed.
    BlockResolution(String),
    /// The supplied address does not parse (the click `Abort` arm of
    /// `aave position show`).
    InvalidAddress(String),
    /// A required command argument is missing or malformed (`--pool-manager`
    /// on `--family v4`, an out-of-range chain id).
    InvalidArgument(String),
    /// `aave update` found no active Aave markets (the Python
    /// `DegenbotValueError`).
    NoActiveAaveMarkets,
    /// A `pool update` core failure (DB/RPC cancelled/verification).
    PoolUpdate(PoolRunError),
    /// An `aave` core failure (DB/RPC/verification/market-not-found).
    AaveUpdate(AaveRunError),
    /// A command arm that `block_on`s the process-wide shared runtime was invoked
    /// from inside an existing `tokio` runtime. `run_pool_update`/`run_aave_update`
    /// ride `get_runtime()` (TD3/A1) and must not nest; the arms hold the same
    /// constraint.
    RuntimeNested,
    /// The operator host refused a command: the `{"ok": false, "error": ...}`
    /// frame, rendered as one line (ADR-051 D6).
    OperatorRefused(String),
    /// A protocol-level failure talking to the operator host: an unreachable
    /// socket, a timed-out exchange, or a malformed/non-object/missing-`ok`
    /// response frame.
    OperatorProtocol(String),
    /// A client-side wire-hygiene refusal (an unknown `cordon_*` key, an
    /// empty posture patch, an unknown hop-family string), raised BEFORE the
    /// socket is touched. Domain validation stays server-side.
    OperatorHygiene(String),
}

impl CliError {
    /// The operator-facing line for this failure.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::BootRefused(message)
            | Self::BlockResolution(message)
            | Self::InvalidArgument(message)
            | Self::OperatorRefused(message)
            | Self::OperatorProtocol(message)
            | Self::OperatorHygiene(message) => message.clone(),
            Self::Aborted => "Aborted!".to_string(),
            Self::DatabaseUpgradeRetired => {
                "the database upgrades itself at open; for an explicit repair, run \
                 `degenbot database heal`"
                    .to_string()
            }
            Self::DatabaseForeign => {
                "The database is unrecognized (a foreign SQLite file); refused.".to_string()
            }
            Self::DatabaseNothingToDo => {
                "The database has no legacy history; there is nothing to cut over.".to_string()
            }
            Self::Database(err) => err.to_string(),
            Self::Config(err) => err.to_string(),
            Self::UnknownChain { chain } => format!(
                "Unknown chain {chain:?}: expected a chain slug (base, ethereum) or a numeric \
                 chain id."
            ),
            Self::UnknownDeployment { chain_id, name } => {
                format!("The deployments registry has no record for {name:?} on chain {chain_id}.")
            }
            Self::InvalidBlockTag(tag) => format!("Invalid block tag: {tag}"),
            Self::InvalidAddress(address) => format!("Invalid address: {address}"),
            Self::NoActiveAaveMarkets => "No active Aave markets found.".to_string(),
            Self::PoolUpdate(err) => err.to_string(),
            Self::AaveUpdate(err) => err.to_string(),
            Self::RuntimeNested => "the command arms own their tokio runtime; do not run them \
                 from inside an existing runtime"
                .to_string(),
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
            Self::PoolUpdate(err) => Some(err),
            Self::AaveUpdate(err) => Some(err),
            _ => None,
        }
    }
}

/// Map a database error onto its typed console failure.
///
/// A foreign-file failure stays typed here — the facade must be able to point
/// the operator at the right remedy without string matching.
impl From<DbError> for CliError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::UnrecognizedSchema => Self::DatabaseForeign,
            other => Self::Database(other),
        }
    }
}

/// Map a config-resolution error onto its typed console failure.
impl From<ConfigError> for CliError {
    fn from(err: ConfigError) -> Self {
        Self::Config(err)
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
            | CliError::DatabaseForeign
            | CliError::DatabaseNothingToDo
            | CliError::Database(_)
            | CliError::Config(_)
            | CliError::UnknownChain { .. }
            | CliError::UnknownDeployment { .. }
            | CliError::InvalidBlockTag(_)
            | CliError::BlockResolution(_)
            | CliError::InvalidAddress(_)
            | CliError::InvalidArgument(_)
            | CliError::NoActiveAaveMarkets
            | CliError::PoolUpdate(_)
            | CliError::AaveUpdate(_)
            | CliError::RuntimeNested
            | CliError::OperatorRefused(_)
            | CliError::OperatorProtocol(_)
            | CliError::OperatorHygiene(_) => Self::Failure,
        }
    }
}

impl From<CliError> for ExitCode {
    fn from(err: CliError) -> Self {
        Self::from(&err)
    }
}

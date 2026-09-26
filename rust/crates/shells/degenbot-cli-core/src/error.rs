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

/// One exchange's committed resume state, snapshotted read-only from the
/// operator database's `exchanges.last_update_block` cursor column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeResumeState {
    /// The `exchanges.name` slug.
    pub name: String,
    /// The committed cursor; `None` when the exchange was never updated.
    pub last_update_block: Option<i64>,
}

/// A `pool update` (or `pool verify`) core failure plus the run context the
/// arm held when the core returned it.
///
/// The bare library text (`api error: backend connection task has stopped`)
/// alone is not an operator diagnostic, and the arm must not have to
/// reconstruct the run's identity at the print site — so the payload carries
/// it structurally: the endpoint, the chain, the requested block range, and,
/// for a mid-run failure, the per-exchange resume state read out of the
/// operator database. The updater commits per chunk, so a failed run keeps
/// every committed chunk and leaves the remaining exchanges' outstanding
/// work recorded in their cursors; the failure report makes it visible.
#[derive(Debug)]
pub struct PoolUpdateFailure {
    /// The underlying core error.
    pub error: PoolRunError,
    /// The RPC endpoint the run was bound to.
    pub rpc_url: String,
    /// The chain the run advances.
    pub chain_id: i64,
    /// The first block the run intended to process (the earliest committed
    /// cursor + 1; mirrors the core's own `initial_start_block`).
    pub from_block: u64,
    /// The requested upper bound; `None` when the run targeted the chain tip.
    pub to_block: Option<u64>,
    /// The post-failure per-exchange resume snapshot; `None` when the
    /// read-only cursor read itself failed (or a non-run failure carried no
    /// snapshot).
    pub resume: Option<Vec<ExchangeResumeState>>,
}

impl PoolUpdateFailure {
    /// The operator-facing failure text.
    #[must_use]
    pub fn message(&self) -> String {
        let error_text = self.error.to_string();
        let range = self.range_text();
        let mut lines = if self.is_rpc_connection_failure(&error_text) {
            vec![
                format!(
                    "Chain {}: the RPC connection to {} dropped mid-run while advancing blocks \
                     {}; chunks already committed are kept. Rerunning resumes from the recorded \
                     per-exchange cursors.",
                    self.chain_id, self.rpc_url, range
                ),
                format!("  underlying error: {error_text}"),
            ]
        } else {
            vec![format!(
                "Chain {}: pool update against {} failed while advancing blocks {}: {error_text}",
                self.chain_id, self.rpc_url, range
            )]
        };
        lines.extend(self.resume_lines());
        lines.join("\n")
    }

    /// The range half of the failure line: `A-B`, or `A onward` for a tip run.
    fn range_text(&self) -> String {
        match self.to_block {
            Some(to) => format!("{}-{to}", self.from_block),
            None => format!("{} onward (the chain tip)", self.from_block),
        }
    }

    /// Whether the core error is the RPC-connection class — a dropped or
    /// unreachable transport. The provider layer maps the transport's
    /// `BackendGone` (and the rest of the connection class) onto the
    /// `Connection failed` / `Request timeout` variants, which is what the
    /// updater surfaces inside `PoolRunError::Provider`.
    ///
    /// The provider error's type is not nameable at this console layer (it
    /// lives in `degenbot-core`, which the console does not depend on), so the
    /// class is read off the stable `#[error]` prefixes that layer renders;
    /// the variant boundary is still matched structurally.
    fn is_rpc_connection_failure(&self, error_text: &str) -> bool {
        matches!(&self.error, PoolRunError::Provider(_))
            && (error_text.starts_with("rpc error: Connection failed: ")
                || error_text.starts_with("rpc error: Request timeout: "))
    }

    /// The per-exchange resume lines: which exchanges are current, which are
    /// behind, and which were never updated — the outstanding work a rerun
    /// picks up from the recorded cursors.
    fn resume_lines(&self) -> Vec<String> {
        let Some(rows) = &self.resume else {
            return vec![
                "  exchange cursor state unavailable: the read-only resume check failed"
                    .to_string(),
            ];
        };
        if rows.is_empty() {
            return vec![format!(
                "  no active exchanges were registered for chain {}.",
                self.chain_id
            )];
        }
        let mut current = Vec::new();
        let mut behind = Vec::new();
        let mut never_updated = Vec::new();
        for row in rows {
            match (row.last_update_block, self.to_block) {
                (None, _) => never_updated.push(row.name.clone()),
                // A run targeting the chain tip leaves unprocessed work in
                // front of every cursor, so none is current at the target.
                (Some(block), None) => {
                    behind.push(format!("{} (block {block})", row.name));
                }
                (Some(block), Some(to)) => {
                    let to = i64::try_from(to).unwrap_or(i64::MAX);
                    if block >= to {
                        current.push(row.name.clone());
                    } else {
                        behind.push(format!("{} (block {block})", row.name));
                    }
                }
            }
        }
        let mut lines = Vec::new();
        if !current.is_empty() {
            lines.push(format!(
                "  current at the requested target: {}",
                current.join(", ")
            ));
        }
        if !behind.is_empty() {
            lines.push(format!(
                "  behind (last committed block): {}",
                behind.join(", ")
            ));
        }
        if !never_updated.is_empty() {
            lines.push(format!("  never updated: {}", never_updated.join(", ")));
        }
        lines
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
    /// A filesystem failure outside the database (e.g. `database reset`
    /// bootstrapping a missing state-home directory chain).
    Io(std::io::Error),
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
    /// A `pool update` / `pool verify` core failure (DB/RPC/cancelled/
    /// verification), wrapped with the run context the arm held when the core
    /// returned it (endpoint, chain, requested range, per-exchange resume
    /// state) so the rendered diagnostic is complete.
    ///
    /// The payload is boxed to keep the variant out of the
    /// `result_large_err` budget every arm's `Result` shares.
    PoolUpdate(Box<PoolUpdateFailure>),
    /// An `aave` core failure (DB/RPC/verification/market-not-found).
    AaveUpdate(AaveRunError),
    /// A command arm that `block_on`s the process-wide shared runtime was invoked
    /// from inside an existing `tokio` runtime. `run_pool_update`/`run_aave_update`
    /// ride `get_runtime()` and must not nest; the arms hold the same
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
            Self::Io(err) => err.to_string(),
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
            Self::PoolUpdate(failure) => failure.message(),
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
            Self::PoolUpdate(failure) => Some(&failure.error),
            Self::AaveUpdate(err) => Some(err),
            Self::Io(err) => Some(err),
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
            | CliError::Io(_)
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

#[cfg(test)]
mod tests {
    //! The pool-failure rendering (the `pool update` diagnostic).
    use super::*;
    use degenbot_db::DbError;

    #[test]
    fn non_connection_failure_names_endpoint_chain_range_and_error() {
        let failure = PoolUpdateFailure {
            error: PoolRunError::Db(DbError::MissingRow("chunk row".to_string())),
            rpc_url: "http://reth.local:8545".to_string(),
            chain_id: 8453,
            from_block: 26_055_206,
            to_block: Some(26_059_263),
            resume: None,
        };
        let message = failure.message();
        assert!(message.contains("http://reth.local:8545"), "{message}");
        assert!(message.contains("8453"), "{message}");
        assert!(message.contains("blocks 26055206-26059263"), "{message}");
        assert!(
            message.contains("required row not found: chunk row"),
            "{message}"
        );
        assert!(!message.contains("dropped mid-run"), "{message}");
    }

    #[test]
    fn tip_run_reports_an_open_ended_range() {
        let failure = PoolUpdateFailure {
            error: PoolRunError::Db(DbError::MissingRow("tip".to_string())),
            rpc_url: "http://reth.local:8545".to_string(),
            chain_id: 1,
            from_block: 5,
            to_block: None,
            resume: None,
        };
        assert!(failure
            .message()
            .contains("blocks 5 onward (the chain tip)"));
    }

    #[test]
    fn resume_groups_render_current_behind_and_never_updated() {
        let failure = PoolUpdateFailure {
            error: PoolRunError::Db(DbError::MissingRow("row".to_string())),
            rpc_url: "http://reth.local:8545".to_string(),
            chain_id: 8453,
            from_block: 1,
            to_block: Some(100),
            resume: Some(vec![
                ExchangeResumeState {
                    name: "uniswap_v2".to_string(),
                    last_update_block: Some(100),
                },
                ExchangeResumeState {
                    name: "uniswap_v3".to_string(),
                    last_update_block: Some(50),
                },
                ExchangeResumeState {
                    name: "uniswap_v4".to_string(),
                    last_update_block: None,
                },
            ]),
        };
        let message = failure.message();
        assert!(
            message.contains("current at the requested target: uniswap_v2"),
            "{message}"
        );
        assert!(
            message.contains("behind (last committed block): uniswap_v3 (block 50)"),
            "{message}"
        );
        assert!(message.contains("never updated: uniswap_v4"), "{message}");
    }

    #[test]
    fn a_tip_run_leaves_no_exchange_current_at_the_target() {
        let failure = PoolUpdateFailure {
            error: PoolRunError::Db(DbError::MissingRow("row".to_string())),
            rpc_url: "http://reth.local:8545".to_string(),
            chain_id: 8453,
            from_block: 1,
            to_block: None,
            resume: Some(vec![ExchangeResumeState {
                name: "uniswap_v2".to_string(),
                last_update_block: Some(26_059_263),
            }]),
        };
        let message = failure.message();
        assert!(
            !message.contains("current at the requested target"),
            "{message}"
        );
        assert!(
            message.contains("behind (last committed block): uniswap_v2 (block 26059263)"),
            "{message}"
        );
    }

    #[test]
    fn an_unavailable_snapshot_is_said_so() {
        let failure = PoolUpdateFailure {
            error: PoolRunError::Db(DbError::MissingRow("row".to_string())),
            rpc_url: "http://reth.local:8545".to_string(),
            chain_id: 8453,
            from_block: 1,
            to_block: Some(100),
            resume: None,
        };
        assert!(failure
            .message()
            .contains("exchange cursor state unavailable"),);
    }
}

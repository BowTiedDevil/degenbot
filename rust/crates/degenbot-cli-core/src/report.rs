//! Typed command reports (ADR-051 D1: execution returns typed results; the
//! facade renders them — Q1).

use std::path::PathBuf;

use alloy::primitives::U256;
use degenbot_db::ops::HealReport;
use degenbot_db::SchemaState;

use crate::error::{CliError, ExitCode};
use crate::pool::PoolFamily;

/// The typed result of one command execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandReport {
    /// A `database` command report.
    Database(DatabaseReport),
    /// An `exchange` command report.
    Exchange(ExchangeReport),
    /// A `pool` command report.
    Pool(PoolReport),
    /// An `aave` command report.
    Aave(AaveReport),
    /// A `fleet` command report.
    Fleet(FleetReport),
    /// A `path` command report.
    Path(PathReport),
}

impl CommandReport {
    /// The operator-facing lines the argv facade renders.
    #[must_use]
    pub fn render_lines(&self) -> Vec<String> {
        match self {
            Self::Database(report) => report.render_lines(),
            Self::Exchange(report) => report.render_lines(),
            Self::Pool(report) => report.render_lines(),
            Self::Aave(report) => report.render_lines(),
            Self::Fleet(report) => report.render_lines(),
            Self::Path(report) => report.render_lines(),
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

/// Whether an `exchange activate` flipped the row or found it already active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivateOutcome {
    /// The row was newly activated (or re-activated from inactive).
    Activated,
    /// The row was already active; nothing was written.
    AlreadyActive,
}

/// Whither an `exchange deactivate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeactivateOutcome {
    /// The row was newly deactivated.
    Deactivated,
    /// The row was already inactive.
    AlreadyDeactivated,
    /// The DB has no row for the pair.
    NoEntry,
}

/// The typed result of an `exchange` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExchangeReport {
    /// `exchange activate`.
    Activated {
        /// The resolved chain id.
        chain_id: u64,
        /// The human chain label.
        chain_label: &'static str,
        /// The human DEX label.
        display_name: &'static str,
        /// The DEX name slug.
        dex_slug: &'static str,
        /// Whether the row flipped or was already active.
        outcome: ActivateOutcome,
    },
    /// `exchange deactivate`.
    Deactivated {
        /// The resolved chain id.
        chain_id: u64,
        /// The human chain label.
        chain_label: &'static str,
        /// The human DEX label.
        display_name: &'static str,
        /// The DEX name slug.
        dex_slug: &'static str,
        /// The deactivation outcome.
        outcome: DeactivateOutcome,
    },
}

impl ExchangeReport {
    /// The operator-facing lines for this report.
    #[must_use]
    pub fn render_lines(&self) -> Vec<String> {
        match self {
            Self::Activated {
                chain_id,
                chain_label,
                display_name,
                outcome,
                ..
            } => match outcome {
                ActivateOutcome::Activated => vec![format!(
                    "Activated {display_name} on {chain_label} (chain ID {chain_id})."
                )],
                ActivateOutcome::AlreadyActive => {
                    vec!["Exchange is already activated.".to_string()]
                }
            },
            Self::Deactivated {
                chain_id,
                chain_label,
                display_name,
                outcome,
                ..
            } => match outcome {
                DeactivateOutcome::Deactivated => vec![format!(
                    "Deactivated {display_name} on {chain_label} (chain ID {chain_id})."
                )],
                DeactivateOutcome::AlreadyDeactivated => {
                    vec!["Exchange is already deactivated.".to_string()]
                }
                DeactivateOutcome::NoEntry => vec![format!(
                    "The database has no entry for {display_name} on {chain_label} \
                     (chain ID {chain_id})."
                )],
            },
        }
    }
}

/// The typed result of a `pool` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolReport {
    /// `pool update` advanced the chain.
    Updated {
        /// The chain advanced.
        chain_id: i64,
        /// The first block processed.
        from_block: u64,
        /// The last block advanced to.
        to_block: u64,
        /// Chunks committed.
        chunks_committed: usize,
        /// Pool rows written.
        total_pools_written: usize,
        /// Per-pool liquidity applies.
        total_liquidity_applies: usize,
    },
    /// `pool update` was cooperatively cancelled; committed chunks stay durable.
    UpdateCancelled {
        /// The chain.
        chain_id: i64,
    },
    /// `pool verify` compared the committed map against on-chain truth.
    Verified {
        /// The pool identifier.
        pool: String,
        /// The family.
        family: PoolFamily,
        /// The block the truth was read at.
        block_number: u64,
        /// The named divergences (empty = GREEN).
        divergences: Vec<degenbot_pool_updater::LiquidityDivergence>,
    },
}

impl PoolReport {
    /// The operator-facing lines for this report.
    #[must_use]
    pub fn render_lines(&self) -> Vec<String> {
        match self {
            Self::Updated {
                chain_id,
                from_block,
                to_block,
                chunks_committed,
                total_pools_written,
                total_liquidity_applies,
            } => vec![format!(
                "Chain {chain_id}: advanced {from_block}->{to_block} in {chunks_committed} \
                 chunks ({total_pools_written} pools written, {total_liquidity_applies} \
                 liquidity applies)."
            )],
            Self::UpdateCancelled { chain_id } => vec![format!(
                "Chain {chain_id}: cancelled (committed chunks stay durable)."
            )],
            Self::Verified {
                pool,
                family,
                block_number,
                divergences,
            } => verification_lines(pool, *family, *block_number, divergences),
        }
    }
}

/// The typed result of a `fleet` command (ADR-051 D6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetReport {
    /// The live posture echo: the six `cordon_*` values plus `posture`,
    /// rendered as one compact JSON object with sorted keys (the Python
    /// `json.dumps(effective, sort_keys=True)` line).
    Posture {
        /// The rendered effective policy (`{}` when the host echoed none).
        effective: String,
    },
}

impl FleetReport {
    /// The operator-facing lines for this report.
    #[must_use]
    pub fn render_lines(&self) -> Vec<String> {
        match self {
            Self::Posture { effective } => vec![effective.clone()],
        }
    }
}

/// The typed result of a `path` command (ADR-051 D6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathReport {
    /// `path add` enqueued the path.
    Added {
        /// The host's `detail` receipt.
        detail: String,
    },
    /// `path discover` completed a bounded sweep.
    Discovered {
        /// The host's `detail` receipt.
        detail: String,
    },
}

impl PathReport {
    /// The operator-facing lines for this report.
    #[must_use]
    pub fn render_lines(&self) -> Vec<String> {
        match self {
            Self::Added { detail } | Self::Discovered { detail } => vec![detail.clone()],
        }
    }
}

/// The green / red `pool verify` text.
fn verification_lines(
    pool: &str,
    family: PoolFamily,
    block_number: u64,
    divergences: &[degenbot_pool_updater::LiquidityDivergence],
) -> Vec<String> {
    use degenbot_pool_updater::LiquidityDivergence;
    if divergences.is_empty() {
        return vec![format!(
            "GREEN: {} pool {pool} matches on-chain truth at block {block_number}.",
            family.as_str()
        )];
    }
    let mut lines = vec![format!(
        "RED: {} divergence(s) for {} pool {pool} at block {block_number}:",
        divergences.len(),
        family.as_str()
    )];
    for divergence in divergences {
        lines.push(match divergence {
            LiquidityDivergence::TickGross {
                tick,
                expected,
                actual,
            } => format!("  tick {tick}: TickGross expected={expected} actual={actual}"),
            LiquidityDivergence::TickNet {
                tick,
                expected,
                actual,
            } => format!("  tick {tick}: TickNet expected={expected} actual={actual}"),
            LiquidityDivergence::BitmapWord {
                word,
                expected,
                actual,
            } => format!("  word {word}: BitmapWord expected={expected} actual={actual}"),
            LiquidityDivergence::TickCallReverted { tick } => {
                format!("  tick {tick}: TickCallReverted")
            }
            LiquidityDivergence::BitmapCallReverted { word } => {
                format!("  word {word}: BitmapCallReverted")
            }
        });
    }
    lines
}

/// One `aave update` market's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AaveUpdateOutcome {
    /// The market advanced.
    Advanced {
        /// First block processed.
        from_block: u64,
        /// Last block advanced to.
        to_block: u64,
        /// Chunks committed.
        chunks_committed: usize,
        /// Events applied.
        total_events_applied: usize,
    },
    /// The market's run was cooperatively cancelled.
    Cancelled,
    /// The market has no `last_update_block`; it is skipped (must be bootstrapped).
    NeedsBootstrap,
    /// `--dry-run`: the would-be advance, with nothing committed.
    DryRun {
        /// The market's `last_update_block`.
        last_update_block: i64,
        /// The resolved target (`None` = chain tip).
        to_block: Option<u64>,
    },
}

/// One `aave update` market row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AaveUpdateEntry {
    /// The chain.
    pub chain_id: i64,
    /// The market id.
    pub market_id: i64,
    /// The market name.
    pub market_name: String,
    /// The outcome.
    pub outcome: AaveUpdateOutcome,
}

/// One position row (scaled balance + token symbol).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AavePositionLine {
    /// The underlying token symbol (`Unknown` when unresolved).
    pub symbol: String,
    /// The scaled balance.
    pub balance: U256,
}

/// The typed result of an `aave` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AaveReport {
    /// `aave activate`.
    Activated {
        /// The chain id.
        chain_id: u64,
        /// The human chain label.
        chain_label: &'static str,
        /// The market id.
        market_id: i64,
        /// The on-chain market name.
        market_name: String,
        /// Whether the market was newly created.
        created: bool,
    },
    /// `aave deactivate`.
    Deactivated {
        /// The chain id.
        chain_id: u64,
        /// The market id, when a row was found.
        market_id: Option<i64>,
        /// The outcome.
        outcome: DeactivateOutcome,
    },
    /// `aave update`.
    Updated {
        /// Per-market outcomes.
        entries: Vec<AaveUpdateEntry>,
    },
    /// `aave position show`.
    Position {
        /// The user address (checksummed).
        user_address: String,
        /// The market name.
        market: String,
        /// The chain id.
        chain_id: u64,
        /// Collateral positions.
        collateral: Vec<AavePositionLine>,
        /// Debt positions.
        debt: Vec<AavePositionLine>,
    },
    /// `aave position show` found no market.
    PositionNoMarket {
        /// The market name.
        market: String,
        /// The chain id.
        chain_id: u64,
    },
    /// `aave position show` found no user row.
    PositionNoUser {
        /// The user address (checksummed).
        user_address: String,
        /// The market name.
        market: String,
        /// The chain id.
        chain_id: u64,
    },
}

impl AaveReport {
    /// The operator-facing lines for this report.
    #[must_use]
    pub fn render_lines(&self) -> Vec<String> {
        match self {
            Self::Activated {
                chain_id,
                chain_label,
                market_id,
                market_name,
                created,
            } => vec![
                format!("Activated Aave V3 on {chain_label} (chain ID {chain_id})."),
                format!("  Market: {market_name} (id={market_id}, created={created})."),
            ],
            Self::Deactivated {
                chain_id, outcome, ..
            } => match outcome {
                DeactivateOutcome::Deactivated => vec![format!(
                    "Deactivated Aave V3 on {} (chain ID {chain_id}).",
                    chain_label_for(*chain_id)
                )],
                DeactivateOutcome::AlreadyDeactivated => Vec::new(),
                DeactivateOutcome::NoEntry => {
                    vec![format!(
                        "The database has no entry for Aave V3 on {} (chain ID {chain_id}).",
                        chain_label_for(*chain_id)
                    )]
                }
            },
            Self::Updated { entries } => entries.iter().flat_map(entry_lines).collect(),
            Self::Position {
                user_address,
                market,
                chain_id,
                collateral,
                debt,
            } => position_lines(user_address, market, *chain_id, collateral, debt),
            Self::PositionNoMarket { market, chain_id } => {
                vec![format!(
                    "No market found with name '{market}' on chain {chain_id}."
                )]
            }
            Self::PositionNoUser {
                user_address,
                market,
                chain_id,
            } => vec![format!(
                "No Aave user found for address {user_address} in market '{market}' on chain \
                 {chain_id}."
            )],
        }
    }
}

/// The human chain label used in the aave report lines.
fn chain_label_for(chain_id: u64) -> String {
    match chain_id {
        1 => "Ethereum".to_string(),
        8453 => "Base".to_string(),
        other => other.to_string(),
    }
}

/// The lines for one `aave update` market.
fn entry_lines(entry: &AaveUpdateEntry) -> Vec<String> {
    let AaveUpdateEntry {
        chain_id,
        market_id,
        market_name,
        outcome,
    } = entry;
    match outcome {
        AaveUpdateOutcome::Advanced {
            from_block,
            to_block,
            chunks_committed,
            total_events_applied,
        } => vec![format!(
            "Chain {chain_id} market {market_id} ({market_name}): advanced {from_block}-> \
             {to_block} in {chunks_committed} chunks ({total_events_applied} events applied)."
        )],
        AaveUpdateOutcome::Cancelled => vec![format!(
            "Chain {chain_id} market {market_id}: cancelled (committed chunks stay durable)."
        )],
        AaveUpdateOutcome::NeedsBootstrap => vec![format!(
            "Chain {chain_id} market {market_id} ({market_name}): needs bootstrapping \
             (last_update_block is None); skipping. Bootstrap the stamp before running."
        )],
        AaveUpdateOutcome::DryRun {
            last_update_block,
            to_block,
        } => vec![format!(
            "Dry run: would advance chain {chain_id} market {market_id} ({market_name}) from \
             block {last_update_block} to {} (no changes committed).",
            render_opt_block(*to_block)
        )],
    }
}

/// Render an optional resolved block the way Python's `{value!r}` does.
fn render_opt_block(block: Option<u64>) -> String {
    block.map_or_else(|| "None".to_string(), |n| n.to_string())
}

/// The ported `aave position show` body.
fn position_lines(
    user_address: &str,
    market: &str,
    chain_id: u64,
    collateral: &[AavePositionLine],
    debt: &[AavePositionLine],
) -> Vec<String> {
    let mut lines = vec![
        format!("Aave V3 Positions for {user_address}"),
        format!("Market: {market} (Chain: {chain_id})"),
        "=".repeat(60),
    ];
    if collateral.is_empty() {
        lines.push("No collateral positions found.".to_string());
    } else {
        lines.push("Collateral Positions:".to_string());
        lines.push("-".repeat(60));
        lines.extend(
            collateral
                .iter()
                .map(|p| format!("  {}: {} (scaled)", p.symbol, p.balance)),
        );
    }
    if debt.is_empty() {
        lines.push("No debt positions found.".to_string());
    } else {
        lines.push("Debt Positions:".to_string());
        lines.push("-".repeat(60));
        lines.extend(
            debt.iter()
                .map(|p| format!("  {}: {} (scaled)", p.symbol, p.balance)),
        );
    }
    lines
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
        SchemaState::LegacyAlembic => format!(
            "Schema state: {label}. Would cutover from Alembic to Rust ownership (drop \
             `alembic_version`, stamp `_degenbot_db_schema_version`)."
        ),
        SchemaState::RustOwned { .. } => {
            format!("Schema state: {label}. Already Rust-owned — cutover is a no-op.")
        }
        SchemaState::FreshStandalone { .. } => {
            format!("Schema state: {label}. No legacy history — nothing to cutover.")
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
        SchemaState::LegacyAlembic => format!(
            "Schema state: {label}. Would heal: rebuild at the Rust head schema, copy all \
             rows, drop alembic_version, stamp _degenbot_db_schema_version, atomic-swap with \
             a *.bak backup."
        ),
        SchemaState::RustOwned { .. } => {
            format!("Schema state: {label}. Already Rust-owned — heal is a no-op.")
        }
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
        SchemaState::LegacyAlembic => "legacy_alembic",
        SchemaState::FreshStandalone { .. } => "fresh_standalone",
        SchemaState::RustOwned { .. } => "rust_owned",
        SchemaState::Unrecognized => "unrecognized",
    }
}

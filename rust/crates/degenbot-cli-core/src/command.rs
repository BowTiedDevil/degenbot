//! The constructor-parsed command model (ADR-051 D1).
//!
//! One `Command` enum, one execution entry. The argv facade maps clap matches
//! into a `Command` value; it never re-encodes semantics.

use crate::context::CliContext;
use crate::database;
use crate::error::CliError;
use crate::prompt::{PromptPlan, Prompter};
use crate::report::CommandReport;

/// A console command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// The `database` command group.
    Database(DatabaseCommand),
}

impl Command {
    /// The command's declared confirmation policy (ADR-051 D4).
    #[must_use]
    pub fn prompt_plan(&self, ctx: &CliContext<'_>) -> PromptPlan {
        match self {
            Self::Database(command) => command.prompt_plan(ctx),
        }
    }

    /// Execute the command, asking `prompter` per the command's [`PromptPlan`].
    ///
    /// # Errors
    ///
    /// [`CliError`] for a declined prompt, a database failure, a schema refusal,
    /// or an unresolved driver-domain value.
    pub fn execute(
        &self,
        ctx: &CliContext<'_>,
        prompter: &dyn Prompter,
    ) -> Result<CommandReport, CliError> {
        match self {
            Self::Database(command) => {
                database::execute(command, ctx, prompter).map(CommandReport::Database)
            }
        }
    }
}

/// The `database` command group (the template group; ports
/// `src/degenbot/cli/database.py` arm for arm).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseCommand {
    /// Back up the database to its `.db.bak` sibling. Confirms replacement only
    /// when the target already exists.
    Backup,
    /// Remove and recreate the database (confirms unless `--force`).
    Reset {
        /// Skip the confirmation prompt.
        force: bool,
    },
    /// RETIRED: render the pointed-retirement error and exit 1. The database
    /// upgrades itself at open (ADR-052).
    Upgrade {
        /// Accepted for argv parity; the retirement error fires before any prompt.
        force: bool,
    },
    /// Compact the database (`VACUUM`); never prompts.
    Compact,
    /// Flip an Alembic-stamped DB into Rust schema ownership (ADR-010).
    Cutover {
        /// Report the schema state and what cutover would do; write nothing.
        dry_run: bool,
        /// Skip the confirmation prompt.
        force: bool,
    },
    /// Rebuild an Alembic-stamped DB into Rust ownership out-of-place (ADR-011).
    Heal {
        /// Report the schema state and what heal would do; write nothing.
        dry_run: bool,
        /// Skip the confirmation prompt.
        force: bool,
    },
    /// Read-only schema-state inspection; never triggers a write.
    Inspect,
}

impl DatabaseCommand {
    /// The arm's declared confirmation policy — ported verbatim from the click
    /// handlers (backup: replace-target-exists; reset/cutover/heal: unless-force;
    /// compact/inspect: none).
    ///
    /// `upgrade` reports [`PromptPlan::UnlessForce`] for argv parity with the
    /// retired handler, but its arm returns [`CliError::DatabaseUpgradeRetired`]
    /// before any prompt is reached.
    #[must_use]
    pub fn prompt_plan(&self, ctx: &CliContext<'_>) -> PromptPlan {
        match self {
            Self::Backup => PromptPlan::OnCondition(
                database::database_backup_path(&ctx.database_path().value).exists(),
            ),
            Self::Reset { .. }
            | Self::Upgrade { .. }
            | Self::Cutover { .. }
            | Self::Heal { .. } => PromptPlan::UnlessForce,
            Self::Compact | Self::Inspect => PromptPlan::None,
        }
    }
}

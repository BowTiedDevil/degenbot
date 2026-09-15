//! `degenbot-cli-core` — the clap-free semantics home for the degenbot console
//! (ADR-051 D1/D2).
//!
//! The console has two first-class front ends: the pure-Rust argv facade
//! (`degenbot-cli`, clap) and the Python passthrough (`src/degenbot/_cli.py`).
//! Both map argv into the SAME command model declared here, so command
//! semantics — prompt policy, dry-run text, exit-code mapping — exist exactly
//! once. This crate is deliberately **pyo3-free** (so the pure-Rust consumer can
//! depend on it) and **clap-free/indicatif-free** (argv spelling and progress
//! rendering are the facade's job; asserted by `just check-cli-core-purity`).
//!
//! # Shape
//!
//! - [`Command`] + [`DatabaseCommand`]: the constructor-parsed command enum.
//! - [`Command::execute`] / [`run`]: the ONE execution entry, returning a typed
//!   [`CommandReport`].
//! - [`CliError`] → [`ExitCode`]: declared at exactly one `From` site; the
//!   workspace `exit = "deny"` lint stands, so `run` returns codes and never
//!   aborts the process. The typed fleet boot refusal (FF-T1) maps to
//!   `EX_CONFIG` 78 (lifted out of `DegenbotCLI.invoke`).
//! - [`PromptPlan`] + [`Prompter`]: interactive policy is declared data, ported
//!   verbatim from the click handlers and audited, never redesigned (D4).
//!
//! # Database arms are the template
//!
//! [`DatabaseCommand`] mirrors `src/degenbot/cli/database.py` arm for arm:
//! `backup` / `reset` / `compact` / `cutover` / `heal` / `inspect`, plus the
//! retired `upgrade` subcommand. Each arm delegates to the degenbot-db ops
//! (`backup_database`, `create_new_database`, `compact_database`,
//! `convert_alembic_to_rust_owned`, `heal_database`) and keeps the exact
//! dry-run/confirm flow. The auto-heal epic (ergo `6ATMVN`) owns schema
//! self-healing inside `ensure_schema`; these arms never call `heal` on any
//! other command's behalf.

pub mod command;
pub mod context;
pub mod database;
pub mod error;
pub mod prompt;
pub mod report;

pub use command::{Command, DatabaseCommand};
pub use context::CliContext;
pub use database::database_backup_path;
pub use error::{CliError, ExitCode};
pub use prompt::{PromptPlan, Prompter};
pub use report::{
    schema_state_label, CommandOutcome, CommandReport, CutoverOutcome, DatabaseReport, DryRunKind,
};

/// The ONE execution entry: run `command` against `ctx`, asking `prompter` when
/// the command's [`PromptPlan`] requires it.
///
/// Returns a [`CommandOutcome`] carrying the typed [`CommandReport`] (on
/// success) and the [`ExitCode`] derived at the single `From<&CliError>` site.
/// The argv facade renders the report and never sees a process abort.
#[must_use]
pub fn run(command: &Command, ctx: &CliContext<'_>, prompter: &dyn Prompter) -> CommandOutcome {
    let result = command.execute(ctx, prompter);
    let exit_code = match &result {
        Ok(_) => ExitCode::Success,
        Err(err) => ExitCode::from(err),
    };
    CommandOutcome::new(result, exit_code)
}

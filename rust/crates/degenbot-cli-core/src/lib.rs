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
//! - [`Command`] + the per-group enums: the constructor-parsed command model.
//! - [`Command::execute`] / [`run_with_cancel`]: the execution entries,
//!   returning a typed [`CommandReport`].
//! - [`CliError`] → [`ExitCode`]: declared at exactly one `From` site; the
//!   workspace `exit = "deny"` lint stands, so `run` returns codes and never
//!   aborts the process. The typed fleet boot refusal (FF-T1) maps to
//!   `EX_CONFIG` 78 (lifted out of `DegenbotCLI.invoke`).
//! - [`PromptPlan`] + [`Prompter`]: interactive policy is declared data, ported
//!   verbatim from the click handlers and audited, never redesigned (D4).
//! - [`CancelHandle`]: the cooperative cancel carrier the updater arms thread
//!   into `run_pool_update` / `run_aave_update` (the facade owns the SIGINT
//!   policy, ADR-051 D7).
//!
//! # Groups
//!
//! - `database` ([`DatabaseCommand`]): the template group; ports
//!   `src/degenbot/cli/database.py` arm for arm.
//! - `exchange` ([`ExchangeCommand`]): the 34 Python click verbs collapse to
//!   one data-driven command resolving `(chain, name)` through the
//!   `degenbot-uniswap` deployments registry (ADR-051 D5).
//! - `pool` ([`PoolCommand`]): the `degenbot-pool-updater` chunk loop +
//!   on-chain-truth verify.
//! - `aave` ([`AaveCommand`]): the `degenbot-aave` market run + row flips.
//! - `fleet` ([`FleetCommand`]): the live cordon posture over the
//!   operator command channel (ADR-051 D6).
//! - `path` ([`PathCommand`]): live add-path / bounded discovery over
//!   the same operator command channel.

pub mod aave;
pub mod block;
pub mod cancel;
pub mod command;
pub mod context;
pub mod database;
pub mod error;
pub mod exchange;
pub mod fleet;
pub mod operator;
pub mod path;
pub mod pool;
pub mod prompt;
pub mod report;

pub use aave::{resolve_aave_deployment, AaveCommand, AaveDeployment, AAVE_DEPLOYMENTS};
pub use block::{
    parse_to_block, resolve_chain_selector, resolve_to_block, BlockTag, ToBlockSpec,
    DEFAULT_CHUNK_SIZE, DEFAULT_TO_BLOCK, DEFAULT_VERIFY_ALL_INTERVAL,
};
pub use cancel::CancelHandle;
pub use command::{Command, DatabaseCommand};
pub use context::CliContext;
pub use database::database_backup_path;
pub use error::{CliError, ExitCode};
pub use exchange::{
    resolve_deployment, ExchangeCommand, ExchangeDeployment, PoolManagerDeployment,
    RETIRED_EXCHANGES,
};
pub use fleet::FleetCommand;
pub use operator::{
    parse_hop_token, parse_sim_intake_floor, render_json_sorted, resolve_socket,
    validate_posture_patch, PathDirection, PathFamily, PathStep, PosturePatchEntry,
    PosturePatchValue, WireRequest, WireResponse, FLEET_POSTURE_THRESHOLD_KEYS,
    SIM_INTAKE_FLOOR_RESTORE, SOCKET_DEFAULT, SOCKET_ENV,
};
pub use path::PathCommand;
pub use pool::{PoolCommand, PoolFamily};
pub use prompt::{PromptPlan, Prompter};
pub use report::{
    schema_state_label, AavePositionLine, AaveReport, AaveUpdateEntry, AaveUpdateOutcome,
    ActivateOutcome, CommandOutcome, CommandReport, CutoverOutcome, DatabaseReport,
    DeactivateOutcome, DryRunKind, ExchangeReport, FleetReport, PathReport, PoolReport,
};

/// The ONE execution entry: run `command` against `ctx`, asking `prompter` when
/// the command's [`PromptPlan`] requires it.
///
/// Returns a [`CommandOutcome`] carrying the typed [`CommandReport`] (on
/// success) and the [`ExitCode`] derived at the single `From<&CliError>` site.
/// A fresh [`CancelHandle`] is created (no SIGINT wiring); use
/// [`run_with_cancel`] to share the facade's handle.
#[must_use]
pub fn run(command: &Command, ctx: &CliContext<'_>, prompter: &dyn Prompter) -> CommandOutcome {
    run_with_cancel(command, ctx, prompter, &CancelHandle::new())
}

/// The cancellable execution entry (ADR-051 D7).
#[must_use]
pub fn run_with_cancel(
    command: &Command,
    ctx: &CliContext<'_>,
    prompter: &dyn Prompter,
    cancel: &CancelHandle,
) -> CommandOutcome {
    let result = command.execute_with_cancel(ctx, prompter, cancel);
    let exit_code = match &result {
        Ok(_) => ExitCode::Success,
        Err(err) => ExitCode::from(err),
    };
    CommandOutcome::new(result, exit_code)
}

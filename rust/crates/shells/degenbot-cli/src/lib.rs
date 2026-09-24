//! `degenbot-cli` - the degenbot console binary (ADR-051 D2).
//!
//! One argv facade, one command model. This crate owns exactly the things
//! [`degenbot_cli_core`] cannot own by charter:
//!
//! - **argv declaration** ([`argv`]): the clap v4 derive tree for the whole
//!   command vocabulary, and the argv -> [`Command`](degenbot_cli_core::Command)
//!   mapping. See `argv`'s module docs for where clap's shape differs from the
//!   retired Python click tree.
//! - **rendering** ([`render`]): typed [`CommandReport`](degenbot_cli_core::CommandReport)
//!   lines to stdout, typed [`CliError`](degenbot_cli_core::CliError) to stderr
//!   with its `ExitCode`.
//! - **interaction** ([`prompt`]): the stdin/stdout [`Prompter`](degenbot_cli_core::Prompter)
//!   the arms ask; the prompt *policy* stays declared data in cli-core (D4).
//! - **sinks** ([`sinks`]): the tracing registry + console fmt layer, booted in
//!   the same order as the Python driver (typed config first, then the
//!   subscriber), and **progress** ([`progress`]): the `indicatif` bar, which
//!   exists only here (D9).
//! - **SIGINT ownership** ([`signal`], D7): first Ctrl+C feeds cli-core's
//!   [`CancelHandle`](degenbot_cli_core::CancelHandle); a second aborts.
//!
//! cli-core stays clap-free and indicatif-free (asserted by
//! `just check-cli-core-purity`); this crate stays free of every domain-engine
//! dependency (asserted by `just check-cli-shell-purity`).

pub mod argv;
pub mod progress;
pub mod prompt;
pub mod render;
pub mod signal;
pub mod sinks;

pub use argv::{Cli, Commands};

use std::io::Write as _;

use degenbot_cli_core::CancelHandle;
use degenbot_config::ProcessEnv;

/// The banner `--version` prints: the workspace version (ADR-009 lockstep) plus
/// the shared build receipt, embedded by `build.rs`.
///
/// The fingerprint half is byte-identical to the Python FFI's
/// `degenbot._ffi.build_fingerprint()`, so the two entry surfaces can be
/// compared directly rather than trusted.
pub const VERSION_LINE: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (build ",
    env!("DEGENBOT_CLI_BUILD_NUMBER"),
    " ",
    env!("DEGENBOT_CLI_BUILD_FINGERPRINT"),
    ")"
);

/// Parse argv, boot the sinks, resolve the argv into a
/// [`Command`](degenbot_cli_core::Command), run it and render the result.
///
/// Returns the process exit code: clap owns `--help`/`--version`/usage errors
/// (its own exit codes apply), a refused typed config is `2` (the same refusal
/// exit the Python module init uses), and every command outcome maps through
/// cli-core's single `CliError -> ExitCode` site.
#[must_use]
pub fn run() -> i32 {
    let cli = <Cli as clap::Parser>::parse();
    execute(&cli)
}

fn execute(cli: &Cli) -> i32 {
    let env = ProcessEnv;
    if cli.command.is_none() {
        argv::write_missing_subcommand_error();
        return 2;
    }

    // Sinks before anything that emits, exactly like the Python module init.
    let _telemetry = match sinks::boot() {
        Ok(telemetry) => telemetry,
        Err(refusal) => {
            let _ = writeln!(std::io::stderr().lock(), "{refusal}");
            return 2;
        }
    };

    let ctx = argv::context(cli, &env);
    let command = match argv::resolve_with_env(cli, &env) {
        Ok(command) => command,
        Err(error) => return render::error(&error),
    };

    let prompter = prompt::ConsolePrompter::new();
    let cancel = CancelHandle::new();
    let _sigint = signal::install(cancel.clone());

    let outcome = degenbot_cli_core::run_with_cancel(&command, &ctx, &prompter, &cancel);
    render::outcome(&outcome)
}

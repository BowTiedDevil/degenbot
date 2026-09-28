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

/// Parse `args` (argv WITHOUT the program name), boot the sinks, resolve the
/// argv into a [`Command`](degenbot_cli_core::Command), run it and render the
/// result. This is the ONE console composition root: the `degenbot` binary and
/// the Python `cli_main` passthrough both land here, so their sequences cannot
/// drift apart.
///
/// Returns the process exit code and never exits the process, so an embedded
/// host can drive the console directly: clap owns `--help`/`--version`/usage
/// errors (its own exit codes apply), a refused typed config is `2` (the same
/// refusal exit the Python module init uses), and every command outcome maps
/// through cli-core's single `CliError -> ExitCode` site.
///
/// There is deliberately no host parameter here. The native and Python hosts
/// are not abstracted behind a flag: their genuine difference lives upstream
/// of this function — the Python `#[pymodule]` init has already installed the
/// typed config and the driver's log forwarder by the time this runs, while
/// the native path gets both from `sinks::boot()` below — and `boot`'s
/// first-wins installs keep that difference in place. A switch would hide the
/// difference it cannot actually remove.
#[must_use]
pub fn run_args<T>(args: &[T]) -> i32
where
    T: Into<std::ffi::OsString> + Clone,
{
    let parsed = <Cli as clap::Parser>::try_parse_from(
        std::iter::once(std::ffi::OsString::from("degenbot"))
            .chain(args.iter().map(|arg| arg.clone().into())),
    );
    let cli = match parsed {
        Ok(cli) => cli,
        Err(error) => {
            // clap's `Error::exit` is print-then-exit-code with the print
            // result discarded; a broken pipe on `--help` is swallowed the
            // same way here.
            let code = error.exit_code();
            let _ = error.print();
            return code;
        }
    };

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

    let ctx = argv::context(&cli, &env);
    let command = match argv::resolve_with_env(&cli, &env) {
        Ok(command) => command,
        Err(error) => return render::error(&error),
    };

    let prompter = prompt::ConsolePrompter::new();
    let cancel = CancelHandle::new();
    let _sigint = signal::install(cancel.clone());

    let outcome = degenbot_cli_core::run_with_cancel(&command, &ctx, &prompter, &cancel);
    render::outcome(&outcome)
}

/// The binary entry: collect the process argv (minus the program name) and
/// delegate to the console composition root.
#[must_use]
pub fn run() -> i32 {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    run_args(&args)
}

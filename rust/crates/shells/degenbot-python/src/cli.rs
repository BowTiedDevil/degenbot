//! The Python-side console seam (ADR-051 D3): argv passthrough into the
//! Rust-owned console.
//!
//! `cli_main` is the only call the Python driver makes (`_cli.py` raises
//! `SystemExit(_ffi.cli_main(sys.argv[1:]))`). This PyO3 seam forwards argv
//! VERBATIM through the SAME clap tree the `degenbot` binary parses
//! (`degenbot_cli::argv`) and composes the SAME sequence the binary composes
//! (`sinks::boot` -> `argv::context`/`resolve_with_env` -> `signal::install` ->
//! `run_with_cancel` -> `render`), so both entry surfaces share one command
//! model (degenbot-cli-core) and one rendering. There is no translation layer,
//! no second argv vocabulary, and no re-implemented semantics here.
//!
//! The GIL is released for the whole run ([`Python::detach`]): a console
//! command can spend minutes in RPC/DB work, and holding the GIL would wedge
//! every other Python thread. The typed `CliError -> ExitCode` mapping stays
//! in degenbot-cli-core (the one `From<&CliError>` site); this module only
//! renders the typed outcome.

use std::io::Write as _;

use clap::Parser as _;
use degenbot_cli_core::CancelHandle;
use degenbot_config::ProcessEnv;
use pyo3::prelude::*;

/// Run the degenbot console with `args` (argv WITHOUT the program name) and
/// return the process exit code.
///
/// Args:
///     `args`: the console argv, verbatim (`sys.argv[1:]`).
///
/// Returns:
///     The process exit code: clap owns `--help`/`--version`/usage codes, a
///     refused typed config is `2` (the same refusal exit the Python module
///     init uses), and every command outcome maps through degenbot-cli-core's
///     single `CliError -> ExitCode` site (including the fleet boot refusal,
///     `EX_CONFIG` 78).
///
/// # Errors
///
/// Never in practice: every arm returns a numeric code rather than raising.
/// The `PyResult` return is the `#[pyfunction]` contract.
#[pyfunction]
pub fn cli_main(py: Python<'_>, args: Vec<String>) -> PyResult<i32> {
    // Release the GIL across the FULL run, then run the console.
    Ok(py.detach(move || run(&args)))
}

/// The console run itself, composed exactly like `degenbot_cli::execute`.
fn run(args: &[String]) -> i32 {
    // 1. Parse argv through the ONE clap tree. clap owns --help/--version/
    // usage diagnostics: print through clap's own channel and honor its exit
    // code, exactly as `<Cli as Parser>::parse()` does in the binary.
    let parsed = degenbot_cli::argv::Cli::try_parse_from(
        std::iter::once("degenbot".to_owned()).chain(args.iter().cloned()),
    );
    let cli = match parsed {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            let _ = error.print();
            return code;
        }
    };
    if cli.command.is_none() {
        degenbot_cli::argv::write_missing_subcommand_error();
        return 2;
    }

    // 2. Sinks before anything that emits, mirroring the binary's boot order.
    // The Python driver already installed its Rust->Python log forwarder at
    // module import, so on this path the global-subscriber install is a
    // no-op (`boot` logs the already-installed warning); the typed-config
    // refusal is still honored and returns the same `2` the module init uses.
    let _telemetry = match degenbot_cli::sinks::boot() {
        Ok(telemetry) => telemetry,
        Err(refusal) => {
            let _ = writeln!(std::io::stderr().lock(), "{refusal}");
            return 2;
        }
    };

    // 3. Resolve argv -> Command (the process env is the only env source; the
    // resolver cascade is degenbot-config's, ADR-051 D8).
    let env = ProcessEnv;
    let ctx = degenbot_cli::argv::context(&cli, &env);
    let command = match degenbot_cli::argv::resolve_with_env(&cli, &env) {
        Ok(command) => command,
        Err(error) => return degenbot_cli::render::error(&error),
    };

    // 4. The interaction + cancel policy (stdlib prompter; SIGINT owned by the
    // facade on both entry paths, ADR-051 D7).
    let prompter = degenbot_cli::prompt::ConsolePrompter::new();
    let cancel = CancelHandle::new();
    let _sigint = degenbot_cli::signal::install(cancel.clone());

    // 5. The single execution entry + the single renderer.
    let outcome = degenbot_cli_core::run_with_cancel(&command, &ctx, &prompter, &cancel);
    degenbot_cli::render::outcome(&outcome)
}

#[cfg(test)]
mod tests {
    use degenbot_cli_core::{CliError, ExitCode};

    #[test]
    fn boot_refusal_maps_to_ex_config() {
        // FF-T1: the typed fleet boot refusal is the lone EX_CONFIG (78) arm.
        // Pin it at the Python seam even though no console arm constructs it
        // yet, so the passthrough cannot silently regress the code.
        let error = CliError::BootRefused("fleet refused".to_string());
        assert_eq!(ExitCode::from(&error).code(), 78);
    }

    #[test]
    fn retired_upgrade_points_at_heal() {
        // ADR-052 D4: the dead subcommand renders the pointed retirement line.
        let message = CliError::DatabaseUpgradeRetired.message();
        assert!(message.contains("upgrades itself at open"));
        assert!(message.contains("degenbot database heal"));
    }
}

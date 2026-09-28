//! The Python-side console seam (ADR-051 D3): argv passthrough into the
//! Rust-owned console.
//!
//! `cli_main` is the only call the Python driver makes (`_cli.py` raises
//! `SystemExit(_ffi.cli_main(sys.argv[1:]))`). This PyO3 seam forwards argv
//! VERBATIM into `degenbot_cli::run_args`, the ONE composition root the
//! `degenbot` binary also uses, so both entry surfaces share one command
//! model (degenbot-cli-core), one argv vocabulary, and one rendering. There
//! is no translation layer, no second argv vocabulary, and no re-implemented
//! semantics here.
//!
//! The GIL is released for the whole run ([`Python::detach`]): a console
//! command can spend minutes in RPC/DB work, and holding the GIL would wedge
//! every other Python thread.
//!
//! The host difference stays upstream of the composition root: the
//! `#[pymodule]` init has already installed the typed config and the driver's
//! log forwarder before this seam is called, and the facade's `sinks::boot()`
//! honors both with first-wins installs — including the typed-config refusal
//! exit (`2`) and the already-installed-subscriber warning.

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

/// The console run itself: a delegation into the facade's single composition
/// root. The host difference stays upstream — see the module docs.
fn run(args: &[String]) -> i32 {
    degenbot_cli::run_args(args)
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

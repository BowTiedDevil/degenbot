//! stdout/stderr rendering of typed console results (ADR-051 D2/Q1).
//!
//! Execution returns typed values (cli-core's `CommandReport`) and typed
//! failures (`CliError`); rendering is the facade's job. Reports render as
//! human lines on stdout - the same body text the ported click handlers echoed
//! (`CommandReport::render_lines`). Failures render as one line on stderr and
//! carry cli-core's single `CliError -> ExitCode` mapping.

use std::io::Write as _;

use degenbot_cli_core::{CliError, CommandOutcome, ExitCode};

/// Render a completed run: report lines to stdout, the typed error to stderr,
/// and the outcome's exit code.
#[must_use]
pub fn outcome(outcome: &CommandOutcome) -> i32 {
    let mut stdout = std::io::stdout().lock();
    if let Some(report) = outcome.report() {
        for line in report.render_lines() {
            let _ = writeln!(stdout, "{line}");
        }
    }
    let _ = stdout.flush();
    if let Some(error) = outcome.error() {
        write_error(error);
    }
    outcome.exit_code.code()
}

/// Render a typed failure that never reached execution (an argv/resolution
/// refusal) and return its exit code.
#[must_use]
pub fn error(error: &CliError) -> i32 {
    write_error(error);
    ExitCode::from(error).code()
}

/// The operator-facing stderr line for `error`.
///
/// The fleet boot refusal renders its named fail-fast line; every other arm
/// renders [`CliError::message`] verbatim. No in-message `[tag]` prefix — the
/// console area is derived from the closed domain target (ADR-043 §7).
fn write_error(error: &CliError) {
    let mut stderr = std::io::stderr().lock();
    match error {
        CliError::BootRefused(message) => {
            let _ = writeln!(stderr, "REFUSED — {message}");
        }
        other => {
            let _ = writeln!(stderr, "{}", other.message());
        }
    }
}

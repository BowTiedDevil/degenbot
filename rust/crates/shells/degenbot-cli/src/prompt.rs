//! The stdin/stdout [`Prompter`] (ADR-051 D4/D2).
//!
//! cli-core owns the prompt *policy* as declared data (`PromptPlan`) and the
//! *message text* each arm asks with; this module owns only the interaction:
//! ask on stdout, read one line from stdin, and mirror `click.confirm`'s
//! semantics - a bare Enter yields the default, anything that is not `y`/`yes`
//! declines, and EOF declines (click raises `Abort` on a closed stdin, which
//! the arm maps to [`CliError::Aborted`](degenbot_cli_core::CliError::Aborted)).
//!
//! No terminal control is used: the progress bar owns stderr, the prompt owns
//! stdout, so the two never fight over the same draw surface (ADR-051 D9).

use std::io::{BufRead, Write};

use degenbot_cli_core::Prompter;

/// The real stdin/stdout prompter.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConsolePrompter;

impl ConsolePrompter {
    /// A prompter over the process's stdin/stdout.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Prompter for ConsolePrompter {
    fn confirm(&self, message: &str, default: bool) -> bool {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        let stdout = std::io::stdout();
        let mut writer = stdout.lock();
        confirm_with(&mut reader, &mut writer, message, default)
    }
}

/// Ask `message` on `writer` and read one line from `reader`.
///
/// `default` is what a bare Enter yields; a closed stdin (EOF) declines, the
/// way `click.confirm` aborts on EOF. Split from [`ConsolePrompter`] so the
/// interaction is unit-testable without touching the process streams.
#[must_use]
pub fn confirm_with<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    message: &str,
    default: bool,
) -> bool {
    let suffix = if default { "[Y/n]" } else { "[y/N]" };
    let _ = write!(writer, "{message} {suffix} ");
    let _ = writer.flush();
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => false,
        Ok(_) => match line.trim().to_ascii_lowercase().as_str() {
            "" => default,
            "y" | "yes" => true,
            _ => false,
        },
    }
}

//! Aggregate configuration error type (fail-closed loader).

use std::fmt;

/// All problems found while loading a [`BotConfig`](crate::BotConfig).
///
/// The loader is fail-closed: parse errors and unknown keys are collected
/// and reported together instead of silently falling back per key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigError {
    /// One human-readable line per problem.
    pub problems: Vec<String>,
}

impl ConfigError {
    /// Build from a non-empty problem list.
    pub(crate) fn of(problems: Vec<String>) -> Self {
        Self { problems }
    }

    /// `true` when no problems were recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.problems.is_empty()
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "bot configuration invalid ({} problem(s)):",
            self.problems.len()
        )?;
        for p in &self.problems {
            writeln!(f, "  - {p}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

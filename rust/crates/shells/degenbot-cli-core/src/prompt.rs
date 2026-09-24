//! Interactive policy as declared data (ADR-051 D4).
//!
//! Each command carries a [`PromptPlan`]; the plan declares WHEN the operator is
//! asked, never WHAT is asked (the arm owns the ported message text). Ported
//! verbatim from the click handlers and audited, never redesigned.

/// Asks the operator a yes/no question.
pub trait Prompter {
    /// Ask `message`, returning the answer. `default` is the value a bare Enter
    /// yields (every ported console prompt defaults to `false`).
    fn confirm(&self, message: &str, default: bool) -> bool;
}

/// A command's declared confirmation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptPlan {
    /// Never prompt (`compact` / `inspect`).
    None,
    /// Prompt unless the arm's `--force` flag is set (`reset` / `upgrade` /
    /// `cutover` / `heal`).
    UnlessForce,
    /// Prompt only when the named runtime condition holds — the backup arm's
    /// `backup-target-exists` condition.
    OnCondition(bool),
}

impl PromptPlan {
    /// Whether this plan asks, given the arm's `--force` flag.
    #[must_use]
    pub const fn asks(self, force: bool) -> bool {
        match self {
            Self::None => false,
            Self::UnlessForce => !force,
            Self::OnCondition(condition) => condition,
        }
    }
}

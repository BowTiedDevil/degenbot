//! Interactive policy as declared data (ADR-051 D4).
//!
//! Each command carries a [`PromptPlan`]; the plan declares WHEN the operator is
//! asked, never WHAT is asked (the arm owns the message text).

pub trait Prompter {
    /// Ask `message`, returning the answer. `default` is the value a bare Enter
    /// yields (every console prompt defaults to `false`).
    fn confirm(&self, message: &str, default: bool) -> bool;
}

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
    #[must_use]
    pub const fn asks(self, force: bool) -> bool {
        match self {
            Self::None => false,
            Self::UnlessForce => !force,
            Self::OnCondition(condition) => condition,
        }
    }
}

//! The capital posture of one simulated execution, and the override plan it
//! projects to.
//!
//! Two representations, and exactly one of each per session:
//!
//! - [`ProbeCapital`] — the MODE. It names what the session (or an explicit
//!   execution probe) assumes about the execution target's own capital:
//!   `Production` (the account's real balances — no override at all),
//!   `SelfFund` (the known asset set installed at the executor, with the
//!   self-funded stream encoding), or `ZeroBalance` (the known asset set
//!   stripped from the executor, with the in-path-flash encoding). It is the
//!   ONE capital value an operator, a driver, or the FFI names.
//! - [`CapitalOverridePlan`] — the PLAN. The same mode with the account it
//!   applies to BOUND, produced ONCE where a deployment is composed
//!   (`degenbot_arbitrage::SimulationDeployment`) and consumed by
//!   [`crate::SimulationOverrideParams`]. Binding the address at composition is
//!   what keeps a probe's capital on the executor it names in both deployment
//!   arms (declared code and deployed code), independent of whichever account a
//!   settlement warmup keyed.
//!
//! # One value, one projection
//!
//! [`ProbeCapital::override_plan`] is the whole mode→plan conversion, and it is
//! the only one: a deployment stores the plan, the override application reads
//! it, and nothing re-derives a "self-fund" or "zero-balance" boolean from a
//! neighbouring axis. The encode axes a mode implies (`funding` and the grammar's
//! zero-balance stance) are derived from the SAME value at the same composition
//! point, so a session can never fund one posture and override another.
//!
//! The modes are mutually exclusive BY CONSTRUCTION: they are variants, not a
//! pair of booleans, so "self-fund AND zero-balance" has no representation to
//! refuse.

use alloy::primitives::Address;

/// The capital posture one session — or one explicit execution probe — assumes
/// for its execution target.
///
/// Ordinary solver candidates always use `Production`; the two probe modes are
/// explicit operator/driver choices and exclude each other by construction.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub enum ProbeCapital {
    /// The deployed contract's own capital: production encoding with the
    /// account's real balances, and no capital override at all.
    #[default]
    Production,
    /// Executor-held capital from the known asset set, with the self-funded
    /// stream encoding.
    SelfFund,
    /// The known asset set stripped from the executor, with the in-path-flash
    /// stream encoding.
    ZeroBalance,
}

impl ProbeCapital {
    /// The accepted spellings, in refusal-message order.
    ///
    /// One vocabulary for the operator wire, the Python construction boundary,
    /// and the `PyO3` boundary's validation, so a mode can never be spelled one
    /// way in a driver and another in the FFI.
    pub const NAMES: &'static [&'static str] = &["production", "self_fund", "zero_balance"];

    /// Parse a mode from its spelling; `None` for anything else.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "production" => Some(Self::Production),
            "self_fund" => Some(Self::SelfFund),
            "zero_balance" => Some(Self::ZeroBalance),
            _ => None,
        }
    }

    /// This mode's spelling ([`Self::NAMES`] entry).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::SelfFund => "self_fund",
            Self::ZeroBalance => "zero_balance",
        }
    }

    /// Bind the account this mode applies to — the ONE mode→plan projection,
    /// run once where a deployment is composed.
    ///
    /// `holder` is the deployment's execution target (the executor): the account
    /// the probe funds or clears, never the owner, who only funds gas.
    #[must_use]
    pub const fn override_plan(self, holder: Address) -> CapitalOverridePlan {
        match self {
            Self::Production => CapitalOverridePlan::Production,
            Self::SelfFund => CapitalOverridePlan::SelfFund { holder },
            Self::ZeroBalance => CapitalOverridePlan::ZeroBalance { holder },
        }
    }
}

/// A [`ProbeCapital`] mode with the account it applies to resolved: the
/// simulation-override plan a deployment carries.
///
/// A deployment produces it once (from its own execution target), and the
/// override application reads it: `Production` declares nothing, the two probe
/// arms name the one account whose capital they install or strip.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub enum CapitalOverridePlan {
    /// No capital override: the simulated block keeps the account's own state.
    #[default]
    Production,
    /// Install the known asset set at `holder`.
    SelfFund {
        /// The capital target — the deployment's execution target.
        holder: Address,
    },
    /// Strip the known asset set from `holder`.
    ZeroBalance {
        /// The capital target — the deployment's execution target.
        holder: Address,
    },
}

impl CapitalOverridePlan {
    /// The mode this plan was projected from, without its account.
    #[must_use]
    pub const fn mode(self) -> ProbeCapital {
        match self {
            Self::Production => ProbeCapital::Production,
            Self::SelfFund { .. } => ProbeCapital::SelfFund,
            Self::ZeroBalance { .. } => ProbeCapital::ZeroBalance,
        }
    }

    /// The account this plan funds or clears; `None` for `Production`.
    #[must_use]
    pub const fn holder(self) -> Option<Address> {
        match self {
            Self::Production => None,
            Self::SelfFund { holder } | Self::ZeroBalance { holder } => Some(holder),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_round_trips_through_its_spelling() {
        assert_eq!(ProbeCapital::NAMES.len(), 3);
        for mode in [
            ProbeCapital::Production,
            ProbeCapital::SelfFund,
            ProbeCapital::ZeroBalance,
        ] {
            assert!(ProbeCapital::NAMES.contains(&mode.name()));
            assert_eq!(ProbeCapital::from_name(mode.name()), Some(mode));
        }
        assert_eq!(ProbeCapital::from_name("self-funded"), None);
        assert_eq!(ProbeCapital::from_name("self_fund "), None);
        assert_eq!(ProbeCapital::from_name(""), None);
    }

    #[test]
    fn the_plan_binds_the_holder_and_keeps_the_mode() {
        let executor = Address::from([0x42; 20]);
        for mode in [
            ProbeCapital::Production,
            ProbeCapital::SelfFund,
            ProbeCapital::ZeroBalance,
        ] {
            let plan = mode.override_plan(executor);
            assert_eq!(plan.mode(), mode, "the plan keeps the mode it came from");
            assert_eq!(
                plan.holder(),
                (mode != ProbeCapital::Production).then_some(executor),
                "only the two probe modes name a capital target",
            );
        }
        assert_eq!(
            CapitalOverridePlan::default(),
            CapitalOverridePlan::Production
        );
    }
}

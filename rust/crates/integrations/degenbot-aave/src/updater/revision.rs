//! The Aave revision policy: the one home of the revision thresholds the
//! updater reads and the open-ended ranges that introduce them.
//!
//! A revisions threshold is a protocol behaviour a contract revision
//! introduces. The behaviour is introduced AT a revision and stays live on
//! every later compatible revision, so a threshold is a half-open range
//! ([`RevisionRange`]) rather than a set of specific revisions: a later
//! revision carrying a known behaviour needs no new constant, and no consumer
//! can state an upper bound the chain never stated.
//!
//! Two behaviours exist, both parity-gated against the Python:
//!
//! - `Pool` revision 9 floors scaled amounts, so a matched underlying amount
//!   may deviate by the parser's tolerance (`amounts_match`,
//!   `dispatch_mint_to_treasury`).
//! - GHO vToken revision 4 retires the discount mechanism, so the discount
//!   pre-pass reads no `getDiscountPercent` and the verifier skips that check.
//!
//! The thresholds live here and ONLY here: a consumer names the behaviour
//! (`scaled_amount_flooring`, `gho_discount_retired`) instead of restating
//! a revision literal, so a threshold move is one edit.

/// An open-ended range of contract revisions, stated by the revision that
/// introduces it: `from..`.
///
/// Open-ended by construction: a behaviour a revision introduces is live on
/// every later compatible revision, and only contract evidence could state
/// where it ends. No endpoint exists to get stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RevisionRange {
    from: u32,
}

impl RevisionRange {
    /// The range that starts at `from` and carries every later revision.
    #[must_use]
    pub const fn from(from: u32) -> Self {
        Self { from }
    }

    /// The revision the range starts at — the one that introduces the
    /// behaviour.
    #[must_use]
    pub const fn start(self) -> u32 {
        self.from
    }

    /// Whether `revision` carries the behaviour.
    #[must_use]
    pub const fn contains(self, revision: u32) -> bool {
        revision >= self.from
    }
}

/// A protocol behaviour a contract revision introduces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevisionChange {
    /// Pool revision 9 floors a scaled amount when it computes the underlying
    /// one, so a matched amount may deviate by the parser's tolerance.
    ScaledAmountFlooring,
    /// GHO vToken revision 4 retires the discount mechanism.
    GhoDiscountRetirement,
}

impl RevisionChange {
    /// Every behaviour this policy states.
    pub const ALL: [Self; 2] = [Self::ScaledAmountFlooring, Self::GhoDiscountRetirement];

    /// The behaviour's name, for a refusal or a census.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ScaledAmountFlooring => "scaled amount flooring",
            Self::GhoDiscountRetirement => "GHO discount retirement",
        }
    }

    /// The revision the behaviour is introduced at.
    #[must_use]
    pub const fn introduced_at(self) -> u32 {
        match self {
            Self::ScaledAmountFlooring => 9,
            Self::GhoDiscountRetirement => 4,
        }
    }

    /// The open-ended range of revisions that carry the behaviour.
    #[must_use]
    pub const fn introduction(self) -> RevisionRange {
        RevisionRange::from(self.introduced_at())
    }

    /// Whether `revision` is at or past the behaviour's introduction.
    #[must_use]
    pub const fn is_live_at(self, revision: u32) -> bool {
        self.introduction().contains(revision)
    }
}

/// Whether a `Pool` reporting `revision` floors scaled amounts.
#[must_use]
pub const fn scaled_amount_flooring(pool_revision: u32) -> bool {
    RevisionChange::ScaledAmountFlooring.is_live_at(pool_revision)
}

/// Whether a GHO vToken reporting `revision` has retired the discount
/// mechanism.
#[must_use]
pub const fn gho_discount_retired(vtoken_revision: u32) -> bool {
    RevisionChange::GhoDiscountRetirement.is_live_at(vtoken_revision)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An introduced behaviour is live AT its introduction revision and on
    /// every later revision — the range has no upper bound to get stale.
    #[test]
    fn an_introduction_range_is_open_ended_upward() {
        let range = RevisionRange::from(9);

        assert!(!range.contains(8), "the revision before the introduction");
        assert!(range.contains(9), "the introduction revision itself");
        assert!(range.contains(12), "a later compatible revision");
        assert!(range.contains(u32::MAX), "every later revision");
        assert_eq!(range.start(), 9);
    }

    /// The two behaviours' thresholds are the parity-gated ones, and each
    /// states its own boundary.
    #[test]
    fn the_policy_states_both_thresholds_and_their_boundaries() {
        assert_eq!(
            RevisionChange::ScaledAmountFlooring.introduced_at(),
            9,
            "the Pool revision that floors scaled amounts"
        );
        assert_eq!(
            RevisionChange::GhoDiscountRetirement.introduced_at(),
            4,
            "the GHO vToken revision that retires the discount"
        );
        for change in RevisionChange::ALL {
            assert_eq!(
                change.introduction(),
                RevisionRange::from(change.introduced_at()),
                "{}: the range starts at its introduction",
                change.label()
            );
            assert!(
                !change.introduction().contains(change.introduced_at() - 1),
                "{}: the revision before the introduction does not carry it",
                change.label()
            );
            assert!(
                change.is_live_at(change.introduced_at()),
                "{}: the introduction revision carries the behaviour",
                change.label()
            );
        }
    }

    /// The two consumers' predicates read the policy rather than a literal.
    #[test]
    fn the_consumers_predicates_are_the_thresholds() {
        assert!(!scaled_amount_flooring(8));
        assert!(scaled_amount_flooring(9));
        assert!(scaled_amount_flooring(11));

        assert!(!gho_discount_retired(3));
        assert!(gho_discount_retired(4));
        assert!(gho_discount_retired(5));
    }
}

//! The trust-flag vocabulary every simulated and solved outcome carries.
//!
//! A flag SET, not a bool (ADR-037 D4): an empty set means "this number is
//! exact", and new inaccuracy sources become new flags rather than a signature
//! change. One vocabulary serves both channels that can be approximate — the
//! per-pool swap read (whose outcome adapters live in `degenbot-substrate`) and
//! the multi-hop solve result ([`crate::solve::SolvePathResult`]) — because the
//! inaccuracy sources are the same facts, so "approximate" must mean the same
//! thing at both seams.

use crate::state::v3_state::PoolTickCoverage;

/// Additive trust flags carried on a simulated or solved outcome.
///
/// An EMPTY set means "this number is exact". The set is deliberately
/// `#[non_exhaustive]`: new inaccuracy sources become new flags, never a
/// signature change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Caveats(u8);

impl Caveats {
    /// Sparse tick coverage: the walk may have traversed unfetched regions.
    ///
    /// Derived from a pool's registration-time [`PoolTickCoverage`] (glossary
    /// Tracked/Sparse). On a swap read it is the read pool's own coverage; on a
    /// solve result it is the path-level union — a path is approximate when ANY
    /// hop is sparse.
    pub const SPARSE_COVERAGE: Self = Self(1 << 0);
    /// The pool carries an amount-modifying V4 hook — standard concentrated
    /// liquidity math may mis-price the swap. (Reachable since hooked pools
    /// were admitted.)
    pub const HOOKED_POOL: Self = Self(1 << 1);

    /// No caveats — the outcome is trustworthy.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `self` carries all flags in `other`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Union of two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The trust flags a pool's registration-time tick coverage implies.
    ///
    /// Tracked snapshots provided complete tick data, so results are
    /// trustworthy (glossary). Sparse results may be inaccurate.
    #[must_use]
    pub const fn for_coverage(coverage: PoolTickCoverage) -> Self {
        match coverage {
            PoolTickCoverage::Tracked => Self(0),
            PoolTickCoverage::Sparse => Self::SPARSE_COVERAGE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_means_exact_and_union_is_additive() {
        assert!(Caveats::default().is_empty());
        let sparse = Caveats::SPARSE_COVERAGE;
        assert!(!sparse.is_empty());
        assert!(sparse.contains(Caveats::SPARSE_COVERAGE));
        assert!(!sparse.contains(Caveats::HOOKED_POOL));
        assert_eq!(sparse.union(sparse), sparse);
        assert_eq!(
            sparse.union(Caveats::HOOKED_POOL),
            Caveats::SPARSE_COVERAGE.union(Caveats::HOOKED_POOL)
        );
    }

    #[test]
    fn coverage_maps_onto_the_sparse_bit() {
        assert!(Caveats::for_coverage(PoolTickCoverage::Tracked).is_empty());
        assert_eq!(
            Caveats::for_coverage(PoolTickCoverage::Sparse),
            Caveats::SPARSE_COVERAGE
        );
    }
}

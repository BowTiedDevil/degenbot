//! ADR-059 D2: per-arm family capability declarations — the one place a
//! family's tier support is stated, so "the backrun arm supports V4" is a
//! declared fact with a drift test rather than an inference from whichever
//! seams happen to be wired.
//!
//! The four tiers are ADR-059 D2's pipeline stages: `extract` (touched-state
//! decode), `admit` (typed state into the workspace), `discover` (graph /
//! candidate participation), and `compose` (executor calldata). `solve` is not
//! a D2 tier — it is the mixed solver's `HopType` dispatch, which every
//! [`Family::SOLVER`] family participates in; `compose` is bounded by the
//! executor's opcode arms (D5), so the solve-only families declare
//! `compose = false` until an encoder arm ships.
//!
//! Wiring evidence for each claim lives at its seam (doc-commented in the
//! arms). The mechanically checkable subset is pinned by
//! `degenbot-strategy/tests/family_capabilities.rs`; claims whose wiring is not
//! reachable from a workspace test carry a footnote here instead of a test.
//!
//! # Footnotes
//!
//! - Backrun V4 is hookless-only ([`V4_HOOKLESS_ONLY`]): `connector_index`
//!   (`degenbot-bot`) drops every roster row whose `hooks` is non-zero before
//!   it can become a lane edge. The same arm refuses a key whose fee reaches
//!   [`V4_FEE_CEILING_EXCLUSIVE`] at admission.
//! - Backrun V2 discovery excludes the `AerodromeV2 { stable: true }` species
//!   (`LaneFamilyTag::from_identity` projects it to `None` — the stable branch
//!   is Solidly math the V2 lane cannot express) even though the database
//!   `kind` string `aerodrome_v2` projects to the V2 graph kind.
//! - Backrun V3 discovery admits every `pancakeswap_v3` / `aerodrome_v3` row
//!   the connector index loads, but the frame descriptor hardcodes the
//!   canonical `ConcentratedLiquiditySlotLayout::UniswapV3`. `aerodrome_v3` shares that layout;
//!   `pancakeswap_v3` does not (two-word `slot0`, liquidity@5, ticks@6), so its
//!   rows decode under the wrong slots — a documented extraction gap, not a
//!   capability claim.
//! - Settlement's seven solver families all `solve` (mixed `HopType` dispatch),
//!   but only the three [`ENCODER_FAMILIES`] `compose`; Solidly / Balancer /
//!   Curve have no `composers::HopInfo` arm (`arb_engine::path_info` returns
//!   `UnsupportedHopType`).
//! - [`Family::LfjBinned`] is roadmap-only: both arms declare it unsupported
//!   (D8), and the descriptor seam carries the loud `family-unsupported`
//!   observation when its rows appear. The family also has a taxonomy
//!   identity ([`crate::sim::pool::Identity::BinnedLiquidity`]), so these NONE rows
//!   describe a real identity rather than an unreachable enum arm.

use std::fmt;

use crate::sim::pool::{
    BalanceVectorVariant, BinnedLiquidityVariant, ConcentratedLiquidityVariant, Identity,
    ReservePairVariant,
};
use crate::sim::registry::RegisteredPoolFamily;

/// Which executable arm a capability row describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Arm {
    /// `degenbot-strategy`: extract frames -> admit lanes -> discover through
    /// the connector index -> compose executor calldata.
    Backrun,
    /// `degenbot-bot` arb engine: register pools -> discover paths -> mixed
    /// solve -> compose executor calldata.
    Settlement,
}

impl Arm {
    /// Every arm, so a completeness test cannot silently skip one.
    pub const ALL: [Self; 2] = [Self::Backrun, Self::Settlement];

    /// Stable label for diagnostics and test messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Backrun => "backrun",
            Self::Settlement => "settlement",
        }
    }
}

/// A capability family, projected from the pool taxonomy (ADR-059 D1).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Family {
    /// Constant-product reserve pair (and its volatile Aerodrome species).
    V2,
    /// Concentrated liquidity under the canonical V3 fork layout.
    V3,
    /// V4 concentrated liquidity under the `PoolManager` singleton.
    V4,
    /// Solidly / Aerodrome-stable nonlinear reserve pair.
    SolidlyStable,
    /// Balancer V2 weighted pool.
    BalancerWeighted,
    /// Balancer V2 stable pool.
    BalancerStable,
    /// Curve stableswap pool.
    CurveStableswap,
    /// LFJ binned liquidity — roadmap-only (ADR-059 D8); no tier admits it.
    LfjBinned,
}

/// The identity-layer family table's key: which variant of which identity kind
/// a pool names, with the per-pool payload (`dex`) stripped so the table can be
/// data instead of a function.
///
/// The four identity kinds' variant enums are exhaustive over these keys, so a
/// variant added to [`ReservePairVariant`] / [`ConcentratedLiquidityVariant`] /
/// [`BalanceVectorVariant`] / [`BinnedLiquidityVariant`] fails to compile in
/// [`FamilyKey::index`] until it is given a [`FAMILY_ROWS`] row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FamilyKey {
    /// A two-token reserve pair — constant-product, or the Aerodrome species.
    ReservePair(ReservePairVariant),
    /// A tick-based concentrated-liquidity pool.
    ConcentratedLiquidity(ConcentratedLiquidityVariant),
    /// An N-token balance-vector pool.
    BalanceVector(BalanceVectorVariant),
    /// A discrete-bin liquidity pool (roadmap-only, ADR-059 D8).
    BinnedLiquidity(BinnedLiquidityVariant),
}

impl FamilyKey {
    /// Every key, in [`FAMILY_ROWS`] order (`row_per_key_is_the_table_and_the_keys_agree`
    /// pins the two orders together).
    ///
    /// Test-only: production code reaches a row through [`Self::index`], which is
    /// the compiler-checked projection, so a lib build has no caller for the
    /// enumeration.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 9] = [
        Self::ReservePair(ReservePairVariant::UniswapV2),
        Self::ReservePair(ReservePairVariant::AerodromeV2 { stable: false }),
        Self::ReservePair(ReservePairVariant::AerodromeV2 { stable: true }),
        Self::ConcentratedLiquidity(ConcentratedLiquidityVariant::UniswapV3),
        Self::ConcentratedLiquidity(ConcentratedLiquidityVariant::UniswapV4),
        Self::BalanceVector(BalanceVectorVariant::Curve),
        Self::BalanceVector(BalanceVectorVariant::BalancerWeighted),
        Self::BalanceVector(BalanceVectorVariant::BalancerStable),
        Self::BinnedLiquidity(BinnedLiquidityVariant::Lfj),
    ];

    /// The key's [`FAMILY_ROWS`] index.
    ///
    /// Exhaustive over the four variant enums, so the table cannot lose a row
    /// silently and a new identity variant cannot reach a family by default.
    const fn index(self) -> usize {
        match self {
            Self::ReservePair(ReservePairVariant::UniswapV2) => 0,
            Self::ReservePair(ReservePairVariant::AerodromeV2 { stable: false }) => 1,
            Self::ReservePair(ReservePairVariant::AerodromeV2 { stable: true }) => 2,
            Self::ConcentratedLiquidity(ConcentratedLiquidityVariant::UniswapV3) => 3,
            Self::ConcentratedLiquidity(ConcentratedLiquidityVariant::UniswapV4) => 4,
            Self::BalanceVector(BalanceVectorVariant::Curve) => 5,
            Self::BalanceVector(BalanceVectorVariant::BalancerWeighted) => 6,
            Self::BalanceVector(BalanceVectorVariant::BalancerStable) => 7,
            Self::BinnedLiquidity(BinnedLiquidityVariant::Lfj) => 8,
        }
    }

    /// The capability family this identity variant declares.
    pub(crate) const fn family(self) -> Family {
        FAMILY_ROWS[self.index()].family
    }

    /// The registry family this identity variant registers as; `None` for the
    /// roadmap-only binned identity, which the registry vocabulary does not
    /// name (no tier admits it).
    pub(crate) const fn registered_family(self) -> Option<RegisteredPoolFamily> {
        FAMILY_ROWS[self.index()].registered
    }
}

/// One row of the identity-layer family table.
struct FamilyRow {
    /// The identity variant the row answers for.
    key: FamilyKey,
    /// The capability family the variant declares (ADR-059 D1).
    family: Family,
    /// The registry family it registers as (`None`: the binned identity, which
    /// [`RegisteredPoolFamily`] does not name).
    registered: Option<RegisteredPoolFamily>,
}

/// The ONE identity-layer family table, keyed over all four identity kinds.
///
/// Every projection from a pool's identity onto a family vocabulary reads this
/// table: [`Family::from_identity`] for the capability families, the
/// registered-entry projection
/// ([`PoolEntry::registered_family`](crate::PoolEntry::registered_family)) for
/// [`RegisteredPoolFamily`], and the reverse lookup ([`Family::try_from`]) for
/// a consumer that holds a registry family rather than an identity.
///
/// This table and [`TABLE`] are different axes: this one maps an identity onto
/// a family, [`TABLE`] declares what an `(arm, family)` cell may do.
const FAMILY_ROWS: [FamilyRow; 9] = [
    FamilyRow {
        key: FamilyKey::ReservePair(ReservePairVariant::UniswapV2),
        family: Family::V2,
        registered: Some(RegisteredPoolFamily::V2),
    },
    FamilyRow {
        key: FamilyKey::ReservePair(ReservePairVariant::AerodromeV2 { stable: false }),
        family: Family::V2,
        registered: Some(RegisteredPoolFamily::AerodromeV2),
    },
    FamilyRow {
        key: FamilyKey::ReservePair(ReservePairVariant::AerodromeV2 { stable: true }),
        family: Family::SolidlyStable,
        registered: Some(RegisteredPoolFamily::AerodromeV2),
    },
    FamilyRow {
        key: FamilyKey::ConcentratedLiquidity(ConcentratedLiquidityVariant::UniswapV3),
        family: Family::V3,
        registered: Some(RegisteredPoolFamily::V3),
    },
    FamilyRow {
        key: FamilyKey::ConcentratedLiquidity(ConcentratedLiquidityVariant::UniswapV4),
        family: Family::V4,
        registered: Some(RegisteredPoolFamily::V4),
    },
    FamilyRow {
        key: FamilyKey::BalanceVector(BalanceVectorVariant::Curve),
        family: Family::CurveStableswap,
        registered: Some(RegisteredPoolFamily::Curve),
    },
    FamilyRow {
        key: FamilyKey::BalanceVector(BalanceVectorVariant::BalancerWeighted),
        family: Family::BalancerWeighted,
        registered: Some(RegisteredPoolFamily::BalancerWeighted),
    },
    FamilyRow {
        key: FamilyKey::BalanceVector(BalanceVectorVariant::BalancerStable),
        family: Family::BalancerStable,
        registered: Some(RegisteredPoolFamily::BalancerStable),
    },
    FamilyRow {
        key: FamilyKey::BinnedLiquidity(BinnedLiquidityVariant::Lfj),
        family: Family::LfjBinned,
        registered: None,
    },
];

/// Whether a row answers for the registry species `stable` — the one
/// discriminator the registry vocabulary erases (`AerodromeV2` is one family
/// whose identity splits into a volatile and a stable species; every other
/// identity kind is species-blind).
const fn answers_species(key: FamilyKey, stable: bool) -> bool {
    match key {
        FamilyKey::ReservePair(ReservePairVariant::AerodromeV2 { stable: species }) => {
            species == stable
        }
        _ => true,
    }
}

/// Why a reverse lookup found no identity row: the registry family names no
/// identity of that species. A declaration hole in the family table, not a pool
/// verdict — the same contract [`capabilities`] has for [`TABLE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoFamilyRow {
    /// The registry family the reverse lookup was asked about.
    pub registered: RegisteredPoolFamily,
    /// The Aerodrome species flag it supplied.
    pub stable: bool,
}

impl fmt::Display for NoFamilyRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "no identity-layer family row for {} (stable: {})",
            self.registered.tag(),
            self.stable
        )
    }
}

impl std::error::Error for NoFamilyRow {}

impl Family {
    /// Project a taxonomy [`Identity`] onto the capability family (ADR-059 D1).
    ///
    /// One family-table row per identity variant: every taxonomy identity
    /// names exactly one capability family, including the roadmap-only
    /// [`Self::LfjBinned`] — the declaration table's NONE rows are therefore a
    /// verdict about a real identity, not a placeholder.
    #[must_use]
    pub const fn from_identity(identity: &Identity) -> Self {
        identity.family_key().family()
    }

    /// Every family, including the roadmap-only [`Self::LfjBinned`].
    pub const ALL: [Self; 8] = [
        Self::V2,
        Self::V3,
        Self::V4,
        Self::SolidlyStable,
        Self::BalancerWeighted,
        Self::BalancerStable,
        Self::CurveStableswap,
        Self::LfjBinned,
    ];

    /// The seven families the mixed solver's `HopType` dispatch solves.
    pub const SOLVER: [Self; 7] = [
        Self::V2,
        Self::V3,
        Self::V4,
        Self::SolidlyStable,
        Self::BalancerWeighted,
        Self::BalancerStable,
        Self::CurveStableswap,
    ];

    /// Whether this is one of the [`Self::SOLVER`] families.
    #[must_use]
    pub const fn is_solver(self) -> bool {
        matches!(
            self,
            Self::V2
                | Self::V3
                | Self::V4
                | Self::SolidlyStable
                | Self::BalancerWeighted
                | Self::BalancerStable
                | Self::CurveStableswap
        )
    }
}

/// A family's declared verdict on the four ADR-059 D2 pipeline tiers.
// ADR-059 D2 names exactly these four tier booleans; a two-variant enum per
// tier would obscure the declaration and add nothing.
#[expect(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FamilyCapabilities {
    /// Touched-state decode (journal slots / registration snapshot) into typed
    /// state.
    pub extract: bool,
    /// Typed state into the arm's workspace (`PoolEntry` / lane scope).
    pub admit: bool,
    /// Candidate / graph participation.
    pub discover: bool,
    /// Executor calldata emission (ADR-059 D5 bounds this by opcode arms).
    pub compose: bool,
}

impl FamilyCapabilities {
    /// No tier wired.
    pub const NONE: Self = Self {
        extract: false,
        admit: false,
        discover: false,
        compose: false,
    };

    /// All four tiers wired.
    pub const ALL: Self = Self {
        extract: true,
        admit: true,
        discover: true,
        compose: true,
    };

    /// Construct a verdict.
    #[must_use]
    #[expect(clippy::fn_params_excessive_bools)] // the four ADR-059 D2 tiers
    pub const fn new(extract: bool, admit: bool, discover: bool, compose: bool) -> Self {
        Self {
            extract,
            admit,
            discover,
            compose,
        }
    }
}

/// Families with an executor command-stream encoder arm: `composers::HopInfo`
/// carries V2/V3/V4 only (ADR-059 D5).
pub const ENCODER_FAMILIES: [Family; 3] = [Family::V2, Family::V3, Family::V4];

/// The exclusive fee bound of the executor's 2-byte V4 fee field; a V4 key at
/// or above this can never compose.
pub const V4_FEE_CEILING_EXCLUSIVE: u32 = 0x1_0000;

/// V4 is admitted hookless-only: the connector index drops keys with non-zero
/// `hooks` before they can become lane edges.
pub const V4_HOOKLESS_ONLY: bool = true;

/// The declaration table.
///
/// Settlement `extract` / `admit` describe registration-time state
/// construction; `discover` / `compose` describe the path graph and the
/// executor. The four solve-only families ([`Family::SolidlyStable`],
/// [`Family::BalancerWeighted`], [`Family::BalancerStable`],
/// [`Family::CurveStableswap`]) have no encoder arm today, so their `compose`
/// is false even though the mixed solver solves them.
pub const TABLE: &[(Arm, Family, FamilyCapabilities)] = &[
    // Backrun (`degenbot-strategy`).
    (Arm::Backrun, Family::V2, FamilyCapabilities::ALL),
    (
        Arm::Backrun,
        Family::V3,
        FamilyCapabilities::new(true, true, true, true),
    ),
    (
        Arm::Backrun,
        Family::V4,
        FamilyCapabilities::new(true, true, true, true),
    ),
    (
        Arm::Backrun,
        Family::SolidlyStable,
        FamilyCapabilities::NONE,
    ),
    (
        Arm::Backrun,
        Family::BalancerWeighted,
        FamilyCapabilities::NONE,
    ),
    (
        Arm::Backrun,
        Family::BalancerStable,
        FamilyCapabilities::NONE,
    ),
    (
        Arm::Backrun,
        Family::CurveStableswap,
        FamilyCapabilities::NONE,
    ),
    (Arm::Backrun, Family::LfjBinned, FamilyCapabilities::NONE),
    // Settlement (`degenbot-bot` arb engine).
    (
        Arm::Settlement,
        Family::V2,
        FamilyCapabilities::new(true, true, true, true),
    ),
    (
        Arm::Settlement,
        Family::V3,
        FamilyCapabilities::new(true, true, true, true),
    ),
    (
        Arm::Settlement,
        Family::V4,
        FamilyCapabilities::new(true, true, true, true),
    ),
    (
        Arm::Settlement,
        Family::SolidlyStable,
        FamilyCapabilities::new(true, true, false, false),
    ),
    (
        Arm::Settlement,
        Family::BalancerWeighted,
        FamilyCapabilities::new(true, true, false, false),
    ),
    (
        Arm::Settlement,
        Family::BalancerStable,
        FamilyCapabilities::new(true, true, false, false),
    ),
    (
        Arm::Settlement,
        Family::CurveStableswap,
        FamilyCapabilities::new(true, true, false, false),
    ),
    (Arm::Settlement, Family::LfjBinned, FamilyCapabilities::NONE),
];

impl TryFrom<(&RegisteredPoolFamily, bool)> for Family {
    type Error = NoFamilyRow;

    /// The reverse of [`Family::from_identity`]: the capability family a
    /// REGISTRY family declares, with `stable` supplying the one discriminator
    /// the registry vocabulary erases (`AerodromeV2` is one family whose
    /// identity splits into a volatile species — [`Family::V2`] — and a stable
    /// one — [`Family::SolidlyStable`]).
    ///
    /// A consumer holding a [`RegisteredPoolFamily`] instead of an [`Identity`]
    /// (the session registry's identity, a crawl hop, a registration refusal)
    /// has no species bit, so it supplies one; the families whose identity does
    /// not split on it ignore the flag.
    ///
    /// # Errors
    ///
    /// [`NoFamilyRow`] when the table has no row for `(registered, stable)` — a
    /// declaration hole, never a guessed family.
    fn try_from((registered, stable): (&RegisteredPoolFamily, bool)) -> Result<Self, Self::Error> {
        FAMILY_ROWS
            .iter()
            .find(|row| row.registered == Some(*registered) && answers_species(row.key, stable))
            .map(|row| row.family)
            .ok_or(NoFamilyRow {
                registered: *registered,
                stable,
            })
    }
}

/// The declared capabilities for one `(arm, family)` cell.
///
/// `None` means the cell is missing from [`TABLE`] — a declaration hole, not a
/// silent "unsupported".
#[must_use]
pub fn capabilities(arm: Arm, family: Family) -> Option<FamilyCapabilities> {
    TABLE
        .iter()
        .find_map(|(a, f, c)| (*a == arm && *f == family).then_some(*c))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used)] // `capabilities` returning `None` is the assertion failure

    use super::{
        capabilities, Arm, Family, FamilyCapabilities, FamilyKey, NoFamilyRow, ENCODER_FAMILIES,
        FAMILY_ROWS, TABLE,
    };
    use crate::sim::pool::{BinnedLiquidityVariant, Identity};
    use crate::RegisteredPoolFamily;

    /// One identity per [`FamilyKey`], so a test can walk the table by identity.
    fn identity_of(key: FamilyKey) -> Identity {
        match key {
            FamilyKey::ReservePair(variant) => Identity::ReservePair { variant, dex: None },
            FamilyKey::ConcentratedLiquidity(variant) => {
                Identity::ConcentratedLiquidity { variant, dex: None }
            }
            FamilyKey::BalanceVector(variant) => Identity::BalanceVector { variant, dex: None },
            FamilyKey::BinnedLiquidity(variant) => Identity::BinnedLiquidity { variant, dex: None },
        }
    }

    #[test]
    fn row_per_key_is_the_table_and_the_keys_agree() {
        // What: the table covers every identity variant exactly once, in
        // `FamilyKey::ALL` order (so `FamilyKey::index` is a plain index), and
        // the two projections read their row rather than a second map.
        assert_eq!(FAMILY_ROWS.len(), FamilyKey::ALL.len());
        for (index, key) in FamilyKey::ALL.into_iter().enumerate() {
            let row = &FAMILY_ROWS[index];
            assert_eq!(row.key, key, "row {index} must answer for {key:?}");
            assert_eq!(key.family(), row.family);
            assert_eq!(key.registered_family(), row.registered);
            assert_eq!(Family::from_identity(&identity_of(key)), row.family);
        }
    }

    #[test]
    fn every_identity_kind_is_keyed() {
        // What: all four identity kinds are rows, not just the reserve pairs —
        // the taxonomy's roadmap-only binned identity included.
        let mut seen = [false; 4];
        for row in &FAMILY_ROWS {
            seen[match row.key {
                FamilyKey::ReservePair(..) => 0,
                FamilyKey::ConcentratedLiquidity(..) => 1,
                FamilyKey::BalanceVector(..) => 2,
                FamilyKey::BinnedLiquidity(..) => 3,
            }] = true;
        }
        assert_eq!(
            seen, [true; 4],
            "every identity kind carries at least one row"
        );
    }

    #[test]
    fn the_reverse_lookup_recovers_every_registry_family() {
        // What: a consumer holding only a `RegisteredPoolFamily` (the session
        // identity, a crawl hop) reaches the same capability family the
        // identity table declares — for both Aerodrome species and for the
        // families that ignore the flag.
        for registered in RegisteredPoolFamily::ALL {
            for stable in [false, true] {
                let family = Family::try_from((&registered, stable))
                    .expect("every registry family reverses onto a row");
                let row = FAMILY_ROWS
                    .iter()
                    .find(|row| row.registered == Some(registered) && row.family == family)
                    .expect("a row for the recovered family");
                assert_eq!(
                    row.key.family(),
                    family,
                    "{registered:?} (stable: {stable}) must reverse onto its own row"
                );
            }
        }
    }

    #[test]
    fn the_aerodrome_species_is_the_one_flag_the_reverse_lookup_reads() {
        // What: the registry vocabulary erases the Aerodrome species, so the
        // flag is load-bearing ONLY there — and `AerodromeV2` is the one
        // registry family mapping onto two capability families.
        assert_eq!(
            Family::try_from((&RegisteredPoolFamily::AerodromeV2, false)),
            Ok(Family::V2)
        );
        assert_eq!(
            Family::try_from((&RegisteredPoolFamily::AerodromeV2, true)),
            Ok(Family::SolidlyStable)
        );
        for registered in RegisteredPoolFamily::ALL {
            if registered == RegisteredPoolFamily::AerodromeV2 {
                continue;
            }
            assert_eq!(
                Family::try_from((&registered, false)),
                Family::try_from((&registered, true)),
                "{registered:?} does not split on the species flag"
            );
        }
    }

    #[test]
    fn the_binned_identity_names_no_registry_family() {
        // What: LFJ is roadmap-only (ADR-059 D8) — the capability family exists
        // and the registry vocabulary has no name for it, which is why the row's
        // registry column is `None` and the reverse lookup cannot reach it.
        let lfj = FamilyKey::BinnedLiquidity(BinnedLiquidityVariant::Lfj);
        assert_eq!(lfj.family(), Family::LfjBinned);
        assert_eq!(lfj.registered_family(), None);
    }

    #[test]
    fn no_family_row_names_the_lookup_that_failed() {
        let err = NoFamilyRow {
            registered: RegisteredPoolFamily::Curve,
            stable: false,
        };
        assert_eq!(
            err.to_string(),
            "no identity-layer family row for curve (stable: false)"
        );
    }

    #[test]
    fn table_has_exactly_one_row_per_cell_and_lookup_agrees() {
        for arm in Arm::ALL {
            for family in Family::ALL {
                let rows: Vec<_> = TABLE
                    .iter()
                    .filter(|(a, f, _)| *a == arm && *f == family)
                    .collect();
                assert_eq!(
                    rows.len(),
                    1,
                    "{:?} x {family:?} must have exactly one declaration row",
                    arm.label()
                );
                assert_eq!(capabilities(arm, family), Some(rows[0].2));
            }
        }
    }

    #[test]
    fn backrun_discover_and_compose_are_wired_together() {
        for family in Family::ALL {
            let caps = capabilities(Arm::Backrun, family).expect("table row");
            assert_eq!(
                caps.discover, caps.compose,
                "backrun {family:?}: discovery and composition move together"
            );
        }
    }

    #[test]
    fn settlement_compose_matches_the_encoder_roster() {
        for family in Family::ALL {
            let caps = capabilities(Arm::Settlement, family).expect("table row");
            assert_eq!(
                caps.compose,
                ENCODER_FAMILIES.contains(&family),
                "settlement {family:?}: compose must track the executor's HopInfo arm set"
            );
        }
    }

    #[test]
    fn lfj_identity_reaches_the_declared_lfj_family() {
        let identity = crate::sim::pool::Identity::BinnedLiquidity {
            variant: crate::sim::pool::BinnedLiquidityVariant::Lfj,
            dex: None,
        };
        assert_eq!(Family::from_identity(&identity), Family::LfjBinned);
        for arm in Arm::ALL {
            assert_eq!(
                capabilities(arm, Family::from_identity(&identity)),
                Some(FamilyCapabilities::NONE),
                "the LFJ taxonomy identity must reach the NONE declaration",
            );
        }
    }

    #[test]
    fn lfj_is_declared_unsupported_on_every_arm() {
        for arm in Arm::ALL {
            assert_eq!(
                capabilities(arm, Family::LfjBinned),
                Some(FamilyCapabilities::NONE),
                "LFJ is roadmap-only (D8) and must stay all-false"
            );
        }
    }

    #[test]
    fn solver_set_is_exactly_the_non_roadmap_families() {
        let solver: Vec<Family> = Family::ALL.into_iter().filter(|f| f.is_solver()).collect();
        assert_eq!(solver, Family::SOLVER);
    }
}

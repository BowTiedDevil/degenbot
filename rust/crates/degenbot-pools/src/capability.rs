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
//!   canonical `ClSlotLayout::UniswapV3`. `aerodrome_v3` shares that layout;
//!   `pancakeswap_v3` does not (two-word `slot0`, liquidity@5, ticks@6), so its
//!   rows decode under the wrong slots — a documented extraction gap, not a
//!   capability claim.
//! - Settlement's seven solver families all `solve` (mixed `HopType` dispatch),
//!   but only the three [`ENCODER_FAMILIES`] `compose`; Solidly / Balancer /
//!   Curve have no `composers::HopInfo` arm (`arb_engine::path_info` returns
//!   `UnsupportedHopType`).
//! - [`Family::LfjBinned`] is roadmap-only: both arms declare it unsupported
//!   (D8), and the descriptor seam carries the loud `family-unsupported`
//!   observation when its rows appear.

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

impl Family {
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

    /// Stable label for diagnostics and test messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::V2 => "v2",
            Self::V3 => "v3",
            Self::V4 => "v4",
            Self::SolidlyStable => "solidly-stable",
            Self::BalancerWeighted => "balancer-weighted",
            Self::BalancerStable => "balancer-stable",
            Self::CurveStableswap => "curve-stableswap",
            Self::LfjBinned => "lfj-binned",
        }
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

    use super::{capabilities, Arm, Family, FamilyCapabilities, ENCODER_FAMILIES, TABLE};

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
                    "{} x {} must have exactly one declaration row",
                    arm.label(),
                    family.label()
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
                caps.discover,
                caps.compose,
                "backrun {}: discovery and composition move together",
                family.label()
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
                "settlement {}: compose must track the executor's HopInfo arm set",
                family.label()
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

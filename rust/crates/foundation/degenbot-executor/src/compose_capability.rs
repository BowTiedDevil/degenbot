//! ADR-107 D3 — the static compose-capability evaluation.
//!
//! One evaluator, two fact levels, both amount- and target-independent:
//!
//! - **Family level** ([\`assess_families\`]) reads the ADR-059 declaration
//!   table's \`compose\` tier for each hop family. A missing cell fails closed,
//!   so an undeclared family is never silently admitted OR silently refused.
//! - **Route level** ([\`assess_route\`]) reads the two whole-route composition
//!   authorities: the mixed solver's dispatch rows for the route's family SET
//!   ([\`composes_families\`]) and the encoder's shape key for its hop sequence
//!   ([\`crate::grammar_walker::route_shape_has_producer\`]). Neither is the
//!   other's inverse — a route whose every family declares \`compose\` can still
//!   have no producer, and a family set can reach a refusal row while each of
//!   its hops is individually representable.
//!
//! [\`assess_settlement\`] is the one entry an admission door calls, so a caller
//! cannot consult one level and forget the other.
//!
//! # What this evaluation cannot see
//!
//! Solved amounts, capital policy, the session's \`PoolManager\` and target
//! adapters, hook behaviour, declared-interface validation, chain state, and
//! transport. Those are dynamic facts and keep their existing typed owners
//! downstream; compose capability is a necessary admission condition, never a
//! promise that composition, simulation, or submission will succeed (ADR-107
//! D5).

use core::fmt;

use std::collections::BTreeSet;

use degenbot_execution::PathInfo;
use degenbot_pools::solve::{composes_families, HopType};
use degenbot_pools::{capabilities, Arm, Family, FamilyCapabilities};

use crate::grammar_walker::route_shape_has_producer;

/// Which static fact refused a route (ADR-107 D3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ComposeCapabilityKind {
    /// The \`(arm, family)\` cell is absent from the declaration table: a
    /// declaration hole, refused closed rather than guessed unsupported.
    MissingDeclaration,
    /// The family's declaration denies the command composition tier.
    Family,
    /// The mixed solver's dispatch has no accepting row for the family set.
    SolverComposition,
    /// The hop sequence has no command-stream producer.
    RouteShape,
}

impl ComposeCapabilityKind {
    /// Every kind, so a vocabulary test cannot skip one.
    pub const ALL: [Self; 4] = [
        Self::MissingDeclaration,
        Self::Family,
        Self::SolverComposition,
        Self::RouteShape,
    ];

    /// Stable label for diagnostics and structured refusals.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::MissingDeclaration => "missing-declaration",
            Self::Family => "family",
            Self::SolverComposition => "solver-composition",
            Self::RouteShape => "route-shape",
        }
    }
}

/// A static compose-capability refusal: the route can never be projected into
/// a settlement command stream, whatever the block, amount, or target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposeCapabilityRefusal {
    /// No declaration exists for this family on the arm.
    MissingDeclaration {
        /// The arm whose declaration table was read.
        arm: Arm,
        /// The hop's index in the route.
        hop: usize,
        /// The family the hop declares.
        family: Family,
    },
    /// The family declares no command composition for the arm.
    Family {
        /// The arm whose declaration table was read.
        arm: Arm,
        /// The hop's index in the route.
        hop: usize,
        /// The family the hop declares.
        family: Family,
    },
    /// The mixed solver has no composition row for the route's family set.
    SolverComposition {
        /// The route's distinct families, in hop order of first appearance.
        families: Vec<Family>,
    },
    /// The hop sequence is outside the encoder's shape key.
    RouteShape {
        /// The route's hop count.
        hop_count: usize,
    },
}

impl ComposeCapabilityRefusal {
    /// Which static fact refused the route.
    #[must_use]
    pub const fn kind(&self) -> ComposeCapabilityKind {
        match self {
            Self::MissingDeclaration { .. } => ComposeCapabilityKind::MissingDeclaration,
            Self::Family { .. } => ComposeCapabilityKind::Family,
            Self::SolverComposition { .. } => ComposeCapabilityKind::SolverComposition,
            Self::RouteShape { .. } => ComposeCapabilityKind::RouteShape,
        }
    }

    /// The arm whose declaration refused, for the family-level kinds.
    #[must_use]
    pub const fn arm(&self) -> Option<Arm> {
        match self {
            Self::MissingDeclaration { arm, .. } | Self::Family { arm, .. } => Some(*arm),
            Self::SolverComposition { .. } | Self::RouteShape { .. } => None,
        }
    }

    /// The offending hop index, for the family-level kinds.
    #[must_use]
    pub const fn hop(&self) -> Option<usize> {
        match self {
            Self::MissingDeclaration { hop, .. } | Self::Family { hop, .. } => Some(*hop),
            Self::SolverComposition { .. } | Self::RouteShape { .. } => None,
        }
    }

    /// The offending family, for the family-level kinds.
    #[must_use]
    pub const fn family(&self) -> Option<Family> {
        match self {
            Self::MissingDeclaration { family, .. } | Self::Family { family, .. } => Some(*family),
            Self::SolverComposition { .. } | Self::RouteShape { .. } => None,
        }
    }

    /// The route's distinct families, for the solver-composition kind.
    #[must_use]
    pub fn families(&self) -> &[Family] {
        match self {
            Self::SolverComposition { families } => families,
            Self::MissingDeclaration { .. } | Self::Family { .. } | Self::RouteShape { .. } => &[],
        }
    }
}

impl fmt::Display for ComposeCapabilityRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDeclaration { arm, hop, family } => write!(
                f,
                "hop {hop} ({} pool) has no {} capability declaration",
                family.label(),
                arm.label()
            ),
            Self::Family { arm, hop, family } => write!(
                f,
                "hop {hop} ({} pool) declares no {} command composition",
                family.label(),
                arm.label()
            ),
            Self::SolverComposition { families } => {
                let labels: Vec<&str> = families.iter().map(|family| family.label()).collect();
                write!(
                    f,
                    "no mixed-solver composition for the family set [{}]",
                    labels.join(", ")
                )
            }
            Self::RouteShape { hop_count } => {
                write!(f, "no command-stream producer for a {hop_count}-hop route")
            }
        }
    }
}

impl std::error::Error for ComposeCapabilityRefusal {}

/// Family-level assessment (ADR-107 D3's first fact level): every hop's family
/// must declare the command composition tier on `arm`.
///
/// Usable where only the hop taxonomy is known — the pre-build crawl gate — and
/// it reads no state, no amount, and no target.
///
/// # Errors
///
/// [`ComposeCapabilityRefusal::MissingDeclaration`] for a family the table does
/// not declare on `arm` (fail closed), [`ComposeCapabilityRefusal::Family`] for
/// a declared family denying command composition.
pub fn assess_families(arm: Arm, hops: &[HopType]) -> Result<(), ComposeCapabilityRefusal> {
    assess_families_with(arm, hops, capabilities)
}

/// [`assess_families`] over an injectable declaration lookup: the production
/// entry passes [`capabilities`], so the fail-closed arm is exercised through
/// the same code path the table's completeness cannot reach.
fn assess_families_with(
    arm: Arm,
    hops: &[HopType],
    lookup: impl Fn(Arm, Family) -> Option<FamilyCapabilities>,
) -> Result<(), ComposeCapabilityRefusal> {
    for (hop, hop_type) in hops.iter().enumerate() {
        let family = hop_type.capability_family();
        let Some(declared) = lookup(arm, family) else {
            return Err(ComposeCapabilityRefusal::MissingDeclaration { arm, hop, family });
        };
        if !declared.compose {
            return Err(ComposeCapabilityRefusal::Family { arm, hop, family });
        }
    }
    Ok(())
}

/// Whole-route assessment (ADR-107 D3's second fact level): the mixed solver's
/// composition row for the route's family SET, then the encoder's producer for
/// its hop SEQUENCE.
///
/// `hops` and `path` describe the same route: the taxonomy carries every hop
/// family (including the solve-only ones `PathInfo` cannot express), while
/// `path` carries the sequence the composer reads. Both authorities are
/// consulted deliberately — a family roster cannot answer either question
/// alone (ADR-107's C4 and C5 are independent defects).
///
/// # Errors
///
/// [`ComposeCapabilityRefusal::SolverComposition`] when the dispatch has no
/// accepting row for the family set, [`ComposeCapabilityRefusal::RouteShape`]
/// when the sequence is outside the encoder's shape key.
pub fn assess_route(hops: &[HopType], path: &PathInfo) -> Result<(), ComposeCapabilityRefusal> {
    let family_set: BTreeSet<HopType> = hops.iter().copied().collect();
    if !composes_families(&family_set) {
        let mut families: Vec<Family> = Vec::with_capacity(family_set.len());
        for hop in hops {
            let family = hop.capability_family();
            if !families.contains(&family) {
                families.push(family);
            }
        }
        return Err(ComposeCapabilityRefusal::SolverComposition { families });
    }
    if !route_shape_has_producer(path) {
        return Err(ComposeCapabilityRefusal::RouteShape {
            hop_count: path.hops.len(),
        });
    }
    Ok(())
}

/// The one settlement admission evaluation: both fact levels, family first, so
/// an admission door cannot consult one and forget the other.
///
/// # Errors
///
/// The first [`ComposeCapabilityRefusal`] either fact level returns.
pub fn assess_settlement(
    hops: &[HopType],
    path: &PathInfo,
) -> Result<(), ComposeCapabilityRefusal> {
    assess_families(Arm::Settlement, hops)?;
    assess_route(hops, path)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used, clippy::panic)]

    use super::*;
    use alloy::primitives::Address;
    use degenbot_execution::{HopInfo, V2HopInfo, V3HopInfo, V4HopInfo};
    use degenbot_pools::ENCODER_FAMILIES;

    /// The solve-only families: every one of them solves in the mixed solver
    /// and none of them has a command-stream encoder arm.
    const SOLVE_ONLY: [Family; 4] = [
        Family::SolidlyStable,
        Family::BalancerWeighted,
        Family::BalancerStable,
        Family::CurveStableswap,
    ];

    fn hop_of(family: Family) -> HopType {
        HopType::ALL
            .iter()
            .copied()
            .find(|hop| hop.capability_family() == family)
            .expect("a solve-only family projects from a hop type")
    }

    fn v2(i: usize) -> HopInfo {
        HopInfo::V2(V2HopInfo {
            pool_address: Address::from([u8::try_from(i).unwrap_or(0); 20]),
            token0_address: Address::ZERO,
            token1_address: Address::ZERO,
            fee: 30,
            zfo: true,
        })
    }

    fn v3(i: usize) -> HopInfo {
        HopInfo::V3(V3HopInfo {
            pool_address: Address::from([u8::try_from(i).unwrap_or(0); 20]),
            token0_address: Address::ZERO,
            token1_address: Address::ZERO,
            fee: 3000,
            zfo: true,
        })
    }

    fn v4(i: usize) -> HopInfo {
        HopInfo::V4(V4HopInfo {
            pool_manager_address: Address::ZERO,
            pool_id_hex: format!("0x{i:02x}"),
            currency0_address: Address::ZERO,
            currency1_address: Address::ZERO,
            fee: 500,
            tick_spacing: 10,
            hook_address: Address::ZERO,
            zfo: true,
        })
    }

    /// A neutral \`PathInfo\` over a protocol sequence the encoder names.
    fn encode_path(prots: &[HopType]) -> PathInfo {
        PathInfo::new(
            prots
                .iter()
                .enumerate()
                .map(|(i, prot)| match prot {
                    HopType::V2 => v2(i),
                    HopType::V3 => v3(i),
                    HopType::V4 => v4(i),
                    other => panic!("{other:?} has no command-stream hop"),
                })
                .collect(),
        )
    }

    /// ADR-107 D3 first fact level: every hop family's declared compose tier
    /// IS the verdict, and the four solve-only families are exactly the
    /// families it refuses.
    #[test]
    fn family_assessment_is_the_declared_compose_tier() {
        for &hop in HopType::ALL {
            let family = hop.capability_family();
            let declared = capabilities(Arm::Settlement, family)
                .expect("every solver family owns a settlement declaration row")
                .compose;
            assert_eq!(
                assess_families(Arm::Settlement, &[HopType::V2, hop]).is_ok(),
                declared,
                "{hop:?}: the family verdict must be the declared compose tier"
            );
            assert_eq!(
                !SOLVE_ONLY.contains(&family),
                declared,
                "{family:?}: a compose declaration and the encoder roster disagree"
            );
        }

        for &family in &SOLVE_ONLY {
            assert!(
                !ENCODER_FAMILIES.contains(&family),
                "{family:?} has no encoder arm"
            );
        }

        assert!(assess_families(Arm::Settlement, &[HopType::V2, HopType::V3]).is_ok());
        assert!(assess_families(Arm::Settlement, &[HopType::V4, HopType::V3, HopType::V2]).is_ok());
    }

    /// A hop-anchored refusal names the hop and the family it found, so an
    /// operator reads which hop of a candidate route is uncomposable.
    #[test]
    fn family_refusal_names_the_hop_and_family() {
        for &family in &SOLVE_ONLY {
            let hop = hop_of(family);
            let refusal = assess_families(Arm::Settlement, &[HopType::V2, HopType::V3, hop])
                .expect_err("a solve-only hop is refused");
            assert_eq!(refusal.kind(), ComposeCapabilityKind::Family);
            assert_eq!(refusal.family(), Some(family));
            assert_eq!(refusal.hop(), Some(2), "the third hop");
            assert_eq!(refusal.arm(), Some(Arm::Settlement));
            assert!(refusal.families().is_empty());
            assert_eq!(
                refusal.to_string(),
                format!(
                    "hop 2 ({} pool) declares no settlement command composition",
                    family.label()
                )
            );
        }
    }

    /// ADR-107 D10: a missing capability declaration fails CLOSED. The
    /// declaration table is complete today, so the hole is injected through the
    /// same lookup the production entry passes.
    #[test]
    fn a_missing_declaration_fails_closed() {
        let hole = |arm: Arm, family: Family| -> Option<FamilyCapabilities> {
            if family == Family::V3 {
                None
            } else {
                capabilities(arm, family)
            }
        };
        let refusal = assess_families_with(Arm::Settlement, &[HopType::V2, HopType::V3], hole)
            .expect_err("an undeclared family is refused");
        assert_eq!(refusal.kind(), ComposeCapabilityKind::MissingDeclaration);
        assert_eq!(refusal.family(), Some(Family::V3));
        assert_eq!(refusal.hop(), Some(1));
        assert!(refusal
            .to_string()
            .contains("has no settlement capability declaration"));
    }

    /// ADR-107 D3 second fact level, solver half: the family SET decides, so a
    /// set the solver's dispatch refuses is refused even though each hop is
    /// individually solvable.
    #[test]
    fn route_assessment_refuses_a_family_set_the_solver_cannot_compose() {
        let mixed = encode_path(&[HopType::V2, HopType::V3]);
        let refusal = assess_route(&[HopType::SolidlyStable, HopType::V3], &mixed)
            .expect_err("the solver refuses a Solidly + concentrated-liquidity set");
        assert_eq!(refusal.kind(), ComposeCapabilityKind::SolverComposition);
        assert_eq!(
            refusal.families(),
            &[Family::SolidlyStable, Family::V3],
            "the refusal names the route's family set in hop order"
        );
        assert_eq!(refusal.family(), None, "a set refusal names no single hop");
        assert_eq!(refusal.hop(), None);
        assert_eq!(
            refusal.to_string(),
            "no mixed-solver composition for the family set [solidly-stable, v3]"
        );
    }

    /// ADR-107 D10: a mixed route over fully supported families but outside the
    /// encoder's shape key is refused at admission — the C4 class, which a
    /// family roster check alone cannot see.
    #[test]
    fn route_assessment_refuses_a_sequence_without_a_producer() {
        for prots in [
            vec![HopType::V2, HopType::V3, HopType::V2, HopType::V3],
            vec![
                HopType::V3,
                HopType::V2,
                HopType::V3,
                HopType::V2,
                HopType::V3,
            ],
            vec![HopType::V3],
        ] {
            let path = encode_path(&prots);
            let refusal = assess_route(&prots, &path)
                .expect_err("a sequence outside the shape key is refused");
            assert_eq!(refusal.kind(), ComposeCapabilityKind::RouteShape);
            assert_eq!(
                refusal.to_string(),
                format!("no command-stream producer for a {}-hop route", prots.len())
            );
            assert!(
                assess_families(Arm::Settlement, &prots).is_ok(),
                "{prots:?}: every family still declares compose"
            );
        }
    }

    /// ADR-107 D10: a route over existing supported families needs no per-path
    /// wiring — the all-V2 any-N chain and every 2/3-hop V2/V3/V4 mix pass both
    /// fact levels.
    #[test]
    fn supported_routes_need_no_per_path_wiring() {
        for n in 2..=5usize {
            let prots = vec![HopType::V2; n];
            let path = encode_path(&prots);
            assert!(
                assess_settlement(&prots, &path).is_ok(),
                "{n}-hop all-V2 must pass"
            );
        }
        let fams = [HopType::V2, HopType::V3, HopType::V4];
        for n in [2usize, 3] {
            for index in 0..fams.len().pow(u32::try_from(n).unwrap_or(0)) {
                let prots: Vec<HopType> = (0..n)
                    .map(|i| {
                        fams[(index / fams.len().pow(u32::try_from(i).unwrap_or(0))) % fams.len()]
                    })
                    .collect();
                let path = encode_path(&prots);
                assert!(
                    assess_settlement(&prots, &path).is_ok(),
                    "{prots:?} is a supported route"
                );
            }
        }
    }

    /// The one settlement entry runs both fact levels in order: a solve-only
    /// family is refused at the family level, before any shape question.
    #[test]
    fn settlement_assessment_runs_both_fact_levels() {
        let supported = encode_path(&[HopType::V2, HopType::V3, HopType::V4]);
        assert!(assess_settlement(&[HopType::V2, HopType::V3, HopType::V4], &supported).is_ok());

        let refused = assess_settlement(&[HopType::V2, HopType::BalancerStable], &supported)
            .expect_err("a solve-only family is refused");
        assert_eq!(refused.kind(), ComposeCapabilityKind::Family);
        assert_eq!(refused.hop(), Some(1));
    }
}

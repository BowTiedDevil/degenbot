//! ADR-059 D2/E1 load-bearing drift test: a family capability declared on an
//! arm must match the seam that is actually wired, or CI fails.
//!
//! The declaration lives in `degenbot_pools::capability`; this test is the
//! cross-vocabulary enforcement. The claims that can be checked from a
//! workspace test are asserted here; the rest are footnoted in the declaration
//! module. A deliberately-flipped declaration must fail one of these asserts.

#![expect(clippy::expect_used, clippy::panic)]

use degenbot_db::schema::table::{is_v2_kind, is_v3_kind, is_v4_kind};
use degenbot_pathfinding::PoolKind;
use degenbot_pools::capability::{
    capabilities, Arm, Family, ENCODER_FAMILIES, V4_FEE_CEILING_EXCLUSIVE, V4_HOOKLESS_ONLY,
};
use degenbot_pools::{
    BalanceVectorVariant, ConcentratedLiquidityVariant, Identity, ReservePairVariant,
};
use degenbot_solvers::mixed::HopType;
use degenbot_strategy::backrun_engine::LaneFamilyTag;

/// A representative taxonomy identity for every solver family.
fn identity_for(family: Family) -> Identity {
    match family {
        Family::V2 => Identity::ReservePair {
            variant: ReservePairVariant::UniswapV2,
            dex: None,
        },
        Family::V3 => Identity::ConcentratedLiquidity {
            variant: ConcentratedLiquidityVariant::UniswapV3,
            dex: None,
        },
        Family::V4 => Identity::ConcentratedLiquidity {
            variant: ConcentratedLiquidityVariant::UniswapV4,
            dex: None,
        },
        Family::SolidlyStable => Identity::ReservePair {
            variant: ReservePairVariant::AerodromeV2 { stable: true },
            dex: None,
        },
        Family::BalancerWeighted => Identity::BalanceVector {
            variant: BalanceVectorVariant::BalancerWeighted,
            dex: None,
        },
        Family::BalancerStable => Identity::BalanceVector {
            variant: BalanceVectorVariant::BalancerStable,
            dex: None,
        },
        Family::CurveStableswap => Identity::BalanceVector {
            variant: BalanceVectorVariant::Curve,
            dex: None,
        },
        Family::LfjBinned => panic!("LFJ has no taxonomy identity until E3"),
    }
}

#[test]
fn backrun_declared_discover_and_compose_matches_the_lane_arms() {
    // "arms exist iff declared": the families the lane vocabulary can project
    // (`LaneFamilyTag::from_identity`) are exactly the families the table
    // declares discover + compose for.
    let wired: Vec<Family> = Family::SOLVER
        .into_iter()
        .filter(|family| LaneFamilyTag::from_identity(&identity_for(*family)).is_some())
        .collect();
    let declared: Vec<Family> = Family::SOLVER
        .into_iter()
        .filter(|family| {
            let caps = capabilities(Arm::Backrun, *family).expect("table row");
            caps.discover && caps.compose
        })
        .collect();

    assert_eq!(
        wired, declared,
        "backrun lane arms and the declared discover/compose set drifted"
    );
    assert_eq!(
        wired,
        vec![Family::V2, Family::V3, Family::V4],
        "the backrun arm is V2/V3/V4 only"
    );

    // The tags map one-to-one onto the declared families.
    for (tag, family) in [
        (LaneFamilyTag::V2, Family::V2),
        (LaneFamilyTag::V3, Family::V3),
        (LaneFamilyTag::V4, Family::V4),
    ] {
        assert_eq!(
            LaneFamilyTag::from_identity(&identity_for(family)),
            Some(tag)
        );
    }
}

#[test]
fn hop_type_solve_coverage_and_settlement_compose_match_the_declaration() {
    // HopType is solve-capable by construction: every variant must project
    // from exactly one declared solver family.
    const HOP_TYPES: [(HopType, Family); 7] = [
        (HopType::V2, Family::V2),
        (HopType::V3, Family::V3),
        (HopType::V4, Family::V4),
        (HopType::SolidlyStable, Family::SolidlyStable),
        (HopType::BalancerWeighted, Family::BalancerWeighted),
        (HopType::BalancerStable, Family::BalancerStable),
        (HopType::CurveStableswap, Family::CurveStableswap),
    ];
    for (hop, family) in HOP_TYPES {
        assert_eq!(
            HopType::from(&identity_for(family)),
            hop,
            "{} must project to {hop:?}",
            family.label()
        );
        assert!(family.is_solver());
        assert!(
            capabilities(Arm::Settlement, family).is_some(),
            "settlement is missing a declaration row for {}",
            family.label()
        );
    }
    assert_eq!(Family::SOLVER.len(), HOP_TYPES.len());

    // `compose` is the executor's HopInfo arm set (D5), not the solve set: the
    // four solve-only families have no command-stream encoder.
    for family in Family::ALL {
        let caps = capabilities(Arm::Settlement, family).expect("table row");
        assert_eq!(
            caps.compose,
            ENCODER_FAMILIES.contains(&family),
            "settlement {}: compose must track the encoder roster",
            family.label()
        );
    }
    assert_eq!(ENCODER_FAMILIES, [Family::V2, Family::V3, Family::V4]);
}

#[test]
fn connector_index_kinds_match_backrun_discover_per_kind_string() {
    for &(kind, pool_kind) in PoolKind::KNOWN_KINDS {
        let family = match pool_kind {
            PoolKind::V2 => Family::V2,
            PoolKind::V3 => Family::V3,
            PoolKind::V4 => Family::V4,
            // `PoolKind` is `#[non_exhaustive]`; the table stays closed.
            _ => panic!("unexpected PoolKind in KNOWN_KINDS"),
        };
        let declared_discover = capabilities(Arm::Backrun, family)
            .expect("table row")
            .discover;
        let wired = match pool_kind {
            PoolKind::V2 => is_v2_kind(kind),
            PoolKind::V3 => is_v3_kind(kind),
            PoolKind::V4 => is_v4_kind(kind),
            _ => false,
        };
        assert_eq!(
            declared_discover, wired,
            "kind {kind}: declared backrun discover {declared_discover} != connector admission {wired}"
        );
        // The golden projection stays the single kind source.
        assert_eq!(PoolKind::from_kind_str(kind), Some(pool_kind));
    }
}

#[test]
fn v4_footnotes_reference_the_declaration() {
    // The declared fee ceiling is the executor's 2-byte V4 fee bound.
    assert_eq!(
        V4_FEE_CEILING_EXCLUSIVE,
        degenbot_executor::encoders::V4_FEE_ENCODER_MAX
    );
    // The hookless-only admission is a declared restriction, not a guess.
    const { assert!(V4_HOOKLESS_ONLY) };

    // Both arms declare V4 fully wired post-epic (the capability the V4
    // backrun epic bought).
    for arm in Arm::ALL {
        let caps = capabilities(arm, Family::V4).expect("table row");
        assert!(
            caps.extract && caps.admit && caps.discover && caps.compose,
            "{} V4 must be all-true post-epic",
            arm.label()
        );
    }
}

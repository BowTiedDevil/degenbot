//! Post-target lean-overlay spec (task QZF6HQ).
//!
//! Seam: `degenbot_bot::bot_core::post_target::{v2_post_target, PostTargetPlan}`.
//! Property tests check the overlay against an INDEPENDENT reference getAmountOut
//! (independent-lane implementation) and the staged reserves delta; family
//! gating asserts the exact-sim-only policy.

#![allow(clippy::unwrap_used, clippy::panic)]

use alloy::primitives::U256;
use degenbot_bot::bot_core::post_target::{
    v2_post_target, OverlayError, PostTargetPlan, V2FeeParams,
};
use degenbot_pathfinding::PoolKind;

const FEE_30: V2FeeParams = V2FeeParams {
    gamma_numer: 997,
    fee_denom: 1000,
};

/// Independent reference (written from the on-chain Solidity, NOT the core
/// helper — a lane split for test fidelity).
fn reference_amount_out(reserve_in: U256, reserve_out: U256, amount_in: U256) -> U256 {
    let ain = amount_in * U256::from(997u64);
    let num = ain * reserve_out;
    let den = reserve_in * U256::from(1000u64) + ain;
    num / den
}
#[test]
fn t01_matches_reference_and_updates_reserves() {
    let view = v2_post_target(1_000_000, 2_000_000, FEE_30, 50_000).unwrap();
    let expect = reference_amount_out(
        U256::from(1_000_000u64),
        U256::from(2_000_000u64),
        U256::from(50_000u64),
    );
    assert_eq!(U256::from(view.amount_out), expect);
    assert_eq!(view.new_reserve_in, 1_050_000);
    assert_eq!(view.new_reserve_out, 2_000_000 - view.amount_out);
}

#[test]
fn t02_zero_amount_not_computable() {
    assert_eq!(
        v2_post_target(1_000, 1_000, FEE_30, 0),
        Err(OverlayError::NotComputable)
    );
}

#[test]
fn t03_huge_amounts_stay_chain_valid_and_safe() {
    // A near-u128 amount with tiny reserves: the math stays chain-valid
    // (out approaches the fee-shield asymptote; never reaches reserve_out).

    let view = v2_post_target(1_000, 1_000, FEE_30, u128::MAX / 2).unwrap();

    assert_eq!(view.amount_out, 999);
    assert_eq!(view.new_reserve_out, 1);
}

#[test]

fn t04_v2_single_swap_cannot_fully_drain() {
    // getAmountOut asymptote: out -> reserve_out * 997/1000 — the fee shields

    // the pool from a single-swap full drain, so the zero-reserve guard is a

    // safety net that valid math cannot reach.

    for (r_out, a) in [(1_000u128, u128::MAX / 2), (10_000, 5_000), (50, 1)] {
        let view = v2_post_target(1_000_000, r_out, FEE_30, a).unwrap();

        assert!(view.new_reserve_out >= 1, "fee shield holds (r={r_out})");
    }
}

// Deterministic xorshift64* generator (no external RNG dep).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        let mut x = self.0;

        x ^= x >> 12;

        x ^= x << 25;

        x ^= x >> 27;

        self.0 = x;

        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo {
            return lo;
        }

        lo + (self.next() % (hi - lo))
    }
}

#[test]
fn t05_randomized_property_against_reference() {
    let mut rng = Lcg(0x0BAC_0123_4567_89EF);
    for _ in 0..2_000 {
        let r_in = u128::from(rng.range(1_000, u64::MAX));
        let r_out = u128::from(rng.range(1_000, u64::MAX));
        let cap = u64::try_from(r_in / 2 + 1).unwrap_or(u64::MAX);
        let a_in = u128::from(rng.range(1, cap));
        let Ok(view) = v2_post_target(r_in, r_out, FEE_30, a_in) else {
            // Only overflow (the chain reverting) may ever fail.
            panic!("unexpected overlay failure r={r_in}/{r_out} a={a_in}");
        };
        let expect = reference_amount_out(U256::from(r_in), U256::from(r_out), U256::from(a_in));
        assert_eq!(U256::from(view.amount_out), expect);
        assert_eq!(view.new_reserve_in, r_in + a_in);
        assert_eq!(view.new_reserve_out, r_out - view.amount_out);
        assert!(view.new_reserve_out >= 1);
    }
}

#[test]
fn t06_plan_stages_v2_and_routes_v3_v4_exact() {
    let mut plan = PostTargetPlan::new();
    plan.stage(1, PoolKind::V2, 100_000, 200_000, FEE_30, 1_000)
        .unwrap();
    assert_eq!(plan.overlays.len(), 1);
    assert!(plan.exact_sim_pool_ids.is_empty());

    for (pid, kind) in [(2u64, PoolKind::V3), (3u64, PoolKind::V4)] {
        assert_eq!(
            plan.stage(pid, kind, 1, 1, FEE_30, 10),
            Err(OverlayError::ExactSimOnly)
        );
    }
    assert_eq!(plan.exact_sim_pool_ids, vec![2, 3]);
}

#[test]
fn t07_v3_in_plan_has_no_lean_overlay_record() {
    let mut plan = PostTargetPlan::new();
    let _ = plan.stage(5, PoolKind::V3, 1, 1, FEE_30, 10);
    assert!(plan.overlays.get(&5).is_none(), "v3 stays exact-sim only");
}

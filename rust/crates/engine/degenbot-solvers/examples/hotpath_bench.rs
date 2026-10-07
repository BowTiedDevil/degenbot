#![expect(clippy::expect_used, clippy::doc_markdown)]
//! Fixed-workload solver benchmark for hotpath reports (and hotpath Cloud).
//!
//! Each iteration plays one synthetic block: the CL projection tables are
//! rebuilt for every concentrated-liquidity pool (as `HopProjectionCache`
//! does once per `(pool, direction)` on a state change), then every fixture
//! path is solved through the one `solve_path` entry with offline gate deps.
//! Inputs are constant and offline, so two runs on the same machine do
//! identical work and their reports are comparable.
//!
//! With the `hotpath` feature off, `#[hotpath::main]` and every probe expand to
//! no-ops and this is a plain loop.

use std::hint::black_box;
use std::sync::Arc;

use alloy::primitives::U256;
use degenbot_math::balancer::stable_math::calculate_invariant_deployed;
use degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_math::curve::stableswap::{DVariant, YVariant};
use degenbot_math::v2::IntHopState;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::cl::{build_cl_crossing_table, build_cl_word_profiles_from_crossings};
use degenbot_solvers::mixed::{
    solve_path, BalancerStableHopState, CurveStableswapHopState, ResolvedHop, ResolvedMixedPath,
};
use degenbot_solvers::profit_envelope::GateDeps;

const ITERATIONS: usize = 2_000;
const RANGES_PER_POOL: i32 = 64;
const TICK_SPACING: i32 = 10;
const CL_LIQUIDITY: u128 = 1_000_000_000_000_000_000_000; // 1e21
const GAMMA_NUMER: u64 = 997_000; // 0.3% fee tier
const FEE_DENOM: u64 = 1_000_000;

// Stable-family fixtures: the profitable paths the `digest` bench solves
// (1000/2000 vs 1000/1950 reserves at 18dp, the full 25-iteration
// golden-section search).

fn one_e18() -> U256 {
    U256::from(10u64).pow(U256::from(18u64))
}

/// amp = 200 × AMP_PRECISION(1000) = 200_000 — matches `balancer_stable_params`.
fn balancer_amp() -> U256 {
    U256::from(200_000u64)
}

/// Upscaled balances `[b0, b1]` at 18dp (scaling_factors are identity in the
/// test fixture, so upscale is a no-op). Amp/name come from the test.
fn balancer_balances(b0: u128, b1: u128) -> Vec<U256> {
    let e = one_e18();
    vec![U256::from(b0) * e, U256::from(b1) * e]
}

/// Build a fully-resolved BalancerStable hop state (digest baked in), mirroring
/// what `resolve_path` produces. `invariant_version == 2` → the deployed
/// `calculate_invariant_deployed(amp, &balances, true)` path (round_up=true).
fn balancer_stable_hop(b0: u128, b1: u128, zero_for_one: bool) -> BalancerStableHopState {
    let balances = balancer_balances(b0, b1);
    let invariant = calculate_invariant_deployed(balancer_amp(), &balances, true)
        .expect("invariant converges on valid fixture");
    let (token_index_in, token_index_out) = if zero_for_one { (0, 1) } else { (1, 0) };
    BalancerStableHopState {
        amp: balancer_amp(),
        balances,
        token_index_in,
        token_index_out,
        invariant,
        swap_fee: U256::from(10_000_000_000_000u64), // 0.01% of 1e18
        scaling_factor_in: U256::from(1u64),
        scaling_factor_out: U256::from(1u64),
    }
}

/// Profitable 2-hop balancer-stable path: 1000/2000 (zfo=true) → 1000/1950 (zfo=false).
/// Mirrors `balancer_stable_finds_profitable_arb`.
fn balancer_stable_path() -> ResolvedMixedPath {
    ResolvedMixedPath {
        hops: vec![
            ResolvedHop::BalancerStable {
                state: balancer_stable_hop(1000, 2000, true),
            },
            ResolvedHop::BalancerStable {
                state: balancer_stable_hop(1000, 1950, false),
            },
        ],
        valid: true,
        state_nonces: vec![],
        max_update_block: 0, // no CL hops in this fixture; default write-stamp
    }
}

// --- Curve fixtures ---

fn precision() -> U256 {
    one_e18()
} // 1e18
const CURVE_FEE_DENOM: U256 = U256::from_limbs([10_000_000_000u64, 0, 0, 0]); // 1e10 — const-constructible
const A_PRECISION: U256 = U256::from_limbs([100u64, 0, 0, 0]);

fn curve_amp() -> U256 {
    U256::from(10u64) * A_PRECISION // a_coefficient=10 × A_PRECISION
}

fn curve_balances(b0: u128, b1: u128) -> Vec<U256> {
    let e = one_e18();
    vec![U256::from(b0) * e, U256::from(b1) * e]
}

/// Identity rate multipliers (1e18) — the test fixture's values.
fn curve_rate_multipliers() -> Vec<U256> {
    vec![precision(), precision()]
}

/// Build the xp digest exactly as `resolve_path` does:
/// `xp[i] = balances[i] * rate_multipliers[i] / PRECISION`.
fn curve_xp(balances: &[U256], rate_multipliers: &[U256]) -> Vec<U256> {
    let p = precision();
    balances
        .iter()
        .zip(rate_multipliers.iter())
        .map(|(b, rm)| b.saturating_mul(*rm) / p)
        .collect()
}

fn curve_stable_hop(b0: u128, b1: u128, zero_for_one: bool) -> CurveStableswapHopState {
    let balances = curve_balances(b0, b1);
    let rms = curve_rate_multipliers();
    let xp = curve_xp(&balances, &rms);
    let (token_index_in, token_index_out) = if zero_for_one { (0, 1) } else { (1, 0) };
    CurveStableswapHopState {
        amp: curve_amp(),
        a_precision: A_PRECISION,
        xp,
        token_index_in,
        token_index_out,
        n_coins: U256::from(2u64),
        fee: U256::from(4_000_000u64), // 0.04% of 1e10
        fee_denom: CURVE_FEE_DENOM,
        precision: precision(),
        rate_multiplier_in: rms[token_index_in],
        rate_multiplier_out: rms[token_index_out],
        y_variant: YVariant::try_from_u8(1).expect("standard y variant"),
        d_variant: DVariant::try_from_u8(1).expect("standard d variant"),
    }
}

/// Profitable 2-hop curve path: 1000/2000 → 1000/1950. Mirrors
/// `curve_stable_finds_profitable_arb`.
fn curve_stable_path() -> ResolvedMixedPath {
    ResolvedMixedPath {
        hops: vec![
            ResolvedHop::CurveStableswap {
                state: curve_stable_hop(1000, 2000, true),
            },
            ResolvedHop::CurveStableswap {
                state: curve_stable_hop(1000, 1950, false),
            },
        ],
        valid: true,
        state_nonces: vec![],
        max_update_block: 0, // no CL hops in this fixture; default write-stamp
    }
}

// CL fixtures.

fn sqrt_price_at(tick: i32) -> U256 {
    U256::from(get_sqrt_ratio_at_tick_internal(tick).expect("tick within bounds"))
}

/// `RANGES_PER_POOL` contiguous initialized ranges walked away from
/// `current_tick` in the swap direction, all at `CL_LIQUIDITY`.
fn cl_sequence(current_tick: i32, zero_for_one: bool) -> IntV3TickRangeSequence {
    let ranges = (0..RANGES_PER_POOL)
        .map(|i| {
            let (tick_lo, tick_hi, entry_tick) = if zero_for_one {
                let hi = current_tick - i * TICK_SPACING;
                (hi - TICK_SPACING, hi, hi)
            } else {
                let lo = current_tick + i * TICK_SPACING;
                (lo, lo + TICK_SPACING, lo)
            };
            IntV3TickRangeHop {
                liquidity: CL_LIQUIDITY,
                sqrt_price_x96: sqrt_price_at(entry_tick),
                sqrt_price_lower_x96: sqrt_price_at(tick_lo),
                sqrt_price_upper_x96: sqrt_price_at(tick_hi),
                gamma_numer: GAMMA_NUMER,
                fee_denom: FEE_DENOM,
                zero_for_one,
                word_boundary_prices: Vec::new(),
            }
        })
        .collect();
    IntV3TickRangeSequence::new(ranges).expect("uniform fee and direction")
}

/// Build the per-pool projection tables and wrap them in the resolved V3 hop.
fn v3_hop(seq: &Arc<IntV3TickRangeSequence>) -> ResolvedHop {
    let crossing_table = build_cl_crossing_table(seq);
    let word_profiles = build_cl_word_profiles_from_crossings(&crossing_table);
    ResolvedHop::V3 {
        int_seq: Arc::clone(seq),
        word_profiles: Arc::new(word_profiles),
        crossing_table: Arc::new(crossing_table),
    }
}

fn path(hops: Vec<ResolvedHop>) -> ResolvedMixedPath {
    ResolvedMixedPath {
        hops,
        valid: true,
        state_nonces: vec![],
        max_update_block: 0,
    }
}

#[hotpath::main(percentiles = [50, 95, 99])]
fn main() {
    // Pool A sells token0 at price 1.0 (tick 0); pool B and the V2 pool buy it
    // back ~3-4% cheaper, so both CL round trips are profitable after fees.
    let pool_a = Arc::new(cl_sequence(0, true));
    let pool_b = Arc::new(cl_sequence(-400, false));
    let e21 = U256::from(CL_LIQUIDITY);
    let v2_back = IntHopState::new(
        e21,
        e21 * U256::from(103u64) / U256::from(100u64),
        GAMMA_NUMER,
        FEE_DENOM,
    );

    let stable_paths = [
        ("balancer_stable", balancer_stable_path()),
        ("curve_stable", curve_stable_path()),
    ];

    for _ in 0..ITERATIONS {
        let a = v3_hop(&pool_a);
        let cl_paths = [
            ("v3_v3", path(vec![a.clone(), v3_hop(&pool_b)])),
            (
                "v3_v2",
                path(vec![
                    a,
                    ResolvedHop::V2 {
                        state: v2_back.clone(),
                    },
                ]),
            ),
        ];
        for (name, p) in stable_paths.iter().chain(cl_paths.iter()) {
            let solved = solve_path(black_box(p), &GateDeps::offline());
            assert!(
                solved.result.as_ref().is_some_and(|r| !r.profit.is_zero()),
                "{name} fixture must stay profitable"
            );
            black_box(solved);
        }
    }
}

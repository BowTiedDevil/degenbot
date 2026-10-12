#![expect(clippy::expect_used)]
//! Regression net: the golden-section Solidly arm lands within a few wei of
//! the discrete optimum on realistic one-Solidly-hop pools.
//!
//! On these paths the stable-pool profit curve is a flat plateau near its
//! peak, so the arm's bracket residual — its final integer verify scans only
//! ±3 candidates — costs a handful of wei at most. That flatness is the whole
//! reason the golden arm may stand in for an exact search.
//!
//! Why this test exists: the flatness is a *measured* property, not a proof.
//! A future change to the arm (its bracket, its iteration count, its verify
//! radius) or to the Solidly math leaf can move the peak or widen the
//! residual. This test sweeps a fine window around the arm's own answer at
//! unit resolution and fails, loudly and with the offending path's
//! parameters, whenever the arm's profit falls more than [`TOLERANCE_WEI`]
//! below the brute-force discrete maximum over that window.
//!
//! The corpus generator and the fine-sweep verifier are salvaged from a
//! rejected t-parametrized arm's test scaffolding. Only the test scaffolding
//! survived; nothing here depends on that module's production half.

use alloy::primitives::U256;
use degenbot_math::v2::IntHopState;
use degenbot_solvers::mixed::{
    simulate_solidly_path, solve_path, ResolvedHop, ResolvedMixedPath, SolidlyHopState,
    SolvePathResult,
};
use degenbot_solvers::profit_envelope::GateDeps;
use degenbot_uniswap::dex_identity::DexVariant;

/// Sweep half-width, in native input units, around the arm's returned answer.
/// The arm's own integer verify only scans ±3, so this window is an
/// independent check of the local plateau rather than a replay of the arm.
/// Chosen (with [`SAMPLE_SIZE`]) so the sampled sweep stays well under ~2 s
/// on the dispatch machine.
const SWEEP_RADIUS: u64 = 8_000;

/// Documented tolerance: the arm may sit at most this many wei below the
/// brute-force discrete maximum over the swept window. The flat stable-pool
/// plateau leaves a residual of 0-2 wei in practice; the bound admits the
/// leaf's floor-division jitter without admitting a moved peak.
const TOLERANCE_WEI: u64 = 8;

/// How many profitable paths the deterministic sample sweeps (capped by the
/// profitable subset's size). Spread evenly across that subset.
const SAMPLE_SIZE: usize = 32;

/// The pinned size of the corpus matrix: 4 shapes x 3 scales x 3 Solidly fees
/// x 2 V2 fees x 4 imbalances.
const CORPUS_SIZE: usize = 288;

/// The pinned count of corpus paths the committed arm finds profitable. A
/// change here means the arm's coverage of the matrix moved — read the diff
/// before re-pinning.
const PROFITABLE_SIZE: usize = 143;

/// One corpus entry: the path plus a human-readable parameter tag quoted in
/// failure messages so a regression names the shape that broke.
struct Case {
    tag: String,
    path: ResolvedMixedPath,
}

/// A constant-product hop: reserves are already oriented (in, out).
fn v2_hop(reserve_in: u128, reserve_out: u128, gamma_numer: u64, fee_denom: u64) -> ResolvedHop {
    ResolvedHop::V2 {
        state: IntHopState::new(
            U256::from(reserve_in),
            U256::from(reserve_out),
            gamma_numer,
            fee_denom,
        ),
    }
}

/// A Solidly/Aerodrome stable hop. `scale_*` are the token decimal scales
/// (`10^decimals`), the divisor the leaf normalizes reserves by; `token_in`
/// is the leaf's un-oriented direction flag.
fn solidly_hop(
    reserves_0: u128,
    reserves_1: u128,
    scale_0: u128,
    scale_1: u128,
    token_in: u8,
    fee_numer: u64,
    fee_denom: u64,
) -> ResolvedHop {
    ResolvedHop::SolidlyStable {
        state: SolidlyHopState {
            reserves_0: U256::from(reserves_0),
            reserves_1: U256::from(reserves_1),
            decimals_0: U256::from(scale_0),
            decimals_1: U256::from(scale_1),
            token_in,
            fee_numer: U256::from(fee_numer),
            fee_denom: U256::from(fee_denom),
            stable: true,
            variant: DexVariant::AerodromeV2Stable,
        },
    }
}

fn path(hops: Vec<ResolvedHop>) -> ResolvedMixedPath {
    let nonces = vec![1u64; hops.len()];
    ResolvedMixedPath {
        hops,
        valid: true,
        state_nonces: nonces,
        max_update_block: 0,
    }
}

/// The smallest native reserve whose scaled value clears the Solidly leaf's
/// faithful-arithmetic floor. The leaf normalizes `reserve * ONE / scale`, so
/// a native reserve of `floor * scale / ONE` yields a scaled reserve at the
/// floor — below it the leaf's integer `get_y` degrades and the corpus would
/// measure arithmetic noise instead of the solver's optimality.
fn faithful_base(scale: u128) -> u128 {
    let scaled_floor = 4u128 << 60;
    (scaled_floor.saturating_mul(scale) / 1_000_000_000_000_000_000).max(2)
}

/// The one-Solidly-hop corpus: four path shapes x three decimal scales x
/// three Solidly fees x two V2 fees x four basis-point imbalances = 288 paths.
///
/// The shapes place the Solidly hop first, last, and in the middle among V2
/// hops (and once with the Solidly hop's own reserves imbalanced), so the
/// mispricing is harvested on both travel directions: outbound (shape 2) and
/// return (shapes 1 and 3), plus the Solidly hop itself (shape 4).
fn synth_one_solidly_corpus() -> Vec<Case> {
    let scales = [1u128, 1_000_000, 1_000_000_000_000_000_000];
    let solidly_fees = [(3u64, 1000u64), (1, 1000), (3, 10000)];
    let v2_fees = [(997u64, 1000u64), (999, 1000)];
    // Basis-point mispricings (1.01x .. 1.10x): the modest imbalances a real
    // cross-venue arb sees, not a 10x dislocation.
    let imbalances = [101u128, 102, 105, 110];
    let mut out = Vec::new();
    for &scale in &scales {
        let base = faithful_base(scale);
        for &(sf_n, sf_d) in &solidly_fees {
            for &(vf_n, vf_d) in &v2_fees {
                for &b in &imbalances {
                    let heavier = base.saturating_mul(b) / 100;
                    let tag = |shape: &str| {
                        format!(
                            "{shape} scale={scale} solidly_fee={sf_n}/{sf_d} \
                             v2_fee={vf_n}/{vf_d} imbalance_bps={b}"
                        )
                    };
                    // Solidly A->B, V2 B->A (return-leg mispricing).
                    out.push(Case {
                        tag: tag("solidly-first"),
                        path: path(vec![
                            solidly_hop(base, base, scale, scale, 0, sf_n, sf_d),
                            v2_hop(base, heavier, vf_n, vf_d),
                        ]),
                    });
                    // V2 A->B, Solidly B->A (outbound-leg mispricing).
                    out.push(Case {
                        tag: tag("solidly-last"),
                        path: path(vec![
                            v2_hop(base, heavier, vf_n, vf_d),
                            solidly_hop(base, base, scale, scale, 1, sf_n, sf_d),
                        ]),
                    });
                    // V2 A->B, Solidly B->C, V2 C->A (Solidly in the middle).
                    out.push(Case {
                        tag: tag("solidly-middle"),
                        path: path(vec![
                            v2_hop(base, base, vf_n, vf_d),
                            solidly_hop(base, base, scale, scale, 0, sf_n, sf_d),
                            v2_hop(base, heavier, vf_n, vf_d),
                        ]),
                    });
                    // Solidly A->B with its own B reserve heavier.
                    out.push(Case {
                        tag: tag("solidly-imbalanced"),
                        path: path(vec![
                            solidly_hop(base, heavier, scale, scale, 0, sf_n, sf_d),
                            v2_hop(base, base, vf_n, vf_d),
                        ]),
                    });
                }
            }
        }
    }
    out
}

/// The committed dispatch entry: the gate plus the golden-section arm, with
/// offline deps (no cache, no capture) so the test is deterministic.
fn solve(case: &Case) -> Option<SolvePathResult> {
    solve_path(&case.path, &GateDeps::offline()).result
}

/// Brute-force the discrete optimum at unit resolution over
/// `[center - SWEEP_RADIUS, center + SWEEP_RADIUS]`, returning the argmax
/// input and its profit.
fn brute_force_max(case: &Case, center: U256) -> (U256, U256) {
    let one = U256::from(1u64);
    let lo = center.saturating_sub(U256::from(SWEEP_RADIUS)).max(one);
    let hi = center.saturating_add(U256::from(SWEEP_RADIUS));
    let mut best_input = lo;
    let mut best_profit = U256::ZERO;
    let mut x = lo;
    while x <= hi {
        let profit = simulate_solidly_path(x, &case.path.hops).saturating_sub(x);
        if profit > best_profit {
            best_profit = profit;
            best_input = x;
        }
        if x == U256::MAX {
            break;
        }
        x += one;
    }
    (best_input, best_profit)
}

#[test]
fn golden_arm_lands_within_tolerance_of_the_discrete_optimum() {
    let corpus = synth_one_solidly_corpus();
    assert_eq!(
        corpus.len(),
        CORPUS_SIZE,
        "the corpus matrix must stay pinned at 4 shapes x 3 scales x 3 Solidly \
         fees x 2 V2 fees x 4 imbalances"
    );

    let profitable: Vec<&Case> = corpus
        .iter()
        .filter(|case| {
            solve(case).is_some_and(|r| r.profit > U256::ZERO && !r.optimal_input.is_zero())
        })
        .collect();
    assert_eq!(
        profitable.len(),
        PROFITABLE_SIZE,
        "the committed arm's profitable coverage of the corpus moved: {} of {} \
         paths profitable",
        profitable.len(),
        corpus.len()
    );

    // Deterministic, matrix-spread sample: evenly spaced indices over the
    // profitable subset, so the sample tracks the shape/scale/fee/imbalance
    // spread of the corpus rather than one corner of it.
    let sample = SAMPLE_SIZE.min(profitable.len());
    assert!(
        sample >= 16,
        "the sample must sweep at least 16 profitable paths (got {sample})"
    );
    let mut sampled = 0usize;
    for j in 0..sample {
        let case = profitable[j * profitable.len() / sample];
        let result = solve(case).expect("sampled path is profitable by construction");
        let (brute_input, brute_profit) = brute_force_max(case, result.optimal_input);
        let residual = brute_profit.saturating_sub(result.profit);
        assert!(
            residual <= U256::from(TOLERANCE_WEI),
            "golden arm lost {residual} wei to the discrete optimum (tolerance {} wei): {} \
             arm_input={} arm_profit={} brute_input={brute_input} brute_profit={brute_profit}",
            TOLERANCE_WEI,
            case.tag,
            result.optimal_input,
            result.profit
        );
        sampled += 1;
    }
    assert_eq!(sampled, sample, "the sample must be fully populated");
}

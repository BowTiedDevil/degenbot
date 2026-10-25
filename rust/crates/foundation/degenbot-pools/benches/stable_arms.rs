#![expect(clippy::expect_used)]

//! Latency net for the four stable-family solve arms plus the all-V2
//! exact-Mobius reference.
//!
//! Why this exists: the golden-section search inside each stable arm sits on
//! the hot path of a latency-sensitive arbitrage bot, yet every correctness
//! test in this crate stays green while an arm's wall time silently triples.
//! The session's most expensive finding made that concrete — a t-parametrized
//! search variant was rejected only after its own throwaway bench measured an
//! 81x regression against the numeric search it was meant to replace. A
//! closed-form probe can cost far more than the numeric search it removes.
//! That bench was deleted with the revert, so the regression it caught was
//! never pinned. This module is the permanent replacement: a fixed,
//! deterministic corpus per arm driven through the public `solve_path` entry,
//! so a future change is judged by criterion's change detection against the
//! recorded baseline rather than by an ad-hoc comparison nobody reruns.
//!
//! Corpus shapes mirror `tests/stable_golden_drift_net.rs` (the correctness
//! drift net for the same four arms): a couple of bases, a couple of fees, a
//! couple of imbalances, and both path shapes (family hop first and family hop
//! last). Benches are separate binaries and cannot import `tests/common/mod.rs`,
//! so the hop builders are inlined here at the same resolved-state convention
//! the drift net uses (all nonces one, valid, block zero).
//!
//! Each arm is one criterion group; each path in the corpus is one
//! `bench_function`, so criterion reports per-path wall time and its change
//! detection flags a shift on any single path. The all-V2 group is the
//! exact-Mobius reference: the same `solve_path` entry over a pure
//! constant-product path, so a change that penalizes the stable arms but not
//! the closed-form V2 arm (or the reverse) shows up as a divergence.
//!
//! The sampling window is shortened from criterion's defaults (a tenth of a
//! second of warm-up, four tenths of a second of measurement) so the whole
//! bench fits inside the foreground harness bound; the corpus is deterministic
//! and each solve is microseconds, so the shortened window still resolves a
//! per-arm latency shift.

use std::hint::black_box;
use std::time::Duration;

use alloy::primitives::U256;
use criterion::{criterion_group, criterion_main, Criterion};
use degenbot_math::balancer::constants::PowVersion;
use degenbot_math::curve::stableswap::{DVariant, YVariant};
use degenbot_math::solidly::SOLIDLY_MIN_SCALED_RESERVE;
use degenbot_math::v2::IntHopState;
use degenbot_pools::solve::gate::GateDeps;
use degenbot_pools::solve::{
    solve_path, BalancerStableHopState, BalancerWeightedHopState, CurveStableswapHopState,
    ResolvedHop, ResolvedMixedPath, SolidlyHopState,
};
use degenbot_uniswap::dex_identity::DexVariant;

/// 18-decimal scale: the token decimals scale for Solidly hops and the
/// precision/rate unit for the Curve hop.
const ONE_E18: u128 = 1_000_000_000_000_000_000;

/// The two balances every family corpus is built from — a unit scale and a
/// four-unit scale, matching the drift-net bases.
const BASES: [u128; 2] = [ONE_E18, 4 * ONE_E18];

/// The imbalance percentages the V2 leg is scaled by (a couple, mirroring the
/// drift-net corpus); the family hop itself stays balanced so the corpus
/// measures the search rather than an arithmetic edge.
const IMBALANCES: [u128; 2] = [101, 110];

/// Shortened sampling window — see the module docs for why.
const WARM_UP: Duration = Duration::from_millis(100);
/// Shortened measurement window — see the module docs for why.
const MEASUREMENT: Duration = Duration::from_millis(400);
/// Samples per path; criterion's floor.
const SAMPLES: usize = 10;

// ---------------------------------------------------------------------------
// Hop builders — inlined from tests/common/mod.rs (benches cannot import it).
// ---------------------------------------------------------------------------

/// Wrap hops into a resolved path at the resolved-state convention (all nonces
/// one, valid, block zero).
fn path(hops: Vec<ResolvedHop>) -> ResolvedMixedPath {
    let nonces = vec![1u64; hops.len()];
    ResolvedMixedPath {
        hops,
        valid: true,
        state_nonces: nonces,
        max_update_block: 0,
    }
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

/// A Solidly/Aerodrome stable hop; `token_in` is the leaf's un-oriented
/// direction flag and `scale_*` are the token decimal scales.
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

/// A balanced 50/50 Balancer weighted hop. `reverse` traverses it
/// output-to-input; equal balances make the direction numerically irrelevant.
fn weighted_hop(base: u128, swap_fee: u128, reverse: bool) -> ResolvedHop {
    let half = U256::from(ONE_E18) / U256::from(2u64);
    let _ = reverse;
    ResolvedHop::BalancerWeighted {
        state: BalancerWeightedHopState {
            balance_in: U256::from(base),
            balance_out: U256::from(base),
            weight_in: half,
            weight_out: half,
            swap_fee: U256::from(swap_fee),
            pow_version: PowVersion::V2,
            scaling_factor_in: U256::from(1u64),
            scaling_factor_out: U256::from(1u64),
        },
    }
}

/// A balanced Balancer stable hop with its invariant pre-computed at the
/// resolved-state convention.
fn stable_hop(base: u128, swap_fee: u128, reverse: bool) -> ResolvedHop {
    let balances = vec![U256::from(base), U256::from(base)];
    let amp = U256::from(200_000u64);
    let invariant =
        degenbot_math::balancer::stable_math::calculate_invariant_deployed(amp, &balances, true)
            .expect("balanced positive balances converge");
    ResolvedHop::BalancerStable {
        state: BalancerStableHopState {
            amp,
            balances,
            token_index_in: usize::from(reverse),
            token_index_out: usize::from(!reverse),
            invariant,
            swap_fee: U256::from(swap_fee),
            scaling_factor_in: U256::from(1u64),
            scaling_factor_out: U256::from(1u64),
        },
    }
}

/// A balanced two-coin Curve stableswap hop (identity rates, standard
/// variants).
fn curve_hop(base: u128, fee: u128, reverse: bool) -> ResolvedHop {
    let precision = U256::from(ONE_E18);
    ResolvedHop::CurveStableswap {
        state: CurveStableswapHopState {
            amp: U256::from(1_000u64),
            a_precision: U256::from(100u64),
            xp: vec![U256::from(base), U256::from(base)],
            token_index_in: usize::from(reverse),
            token_index_out: usize::from(!reverse),
            n_coins: U256::from(2u64),
            fee: U256::from(fee),
            fee_denom: U256::from(10_000_000_000u64),
            precision,
            rate_multiplier_in: precision,
            rate_multiplier_out: precision,
            y_variant: YVariant::Standard,
            d_variant: DVariant::Standard,
        },
    }
}

/// The smallest native reserve whose scaled value clears the Solidly leaf's
/// faithful-arithmetic floor: four times the leaf floor, so the corpus sits
/// inside the faithful domain rather than on its boundary.
fn faithful_base(scale: u128) -> u128 {
    let scaled_floor = 4 * SOLIDLY_MIN_SCALED_RESERVE;
    (scaled_floor.saturating_mul(scale) / ONE_E18).max(2)
}

// ---------------------------------------------------------------------------
// Corpus.
// ---------------------------------------------------------------------------

/// One corpus entry: the path plus the tag criterion reports it under.
///
/// Tags stay short: criterion derives its on-disk directory from the first 64
/// characters of the full id, so two long tags sharing that prefix would
/// collide and silently overwrite each other's baseline.
struct Case {
    tag: String,
    path: ResolvedMixedPath,
}

/// Compact label for a corpus base, so the reported tag stays under criterion's
/// directory-name cap.
fn base_label(base: u128) -> &'static str {
    if base == ONE_E18 {
        "1e18"
    } else {
        "4e18"
    }
}

/// The Solidly corpus: two decimal scales x two fees x two imbalances x two
/// shapes = 16 paths.
fn solidly_corpus() -> Vec<Case> {
    let scales = [(1_000_000u128, "1e6"), (ONE_E18, "1e18")];
    let fees = [(3u64, 1_000u64, "3of1000"), (1, 1_000, "1of1000")];
    let mut out = Vec::new();
    for &(scale, slabel) in &scales {
        let base = faithful_base(scale);
        for &(sfn, sfd, flabel) in &fees {
            for &b in &IMBALANCES {
                let heavier = base.saturating_mul(b) / 100;
                out.push(Case {
                    tag: format!("solidly_first_s{slabel}_fee{flabel}_i{b}"),
                    path: path(vec![
                        solidly_hop(base, base, scale, scale, 0, sfn, sfd),
                        v2_hop(base, heavier, 997, 1_000),
                    ]),
                });
                out.push(Case {
                    tag: format!("solidly_last_s{slabel}_fee{flabel}_i{b}"),
                    path: path(vec![
                        v2_hop(base, heavier, 997, 1_000),
                        solidly_hop(base, base, scale, scale, 1, sfn, sfd),
                    ]),
                });
            }
        }
    }
    out
}

/// The Balancer weighted corpus: two bases x two fees x two imbalances x two
/// shapes = 16 paths.
fn weighted_corpus() -> Vec<Case> {
    let fees = [
        (3_000_000_000_000_000u128, "3e15"),
        (1_000_000_000_000_000, "1e15"),
    ];
    let mut out = Vec::new();
    for &base in &BASES {
        let blabel = base_label(base);
        for &(fee, flabel) in &fees {
            for &b in &IMBALANCES {
                let heavier = base.saturating_mul(b) / 100;
                out.push(Case {
                    tag: format!("weighted_first_b{blabel}_fee{flabel}_i{b}"),
                    path: path(vec![
                        weighted_hop(base, fee, false),
                        v2_hop(base, heavier, 997, 1_000),
                    ]),
                });
                out.push(Case {
                    tag: format!("weighted_last_b{blabel}_fee{flabel}_i{b}"),
                    path: path(vec![
                        v2_hop(base, heavier, 997, 1_000),
                        weighted_hop(base, fee, true),
                    ]),
                });
            }
        }
    }
    out
}

/// The Balancer stable corpus: two bases x two fees x two imbalances x two
/// shapes = 16 paths.
fn stable_corpus() -> Vec<Case> {
    let fees = [
        (10_000_000_000_000u128, "1e13"),
        (1_000_000_000_000_000, "1e15"),
    ];
    let mut out = Vec::new();
    for &base in &BASES {
        let blabel = base_label(base);
        for &(fee, flabel) in &fees {
            for &b in &IMBALANCES {
                let heavier = base.saturating_mul(b) / 100;
                out.push(Case {
                    tag: format!("stable_first_b{blabel}_fee{flabel}_i{b}"),
                    path: path(vec![
                        stable_hop(base, fee, false),
                        v2_hop(base, heavier, 997, 1_000),
                    ]),
                });
                out.push(Case {
                    tag: format!("stable_last_b{blabel}_fee{flabel}_i{b}"),
                    path: path(vec![
                        v2_hop(base, heavier, 997, 1_000),
                        stable_hop(base, fee, true),
                    ]),
                });
            }
        }
    }
    out
}

/// The Curve corpus: two bases x two fees x two imbalances x two shapes = 16
/// paths.
fn curve_corpus() -> Vec<Case> {
    let fees = [(4_000_000u128, "4e6"), (1_000_000, "1e6")];
    let mut out = Vec::new();
    for &base in &BASES {
        let blabel = base_label(base);
        for &(fee, flabel) in &fees {
            for &b in &IMBALANCES {
                let heavier = base.saturating_mul(b) / 100;
                out.push(Case {
                    tag: format!("curve_first_b{blabel}_fee{flabel}_i{b}"),
                    path: path(vec![
                        curve_hop(base, fee, false),
                        v2_hop(base, heavier, 997, 1_000),
                    ]),
                });
                out.push(Case {
                    tag: format!("curve_last_b{blabel}_fee{flabel}_i{b}"),
                    path: path(vec![
                        v2_hop(base, heavier, 997, 1_000),
                        curve_hop(base, fee, true),
                    ]),
                });
            }
        }
    }
    out
}

/// The all-V2 exact-Mobius reference: two bases x three imbalances = 6 pure
/// constant-product paths. Both hops quote the same in-to-out ratio, so the
/// round trip is profitable and the closed-form arm actually runs.
fn v2_reference_corpus() -> Vec<Case> {
    let mut out = Vec::new();
    for &base in &BASES {
        let blabel = base_label(base);
        for &b in &[101u128, 105, 110] {
            let heavier = base.saturating_mul(b) / 100;
            out.push(Case {
                tag: format!("v2_reference_b{blabel}_i{b}"),
                path: path(vec![
                    v2_hop(base, heavier, 997, 1_000),
                    v2_hop(base, heavier, 997, 1_000),
                ]),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Benches.
// ---------------------------------------------------------------------------

/// Drive one arm's corpus: one `bench_function` per path so criterion reports
/// per-path wall time. Each path is probed once so the corpus is pinned as
/// solvable — an arm that stopped solving would otherwise bench its gate-skip.
fn drive_group(c: &mut Criterion, name: &str, corpus: &[Case]) {
    let mut group = c.benchmark_group(name);
    group
        .warm_up_time(WARM_UP)
        .measurement_time(MEASUREMENT)
        .sample_size(SAMPLES);
    for case in corpus {
        let probe = solve_path(&case.path, &GateDeps::offline());
        assert!(
            probe.result.is_some(),
            "corpus path {} must solve to a result",
            case.tag
        );
        group.bench_function(&case.tag, |bencher| {
            bencher.iter(|| {
                let outcome = solve_path(black_box(&case.path), &GateDeps::offline());
                black_box(outcome);
            });
        });
    }
    group.finish();
}

fn bench_solidly(c: &mut Criterion) {
    drive_group(c, "stable_arms_solidly", &solidly_corpus());
}

fn bench_weighted(c: &mut Criterion) {
    drive_group(c, "stable_arms_balancer_weighted", &weighted_corpus());
}

fn bench_stable(c: &mut Criterion) {
    drive_group(c, "stable_arms_balancer_stable", &stable_corpus());
}

fn bench_curve(c: &mut Criterion) {
    drive_group(c, "stable_arms_curve", &curve_corpus());
}

fn bench_v2_reference(c: &mut Criterion) {
    drive_group(c, "stable_arms_v2_reference", &v2_reference_corpus());
}

criterion_group!(
    benches,
    bench_solidly,
    bench_weighted,
    bench_stable,
    bench_curve,
    bench_v2_reference,
);
criterion_main!(benches);

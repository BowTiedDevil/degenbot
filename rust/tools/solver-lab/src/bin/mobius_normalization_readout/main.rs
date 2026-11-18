// Dev-only interleaved A/B readout for shifted-piece arithmetic.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::too_many_lines,
    reason = "one interleaved measurement driver: capture/deep loops and their reports share one stdout contract"
)]

//! Compare exact I1024/BigInt shifted-piece composition with the experimental
//! normalized I512 composer over captured concentrated-liquidity paths.
//! Prepared hops are built once, the composition memo is disabled, and arm
//! order alternates on every repetition to cancel cache/thermal drift.
//!
//! `DR_NORMALIZATION_ITERS` controls repetitions per arm (default 9). Build
//! with `capture-fixtures,telemetry` to include the anchor build/compose/
//! argmax wall-time split; the default capture-only lane times whole solves.

use std::hint::black_box;
use std::sync::Arc;

use alloy::primitives::U256;
use degenbot_pools::solve::concentrated_liquidity::{
    solve_concentrated_liquidity_piecewise, solve_piecewise, MixedHopView,
    PreparedConcentratedLiquidityHop, WalkOutcome, WalkSolution, WalkTelemetry,
};
use degenbot_pools::solve::MobiusPieceArithmetic;
use degenbot_pools::state::int_v3_hop::IntV3TickRangeSequence;
use degenbot_pools::SolveRuntimeConfig;
use degenbot_solver_lab::capture_rows::capture_arg;
use degenbot_solver_lab::concentrated_liquidity_rows::parse_concentrated_liquidity_hops;
use serde_json::Value;

#[derive(Default)]
struct ArmTimes {
    total_ns: u128,
    path_medians_ns: Vec<u128>,
}

fn median(values: &mut [u128]) -> u128 {
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or(0)
}

fn p95(values: &mut [u128]) -> u128 {
    values.sort_unstable();
    values
        .get((values.len() * 94 / 100).min(values.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0)
}

fn ratio_bp(numerator: u128, denominator: u128) -> u128 {
    if denominator == 0 {
        return 0;
    }
    numerator.saturating_mul(10_000) / denominator
}

fn print_ratio(label: &str, numerator: u128, denominator: u128) {
    let bp = ratio_bp(numerator, denominator);
    println!("{label}: {}.{:04}x", bp / 10_000, bp % 10_000);
}

#[cfg(feature = "telemetry")]
fn telemetry(tele: &WalkTelemetry) -> Option<&WalkTelemetry> {
    Some(tele)
}

#[cfg(not(feature = "telemetry"))]
fn telemetry(_tele: &WalkTelemetry) -> Option<&WalkTelemetry> {
    None
}

fn solve(
    prepared: &[PreparedConcentratedLiquidityHop],
    cfg: &SolveRuntimeConfig,
    tele: &WalkTelemetry,
) -> degenbot_pools::solve::concentrated_liquidity::WalkOutcome {
    solve_concentrated_liquidity_piecewise(prepared, None, telemetry(tele), cfg, None)
}

fn solve_deep(
    views: &[MixedHopView<'_>],
    cfg: &SolveRuntimeConfig,
    tele: &WalkTelemetry,
) -> WalkOutcome {
    solve_piecewise(views, None, telemetry(tele), cfg, None)
}

fn profit(solution: &Option<WalkSolution>) -> Option<U256> {
    solution.as_ref().map(|s| s.profit)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = capture_arg(&args, "heavy_concentrated_liquidity_solve_captures.jsonl");
    let iters: usize = std::env::var("DR_NORMALIZATION_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9)
        .max(1);
    let content = degenbot_pools::solve::fixtures::read_fixture(&path);
    let exact_cfg = SolveRuntimeConfig::default();
    let normalized_cfg = SolveRuntimeConfig {
        mobius_piece_arithmetic: MobiusPieceArithmetic::NormalizedI512,
        ..exact_cfg
    };
    let exact_tele = WalkTelemetry::new();
    let normalized_tele = WalkTelemetry::new();
    let mut exact = ArmTimes::default();
    let mut normalized = ArmTimes::default();
    let mut n_paths = 0u64;
    let mut full_divergences = 0u64;
    let mut profit_divergences = 0u64;
    let mut input_divergences = 0u64;
    let mut presence_divergences = 0u64;

    for (line_index, line) in content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
    {
        let doc: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("line {}: bad capture: {error}", line_index + 1);
                continue;
            }
        };
        let pid = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let Some(hops) = doc.get("hops").and_then(Value::as_array) else {
            continue;
        };
        let seqs = match parse_concentrated_liquidity_hops(hops) {
            Ok(seqs) => seqs,
            Err(error) => {
                eprintln!("path {pid}: skip ({error})");
                continue;
            }
        };
        if seqs.len() < 2 {
            continue;
        }
        n_paths += 1;
        let prepared: Vec<PreparedConcentratedLiquidityHop> = seqs
            .iter()
            .map(|seq| {
                PreparedConcentratedLiquidityHop::derive(Arc::new(IntV3TickRangeSequence::clone(
                    seq,
                )))
            })
            .collect();

        // Warm both arms and compare once. Every timed result is also black-boxed;
        // deterministic solver behavior is covered by the owning crate's tests.
        let expected = solve(&prepared, &exact_cfg, &exact_tele).result;
        let actual = solve(&prepared, &normalized_cfg, &normalized_tele).result;
        if expected != actual {
            full_divergences += 1;
            if profit(&expected) != profit(&actual) {
                profit_divergences += 1;
            }
            match (&expected, &actual) {
                (Some(want), Some(got)) if want.optimal_input != got.optimal_input => {
                    input_divergences += 1;
                }
                (Some(_), None) | (None, Some(_)) => presence_divergences += 1,
                _ => {}
            }
            eprintln!("path {pid}: DIVERGENCE exact={expected:?} normalized={actual:?}");
        }

        let mut exact_path = Vec::with_capacity(iters);
        let mut normalized_path = Vec::with_capacity(iters);
        for rep in 0..iters {
            let exact_first = rep % 2 == 0;
            for exact_arm in [exact_first, !exact_first] {
                let start = std::time::Instant::now();
                if exact_arm {
                    black_box(solve(&prepared, &exact_cfg, &exact_tele).result);
                } else {
                    black_box(solve(&prepared, &normalized_cfg, &normalized_tele).result);
                }
                let elapsed = start.elapsed().as_nanos();
                if exact_arm {
                    exact.total_ns += elapsed;
                    exact_path.push(elapsed);
                } else {
                    normalized.total_ns += elapsed;
                    normalized_path.push(elapsed);
                }
            }
        }
        exact.path_medians_ns.push(median(&mut exact_path));
        normalized
            .path_medians_ns
            .push(median(&mut normalized_path));
    }

    let exact_ns = exact_tele.drain_ns();
    let normalized_ns = normalized_tele.drain_ns();
    println!();
    println!("== Normalized I512 Möbius A/B: {n_paths} paths, {iters} interleaved reps/arm ==");
    println!("divergent paths: full={full_divergences} profit={profit_divergences} input={input_divergences} presence={presence_divergences}");
    println!();
    println!("                         total_us   path_p50_us   path_p95_us");
    println!(
        "exact I1024/BigInt       {:>12} {:>13} {:>13}",
        exact.total_ns / 1_000,
        median(&mut exact.path_medians_ns) / 1_000,
        p95(&mut exact.path_medians_ns) / 1_000
    );
    println!(
        "normalized I512          {:>12} {:>13} {:>13}",
        normalized.total_ns / 1_000,
        median(&mut normalized.path_medians_ns) / 1_000,
        p95(&mut normalized.path_medians_ns) / 1_000
    );
    print_ratio(
        "normalized/exact whole solve",
        normalized.total_ns,
        exact.total_ns,
    );
    println!();
    println!("anchor wall time (includes one warm solve per arm/path)");
    println!("                         exact_us   normalized_us");
    println!(
        "build                    {:>10} {:>15}",
        exact_ns.anchor_build / 1_000,
        normalized_ns.anchor_build / 1_000
    );
    println!(
        "compose                  {:>10} {:>15}",
        exact_ns.anchor_compose / 1_000,
        normalized_ns.anchor_compose / 1_000
    );
    println!(
        "argmax                   {:>10} {:>15}",
        exact_ns.anchor_argmax / 1_000,
        normalized_ns.anchor_argmax / 1_000
    );
    print_ratio(
        "normalized/exact compose",
        u128::from(normalized_ns.anchor_compose),
        u128::from(exact_ns.anchor_compose),
    );
    print_ratio(
        "normalized/exact argmax",
        u128::from(normalized_ns.anchor_argmax),
        u128::from(exact_ns.anchor_argmax),
    );

    // Force the exact arm's arbitrary-precision fallback. Six full-U256 V2
    // hops grow past I1024 during composition; normalized storage caps every
    // stage at I512. This is a width-regime microbench, not a realistic pool.
    let deep_hops: Vec<degenbot_math::v2::IntHopState> = (0..6)
        .map(|i| {
            degenbot_math::v2::IntHopState::new(
                U256::MAX - U256::from(i),
                U256::MAX - U256::from(i + 1),
                997,
                1000,
            )
        })
        .collect();
    let deep_views: Vec<MixedHopView<'_>> = deep_hops.iter().map(MixedHopView::V2).collect();
    let deep_expected = solve_deep(&deep_views, &exact_cfg, &exact_tele).result;
    let deep_actual = solve_deep(&deep_views, &normalized_cfg, &normalized_tele).result;
    let deep_iters: usize = std::env::var("DR_DEEP_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200)
        .max(1);
    let mut deep_exact_ns = 0u128;
    let mut deep_normalized_ns = 0u128;
    for rep in 0..deep_iters {
        let exact_first = rep % 2 == 0;
        for exact_arm in [exact_first, !exact_first] {
            let start = std::time::Instant::now();
            if exact_arm {
                black_box(solve_deep(&deep_views, &exact_cfg, &exact_tele).result);
            } else {
                black_box(solve_deep(&deep_views, &normalized_cfg, &normalized_tele).result);
            }
            let elapsed = start.elapsed().as_nanos();
            if exact_arm {
                deep_exact_ns += elapsed;
            } else {
                deep_normalized_ns += elapsed;
            }
        }
    }
    println!();
    println!(
        "== Deep full-U256 6-hop microbench: {deep_iters} interleaved reps/arm; result_equal={} ==",
        deep_expected == deep_actual
    );
    println!(
        "exact I1024 -> BigInt total_us: {:>12}",
        deep_exact_ns / 1_000
    );
    println!(
        "normalized I512 total_us:       {:>12}",
        deep_normalized_ns / 1_000
    );
    print_ratio(
        "normalized/exact deep solve",
        deep_normalized_ns,
        deep_exact_ns,
    );
    let deep_exact_telemetry = exact_tele.drain_ns();
    let deep_normalized_telemetry = normalized_tele.drain_ns();
    println!(
        "deep compose us: exact={} normalized={}",
        deep_exact_telemetry.anchor_compose / 1_000,
        deep_normalized_telemetry.anchor_compose / 1_000
    );
    println!(
        "deep argmax us:  exact={} normalized={}",
        deep_exact_telemetry.anchor_argmax / 1_000,
        deep_normalized_telemetry.anchor_argmax / 1_000
    );
    print_ratio(
        "normalized/exact deep compose",
        u128::from(deep_normalized_telemetry.anchor_compose),
        u128::from(deep_exact_telemetry.anchor_compose),
    );
}

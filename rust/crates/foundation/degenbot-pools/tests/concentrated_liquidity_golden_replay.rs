//! Owning integration tests for the concentrated-liquidity two-sided golden
//! replay correctness contract.
//!
//! This file is the one authority for the two-sided golden gate over the
//! committed capture corpora: profit under-shoot bounded by [`PROFIT_EPS`],
//! over-shoot beyond [`OVER_SHOOT_TOLERANCE_WEI`] a hard failure,
//! determinism across repetitions, and null-golden captures reconciled out
//! without failing the gate. The rules live in exactly one implementation,
//! `degenbot_pools::solve::golden_gate`; the solver-lab replay reads the same
//! evaluator for its measurement/readout pass and its over-shoot-only golden
//! regeneration, so no consumer carries its own copy of the tolerances or the
//! comparison.
//!
//! Both committed corpora are replayed through the production solver entry
//! point: the single-block anchor and every row of the heavy corpus, in every
//! lane that enables `capture-fixtures`.

// The replay driver is fatal-by-design like the fixture reader it stands on: a
// capture row missing its `hops` array (or an unparsable one) is corpus
// corruption, not a skip.
#![expect(clippy::expect_used, clippy::panic)]

pub mod common;

use alloy::primitives::U256;
use common::concentrated_liquidity_capture::parse_capture_hops;
use degenbot_pools::solve::concentrated_liquidity::derive_and_solve_concentrated_liquidity_piecewise;
use degenbot_pools::solve::fixtures::{fixture_path, read_fixture, rows};
use degenbot_pools::solve::golden_gate::{
    classify, parse_golden, replay_profitable, results_consistent, GateVerdict, GoldenOutcome,
    ReplaySolution, OVER_SHOOT_TOLERANCE_WEI, PROFIT_EPS,
};
use degenbot_pools::solve::runtime::SolveRuntimeConfig;
use degenbot_pools::state::int_v3_hop::IntV3TickRangeSequence;
use serde_json::Value;

/// Independent repetitions per captured path: the determinism leg of the
/// gate (pure integer math must agree run over run).
const REPS: usize = 3;

/// The single-block anchor corpus: 12 rows, 2 with a recorded golden. The
/// row counts are asserted so a fixture home silently resolved to nothing
/// can never read as a green gate.
const ANCHOR_CORPUS: &str = "concentrated_liquidity_capture_offline.jsonl";
/// The heavy corpus: 156 rows, 26 with a recorded golden (the rest are
/// null-golden gate-skipped captures, reconciled out without failing).
const HEAVY_CORPUS: &str = "heavy_concentrated_liquidity_solve_captures.jsonl";

/// Replay every row of a committed corpus through the production solver and
/// hold each row to the two-sided golden gate: deterministic across reps,
/// no under-shoot past [`PROFIT_EPS`], no over-shoot past
/// [`OVER_SHOOT_TOLERANCE_WEI`], null goldens reconciled out non-failingly.
fn replay_golden_gate(
    file_name: &str,
    expected_rows: usize,
    expected_goldens: usize,
    expected_gate_skipped: usize,
) {
    let content = read_fixture(fixture_path(file_name));
    let mut rows_checked = 0usize;
    let mut goldens_checked = 0usize;
    let mut gate_skipped = 0usize;
    for doc in rows(&content) {
        let pid: u64 = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let hops = doc
            .get("hops")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("pid {pid}: capture row without a hops array"));
        let seqs: Vec<IntV3TickRangeSequence> = parse_capture_hops(hops)
            .unwrap_or_else(|| panic!("pid {pid}: capture hop reconstruction failed"));
        let refs: Vec<&IntV3TickRangeSequence> = seqs.iter().collect();
        let mut runs: Vec<Option<ReplaySolution>> = Vec::with_capacity(REPS);
        for _ in 0..REPS {
            let outcome = derive_and_solve_concentrated_liquidity_piecewise(
                &refs,
                &SolveRuntimeConfig::default(),
            );
            runs.push(outcome.result.map(|r| ReplaySolution {
                optimal_input: r.optimal_input,
                hop_outputs: r.hop_outputs,
            }));
        }
        assert!(
            results_consistent(&runs),
            "pid {pid}: nondeterministic across {REPS} repetitions"
        );
        let golden = parse_golden(doc.get("golden"));
        match golden.as_ref() {
            // Null-golden reconciliation, non-failing by contract.
            None => gate_skipped += 1,
            Some(g) => {
                goldens_checked += 1;
                let verdict = classify(Some(g), runs[0].as_ref());
                assert!(
                    !verdict.is_gate_failure(),
                    "pid {pid}: two-sided golden gate failed: {verdict:?}"
                );
            }
        }
        rows_checked += 1;
    }
    assert_eq!(
        rows_checked, expected_rows,
        "{file_name}: replayed row count diverged from the committed corpus"
    );
    assert_eq!(
        goldens_checked, expected_goldens,
        "{file_name}: golden-bearing row count diverged from the committed corpus"
    );
    assert_eq!(
        gate_skipped, expected_gate_skipped,
        "{file_name}: null-golden reconciliation count diverged from the committed corpus"
    );
}

#[test]
fn two_sided_golden_gate_single_block_anchor() {
    replay_golden_gate(ANCHOR_CORPUS, 12, 2, 10);
}

#[test]
fn two_sided_golden_gate_heavy_corpus() {
    replay_golden_gate(HEAVY_CORPUS, 156, 26, 130);
}

/// A replay solution reporting `profit` net over `optimal_input`.
fn solution_with_profit(optimal_input: u64, profit: u128) -> ReplaySolution {
    ReplaySolution {
        optimal_input: U256::from(optimal_input),
        hop_outputs: vec![U256::ZERO, U256::from(optimal_input) + U256::from(profit)],
    }
}

#[test]
fn golden_gate_rejects_excessive_under_shoot() {
    let golden = GoldenOutcome {
        optimal_input: U256::from(1_000u64),
        profit: U256::from(1_000_000u64),
    };
    // Under-shooting by exactly the epsilon is the tolerance boundary: OK.
    let boundary = solution_with_profit(1_000, 1_000_000 - PROFIT_EPS);
    assert!(matches!(
        classify(Some(&golden), Some(&boundary)),
        GateVerdict::Within { .. }
    ));
    // One wei past the epsilon is a hard gate failure.
    let beyond = solution_with_profit(1_000, 1_000_000 - PROFIT_EPS - 1);
    let verdict = classify(Some(&golden), Some(&beyond));
    assert!(
        matches!(verdict, GateVerdict::UnderShoot { .. }),
        "{verdict:?}"
    );
    assert!(verdict.is_gate_failure());
}

#[test]
fn golden_gate_rejects_excessive_over_shoot() {
    let golden = GoldenOutcome {
        optimal_input: U256::from(1_000u64),
        profit: U256::from(1_000_000u64),
    };
    // Over-shooting by exactly the few-wei tolerance is the boundary: OK.
    let boundary = solution_with_profit(1_000, 1_000_000 + OVER_SHOOT_TOLERANCE_WEI);
    assert!(matches!(
        classify(Some(&golden), Some(&boundary)),
        GateVerdict::Within { .. }
    ));
    // One wei past the tolerance is a hard gate failure (phantom profit or a
    // stale golden), and the only regeneration-eligible shape.
    let beyond = solution_with_profit(1_000, 1_000_000 + OVER_SHOOT_TOLERANCE_WEI + 1);
    let verdict = classify(Some(&golden), Some(&beyond));
    assert!(
        matches!(verdict, GateVerdict::OverShoot { .. }),
        "{verdict:?}"
    );
    assert!(verdict.is_gate_failure());
}

#[test]
fn golden_gate_rejects_nondeterministic_repetitions() {
    let a = solution_with_profit(1_000, 5_000);
    let b = solution_with_profit(2_000, 5_000);
    assert!(!results_consistent(&[Some(a.clone()), Some(b)]));
    // A rep that yields nothing while another solves is disagreement too.
    assert!(!results_consistent(&[Some(a.clone()), None]));
    assert!(!results_consistent(&[None, Some(a.clone())]));
    // Identical repeats (and a single rep) are consistent.
    assert!(results_consistent(&[Some(a.clone()), Some(a.clone())]));
    assert!(results_consistent(&[Some(a)]));
}

#[test]
fn golden_gate_reconciles_null_golden_without_failure() {
    // A null (gate-skipped) capture never fails the gate, whatever the
    // ungated replay found; the reconciliation volume is telemetry.
    let profitable = solution_with_profit(1_234, 99_000_000);
    assert_eq!(classify(None, Some(&profitable)), GateVerdict::GateSkipped);
    assert!(!classify(None, Some(&profitable)).is_gate_failure());
    assert!(replay_profitable(&profitable));
    let nothing = ReplaySolution {
        optimal_input: U256::ZERO,
        hop_outputs: vec![],
    };
    assert!(!replay_profitable(&nothing));
    // Absent and JSON-null goldens both reconcile to the skipped verdict.
    assert!(parse_golden(None).is_none());
    assert!(parse_golden(Some(&Value::Null)).is_none());
}

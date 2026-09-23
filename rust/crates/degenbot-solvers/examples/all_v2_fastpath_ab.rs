#![expect(clippy::print_stdout, clippy::cast_precision_loss)]

//! All-V2 fast-path A/B: `exact_mobius_solve` (the closed-form + ±2 sweep the
//! all-V2 dispatch arm uses) against the unified walk's all-V2 projection
//! (every hop a constant-product `PieceView`, driven through
//! `solve_mixed_piecewise`).
//!
//! Corpus: real oriented V2 states mined from the mixed capture fixture
//! (`heavy_mixed_solve_captures.jsonl`) paired into 2-hop all-V2 paths, plus
//! synthetic 2-hop and 3-hop round-trip grids over fee ∈ {30bp, 25bp, 5bp},
//! reserve ratios {1x, 10x, 100x}, and magnitudes {1e18, 1e24}.
//!
//! For every path the probe records byte-parity on the returned
//! `(optimal_input, profit, hop_outputs)` and the derived `consumed_inputs`
//! under the dispatch's profitability rule, and times both solvers over 200
//! repetitions per path. The verdict deletes the fast path only when the walk
//! is byte-identical everywhere AND not slower beyond measurement noise.

use std::path::PathBuf;
use std::time::Instant;

use alloy::primitives::U256;
use degenbot_math::v2::IntHopState;
use degenbot_solvers::cl::{solve_mixed_piecewise, ClSolveTables, IntV3TickRangeSequence};
use degenbot_solvers::mobius_int_exact::exact_mobius_solve;
use degenbot_solvers::runtime::SolveRuntimeConfig;
use serde_json::Value;

/// Number of timed repetitions per path.
const REPS: usize = 200;

/// `(optimal_input, profit, hop_outputs)` under the dispatch's profitability
/// rule (profitable && non-zero input && non-zero profit).
type Solve = Option<(U256, U256, Vec<U256>)>;

fn baseline(hops: &[IntHopState]) -> Solve {
    let r = exact_mobius_solve(hops).ok()?;
    if !r.is_profitable || r.optimal_input.is_zero() || r.profit.is_zero() {
        return None;
    }
    Some((r.optimal_input, r.profit, r.hop_outputs))
}

fn walk(hops: &[IntHopState]) -> Solve {
    let v2: Vec<Option<IntHopState>> = hops.iter().cloned().map(Some).collect();
    let seqs: Vec<Option<&IntV3TickRangeSequence>> = (0..hops.len()).map(|_| None).collect();
    let prepared: Vec<Option<ClSolveTables>> = (0..hops.len()).map(|_| None).collect();
    let order: Vec<bool> = (0..hops.len()).map(|_| true).collect();
    solve_mixed_piecewise(
        &v2,
        &seqs,
        &prepared,
        &order,
        &SolveRuntimeConfig::default(),
        None,
    )
    .result
}

/// Derived `consumed_inputs`: `[optimal_input, hop_outputs[..n-1]]`.
fn consumed(opt: &U256, outs: &[U256]) -> Vec<U256> {
    let mut v = Vec::with_capacity(outs.len());
    v.push(*opt);
    v.extend(outs.iter().take(outs.len().saturating_sub(1)).copied());
    v
}

#[derive(Default)]
struct Stats {
    paths: u64,
    both_none: u64,
    both_some: u64,
    byte_equal: u64,
    none_mismatch: u64,
    field_mismatch: u64,
    opt_diff: u64,
    profit_diff: u64,
    outputs_diff: u64,
    consumed_diff: u64,
}

impl Stats {
    fn verdict(&self) -> &'static str {
        if self.byte_equal == self.paths {
            "BYTE-IDENTICAL"
        } else {
            "DIVERGENT"
        }
    }

    fn json(&self) -> Value {
        serde_json::json!({
            "paths": self.paths,
            "both_none": self.both_none,
            "both_some": self.both_some,
            "byte_equal": self.byte_equal,
            "byte_equal_pct": if self.paths > 0 { 100.0 * self.byte_equal as f64 / self.paths as f64 } else { 0.0 },
            "none_mismatch": self.none_mismatch,
            "field_mismatch": self.field_mismatch,
            "opt_diff": self.opt_diff,
            "profit_diff": self.profit_diff,
            "outputs_diff": self.outputs_diff,
            "consumed_diff": self.consumed_diff,
            "verdict": self.verdict(),
        })
    }
}

fn compare(base: &Solve, cand: &Solve, s: &mut Stats, examples: &mut Vec<Value>, label: &str) {
    s.paths += 1;
    match (base, cand) {
        (None, None) => {
            s.both_none += 1;
            s.byte_equal += 1;
        }
        (Some(b), Some(c)) => {
            s.both_some += 1;
            let bc = consumed(&b.0, &b.2);
            let cc = consumed(&c.0, &c.2);
            if b.0 == c.0 && b.1 == c.1 && b.2 == c.2 && bc == cc {
                s.byte_equal += 1;
            } else {
                s.field_mismatch += 1;
                if b.0 != c.0 {
                    s.opt_diff += 1;
                }
                if b.1 != c.1 {
                    s.profit_diff += 1;
                }
                if b.2 != c.2 {
                    s.outputs_diff += 1;
                }
                if bc != cc {
                    s.consumed_diff += 1;
                }
                if examples.len() < 12 {
                    examples.push(serde_json::json!({
                        "path": label,
                        "base_opt": b.0.to_string(),
                        "walk_opt": c.0.to_string(),
                        "base_profit": b.1.to_string(),
                        "walk_profit": c.1.to_string(),
                        "base_outputs": b.2.iter().map(ToString::to_string).collect::<Vec<_>>(),
                        "walk_outputs": c.2.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    }));
                }
            }
        }
        (Some(_), None) | (None, Some(_)) => {
            s.none_mismatch += 1;
            if examples.len() < 12 {
                examples.push(serde_json::json!({
                    "path": label,
                    "base_some": base.is_some(),
                    "walk_some": cand.is_some(),
                }));
            }
        }
    }
}

/// Byte-parity stats over the corpus subset whose label matches `pred`.
fn parity_for(
    corpus: &[(String, Vec<IntHopState>)],
    pred: impl Fn(&str) -> bool,
    examples: &mut Vec<Value>,
) -> Stats {
    let mut s = Stats::default();
    for (label, hops) in corpus {
        if pred(label) {
            let b = baseline(hops);
            let w = walk(hops);
            compare(&b, &w, &mut s, examples, label);
        }
    }
    s
}

fn pow10(e: u32) -> U256 {
    let mut p = U256::from(1u64);
    for _ in 0..e {
        p *= U256::from(10u64);
    }
    p
}

/// Real oriented V2 states mined from the mixed capture fixture. Token
/// identity is irrelevant to the solver composition, so any pair composes.
fn real_states() -> Vec<IntHopState> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest.join("tests/fixtures/heavy_mixed_solve_captures.jsonl");
    let content = degenbot_solvers::capture_fixture::read_fixture(&path);
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for line in content.lines() {
        let Ok(doc) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(hops) = doc.get("hops").and_then(Value::as_array) else {
            continue;
        };
        for hop in hops {
            if hop.get("kind").and_then(Value::as_str) != Some("V2") {
                continue;
            }
            let ri = hop
                .get("reserve_in")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<U256>().ok());
            let ro = hop
                .get("reserve_out")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<U256>().ok());
            let g = hop
                .get("gamma_numer")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<u64>().ok());
            let d = hop
                .get("fee_denom")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<u64>().ok());
            if let (Some(ri), Some(ro), Some(g), Some(d)) = (ri, ro, g, d) {
                let key = format!("{ri}|{ro}|{g}|{d}");
                if seen.insert(key) {
                    out.push(IntHopState::new(ri, ro, g, d));
                    if out.len() >= 10 {
                        return out;
                    }
                }
            }
        }
    }
    out
}

/// 2-hop and 3-hop grids. hop0 pays `ratio` out per in, the closing hop
/// returns `base` per `base * ratio`, so the composed rate is the product of
/// the chosen ratios divided by the last — spanning profitable and
/// unprofitable paths.
fn synthetic_corpus() -> Vec<(String, Vec<IntHopState>)> {
    let fees = [
        (997u64, 1000u64, "30bp"),
        (9975, 10000, "25bp"),
        (9995, 10000, "5bp"),
    ];
    let ratios = [1u64, 10, 100];
    let mags = [18u32, 24];
    let mut out = Vec::new();
    for (g, d, fl) in fees {
        for m in mags {
            let base = pow10(m);
            for a in ratios {
                for b in ratios {
                    let hop0 = IntHopState::new(base, base * U256::from(a), g, d);
                    let hop1 = IntHopState::new(base * U256::from(b), base, g, d);
                    out.push((format!("2hop {fl} 1e{m} a{a} b{b}"), vec![hop0, hop1]));
                }
            }
            for a in ratios {
                for b in ratios {
                    for c in ratios {
                        let hop0 = IntHopState::new(base, base * U256::from(a), g, d);
                        let hop1 = IntHopState::new(base, base * U256::from(b), g, d);
                        let hop2 = IntHopState::new(base * U256::from(c), base, g, d);
                        out.push((
                            format!("3hop {fl} 1e{m} a{a} b{b} c{c}"),
                            vec![hop0, hop1, hop2],
                        ));
                    }
                }
            }
        }
    }
    out
}

fn median_us(mut ns: Vec<u128>) -> f64 {
    ns.sort_unstable();
    if ns.is_empty() {
        return 0.0;
    }
    ns[ns.len() / 2] as f64 / 1000.0
}

fn main() {
    let mut corpus: Vec<(String, Vec<IntHopState>)> = Vec::new();

    // Captured-derived all-V2 paths from real oriented V2 states.
    let real = real_states();
    for i in 0..real.len() {
        for j in 0..real.len() {
            if i != j {
                corpus.push((
                    format!("captured-derived {i}->{j}"),
                    vec![real[i].clone(), real[j].clone()],
                ));
            }
        }
    }
    let real_paths = corpus.len();
    corpus.extend(synthetic_corpus());

    // Byte-parity pass.
    let mut stats = Stats::default();
    let mut examples = Vec::new();
    for (label, hops) in &corpus {
        let b = baseline(hops);
        let w = walk(hops);
        compare(&b, &w, &mut stats, &mut examples, label);
    }

    // Timing pass: REPS per path, per-call ns.
    let mut base_ns: Vec<u128> = Vec::with_capacity(corpus.len());
    let mut walk_ns: Vec<u128> = Vec::with_capacity(corpus.len());
    for (_label, hops) in &corpus {
        let mut bt = Vec::with_capacity(REPS);
        let mut wt = Vec::with_capacity(REPS);
        for _ in 0..REPS {
            let t0 = Instant::now();
            let _ = baseline(hops);
            bt.push(t0.elapsed().as_nanos());
            let t1 = Instant::now();
            let _ = walk(hops);
            wt.push(t1.elapsed().as_nanos());
        }
        base_ns.push(median_ns(&mut bt));
        walk_ns.push(median_ns(&mut wt));
    }

    let base_med = median_us(base_ns.clone());
    let walk_med = median_us(walk_ns.clone());
    let ratio = if base_med > 0.0 {
        walk_med / base_med
    } else {
        f64::INFINITY
    };

    println!(
        "SUMMARY {}",
        serde_json::json!({
            "corpus": {
                "total": corpus.len(),
                "captured_derived": real_paths,
                "real_states": real.len(),
                "synthetic": corpus.len() - real_paths,
            },
            "parity": stats.json(),
            "parity_by_kind": {
                "2hop": parity_for(&corpus, |l| l.starts_with("2hop"), &mut Vec::new()).json(),
                "3hop": parity_for(&corpus, |l| l.starts_with("3hop"), &mut Vec::new()).json(),
                "captured_derived": parity_for(&corpus, |l| l.starts_with("captured"), &mut Vec::new()).json(),
            },
            "divergence_examples": examples,
            "timing": {
                "reps_per_path": REPS,
                "baseline_median_us": base_med,
                "walk_median_us": walk_med,
                "walk_over_baseline": ratio,
                "baseline_p95_us": percentile_us(base_ns.clone(), 95),
                "walk_p95_us": percentile_us(walk_ns.clone(), 95),
            },
            "gate": {
                "parity_ok": stats.byte_equal == stats.paths,
                "walk_within_5pct": ratio <= 1.05,
                "delete_all_v2_arm": stats.byte_equal == stats.paths && ratio <= 1.05,
            },
        })
    );
}

fn median_ns(v: &mut [u128]) -> u128 {
    v.sort_unstable();
    v[v.len() / 2]
}

fn percentile_us(mut ns: Vec<u128>, pct: usize) -> f64 {
    ns.sort_unstable();
    if ns.is_empty() {
        return 0.0;
    }
    ns[(ns.len() * pct) / 100] as f64 / 1000.0
}

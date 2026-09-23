//! Swap-step byte-parity probe: a V2 constant-product hop projected as a
//! degenerate single-range CL tick sequence.
//!
//! Reads the real V2 hop states available in the solver capture corpus
//! (`heavy_mixed_solve_captures.jsonl` plus the committed single-path pool
//! fixtures whose V2 hop carries a recorded input), sweeps a synthetic grid,
//! and for every `(state, input)` compares
//!
//!   * baseline: `IntHopState::swap(x)` — the V2 one-DIV rational floor, and
//!   * candidate: the same state projected to one CL range (`L =
//!     floor(sqrt(R_in * R_out))`, `sqrt_price_x96 = floor(L * 2^96 / R_in)`,
//!     empty interior word boundaries, fee mapped onto the identical
//!     `gamma/fee_denom` fraction) then run through the production
//!     `simulate_v3_range_swap`.
//!
//! Both directions of the projection are measured for every state, because the
//! CL rounding path is direction-specific (zfo floors `amount1`, ofz floors
//! `amount0`). The probe never special-cases the production step math.
//!
//! Output: one `DIVERGENCE {...}` JSON line per non-zero step delta, followed
//! by a single `SUMMARY {...}` JSON line with the corpus inventory, divergence
//! distribution, per-direction / per-fee / per-ratio breakdown, and the
//! zero-tolerance verdict.
//!
//! The single-range span is deliberately chosen far wider than every grid input
//! so the CL step stays in the open-AMM (target-unreachable) branch — the only
//! branch with a V2 analogue. A range-boundary hit is therefore a projection
//! artifact, not a V2 behaviour; the probe reports it separately and treats it
//! as a divergence because the consumed input then differs from V2's full-`x`
//! consumption.

#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use alloy::primitives::{U256, U512};
use degenbot_math::v2::IntHopState;
use degenbot_pools::int_v3_hop::IntV3TickRangeHop;
use degenbot_solvers::cl::simulate_v3_range_swap;
use num_bigint::BigUint;
use serde_json::{json, Value};

/// Log2 span of the degenerate range on each side of the spot price. A swap
/// must be able to push the price `2^SPAN_SHIFT` in the swap direction before
/// the artificial range edge is reached; every grid input is orders of
/// magnitude below that, so the step is the open-AMM partial landing.
const SPAN_SHIFT: u32 = 32;

/// Log-spaced input grid size.
const GRID_POINTS: usize = 8;

/// One V2 constant-product hop state, direction-agnostic (input reserve,
/// output reserve, fee = `gamma_numer / fee_denom`).
#[derive(Clone, Copy, Debug)]
struct V2State {
    reserve_in: U256,
    reserve_out: U256,
    gamma_numer: u64,
    fee_denom: u64,
}

impl V2State {
    fn key(self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.reserve_in, self.reserve_out, self.gamma_numer, self.fee_denom
        )
    }

    fn fee_label(self) -> String {
        format!("{}/{}", self.gamma_numer, self.fee_denom)
    }

    fn hop(self) -> IntHopState {
        IntHopState::new(
            self.reserve_in,
            self.reserve_out,
            self.gamma_numer,
            self.fee_denom,
        )
    }
}

/// Project a V2 hop to one degenerate CL range. `zero_for_one` chooses which
/// reserve is projected onto token0; the other projection is the reverse.
///
/// Returns `None` when the state cannot be represented (zero reserve, zero
/// geometric-mean liquidity, or a fee fraction not expressible in the CL
/// millionths-pip grid).
fn project(state: V2State, zero_for_one: bool) -> Option<IntV3TickRangeHop> {
    let (r_in, r_out) = if zero_for_one {
        (state.reserve_in, state.reserve_out)
    } else {
        (state.reserve_out, state.reserve_in)
    };
    if r_in.is_zero() || r_out.is_zero() {
        return None;
    }

    // L = floor(sqrt(R_in * R_out)). The exact geometric mean is irrational for
    // essentially every real pool, so this floor is a lossy projection by
    // construction — the primary expected parity break.
    let product = U512::from(r_in) * U512::from(r_out);
    let product_big = BigUint::from_bytes_be(&product.to_be_bytes::<64>());
    let l_big = product_big.sqrt();
    let liquidity: u128 = l_big.to_str_radix(10).parse().ok()?;
    if liquidity == 0 {
        return None;
    }

    // sqrt_price_x96 = floor(L * 2^96 / R_in), placing the projected spot at
    // the reserve ratio. Also lossy: L * 2^96 / R_in is generally not integral.
    let sqrt_price_x96: U256 = ((U512::from(liquidity) << 96u32) / U512::from(r_in)).to::<U256>();
    if sqrt_price_x96.is_zero() {
        return None;
    }

    // Map the V2 fraction gamma/fee_denom onto CL millionths-of-a-pip. Exact for
    // the standard 0.3% (997/1000), 0.25% (9975/10000), 0.05% (9995/10000), and
    // 1% (99/100) conventions; a non-exact fee fraction is out of scope.
    let fee_pips = 1_000_000u128 * u128::from(state.fee_denom - state.gamma_numer)
        / u128::from(state.fee_denom);
    if fee_pips >= 1_000_000 {
        return None;
    }
    let cl_gamma = (1_000_000 - fee_pips) as u64;

    // One range spanning [sqrtP >> SHIFT, sqrtP] (zfo) or
    // [sqrtP, sqrtP << SHIFT] (ofz).
    let (lower, upper) = if zero_for_one {
        (
            (sqrt_price_x96 >> SPAN_SHIFT).max(U256::from(1u8)),
            sqrt_price_x96,
        )
    } else {
        (sqrt_price_x96, sqrt_price_x96 << SPAN_SHIFT)
    };

    Some(IntV3TickRangeHop {
        liquidity,
        sqrt_price_x96,
        sqrt_price_lower_x96: lower,
        sqrt_price_upper_x96: upper,
        gamma_numer: cl_gamma,
        fee_denom: 1_000_000,
        zero_for_one,
        word_boundary_prices: Vec::new(),
    })
}

/// Powers of ten from `10^0` up to the order of magnitude of `reserve_in`,
/// log-spaced to [`GRID_POINTS`] samples.
fn log_grid(reserve_in: U256) -> Vec<U256> {
    let digits = reserve_in.to_string().len();
    let e_max = digits.saturating_sub(1);
    let mut out = BTreeSet::new();
    for j in 0..GRID_POINTS {
        let e = (j * e_max) / (GRID_POINTS - 1);
        let mut p = U256::from(1u64);
        for _ in 0..e {
            p *= U256::from(10u64);
        }
        out.insert(p);
    }
    out.into_iter().collect()
}

fn u256_from_json(v: &Value) -> Option<U256> {
    if let Some(s) = v.as_str() {
        return s.trim().parse::<U256>().ok();
    }
    v.as_u64().map(U256::from)
}

fn u256_field(v: &Value, k: &str) -> Option<U256> {
    u256_from_json(v.get(k)?)
}

fn parse_capture_v2(hop: &Value) -> Option<V2State> {
    Some(V2State {
        reserve_in: u256_field(hop, "reserve_in")?,
        reserve_out: u256_field(hop, "reserve_out")?,
        gamma_numer: hop.get("gamma_numer")?.as_str()?.parse::<u64>().ok()?,
        fee_denom: hop.get("fee_denom")?.as_str()?.parse::<u64>().ok()?,
    })
}

/// Pull the V2 pool entry `key` from a committed path fixture and orient it by
/// that fixture's recorded V2 direction.
fn parse_path_state(doc: &Value, key: &str) -> Option<V2State> {
    let pool = doc.get("pools")?.get(key)?;
    let r0 = u256_field(pool, "reserve0")?;
    let r1 = u256_field(pool, "reserve1")?;
    let gamma = pool.get("fee_gamma").and_then(Value::as_u64).or_else(|| {
        pool.get("fee_token0")
            .and_then(Value::as_u64)
            .map(|f| 1000 - f)
    })?;
    let denom = pool
        .get("fee_denom")
        .and_then(Value::as_u64)
        .or_else(|| pool.get("fee_denominator").and_then(Value::as_u64))?;
    let zfo = doc
        .get("recorded_solve")
        .and_then(|r| r.get("v2_zero_for_one"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let (reserve_in, reserve_out) = if zfo { (r0, r1) } else { (r1, r0) };
    Some(V2State {
        reserve_in,
        reserve_out,
        gamma_numer: gamma,
        fee_denom: denom,
    })
}

#[derive(Default)]
struct Bucket {
    n: u64,
    divergent: u64,
    positive: u64,
    negative: u64,
    zero: u64,
    consumed_mismatch: u64,
    deltas: Vec<i128>,
}

impl Bucket {
    fn push(&mut self, delta: i128, consumed_ok: bool) {
        self.n += 1;
        match delta.cmp(&0) {
            std::cmp::Ordering::Equal => self.zero += 1,
            std::cmp::Ordering::Greater => {
                self.positive += 1;
                self.divergent += 1;
            }
            std::cmp::Ordering::Less => {
                self.negative += 1;
                self.divergent += 1;
            }
        }
        if !consumed_ok {
            self.consumed_mismatch += 1;
        }
        self.deltas.push(delta);
    }

    fn json(&self) -> Value {
        let mut d = self.deltas.clone();
        d.sort_unstable();
        let nonzero: Vec<i128> = d.iter().copied().filter(|v| *v != 0).collect();
        let med = |v: &[i128]| v.get(v.len() / 2).copied().unwrap_or(0);
        json!({
            "n": self.n,
            "divergent": self.divergent,
            "divergent_pct": if self.n > 0 { 100.0 * self.divergent as f64 / self.n as f64 } else { 0.0 },
            "positive": self.positive,
            "negative": self.negative,
            "zero": self.zero,
            "consumed_mismatch": self.consumed_mismatch,
            "delta": {"min": d.first().copied().unwrap_or(0), "median": med(&d), "max": d.last().copied().unwrap_or(0)},
            "delta_nonzero": {"count": nonzero.len(), "min": nonzero.first().copied().unwrap_or(0), "median": med(&nonzero), "max": nonzero.last().copied().unwrap_or(0)},
        })
    }
}

fn ratio_label(reserve_in: U256, reserve_out: U256) -> String {
    let ri: f64 = reserve_in.to_string().parse().unwrap_or(1.0);
    let ro: f64 = reserve_out.to_string().parse().unwrap_or(1.0);
    let ratio = if ri > 0.0 { ro / ri } else { 0.0 };
    if ratio < 1e-3 {
        "<1e-3".into()
    } else if ratio < 1e-1 {
        "1e-3..1e-1".into()
    } else if ratio < 3.0 {
        "~1x".into()
    } else if ratio < 100.0 {
        "3..100x".into()
    } else {
        ">=100x".into()
    }
}

fn pow10(e: u32) -> U256 {
    let mut p = U256::from(1u64);
    for _ in 0..e {
        p *= U256::from(10u64);
    }
    p
}

struct Job {
    state: V2State,
    dataset: &'static str,
    fee_label: String,
    ratio_label: String,
    extra_inputs: Vec<U256>,
}

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // Repo-root `tests/fixtures` (the crate sits at `rust/crates/<name>`).
    let repo_root = manifest.join("../../..");

    // ── real corpus: heavy mixed captures ────────────────────────────────
    let capture_path = manifest.join("tests/fixtures/heavy_mixed_solve_captures.jsonl");
    let content = degenbot_solvers::capture_fixture::read_fixture(&capture_path);
    let mut unique = BTreeMap::<String, V2State>::new();
    let mut occurrences = 0u64;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(doc) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(hops) = doc.get("hops").and_then(Value::as_array) else {
            continue;
        };
        for hop in hops {
            if hop.get("kind").and_then(Value::as_str) == Some("V2") {
                if let Some(s) = parse_capture_v2(hop) {
                    occurrences += 1;
                    unique.entry(s.key()).or_insert(s);
                }
            }
        }
    }
    let heavy_unique = unique.len();
    let mut real_states: Vec<V2State> = unique.values().copied().collect();

    // ── real corpus: committed single-path fixtures ──────────────────────
    let mut hotspots: Vec<U256> = Vec::new();
    let mut path_states = 0usize;
    for (file, key) in [
        ("tests/fixtures/path5000_v2v4v3_block25704509.json", "v2_0"),
        (
            "tests/fixtures/path110302_v3v4v2_block25711761.json",
            "v2_2",
        ),
        (
            "tests/fixtures/path182449_v4v4v2_block25731019.json",
            "v2_c",
        ),
    ] {
        let p = repo_root.join(file);
        let Ok(txt) = std::fs::read_to_string(&p) else {
            eprintln!("path fixture missing: {}", p.display());
            continue;
        };
        let Ok(doc) = serde_json::from_str::<Value>(&txt) else {
            eprintln!("path fixture parse failed: {}", p.display());
            continue;
        };
        if let Some(s) = parse_path_state(&doc, key) {
            path_states += 1;
            real_states.push(s);
        }
        let recorded = doc.get("recorded_solve");
        if let Some(h) = recorded
            .and_then(|r| r.get("v2_input"))
            .and_then(u256_from_json)
            .or_else(|| {
                recorded
                    .and_then(|r| r.get("optimal_input"))
                    .and_then(u256_from_json)
            })
        {
            hotspots.push(h);
        }
    }

    // ── synthetic grid ───────────────────────────────────────────────────
    let mut jobs: Vec<Job> = Vec::new();
    for state in &real_states {
        jobs.push(Job {
            state: *state,
            dataset: "real",
            fee_label: state.fee_label(),
            ratio_label: ratio_label(state.reserve_in, state.reserve_out),
            extra_inputs: hotspots.clone(),
        });
    }
    let fees = [
        (997u64, 1000u64, "0.30%"),
        (99, 100, "1.00%"),
        (9995, 10000, "0.05%"),
    ];
    let ratios = [(1u128, "near-1x"), (10, "10x"), (1000, "1000x")];
    let magnitudes = [18u32, 21, 24, 27];
    let mut synthetic_states = 0usize;
    for (g, d, fl) in fees {
        for (ratio, rl) in ratios {
            for m in magnitudes {
                let reserve_in = pow10(m);
                let reserve_out = reserve_in * U256::from(ratio);
                jobs.push(Job {
                    state: V2State {
                        reserve_in,
                        reserve_out,
                        gamma_numer: g,
                        fee_denom: d,
                    },
                    dataset: "synthetic",
                    fee_label: fl.to_string(),
                    ratio_label: rl.to_string(),
                    extra_inputs: Vec::new(),
                });
                synthetic_states += 1;
            }
        }
    }

    // ── run ──────────────────────────────────────────────────────────────
    let mut overall = Bucket::default();
    let mut by_direction: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut by_fee: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut by_ratio: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut by_dataset: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut boundary_reached = 0u64;
    let mut divergences: Vec<Value> = Vec::new();

    for job in &jobs {
        let baseline = job.state.hop();
        let mut grid = log_grid(job.state.reserve_in);
        for x in &job.extra_inputs {
            if !grid.contains(x) {
                grid.push(*x);
            }
        }
        grid.sort_unstable();
        for x in grid {
            for zero_for_one in [true, false] {
                let Some(cl_hop) = project(job.state, zero_for_one) else {
                    continue;
                };
                let Ok(out_v2) = baseline.swap(x) else {
                    continue;
                };
                let cl = simulate_v3_range_swap(x, &cl_hop);
                let out_cl = cl.output;
                let delta: i128 = i128::try_from(out_cl).unwrap_or(i128::MAX)
                    - i128::try_from(out_v2).unwrap_or(i128::MAX);
                let consumed_ok = cl.consumed_input == x;
                if !consumed_ok {
                    boundary_reached += 1;
                }
                overall.push(delta, consumed_ok);
                by_direction
                    .entry(if zero_for_one { "zfo" } else { "ofz" }.to_string())
                    .or_default()
                    .push(delta, consumed_ok);
                by_fee
                    .entry(job.fee_label.clone())
                    .or_default()
                    .push(delta, consumed_ok);
                by_ratio
                    .entry(job.ratio_label.clone())
                    .or_default()
                    .push(delta, consumed_ok);
                by_dataset
                    .entry(job.dataset.to_string())
                    .or_default()
                    .push(delta, consumed_ok);

                if delta != 0 || !consumed_ok {
                    divergences.push(json!({
                        "dataset": job.dataset,
                        "fee": job.fee_label.clone(),
                        "ratio": job.ratio_label.clone(),
                        "direction": if zero_for_one { "zfo" } else { "ofz" },
                        "reserve_in": job.state.reserve_in.to_string(),
                        "reserve_out": job.state.reserve_out.to_string(),
                        "x": x.to_string(),
                        "out_v2": out_v2.to_string(),
                        "out_cl": out_cl.to_string(),
                        "delta": delta,
                        "consumed_v2": x.to_string(),
                        "consumed_cl": cl.consumed_input.to_string(),
                    }));
                }
            }
        }
    }

    for row in &divergences {
        println!("DIVERGENCE {row}");
    }

    let verdict = if overall.divergent == 0 && overall.consumed_mismatch == 0 {
        "A-cleared"
    } else {
        "B-keep"
    };

    let bucket_map = |m: &BTreeMap<String, Bucket>| -> Value {
        let mut out = serde_json::Map::new();
        for (k, v) in m {
            out.insert(k.clone(), v.json());
        }
        Value::Object(out)
    };

    let summary = json!({
        "verdict": verdict,
        "zero_tolerance": "A cleared only if every step is byte-identical on output AND consumed input",
        "corpus": {
            "heavy_mixed_unique_v2_states": heavy_unique,
            "heavy_mixed_v2_occurrences": occurrences,
            "path_fixture_v2_states": path_states,
            "real_states_total": real_states.len(),
            "recorded_hotspot_inputs": hotspots.len(),
            "synthetic_states": synthetic_states,
            "jobs": jobs.len(),
        },
        "grid": {"points_per_state": GRID_POINTS, "extra_hotspots_shared_across_real": true},
        "overall": overall.json(),
        "by_direction": bucket_map(&by_direction),
        "by_fee": bucket_map(&by_fee),
        "by_ratio": bucket_map(&by_ratio),
        "by_dataset": bucket_map(&by_dataset),
        "range_boundary_hits": boundary_reached,
        "divergence_examples": divergences.iter().take(8).collect::<Vec<_>>(),
    });

    println!("SUMMARY {summary}");
}

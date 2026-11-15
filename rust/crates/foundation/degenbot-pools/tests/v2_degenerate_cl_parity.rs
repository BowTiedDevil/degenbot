//! Swap-step parity census: a V2 constant-product hop projected as a
//! degenerate single-range concentrated liquidity tick sequence (ADR-060).
//!
//! Durable owner of the decision-critical invariants recorded in
//! `docs/architecture/v2-cl-parity-probe.md`. For every `(state, input)` on the
//! pinned population — 30 real V2 hop states (27 unique from the heavy mixed
//! capture corpus plus the three committed single-path pool fixtures) and 36
//! synthetic grid states, each swept over the 8-point log-spaced input grid
//! plus the recorded hotspot inputs, in both directions — exactly 1,236
//! comparisons, and the census pins:
//!
//!   * `consumed_input == x` on every comparison: the degenerate range is
//!     widened `2^32` past every tested input, so the step stays in the
//!     target-unreachable (open-AMM) branch — the only branch with a V2
//!     analogue — and both simulators consume the full input. A range-boundary
//!     hit is a projection artifact, not a V2 behaviour, and must stay at zero.
//!   * every zero-for-one divergence is strictly non-positive (the zfo
//!     rounding path floors `amount1`; it may undershoot V2, never overshoot).
//!   * zero divergence in the deep sub-`1e-3` reserve-ratio regime.
//!   * the direction / regime / dataset counts that keep those invariants
//!     non-vacuous: divergence exists (225 of 1,236), is two-sided overall
//!     (the one-for-zero arm can overshoot), and concentrates in the deep-
//!     `R_out` regimes.
//!
//! The mapping, the grid, and the corpus reconstruction are the probe's own:
//! the baseline is `degenbot_math::v2::IntHopState::swap` (the V2 one-DIV
//! rational floor) and the candidate is the same state projected to one
//! concentrated liquidity range (`L = floor(sqrt(R_in * R_out))`,
//! `sqrt_price_x96 = floor(L * 2^96 / R_in)`, empty interior word boundaries,
//! fee mapped onto the identical `gamma/fee_denom` fraction) run through the
//! production `simulate_v3_range_swap`. Neither simulator is special-cased.
//! The overall zero-tolerance bar (ADR-060 option A) stays refuted by this
//! census, not cleared: the test exists so a rounding change in either
//! simulator can no longer land silently.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use alloy::primitives::{U256, U512};
use degenbot_math::v2::IntHopState;
use degenbot_pools::solve::concentrated_liquidity::{simulate_v3_range_swap, WalkTally};
use degenbot_pools::state::int_v3_hop::IntV3TickRangeHop;
use num_bigint::BigUint;
use serde_json::Value;

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

    fn hop(self) -> IntHopState {
        IntHopState::new(
            self.reserve_in,
            self.reserve_out,
            self.gamma_numer,
            self.fee_denom,
        )
    }
}

/// Project a V2 hop to one degenerate concentrated liquidity range. `zero_for_one` chooses which
/// reserve is projected onto token0; the other projection is the reverse.
///
/// Returns `None` when the state cannot be represented (zero reserve, zero
/// geometric-mean liquidity, or a fee fraction not expressible in the concentrated liquidity
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

    // Map the V2 fraction gamma/fee_denom onto concentrated liquidity millionths-of-a-pip. Exact for
    // the standard 0.3% (997/1000), 0.25% (9975/10000), 0.05% (9995/10000), and
    // 1% (99/100) conventions; a non-exact fee fraction is out of scope.
    let fee_pips = 1_000_000u128 * u128::from(state.fee_denom - state.gamma_numer)
        / u128::from(state.fee_denom);
    if fee_pips >= 1_000_000 {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "fee_pips is checked strictly below one million just above, so the narrow cannot truncate"
    )]
    let concentrated_liquidity_gamma = (1_000_000 - fee_pips) as u64;

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
        gamma_numer: concentrated_liquidity_gamma,
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

fn ratio_label(reserve_in: U256, reserve_out: U256) -> &'static str {
    let ri: f64 = reserve_in.to_string().parse().unwrap_or(1.0);
    let ro: f64 = reserve_out.to_string().parse().unwrap_or(1.0);
    let ratio = if ri > 0.0 { ro / ri } else { 0.0 };
    if ratio < 1e-3 {
        "<1e-3"
    } else if ratio < 1e-1 {
        "1e-3..1e-1"
    } else if ratio < 3.0 {
        "~1x"
    } else if ratio < 100.0 {
        "3..100x"
    } else {
        ">=100x"
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
    ratio_label: &'static str,
    extra_inputs: Vec<U256>,
}

/// The committed comparison population: every real V2 hop state reachable from
/// the committed corpora plus the synthetic fee/ratio/magnitude grid, each
/// carrying its input grid.
struct Corpus {
    jobs: Vec<Job>,
    heavy_unique: usize,
    heavy_occurrences: u64,
    path_states: usize,
    recorded_hotspots: usize,
    synthetic_states: usize,
}

#[expect(
    clippy::panic,
    reason = "a missing or unparseable committed fixture is unrecoverable: the census must stop loudly naming the path, not silently shrink"
)]
fn build_corpus() -> Corpus {
    // ── real corpus: heavy mixed captures ────────────────────────────────
    let capture_path =
        degenbot_pools::solve::fixtures::fixture_path("heavy_mixed_solve_captures.jsonl");
    let content = degenbot_pools::solve::fixtures::read_fixture(&capture_path);
    let mut unique = BTreeMap::<String, V2State>::new();
    let mut occurrences = 0u64;
    for doc in degenbot_pools::solve::fixtures::rows(&content) {
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
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .join(file);
        let txt = std::fs::read_to_string(&p)
            .unwrap_or_else(|_| panic!("path fixture missing: {}", p.display()));
        let doc: Value = serde_json::from_str(&txt)
            .unwrap_or_else(|e| panic!("path fixture parse failed ({}): {e}", p.display()));
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
            ratio_label: ratio_label(state.reserve_in, state.reserve_out),
            extra_inputs: hotspots.clone(),
        });
    }
    let fees = [(997u64, 1000u64), (99, 100), (9995, 10000)];
    let ratios = [(1u128, "near-1x"), (10, "10x"), (1000, "1000x")];
    let magnitudes = [18u32, 21, 24, 27];
    let mut synthetic_states = 0usize;
    for (g, d) in fees {
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
                    ratio_label: rl,
                    extra_inputs: Vec::new(),
                });
                synthetic_states += 1;
            }
        }
    }

    Corpus {
        jobs,
        heavy_unique,
        heavy_occurrences: occurrences,
        path_states,
        recorded_hotspots: hotspots.len(),
        synthetic_states,
    }
}

/// Per-comparison tallies for one slice of the population. A divergence is a
/// non-zero output delta; `consumed_mismatch` counts a range-boundary hit
/// (the candidate consumed less than the full input).
#[derive(Default, Clone, Copy)]
struct Bucket {
    n: u64,
    divergent: u64,
    positive: u64,
    negative: u64,
    zero: u64,
    consumed_mismatch: u64,
    min_delta: i128,
    max_delta: i128,
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
        if self.n == 1 {
            self.min_delta = delta;
            self.max_delta = delta;
        } else {
            self.min_delta = self.min_delta.min(delta);
            self.max_delta = self.max_delta.max(delta);
        }
    }
}

#[derive(Default)]
struct Census {
    boundary_hits: u64,
    overall: Bucket,
    by_direction: BTreeMap<&'static str, Bucket>,
    by_ratio: BTreeMap<&'static str, Bucket>,
    by_dataset: BTreeMap<&'static str, Bucket>,
}

/// Run every `(state, input, direction)` comparison of the corpus through the
/// two production simulators and tally the deltas.
fn run_population(corpus: &Corpus) -> Census {
    let mut census = Census::default();
    for job in &corpus.jobs {
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
                let Some(concentrated_liquidity_hop) = project(job.state, zero_for_one) else {
                    continue;
                };
                let Ok(out_v2) = baseline.swap(x) else {
                    continue;
                };
                let concentrated_liquidity = simulate_v3_range_swap(
                    x,
                    &concentrated_liquidity_hop,
                    &mut WalkTally::default(),
                );
                let out_concentrated_liquidity = concentrated_liquidity.output;
                let delta: i128 = i128::try_from(out_concentrated_liquidity).unwrap_or(i128::MAX)
                    - i128::try_from(out_v2).unwrap_or(i128::MAX);
                let consumed_ok = concentrated_liquidity.consumed_input == x;
                if !consumed_ok {
                    census.boundary_hits += 1;
                }
                let direction = if zero_for_one { "zfo" } else { "ofz" };
                census.overall.push(delta, consumed_ok);
                census
                    .by_direction
                    .entry(direction)
                    .or_default()
                    .push(delta, consumed_ok);
                census
                    .by_ratio
                    .entry(job.ratio_label)
                    .or_default()
                    .push(delta, consumed_ok);
                census
                    .by_dataset
                    .entry(job.dataset)
                    .or_default()
                    .push(delta, consumed_ok);
            }
        }
    }
    census
}

#[test]
fn v2_degenerate_cl_parity_census() {
    let corpus = build_corpus();
    assert_eq!(
        corpus.heavy_unique, 27,
        "heavy mixed corpus: unique V2 states"
    );
    assert_eq!(
        corpus.heavy_occurrences, 369,
        "heavy mixed corpus: V2 occurrences"
    );
    assert_eq!(corpus.path_states, 3, "committed path fixtures: V2 states");
    assert_eq!(corpus.recorded_hotspots, 3, "recorded hotspot inputs");
    assert_eq!(
        corpus.jobs.len() - corpus.synthetic_states,
        30,
        "real V2 states total (27 heavy mixed + 3 path fixtures)"
    );
    assert_eq!(
        corpus.synthetic_states, 36,
        "synthetic fee x ratio x magnitude states"
    );
    assert_eq!(corpus.jobs.len(), 66, "comparison jobs");

    let census = run_population(&corpus);

    // Invariant 1: consumed_input == x on every comparison. The widened range
    // must never be reached, so a boundary hit means the projection artifact
    // leaked into the comparison population.
    assert_eq!(
        census.boundary_hits, 0,
        "the degenerate range must stay target-unreachable for every grid input"
    );
    assert_eq!(
        census.overall.consumed_mismatch, 0,
        "consumed_input must equal x on every comparison"
    );

    // Invariant 2: every zero-for-one divergence is strictly non-positive.
    let zfo = census.by_direction["zfo"];
    assert_eq!(zfo.n, 618, "zero-for-one comparisons");
    assert_eq!(zfo.consumed_mismatch, 0, "zero-for-one consumed_input");
    assert_eq!(zfo.divergent, 103, "zero-for-one divergent comparisons");
    assert_eq!(
        zfo.positive, 0,
        "zero-for-one must never overshoot the V2 baseline (max delta must stay non-positive)"
    );
    assert_eq!(
        zfo.negative, 103,
        "every zero-for-one divergence is negative"
    );
    assert_eq!(zfo.max_delta, 0, "zero-for-one delta ceiling");
    assert_eq!(
        zfo.min_delta, -530_594_942,
        "zero-for-one deepest undershoot"
    );

    // The one-for-zero arm is the counterweight that keeps the sign invariant
    // from degenerating into 'everything is zero or negative': it diverges
    // both ways, including the +7,255 overshoot.
    let ofz = census.by_direction["ofz"];
    assert_eq!(ofz.n, 618, "one-for-zero comparisons");
    assert_eq!(ofz.consumed_mismatch, 0, "one-for-zero consumed_input");
    assert_eq!(ofz.divergent, 122, "one-for-zero divergent comparisons");
    assert_eq!(ofz.negative, 89, "one-for-zero undershoots");
    assert_eq!(
        ofz.positive, 33,
        "one-for-zero overshoots (two-sided divergence)"
    );
    assert_eq!(ofz.max_delta, 7_255, "one-for-zero deepest overshoot");
    assert_eq!(
        ofz.min_delta, -530_594_942,
        "one-for-zero deepest undershoot"
    );

    // Invariant 3: zero divergence in the deep sub-1e-3 ratio regime. The
    // regime is wide (550 comparisons) so the zero is a population fact, not
    // an empty bucket.
    let deep = census.by_ratio["<1e-3"];
    assert_eq!(deep.n, 550, "deep sub-1e-3 ratio regime comparisons");
    assert_eq!(
        deep.divergent, 0,
        "deep sub-1e-3 ratio regime must be divergence-free"
    );
    assert_eq!(deep.consumed_mismatch, 0, "deep sub-1e-3 consumed_input");

    // The rest of the ratio census: divergence concentrates where R_out
    // dominates R_in, and the mid regimes stay non-empty.
    let near_1x = census.by_ratio["near-1x"];
    assert_eq!(near_1x.n, 192, "near-1x regime comparisons");
    assert_eq!(near_1x.divergent, 10, "near-1x divergent comparisons");
    assert_eq!(
        near_1x.positive, 0,
        "near-1x divergences are undershoots only"
    );
    assert_eq!(census.by_ratio["10x"].n, 192, "10x regime comparisons");
    assert_eq!(
        census.by_ratio["10x"].divergent, 64,
        "10x divergent comparisons"
    );
    assert_eq!(census.by_ratio["1000x"].n, 192, "1000x regime comparisons");
    assert_eq!(
        census.by_ratio["1000x"].divergent, 85,
        "1000x divergent comparisons"
    );
    assert_eq!(
        census.by_ratio[">=100x"].n, 110,
        ">=100x regime comparisons"
    );
    assert_eq!(
        census.by_ratio[">=100x"].divergent, 66,
        ">=100x divergent comparisons"
    );

    // Dataset split: both the real captures and the synthetic grid carry
    // divergence, so neither slice is dead weight.
    let real = census.by_dataset["real"];
    assert_eq!(
        real.n, 660,
        "real corpus comparisons (30 states x 11 inputs x 2 directions)"
    );
    assert_eq!(real.divergent, 66, "real corpus divergent comparisons");
    assert_eq!(real.consumed_mismatch, 0, "real corpus consumed_input");
    let synthetic = census.by_dataset["synthetic"];
    assert_eq!(
        synthetic.n, 576,
        "synthetic corpus comparisons (36 states x 8 inputs x 2 directions)"
    );
    assert_eq!(
        synthetic.divergent, 159,
        "synthetic corpus divergent comparisons"
    );
    assert_eq!(
        synthetic.consumed_mismatch, 0,
        "synthetic corpus consumed_input"
    );

    // Overall population: exactly the pinned 1,236 comparisons, with the
    // divergence census that refutes (never clears) ADR-060's zero-tolerance
    // bar. A change here means a simulator's rounding moved.
    assert_eq!(census.overall.n, 1_236, "total comparisons");
    assert_eq!(census.overall.divergent, 225, "total divergent comparisons");
    assert_eq!(census.overall.negative, 192, "total undershoots");
    assert_eq!(census.overall.positive, 33, "total overshoots");
    assert_eq!(census.overall.zero, 1_011, "byte-identical comparisons");
    assert_eq!(census.overall.min_delta, -530_594_942, "deepest undershoot");
    assert_eq!(census.overall.max_delta, 7_255, "deepest overshoot");
}

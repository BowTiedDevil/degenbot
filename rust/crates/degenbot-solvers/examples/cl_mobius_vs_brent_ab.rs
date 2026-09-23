// Dev/example-only harness: an A/B benchmark, not a gate.
// Pedantic + restriction lints that production code denies are relaxed here.
#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::unwrap_used
)]

//! Report-only A/B: the piecewise-Möbius active-set walk
//! (`solve_cl_piecewise`, machinery unmodified) versus the pure-Rust
//! bounded-Brent scalar minimizer (`bounded_brent::minimize_scalar_bounded`)
//! on synthetic three-hop all-CL (Uniswap V3) arbitrage paths.
//!
//! The corpus is generated at runtime from a fixed XORSHIFT seed — no fixture
//! files. Every hop owns a distinct synthetic V3 pool (dense tick map around an
//! anchor), and both solvers see the same physical path through different
//! oracles:
//!
//! - **Möbius** solves the `IntV3TickRangeSequence` projection through its own
//!   active-set walk; every timed solve re-derives [`ClSolveTables`] per hop
//!   (cold end-to-end cost, matching the production call shape).
//! - **Brent** minimizes the archived Python objective
//!   `int(x) -> hop-by-hop v3_simulate_swap -> out - int(x)` over
//!   `x ∈ (1.0, MAX_INPUT]` with `xatol = 1.0` and no bracket.
//! - **Brent (cross-oracle)** runs the same minimizer against the public
//!   byte-exact helper sim ([`ClPathSim`]) so both solvers can be timed on the
//!   walk's own oracle.
//!
//! Everything is report-only: no verdict, no exit code, no CI gate. Output is
//! a per-path JSONL (`<out>`) plus a cross-oracle JSONL
//! (`<out stem>_cross_oracle.jsonl`) and a stdout summary. All timing lives
//! under a `timing` key so two runs are byte-identical once it is stripped.
//!
//! Environment knobs: `DR_AB_SCALE` (family multiplicity, default 1),
//! `DR_AB_WINDOW` (relative input agreement window, default 0.001),
//! `DR_AB_OUT` (JSONL path, default `target/cl_mobius_vs_brent_ab.jsonl`),
//! `DR_AB_REPS` (timed repetitions, default 25).

use std::path::PathBuf;
use std::time::Instant;

use alloy::primitives::{I256, U128, U256, U512};
use degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_pools::v3_state::{
    v3_simulate_swap, PoolTickCoverage, RegisterV3PoolParams, V3PoolState, V3SwapOutcome,
};
use degenbot_pools::TickInfo;
use degenbot_solvers::bounded_brent::{minimize_scalar_bounded, BrentMinimize, DEFAULT_MAXFUN};
use degenbot_solvers::cl::{
    build_cl_crossing_table, derive_and_solve_cl_piecewise, ClPathSim, IntV3TickRangeSequence,
    WalkOutcome, WalkStats, DENSE_OBSERVE_THRESHOLD,
};
use degenbot_solvers::runtime::SolveRuntimeConfig;
use hashbrown::HashMap;
use serde_json::{json, Value};

/// Corpus RNG seed (XORSHIFT64*). Fixed so runs are reproducible.
const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Upper search bound (wei) of the bounded-Brent objective. The archived
/// Python shape's `MAX_INPUT = 100e18` truncated optima past the first-hop
/// saturation edge (`stress_w` books absorb 3000-8000 tokens, so an edge can
/// sit near 8e21) and manufactured `disagree_cap_hit` rows. The bound must
/// clear the largest in-corpus optimum with headroom; Brent pays for that
/// with more nfev narrowing the wider f64 bracket.
const MAX_INPUT_F64: f64 = 20_000e18;

/// Absolute input tolerance passed to the bounded minimizer.
const XATOL: f64 = 1.0;

/// Timed repetitions per (path, solver).
const DEFAULT_REPS: usize = 25;

/// Paths per family at `DR_AB_SCALE = 1` (7 families -> 301 paths).
const PER_FAMILY: usize = 43;

/// Seven stratified families, equal counts.
const FAMILIES: [&str; 7] = [
    "viz_uniform",
    "viz_decaying",
    "viz_growing",
    "viz_random",
    "stress_w",
    "stress_g",
    "stress_g2",
];

const DEFAULT_WINDOW: f64 = 0.001;

// ---------------------------------------------------------------------------
// Deterministic RNG
// ---------------------------------------------------------------------------

/// XORSHIFT64*: tiny, deterministic, reproducible across platforms.
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    fn range_i32(&mut self, lo: i32, hi: i32) -> i32 {
        lo + i32::try_from(self.below(u64::from((hi - lo + 1) as u32))).unwrap()
    }

    fn range_usize(&mut self, lo: usize, hi: usize) -> usize {
        lo + usize::try_from(self.below(u64::try_from(hi - lo + 1).unwrap())).unwrap()
    }

    fn range_f64(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (self.next_u64() as f64 / u64::MAX as f64) * (hi - lo)
    }

    fn chance(&mut self, p: f64) -> bool {
        self.range_f64(0.0, 1.0) < p
    }
}

// ---------------------------------------------------------------------------
// Synthetic pool construction
// ---------------------------------------------------------------------------

fn u256_to_f64(v: U256) -> f64 {
    let limbs = v.as_limbs();
    let mut out = 0.0_f64;
    for i in (0..4).rev() {
        out = out * 18446744073709551616.0 + limbs[i] as f64;
    }
    out
}

fn sqrt_at(tick: i32) -> U256 {
    U256::from(get_sqrt_ratio_at_tick_internal(tick).expect("tick within V3 bounds"))
}

fn snap(tick: i32, spacing: i32) -> i32 {
    tick.div_euclid(spacing) * spacing
}

/// `(lower, upper)` initialized-tick boundaries of range `i`.
fn range_bounds(anchor: i32, spacing: i32, zfo: bool, i: i32) -> (i32, i32) {
    if zfo {
        (anchor - (i + 1) * spacing, anchor - i * spacing)
    } else {
        (anchor + i * spacing, anchor + (i + 1) * spacing)
    }
}

/// Entry/exit sqrt prices of range `i` in swap order.
fn range_sqrt(anchor: i32, spacing: i32, zfo: bool, i: i32) -> (U256, U256) {
    let (lower, upper) = range_bounds(anchor, spacing, zfo, i);
    if zfo {
        (sqrt_at(upper), sqrt_at(lower))
    } else {
        (sqrt_at(lower), sqrt_at(upper))
    }
}

/// Liquidity that lets one range absorb `depth_tokens` of the input token
/// end-to-end (`liquidityForDepth` from the solver viz):
///
/// - zfo (input token0): `L = dx·S·Sbot / (2^96·(S−Sbot))`.
/// - ofz (input token1): `L = dx·2^96 / (Stop−S)`.
fn liquidity_for_depth(depth_tokens: f64, s: U256, s_next: U256, zfo: bool) -> u128 {
    let dx = U256::from((depth_tokens * 1e18).round() as u128);
    let q96 = U256::from(1u128) << 96;
    let l = if zfo {
        let num = U512::from(dx) * U512::from(s) * U512::from(s_next);
        let den = U512::from(q96) * U512::from(s - s_next);
        num / den
    } else {
        let num = U512::from(dx) * U512::from(q96);
        let den = U512::from(s_next - s);
        num / den
    };
    u128::try_from(l).unwrap_or(u128::MAX).max(1)
}

/// Liquidity for a viz-profile hop: one depth sampled per hop, scaled per range.
fn viz_liquidities(
    rng: &mut Rng,
    profile: &str,
    anchor: i32,
    spacing: i32,
    zfo: bool,
    count: usize,
) -> Vec<u128> {
    let depth = rng.range_f64(5.0, 195.0);
    (0..count)
        .map(|i| {
            let i_i32 = i32::try_from(i).unwrap();
            let factor = match profile {
                "viz_decaying" => 0.62_f64.powi(i_i32),
                "viz_growing" => 1.55_f64.powi(i_i32),
                "viz_random" => 0.35 + rng.range_f64(0.0, 1.9),
                _ => 1.0,
            };
            let (s, s_next) = range_sqrt(anchor, spacing, zfo, i_i32);
            liquidity_for_depth(depth * factor, s, s_next, zfo)
        })
        .collect()
}

/// Uniform liquidity per range for the stress families (depth-derived).
fn depth_liqs(
    rng: &mut Rng,
    anchor: i32,
    spacing: i32,
    zfo: bool,
    count: usize,
    dlo: f64,
    dhi: f64,
) -> Vec<u128> {
    (0..count)
        .map(|i| {
            let i_i32 = i32::try_from(i).unwrap();
            let (s, s_next) = range_sqrt(anchor, spacing, zfo, i_i32);
            liquidity_for_depth(rng.range_f64(dlo, dhi), s, s_next, zfo)
        })
        .collect()
}

struct Hop {
    state: V3PoolState,
    seq: IntV3TickRangeSequence,
    spacing: i32,
    fee: u32,
    zfo: bool,
}

/// Build one synthetic V3 pool from adjacent ranges with the given
/// per-range liquidities, plus the solver projection of that pool.
///
/// `on_tick` places the current tick exactly on a net-carrying initialized
/// boundary (the zfo leading-drain case) when the pool has >= 2 ranges;
/// otherwise the current tick sits mid-range inside the first band.
fn make_hop(
    anchor: i32,
    spacing: i32,
    fee: u32,
    zfo: bool,
    liquidities: &[u128],
    on_tick: bool,
) -> Hop {
    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    let count = i32::try_from(liquidities.len()).unwrap();
    for (i, &liq) in liquidities.iter().enumerate() {
        let i = i32::try_from(i).unwrap();
        let (lower, upper) = range_bounds(anchor, spacing, zfo, i);
        let lo = tick_data.entry(lower).or_insert_with(|| TickInfo {
            liquidity_gross: U128::ZERO,
            liquidity_net: 0,
            block: 0,
        });
        lo.liquidity_gross = U128::from(lo.liquidity_gross.to::<u128>().saturating_add(liq));
        lo.liquidity_net = lo
            .liquidity_net
            .saturating_add(i128::try_from(liq).unwrap());
        let hi = tick_data.entry(upper).or_insert_with(|| TickInfo {
            liquidity_gross: U128::ZERO,
            liquidity_net: 0,
            block: 0,
        });
        hi.liquidity_gross = U128::from(hi.liquidity_gross.to::<u128>().saturating_add(liq));
        hi.liquidity_net = hi
            .liquidity_net
            .saturating_sub(i128::try_from(liq).unwrap());
    }

    let current_tick = if zfo {
        if on_tick && count >= 2 {
            anchor - spacing
        } else {
            anchor - spacing / 2
        }
    } else if on_tick && count >= 2 {
        anchor + spacing
    } else {
        anchor + spacing / 2
    };

    let mut active: u128 = 0;
    for (i, &liq) in liquidities.iter().enumerate() {
        let i = i32::try_from(i).unwrap();
        let (lower, upper) = range_bounds(anchor, spacing, zfo, i);
        if lower <= current_tick && current_tick < upper {
            active = active.saturating_add(liq);
        }
    }

    let params = RegisterV3PoolParams {
        fee,
        tick_spacing: spacing,
        sqrt_price_x96: sqrt_at(current_tick),
        liquidity: active,
        tick: current_tick,
        tick_data,
        coverage: PoolTickCoverage::Tracked,
        ..Default::default()
    };
    let (_identity, state) = V3PoolState::from_params(params, 8);
    let seq = state
        .build_int_v3_sequence(spacing, fee, zfo)
        .expect("synthetic geometry yields a swap-direction sequence");
    Hop {
        state,
        seq,
        spacing,
        fee,
        zfo,
    }
}

// ---------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------

struct PathSpec {
    id: usize,
    family: &'static str,
    profile: &'static str,
    spacings: [i32; 3],
    fees: [u32; 3],
    dirs: [bool; 3],
    ranges: [usize; 3],
    anchors: [i32; 3],
    on_tick: [bool; 3],
    hops: Vec<Hop>,
}

fn pick(values: &[i32], rng: &mut Rng) -> i32 {
    values[rng.range_usize(0, values.len() - 1)]
}

fn dir_pattern(id: usize) -> [bool; 3] {
    let p = id % 8;
    [(p & 1) != 0, (p & 2) != 0, (p & 4) != 0]
}

fn build_viz_path(family: &'static str, id: usize, rng: &mut Rng) -> PathSpec {
    let profile: &'static str = match family {
        "viz_decaying" => "viz_decaying",
        "viz_growing" => "viz_growing",
        "viz_random" => "viz_random",
        _ => "viz_uniform",
    };
    let dirs = dir_pattern(id);
    let spacings = [
        pick(&[200, 1000, 2000, 5000], rng),
        pick(&[200, 1000, 2000, 5000], rng),
        pick(&[200, 1000, 2000, 5000], rng),
    ];
    let fees = [
        pick(&[100, 500, 3000, 10000], rng) as u32,
        pick(&[100, 500, 3000, 10000], rng) as u32,
        pick(&[100, 500, 3000, 10000], rng) as u32,
    ];
    let ranges = [
        rng.range_usize(1, 8),
        rng.range_usize(1, 8),
        rng.range_usize(1, 8),
    ];
    let base = rng.range_i32(-1500, 1500);
    let anchors = [
        snap(base, spacings[0]),
        snap(base + rng.range_i32(500, 3900), spacings[1]),
        snap(base + rng.range_i32(500, 3900), spacings[2]),
    ];
    let mut on_tick = [false; 3];
    let mut hops = Vec::with_capacity(3);
    for i in 0..3 {
        on_tick[i] = ranges[i] >= 2 && rng.chance(0.5);
        let liqs = viz_liquidities(rng, profile, anchors[i], spacings[i], dirs[i], ranges[i]);
        hops.push(make_hop(
            anchors[i],
            spacings[i],
            fees[i],
            dirs[i],
            &liqs,
            on_tick[i],
        ));
    }
    PathSpec {
        id,
        family,
        profile,
        spacings,
        fees,
        dirs,
        ranges,
        anchors,
        on_tick,
        hops,
    }
}

/// Stress-family paths follow `synth_corpus_gen`'s topologies — the
/// walk-heavy deep-late shape and the many-range gate-burst shapes — but route
/// their per-range liquidity through `liquidity_for_depth`. The synth
/// generator's absolute 1e12-scale liquidities make each range absorb ~1e-10
/// tokens, so any input near `MAX_INPUT` drains the whole book and marches
/// empty bitmap words to the price limit; depth-derived liquidity keeps the
/// path inside its own ranges so the A/B measures solver search, not the
/// empty-halt walk.
fn build_stress_path(family: &'static str, id: usize, rng: &mut Rng) -> PathSpec {
    let dirs = [true, false, true];
    let (spacings, ranges, fees, hop_liqs): ([i32; 3], [usize; 3], [u32; 3], [Vec<u128>; 3]) =
        match family {
            "stress_w" => {
                let sp = [pick(&[1000, 1300, 2000], rng), 60, pick(&[10, 20], rng)];
                let ranges = [1usize, 380, rng.range_usize(3, 5)];
                let fees = [500u32, 500, 500];
                // Hop 0: one deep range. Hop 1: many thin ranges with ONE
                // deep-late range at the far end (the walk-heavy family). Hop
                // 2: a few shallow ranges.
                let h0 = depth_liqs(rng, 0, sp[0], dirs[0], ranges[0], 3000.0, 8000.0);
                let h1 = {
                    let mut v = depth_liqs(rng, 0, sp[1], dirs[1], ranges[1] - 1, 0.5, 2.0);
                    let i = i32::try_from(ranges[1] - 1).unwrap();
                    let (s, s_next) = range_sqrt(0, sp[1], dirs[1], i);
                    v.push(liquidity_for_depth(
                        rng.range_f64(3000.0, 8000.0),
                        s,
                        s_next,
                        dirs[1],
                    ));
                    v
                };
                let h2 = depth_liqs(rng, 0, sp[2], dirs[2], ranges[2], 50.0, 200.0);
                (sp, ranges, fees, [h0, h1, h2])
            }
            "stress_g" => {
                let sp = [pick(&[2, 3], rng), pick(&[2, 3], rng), pick(&[2, 3], rng)];
                let ranges = [
                    rng.range_usize(450, 650),
                    rng.range_usize(500, 750),
                    rng.range_usize(450, 650),
                ];
                let fees = [500u32, 500, 500];
                let l = [
                    depth_liqs(rng, 0, sp[0], dirs[0], ranges[0], 1.0, 4.0),
                    depth_liqs(rng, 0, sp[1], dirs[1], ranges[1], 1.0, 5.0),
                    depth_liqs(rng, 0, sp[2], dirs[2], ranges[2], 1.0, 6.0),
                ];
                (sp, ranges, fees, l)
            }
            _ => {
                let sp = [3, 3, 3];
                let ranges = [
                    rng.range_usize(250, 400),
                    rng.range_usize(400, 600),
                    rng.range_usize(300, 450),
                ];
                let fees = [500u32, 500, 500];
                let l = [
                    depth_liqs(rng, 0, sp[0], dirs[0], ranges[0], 1.0, 5.0),
                    depth_liqs(rng, 0, sp[1], dirs[1], ranges[1], 1.0, 5.0),
                    depth_liqs(rng, 0, sp[2], dirs[2], ranges[2], 1.0, 5.0),
                ];
                (sp, ranges, fees, l)
            }
        };

    let base = rng.range_i32(-800, 800);
    let anchors = [
        snap(base, spacings[0]),
        snap(base + rng.range_i32(500, 3900), spacings[1]),
        snap(base + rng.range_i32(500, 3900), spacings[2]),
    ];
    let mut on_tick = [false; 3];
    let mut hops = Vec::with_capacity(3);
    for i in 0..3 {
        on_tick[i] = ranges[i] >= 2 && rng.chance(0.5);
        hops.push(make_hop(
            anchors[i],
            spacings[i],
            fees[i],
            dirs[i],
            &hop_liqs[i],
            on_tick[i],
        ));
    }
    PathSpec {
        id,
        family,
        profile: family,
        spacings,
        fees,
        dirs,
        ranges,
        anchors,
        on_tick,
        hops,
    }
}

fn build_corpus(scale: usize) -> Vec<PathSpec> {
    let mut rng = Rng::new(SEED);
    let mut out = Vec::new();
    let mut id = 0usize;
    for family in FAMILIES {
        for _ in 0..PER_FAMILY * scale {
            let path = if family.starts_with("viz_") {
                build_viz_path(family, id, &mut rng)
            } else {
                build_stress_path(family, id, &mut rng)
            };
            out.push(path);
            id += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Oracles + solvers
// ---------------------------------------------------------------------------

fn hop_output_amount(outcome: &V3SwapOutcome, zfo: bool) -> U256 {
    if zfo {
        outcome.amount1
    } else {
        outcome.amount0
    }
}

/// Native Brent oracle: floor input, hop-by-hop `v3_simulate_swap`.
fn native_output(hops: &[Hop], amount_in: U256) -> U256 {
    let mut current = amount_in;
    for hop in hops {
        if current.is_zero() {
            return U256::ZERO;
        }
        let Ok(amount) = I256::try_from(current) else {
            return U256::ZERO;
        };
        let limit = V3PoolState::default_sqrt_price_limit(hop.zfo);
        match v3_simulate_swap(&hop.state, hop.fee, hop.spacing, hop.zfo, amount, limit) {
            Ok(outcome) => current = hop_output_amount(&outcome, hop.zfo),
            Err(_) => return U256::ZERO,
        }
    }
    current
}

fn floor_input(x: f64) -> U256 {
    if x.is_finite() && x >= 1.0 {
        U256::from(x.floor() as u128)
    } else {
        U256::ZERO
    }
}

#[derive(Clone, PartialEq)]
struct BrentRun {
    x_real: f64,
    x_int: U256,
    profit: U256,
    nfev: usize,
}

fn finalize_brent(r: &BrentMinimize, out: U256, x_int: U256) -> BrentRun {
    BrentRun {
        x_real: r.x,
        x_int,
        profit: out.saturating_sub(x_int),
        nfev: r.nfev,
    }
}

fn brent_solve_native(hops: &[Hop]) -> BrentRun {
    let mut objective = |x: f64| -> f64 {
        let xi = x.floor();
        let x_u = floor_input(x);
        let out = native_output(hops, x_u);
        -(u256_to_f64(out) - xi)
    };
    let r = minimize_scalar_bounded(&mut objective, (1.0, MAX_INPUT_F64), XATOL, DEFAULT_MAXFUN);
    let x_int = floor_input(r.x);
    finalize_brent(&r, native_output(hops, x_int), x_int)
}

fn brent_solve_sim(sim: &ClPathSim) -> BrentRun {
    let mut objective = |x: f64| -> f64 {
        let xi = x.floor();
        let x_u = floor_input(x);
        let out = sim.output(x_u).final_output;
        -(u256_to_f64(out) - xi)
    };
    let r = minimize_scalar_bounded(&mut objective, (1.0, MAX_INPUT_F64), XATOL, DEFAULT_MAXFUN);
    let x_int = floor_input(r.x);
    finalize_brent(&r, sim.output(x_int).final_output, x_int)
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

struct Timed<T> {
    value: T,
    med_ns: u128,
    p95_ns: u128,
    deterministic: bool,
}

fn summarize(times: &mut [u128]) -> (u128, u128) {
    times.sort_unstable();
    let n = times.len();
    let med = times[n / 2];
    let p95 = times[(n * 95) / 100];
    (med, p95)
}

/// The Möbius timed value: the solver's result plus the last rep's walk stats
/// (returned from the timed solve; no separate stats pass).
struct MobiusRun {
    result: Option<(U256, U256)>,
    stats: WalkStats,
}

fn time_mobius(seqs: &[&IntV3TickRangeSequence], reps: usize) -> Timed<MobiusRun> {
    let cfg = SolveRuntimeConfig::default();
    let mut times = Vec::with_capacity(reps);
    let mut first: Option<Option<(U256, U256)>> = None;
    let mut deterministic = true;
    let mut last: Option<WalkOutcome> = None;
    for _ in 0..reps {
        let t0 = Instant::now();
        let out = derive_and_solve_cl_piecewise(seqs, &cfg);
        times.push(t0.elapsed().as_nanos());
        let sig = out.result.as_ref().map(|(x, p, _)| (*x, *p));
        match first {
            None => first = Some(sig),
            Some(ref f) if *f != sig => deterministic = false,
            Some(_) => {}
        }
        last = Some(out);
    }
    let (med_ns, p95_ns) = summarize(&mut times);
    let last = last.expect("reps >= 1");
    Timed {
        value: MobiusRun {
            result: first.flatten(),
            stats: last.stats,
        },
        med_ns,
        p95_ns,
        deterministic,
    }
}

fn time_brent_native(hops: &[Hop], reps: usize) -> Timed<BrentRun> {
    let mut times = Vec::with_capacity(reps);
    let mut first: Option<BrentRun> = None;
    let mut deterministic = true;
    for _ in 0..reps {
        let t0 = Instant::now();
        let r = brent_solve_native(hops);
        times.push(t0.elapsed().as_nanos());
        match first {
            None => first = Some(r),
            Some(ref f) if *f != r => deterministic = false,
            Some(_) => {}
        }
    }
    let (med_ns, p95_ns) = summarize(&mut times);
    Timed {
        value: first.unwrap(),
        med_ns,
        p95_ns,
        deterministic,
    }
}

fn time_brent_sim(sim: &ClPathSim, reps: usize) -> Timed<BrentRun> {
    let mut times = Vec::with_capacity(reps);
    let mut first: Option<BrentRun> = None;
    let mut deterministic = true;
    for _ in 0..reps {
        let t0 = Instant::now();
        let r = brent_solve_sim(sim);
        times.push(t0.elapsed().as_nanos());
        match first {
            None => first = Some(r),
            Some(ref f) if *f != r => deterministic = false,
            Some(_) => {}
        }
    }
    let (med_ns, p95_ns) = summarize(&mut times);
    Timed {
        value: first.unwrap(),
        med_ns,
        p95_ns,
        deterministic,
    }
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

struct Classification {
    class: &'static str,
    rel_input_delta: f64,
    profit_delta_wei: U256,
    profit_delta_bps: f64,
}

fn classify(
    mobius: Option<(U256, U256)>,
    brent: &BrentRun,
    window: f64,
    cap_hit: bool,
) -> Classification {
    let brent_profitable = !brent.profit.is_zero();
    match (mobius, brent_profitable) {
        (None, false) => Classification {
            class: "both_none",
            rel_input_delta: 0.0,
            profit_delta_wei: U256::ZERO,
            profit_delta_bps: 0.0,
        },
        (None, true) => Classification {
            class: "mobius_none_brent_some",
            rel_input_delta: 0.0,
            profit_delta_wei: brent.profit,
            profit_delta_bps: 10_000.0,
        },
        (Some(_), false) => Classification {
            class: "brent_none_mobius_some",
            rel_input_delta: 0.0,
            profit_delta_wei: mobius.map_or(U256::ZERO, |(_, p)| p),
            profit_delta_bps: 10_000.0,
        },
        (Some((x_m, p_m)), true) => {
            let (lo, hi) = if x_m <= brent.x_int {
                (x_m, brent.x_int)
            } else {
                (brent.x_int, x_m)
            };
            let denom = if hi.is_zero() { U256::ONE } else { hi };
            let rel = u256_to_f64(lo.abs_diff(hi)) / u256_to_f64(denom);
            let profit_delta_wei = p_m.abs_diff(brent.profit);
            let scale = p_m.max(brent.profit);
            let profit_delta_bps = if scale.is_zero() {
                0.0
            } else {
                10_000.0 * u256_to_f64(profit_delta_wei) / u256_to_f64(scale)
            };
            let class = if rel <= window {
                "agree"
            } else if cap_hit {
                "disagree_cap_hit"
            } else {
                "disagree_input"
            };
            Classification {
                class,
                rel_input_delta: rel,
                profit_delta_wei,
                profit_delta_bps,
            }
        }
    }
}

/// True when the path optimum `x` sits within 4 wei (inclusive) of a crossing
/// boundary in the landed tuple — the discrete kink the active-set walk is
/// expected to land on. Only the landed range's own (lower) and next (upper)
/// boundaries are considered.
fn is_corner(hops: &[Hop], sim: &ClPathSim, x: U256) -> bool {
    let outcome = sim.output(x);
    let tol = U256::from(4u64);
    for (i, hop) in hops.iter().enumerate() {
        let k = outcome.landed[i];
        let crossings = build_cl_crossing_table(&hop.seq);
        if let Some(next) = crossings.get(k + 1) {
            if x.abs_diff(next.crossing_gross_input) <= tol {
                return true;
            }
        }
        if k > 0 && x.abs_diff(crossings[k].crossing_gross_input) <= tol {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Summary aggregation
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct FamilyAgg {
    paths: u64,
    both_none: u64,
    agree: u64,
    disagree_input: u64,
    disagree_cap_hit: u64,
    mobius_none_brent_some: u64,
    brent_none_mobius_some: u64,
    cap_hits: u64,
    corner: u64,
    mobius_profitable: u64,
    brent_profitable: u64,
    nondeterministic: u64,
    delta_bps: Vec<f64>,
    mobius_med_ns: Vec<u128>,
    brent_med_ns: Vec<u128>,
    sim_med_ns: Vec<u128>,
}

impl FamilyAgg {
    fn add(&mut self, c: &Classification, cap_hit: bool, corner: bool, det: bool) {
        self.paths += 1;
        match c.class {
            "both_none" => self.both_none += 1,
            "agree" => self.agree += 1,
            "disagree_input" => {
                self.disagree_input += 1;
                self.delta_bps.push(c.profit_delta_bps);
            }
            "disagree_cap_hit" => {
                self.disagree_cap_hit += 1;
                self.delta_bps.push(c.profit_delta_bps);
            }
            "mobius_none_brent_some" => self.mobius_none_brent_some += 1,
            _ => self.brent_none_mobius_some += 1,
        }
        if cap_hit {
            self.cap_hits += 1;
        }
        if corner {
            self.corner += 1;
        }
        if !det {
            self.nondeterministic += 1;
        }
    }

    fn json(&self, label: &str) -> Value {
        let agree_total = self.both_none + self.agree;
        let pct = if self.paths > 0 {
            100.0 * agree_total as f64 / self.paths as f64
        } else {
            0.0
        };
        json!({
            "family": label,
            "paths": self.paths,
            "agree": self.agree,
            "both_none": self.both_none,
            "disagree_input": self.disagree_input,
            "disagree_cap_hit": self.disagree_cap_hit,
            "mobius_none_brent_some": self.mobius_none_brent_some,
            "brent_none_mobius_some": self.brent_none_mobius_some,
            "cap_hits": self.cap_hits,
            "corner": self.corner,
            "mobius_profitable": self.mobius_profitable,
            "brent_profitable": self.brent_profitable,
            "nondeterministic": self.nondeterministic,
            "agreement_pct": pct,
            "delta_bps": dist(&self.delta_bps),
            "mobius_med_ns": dist_u128(&self.mobius_med_ns),
            "brent_med_ns": dist_u128(&self.brent_med_ns),
            "brent_sim_med_ns": dist_u128(&self.sim_med_ns),
        })
    }
}

fn dist(v: &[f64]) -> Value {
    if v.is_empty() {
        return json!({ "n": 0 });
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    json!({
        "n": n,
        "min": s[0],
        "p50": s[n / 2],
        "p95": s[(n * 95) / 100],
        "max": s[n - 1],
    })
}

fn dist_u128(v: &[u128]) -> Value {
    if v.is_empty() {
        return json!({ "n": 0 });
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    let n = s.len();
    json!({
        "n": n,
        "min_us": s[0] as f64 / 1000.0,
        "p50_us": s[n / 2] as f64 / 1000.0,
        "p95_us": s[(n * 95) / 100] as f64 / 1000.0,
        "max_us": s[n - 1] as f64 / 1000.0,
    })
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
        .max(1)
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let scale = env_usize("DR_AB_SCALE", 1);
    let reps = env_usize("DR_AB_REPS", DEFAULT_REPS);
    let window = env_f64("DR_AB_WINDOW", DEFAULT_WINDOW);
    let out_path = std::env::var("DR_AB_OUT").map_or_else(
        |_| PathBuf::from("target/cl_mobius_vs_brent_ab.jsonl"),
        PathBuf::from,
    );
    let cross_path = {
        let stem = out_path.file_stem().map_or_else(
            || "cl_mobius_vs_brent_ab".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        out_path.with_file_name(format!("{stem}_cross_oracle.jsonl"))
    };

    // Mirrors `MAX_INPUT_F64` as an integer; the cap-hit guard must
    // classify against the same bound the minimizer was given.
    let max_input_wei = U256::from(20_000_000_000_000_000_000_000u128);

    // Corpus generation is deliberately OUTSIDE every timed region.
    let t_corpus = Instant::now();
    let corpus = build_corpus(scale);
    let corpus_gen_ms = t_corpus.elapsed().as_millis();

    let mut main_lines: Vec<String> = Vec::with_capacity(corpus.len());
    let mut cross_lines: Vec<String> = Vec::with_capacity(corpus.len());
    let mut by_family: HashMap<&'static str, FamilyAgg> = HashMap::new();
    let mut overall = FamilyAgg::default();
    let mut all_delta_bps: Vec<f64> = Vec::new();

    for path in &corpus {
        let refs: Vec<&IntV3TickRangeSequence> = path.hops.iter().map(|h| &h.seq).collect();
        let sim = ClPathSim::new(&refs, None);

        let mobius = time_mobius(&refs, reps);
        let brent = time_brent_native(&path.hops, reps);
        let brent_sim = time_brent_sim(&sim, reps);

        let cap_hit = mobius.value.result.is_some_and(|(x, _)| x >= max_input_wei)
            || brent.value.x_int >= max_input_wei;
        let classification = classify(mobius.value.result, &brent.value, window, cap_hit);
        let corner = mobius
            .value
            .result
            .is_some_and(|(x, _)| is_corner(&path.hops, &sim, x));
        let deterministic = mobius.deterministic && brent.deterministic && brent_sim.deterministic;
        let leading_drain = path
            .hops
            .iter()
            .zip(path.on_tick.iter())
            .any(|(h, &on)| h.zfo && on);
        let stats = mobius.value.stats;

        let row = json!({
            "path_id": path.id,
            "family": path.family,
            "profile": path.profile,
            "spacings": path.spacings,
            "fees": path.fees,
            "dirs": path.dirs.iter().map(|&z| if z { "zfo" } else { "ofz" }).collect::<Vec<_>>(),
            "ranges": path.ranges,
            "anchors": path.anchors,
            "on_tick": path.on_tick,
            "mobius": {
                "x": mobius.value.result.map(|(x, _)| x.to_string()),
                "profit": mobius.value.result.map(|(_, p)| p.to_string()),
                "pieces": stats.pieces,
                "sims": stats.sims,
                "word_steps": stats.word_steps,
                "refine_sims": stats.refine_sims,
                "left_edge_sims": stats.left_edge_sims,
                "right_edge_sims": stats.right_edge_sims,
                "anchor_sims": stats.anchor_sims,
                "max_dense_words": stats.max_dense_words,
            },
            "brent": {
                "x": brent.value.x_int.to_string(),
                "x_real": brent.value.x_real,
                "profit": brent.value.profit.to_string(),
                "nfev": brent.value.nfev,
            },
            "brent_sim": {
                "x": brent_sim.value.x_int.to_string(),
                "x_real": brent_sim.value.x_real,
                "profit": brent_sim.value.profit.to_string(),
                "nfev": brent_sim.value.nfev,
            },
            "class": classification.class,
            "rel_input_delta": classification.rel_input_delta,
            "profit_delta_wei": classification.profit_delta_wei.to_string(),
            "profit_delta_bps": classification.profit_delta_bps,
            "flags": {
                "cap_hit": cap_hit,
                "corner": corner,
                "both_none": classification.class == "both_none",
                "walk_heavy": stats.pieces >= 8,
                "dense": stats.max_dense_words >= DENSE_OBSERVE_THRESHOLD,
                "leading_drain": leading_drain,
            },
            "timing": {
                "reps": reps,
                "mobius_med_ns": mobius.med_ns,
                "mobius_p95_ns": mobius.p95_ns,
                "brent_med_ns": brent.med_ns,
                "brent_p95_ns": brent.p95_ns,
                "brent_sim_med_ns": brent_sim.med_ns,
                "brent_sim_p95_ns": brent_sim.p95_ns,
                "deterministic": deterministic,
            },
        });
        main_lines.push(serde_json::to_string(&row).unwrap());

        let cross = json!({
            "path_id": path.id,
            "family": path.family,
            "brent_sim": {
                "x": brent_sim.value.x_int.to_string(),
                "profit": brent_sim.value.profit.to_string(),
                "nfev": brent_sim.value.nfev,
            },
            "mobius_x": mobius.value.result.map(|(x, _)| x.to_string()),
            "mobius_profit": mobius.value.result.map(|(_, p)| p.to_string()),
            "agrees_with_mobius": match (mobius.value.result, brent_sim.value.profit.is_zero()) {
                (None, true) => true,
                (Some((x, p)), false) => {
                    let lo = x.min(brent_sim.value.x_int);
                    let hi = x.max(brent_sim.value.x_int);
                    let rel = if hi.is_zero() { 1.0 } else { u256_to_f64(lo.abs_diff(hi)) / u256_to_f64(hi) };
                    rel <= window && p == brent_sim.value.profit
                }
                _ => false,
            },
            "timing": {
                "reps": reps,
                "brent_sim_med_ns": brent_sim.med_ns,
                "brent_sim_p95_ns": brent_sim.p95_ns,
            },
        });
        cross_lines.push(serde_json::to_string(&cross).unwrap());

        let mobius_profitable = mobius.value.result.is_some();
        let brent_profitable = !brent.value.profit.is_zero();
        let add_agg = |agg: &mut FamilyAgg| {
            agg.add(&classification, cap_hit, corner, deterministic);
            agg.mobius_med_ns.push(mobius.med_ns);
            agg.brent_med_ns.push(brent.med_ns);
            agg.sim_med_ns.push(brent_sim.med_ns);
            agg.mobius_profitable += u64::from(mobius_profitable);
            agg.brent_profitable += u64::from(brent_profitable);
        };
        add_agg(by_family.entry(path.family).or_default());
        add_agg(&mut overall);
        if classification.class == "agree" {
            all_delta_bps.push(classification.profit_delta_bps);
        }
    }

    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let main_text = format!("{}\n", main_lines.join("\n"));
    let cross_text = format!("{}\n", cross_lines.join("\n"));
    std::fs::write(&out_path, main_text).expect("write main JSONL");
    std::fs::write(&cross_path, cross_text).expect("write cross JSONL");

    let families: Vec<Value> = FAMILIES
        .iter()
        .map(|f| {
            by_family
                .get(f)
                .map_or_else(|| json!({ "family": f, "paths": 0 }), |a| a.json(f))
        })
        .collect();
    let nondet_total = overall.nondeterministic;
    let summary = json!({
        "corpus": {
            "families": FAMILIES.len(),
            "per_family": PER_FAMILY * scale,
            "scale": scale,
            "total": corpus.len(),
            "generation_ms": corpus_gen_ms,
        },
        "window": window,
        "reps": reps,
        "max_input_wei": max_input_wei.to_string(),
        "xatol": XATOL,
        "families": families,
        "overall": overall.json("overall"),
        "delta_bps_agree": dist(&all_delta_bps),
        "nondeterministic_paths": nondet_total,
        "out": out_path.to_string_lossy(),
        "cross_out": cross_path.to_string_lossy(),
    });
    println!("SUMMARY {}", serde_json::to_string(&summary).unwrap());

    if nondet_total > 0 {
        eprintln!(
            "WARNING: {nondet_total} path(s) produced per-rep nondeterministic solver outputs"
        );
    }
}

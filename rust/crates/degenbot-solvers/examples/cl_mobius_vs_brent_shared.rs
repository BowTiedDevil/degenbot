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

//! Report-only shared-pool A/B: does the Möbius solver's per-(pool,direction)
//! table caching amortize across paths so a WARM solve competes with the
//! ALWAYS-COLD Brent minimizer on realistic pool reuse?
//!
//! This is the shared-pool companion to `cl_mobius_vs_brent_ab.rs`. Instead of
//! one fresh synthetic pool per hop (`DR_SHARED_POOLS` default 24 pools,
//! `DR_SHARED_PATHS` default 300 paths of 3 distinct pools each), pools are
//! reused across paths. The cache key is `(pool, direction)`; the reuse
//! histogram reports how many paths touch each entry and what fraction of the
//! cold table-build work the warm cache saves.
//!
//! Four timed conditions per path, all over the same physical path:
//!
//! - `warmup` (measured, untimed region): `ClSolveTables::derive` once per
//!   `(pool, direction)` entry that is actually reused.
//! - `mobius_warm`: `solve_cl_piecewise` fed the cached `Arc` tables (O(1)
//!   clone, no re-derivation in the timed region).
//! - `mobius_cold`: `derive_and_solve_cl_piecewise` re-derives the tables
//!   inside the timed solve (context).
//! - `brent_native` / `brent_helper`: always-cold bounded-Brent against the
//!   native pool-sim objective and the byte-exact `ClPathSim` helper oracle.
//!
//! `mobius_warm` and `mobius_cold` are the same math with different cache
//! residency; their results are asserted byte-identical for every path.
//!
//! Everything is report-only: no verdict, no exit code, no CI gate. Output is
//! a per-path JSONL (`<out>`) plus a stdout summary. All timing lives under a
//! `timing` key so two runs are byte-identical once it is stripped.
//!
//! Environment knobs: `DR_SHARED_POOLS` (pool count, default 24),
//! `DR_SHARED_PATHS` (path count, default 300), `DR_SHARED_REPS` (timed
//! repetitions, default 25), `DR_SHARED_WINDOW` (relative input agreement
//! window, default 0.001), `DR_SHARED_OUT` (JSONL path, default
//! `target/cl_mobius_vs_brent_shared.jsonl`).

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
    build_cl_crossing_table, derive_and_solve_cl_piecewise, solve_cl_piecewise, ClPathSim,
    ClSolveTables, IntV3TickRangeSequence, WalkOutcome, WalkStats, DENSE_OBSERVE_THRESHOLD,
};
use degenbot_solvers::runtime::SolveRuntimeConfig;
use hashbrown::HashMap;
use serde_json::{json, Value};

/// Corpus RNG seed (XORSHIFT64*). Fixed so runs are reproducible.
const SEED: u64 = 0x5DEE_CE66_D1CE_5EED;

/// Upper search bound (wei) of the bounded-Brent objective. The archived
/// Python shape's `MAX_INPUT = 100e18` truncated optima past the first-hop
/// saturation edge (stress_w books absorb 3000-8000 tokens, so an edge can
/// sit near 8e21) and manufactured `disagree_cap_hit` rows. The bound must
/// clear the largest in-corpus optimum with headroom; Brent pays for that
/// with more nfev narrowing the wider f64 bracket.
const MAX_INPUT_F64: f64 = 20_000e18;

/// Absolute input tolerance passed to the bounded minimizer.
const XATOL: f64 = 1.0;

/// Timed repetitions per (path, solver).
const DEFAULT_REPS: usize = 25;
/// Pool count at default scale.
const DEFAULT_POOLS: usize = 24;
/// Path count at default scale.
const DEFAULT_PATHS: usize = 300;
/// Hops per path.
const HOPS: usize = 3;

const DEFAULT_WINDOW: f64 = 0.001;

/// The five per-pool liquidity profiles, cycled so every profile is present.
const PROFILES: [&str; 5] = [
    "uniform",
    "decaying",
    "growing",
    "random",
    "single-range-deep",
];

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
}

// ---------------------------------------------------------------------------
// Synthetic pool construction (local copies of the cold example's builders)
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

/// `(lower, upper)` initialized-tick boundaries of range `i` on the `zfo`
/// (below-anchor) or `ofz` (above-anchor) side.
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

/// Per-range liquidities for one side of a shared pool, scaled by the pool's
/// profile. `depth` is sampled once per pool so the profile shape is stable
/// across both sides.
fn pool_liquidities(
    rng: &mut Rng,
    profile: &str,
    anchor: i32,
    spacing: i32,
    zfo: bool,
    count: usize,
    depth: f64,
) -> Vec<u128> {
    (0..count)
        .map(|i| {
            let i_i32 = i32::try_from(i).unwrap();
            let factor = match profile {
                "decaying" => 0.62_f64.powi(i_i32),
                "growing" => 1.55_f64.powi(i_i32),
                "random" => 0.35 + rng.range_f64(0.0, 1.9),
                // One deep range nearest the tick, thin shelves beyond it.
                "single-range-deep" => {
                    if i == 0 {
                        8.0
                    } else {
                        0.06
                    }
                }
                _ => 1.0,
            };
            let (s, s_next) = range_sqrt(anchor, spacing, zfo, i_i32);
            liquidity_for_depth(depth * factor, s, s_next, zfo)
        })
        .collect()
}

/// Add one range's liquidity to the pool's tick data (matching EVM tick
/// gross/net bookkeeping).
fn add_range(
    tick_data: &mut HashMap<i32, TickInfo>,
    anchor: i32,
    spacing: i32,
    zfo: bool,
    i: i32,
    liq: u128,
) {
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

/// One reusable synthetic V3 pool. Both swap-direction sequences are built
/// from the same dual-sided book so a pool can be referenced `zfo` or `ofz`.
struct SharedPool {
    profile: &'static str,
    spacing: i32,
    fee: u32,
    ranges: usize,
    state: V3PoolState,
    seq_zfo: IntV3TickRangeSequence,
    seq_ofz: IntV3TickRangeSequence,
}

impl SharedPool {
    /// Cache slot index within `pool*2 .. pool*2+2`.
    fn dir_index(zfo: bool) -> usize {
        usize::from(!zfo)
    }

    fn seq(&self, zfo: bool) -> &IntV3TickRangeSequence {
        if zfo {
            &self.seq_zfo
        } else {
            &self.seq_ofz
        }
    }
}

fn make_shared_pool(profile: &'static str, rng: &mut Rng) -> SharedPool {
    let spacing = [200, 1000, 2000, 5000][rng.range_usize(0, 3)];
    let fee = [100, 500, 3000, 10000][rng.range_usize(0, 3)] as u32;
    let ranges = rng.range_usize(1, 8);
    let anchor = snap(rng.range_i32(-1500, 1500), spacing);
    let depth = rng.range_f64(5.0, 195.0);

    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    let low = pool_liquidities(rng, profile, anchor, spacing, true, ranges, depth);
    let high = pool_liquidities(rng, profile, anchor, spacing, false, ranges, depth);
    for (i, &liq) in low.iter().enumerate() {
        add_range(
            &mut tick_data,
            anchor,
            spacing,
            true,
            i32::try_from(i).unwrap(),
            liq,
        );
    }
    for (i, &liq) in high.iter().enumerate() {
        add_range(
            &mut tick_data,
            anchor,
            spacing,
            false,
            i32::try_from(i).unwrap(),
            liq,
        );
    }

    // Current tick sits exactly on the anchor boundary; the active range is
    // the nearest above-anchor shelf (`[anchor, anchor+spacing)`).
    let active: u128 = high.first().copied().unwrap_or(0);

    let params = RegisterV3PoolParams {
        fee,
        tick_spacing: spacing,
        sqrt_price_x96: sqrt_at(anchor),
        liquidity: active,
        tick: anchor,
        tick_data,
        coverage: PoolTickCoverage::Tracked,
        ..Default::default()
    };
    let (_identity, state) = V3PoolState::from_params(params, 8);
    let seq_zfo = state
        .build_int_v3_sequence(spacing, fee, true)
        .expect("pool yields a zfo swap-direction sequence");
    let seq_ofz = state
        .build_int_v3_sequence(spacing, fee, false)
        .expect("pool yields an ofz swap-direction sequence");
    SharedPool {
        profile,
        spacing,
        fee,
        ranges,
        state,
        seq_zfo,
        seq_ofz,
    }
}

fn build_pools(count: usize) -> Vec<SharedPool> {
    let mut rng = Rng::new(SEED);
    (0..count)
        .map(|id| make_shared_pool(PROFILES[id % PROFILES.len()], &mut rng))
        .collect()
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct HopRef {
    pool: usize,
    zfo: bool,
}

struct SharedPath {
    id: usize,
    family: &'static str,
    hops: [HopRef; HOPS],
}

/// Sample `HOPS` distinct pool indices and a random direction per hop.
fn build_paths(pools: &[SharedPool], count: usize) -> Vec<SharedPath> {
    let mut rng = Rng::new(SEED ^ 0xA5A5_5A5A_1234_9ABC);
    let mut out = Vec::with_capacity(count);
    for id in 0..count {
        let mut chosen: Vec<usize> = Vec::with_capacity(HOPS);
        while chosen.len() < HOPS {
            let idx = rng.range_usize(0, pools.len() - 1);
            if !chosen.contains(&idx) {
                chosen.push(idx);
            }
        }
        let mut hops = [HopRef { pool: 0, zfo: true }; HOPS];
        for (slot, &pool) in chosen.iter().enumerate() {
            hops[slot] = HopRef {
                pool,
                zfo: rng.range_f64(0.0, 1.0) < 0.5,
            };
        }
        let family = pools[hops[0].pool].profile;
        out.push(SharedPath { id, family, hops });
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
fn native_output(hops: &[&SharedPool], dirs: &[bool], amount_in: U256) -> U256 {
    let mut current = amount_in;
    for (hop, &zfo) in hops.iter().zip(dirs) {
        if current.is_zero() {
            return U256::ZERO;
        }
        let Ok(amount) = I256::try_from(current) else {
            return U256::ZERO;
        };
        let limit = V3PoolState::default_sqrt_price_limit(zfo);
        match v3_simulate_swap(&hop.state, hop.fee, hop.spacing, zfo, amount, limit) {
            Ok(outcome) => current = hop_output_amount(&outcome, zfo),
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

fn brent_solve_native(hops: &[&SharedPool], dirs: &[bool]) -> BrentRun {
    let mut objective = |x: f64| -> f64 {
        let xi = x.floor();
        let x_u = floor_input(x);
        let out = native_output(hops, dirs, x_u);
        -(u256_to_f64(out) - xi)
    };
    let r = minimize_scalar_bounded(&mut objective, (1.0, MAX_INPUT_F64), XATOL, DEFAULT_MAXFUN);
    let x_int = floor_input(r.x);
    finalize_brent(&r, native_output(hops, dirs, x_int), x_int)
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

/// The Möbius timed value: the solver's result plus the last rep's walk stats.
struct MobiusRun {
    result: Option<(U256, U256)>,
    stats: WalkStats,
}

fn time_mobius_warm(
    seqs: &[&IntV3TickRangeSequence],
    prepared: &[ClSolveTables],
    reps: usize,
) -> Timed<MobiusRun> {
    let cfg = SolveRuntimeConfig::default();
    let mut times = Vec::with_capacity(reps);
    let mut first: Option<Option<(U256, U256)>> = None;
    let mut deterministic = true;
    let mut last: Option<WalkOutcome> = None;
    for _ in 0..reps {
        let t0 = Instant::now();
        let out = solve_cl_piecewise(seqs, prepared, None, &cfg, None);
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

fn time_mobius_cold(seqs: &[&IntV3TickRangeSequence], reps: usize) -> Timed<MobiusRun> {
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

fn time_brent_native(hops: &[&SharedPool], dirs: &[bool], reps: usize) -> Timed<BrentRun> {
    let mut times = Vec::with_capacity(reps);
    let mut first: Option<BrentRun> = None;
    let mut deterministic = true;
    for _ in 0..reps {
        let t0 = Instant::now();
        let r = brent_solve_native(hops, dirs);
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
/// boundary in the landed tuple.
fn is_corner(hops: &[&SharedPool], dirs: &[bool], sim: &ClPathSim, x: U256) -> bool {
    let outcome = sim.output(x);
    let tol = U256::from(4u64);
    for (i, hop) in hops.iter().enumerate() {
        let k = outcome.landed[i];
        let crossings = build_cl_crossing_table(hop.seq(dirs[i]));
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
// Aggregation helpers
// ---------------------------------------------------------------------------

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

fn median_u128(v: &[u128]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    s[s.len() / 2] as f64
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

struct FamilyAgg {
    paths: u64,
    cold_med: Vec<u128>,
    warm_med: Vec<u128>,
    brent_med: Vec<u128>,
}

fn main() {
    let pool_count = env_usize("DR_SHARED_POOLS", DEFAULT_POOLS);
    let path_count = env_usize("DR_SHARED_PATHS", DEFAULT_PATHS);
    let reps = env_usize("DR_SHARED_REPS", DEFAULT_REPS);
    let window = env_f64("DR_SHARED_WINDOW", DEFAULT_WINDOW);
    let out_path = std::env::var("DR_SHARED_OUT").map_or_else(
        |_| PathBuf::from("target/cl_mobius_vs_brent_shared.jsonl"),
        PathBuf::from,
    );

    // Mirrors `MAX_INPUT_F64` as an integer; the cap-hit guard must
    // classify against the same bound the minimizer was given.
    let max_input_wei = U256::from(20_000_000_000_000_000_000_000u128);

    // Corpus generation is deliberately OUTSIDE every timed region.
    let t_corpus = Instant::now();
    let pools = build_pools(pool_count);
    let paths = build_paths(&pools, path_count);
    let corpus_gen_ms = t_corpus.elapsed().as_millis();

    // Per-(pool, direction) reuse histogram from the generated corpus.
    let mut reuse: HashMap<(usize, bool), u64> = HashMap::new();
    for path in &paths {
        for hop in &path.hops {
            *reuse.entry((hop.pool, hop.zfo)).or_insert(0) += 1;
        }
    }
    let total_hop_occurrences: u64 = (path_count * HOPS) as u64;
    let unique_entries = reuse.len() as u64;
    let mut reuse_counts: Vec<u64> = reuse.values().copied().collect();
    reuse_counts.sort_unstable();
    let reuse_min = reuse_counts.first().copied().unwrap_or(0);
    let reuse_med = reuse_counts[reuse_counts.len() / 2];
    let reuse_max = reuse_counts.last().copied().unwrap_or(0);
    let build_saved_pct = if total_hop_occurrences > 0 {
        100.0 * (total_hop_occurrences - unique_entries) as f64 / total_hop_occurrences as f64
    } else {
        0.0
    };

    // Warmup: derive the cached tables exactly once per REUSED entry and time
    // each derivation. Untimed as far as the per-path conditions go, but
    // measured for the amortization report.
    let mut warm_cache: Vec<Option<ClSolveTables>> = (0..pool_count * 2).map(|_| None).collect();
    let mut warmup_times_ns: Vec<u128> = Vec::with_capacity(reuse.len());
    let t_warmup = Instant::now();
    for &(pool, zfo) in reuse.keys() {
        let seq = pools[pool].seq(zfo);
        let t0 = Instant::now();
        let tables = ClSolveTables::derive(seq);
        warmup_times_ns.push(t0.elapsed().as_nanos());
        warm_cache[pool * 2 + SharedPool::dir_index(zfo)] = Some(tables);
    }
    let warmup_total_ns = t_warmup.elapsed().as_nanos();
    let warmup_total_ms = warmup_total_ns as f64 / 1_000_000.0;
    let warmup_per_entry_ns =
        warmup_times_ns.iter().sum::<u128>() as f64 / warmup_times_ns.len() as f64;
    let warmup_entry_median_ns = median_u128(&warmup_times_ns);

    let mut main_lines: Vec<String> = Vec::with_capacity(paths.len());
    let mut overall_cold: Vec<u128> = Vec::new();
    let mut overall_warm: Vec<u128> = Vec::new();
    let mut overall_brent: Vec<u128> = Vec::new();
    let mut overall_brent_sim: Vec<u128> = Vec::new();
    let mut by_family: HashMap<&'static str, FamilyAgg> = HashMap::new();
    let mut class_counts: HashMap<&'static str, u64> = HashMap::new();
    let mut nondet_total = 0u64;

    for path in &paths {
        let hop_pools: Vec<&SharedPool> = path.hops.iter().map(|h| &pools[h.pool]).collect();
        let dirs: Vec<bool> = path.hops.iter().map(|h| h.zfo).collect();
        let seqs: Vec<&IntV3TickRangeSequence> =
            path.hops.iter().map(|h| pools[h.pool].seq(h.zfo)).collect();
        // O(1) Arc-table clone out of the warm cache (no re-derivation).
        let prepared: Vec<ClSolveTables> = path
            .hops
            .iter()
            .map(|h| {
                warm_cache[h.pool * 2 + SharedPool::dir_index(h.zfo)]
                    .clone()
                    .expect("reused entry was warmed")
            })
            .collect();
        let sim = ClPathSim::new(&seqs, None);

        let warm = time_mobius_warm(&seqs, &prepared, reps);
        let cold = time_mobius_cold(&seqs, reps);
        let brent = time_brent_native(&hop_pools, &dirs, reps);
        let brent_sim = time_brent_sim(&sim, reps);

        // Correctness invariant: cache residency must not change the answer.
        assert_eq!(
            warm.value.result, cold.value.result,
            "warm/cold result mismatch on path {}",
            path.id
        );

        let cap_hit = warm.value.result.is_some_and(|(x, _)| x >= max_input_wei)
            || brent.value.x_int >= max_input_wei;
        let classification = classify(warm.value.result, &brent.value, window, cap_hit);
        let corner = warm
            .value
            .result
            .is_some_and(|(x, _)| is_corner(&hop_pools, &dirs, &sim, x));
        let deterministic = warm.deterministic
            && cold.deterministic
            && brent.deterministic
            && brent_sim.deterministic;
        if !deterministic {
            nondet_total += 1;
        }
        let stats = &warm.value.stats;
        let entry_reuse: Vec<u64> = path.hops.iter().map(|h| reuse[&(h.pool, h.zfo)]).collect();

        let row = json!({
            "path_id": path.id,
            "family": path.family,
            "pools": path.hops.iter().map(|h| h.pool).collect::<Vec<_>>(),
            "pool_profiles": path.hops.iter().map(|h| pools[h.pool].profile).collect::<Vec<_>>(),
            "spacings": path.hops.iter().map(|h| pools[h.pool].spacing).collect::<Vec<_>>(),
            "fees": path.hops.iter().map(|h| pools[h.pool].fee).collect::<Vec<_>>(),
            "ranges": path.hops.iter().map(|h| pools[h.pool].ranges).collect::<Vec<_>>(),
            "dirs": dirs.iter().map(|&z| if z { "zfo" } else { "ofz" }).collect::<Vec<_>>(),
            "entry_reuse": entry_reuse,
            "mobius_warm": {
                "x": warm.value.result.map(|(x, _)| x.to_string()),
                "profit": warm.value.result.map(|(_, p)| p.to_string()),
                "pieces": stats.pieces,
                "sims": stats.sims,
                "word_steps": stats.word_steps,
            },
            "mobius_cold": {
                "x": cold.value.result.map(|(x, _)| x.to_string()),
                "profit": cold.value.result.map(|(_, p)| p.to_string()),
            },
            "brent": {
                "x": brent.value.x_int.to_string(),
                "x_real": brent.value.x_real,
                "profit": brent.value.profit.to_string(),
                "nfev": brent.value.nfev,
            },
            "brent_helper": {
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
                "dense": stats.max_dense_words >= DENSE_OBSERVE_THRESHOLD,
            },
            "timing": {
                "reps": reps,
                "mobius_warm_med_ns": warm.med_ns,
                "mobius_warm_p95_ns": warm.p95_ns,
                "mobius_cold_med_ns": cold.med_ns,
                "mobius_cold_p95_ns": cold.p95_ns,
                "brent_med_ns": brent.med_ns,
                "brent_p95_ns": brent.p95_ns,
                "brent_helper_med_ns": brent_sim.med_ns,
                "brent_helper_p95_ns": brent_sim.p95_ns,
                "deterministic": deterministic,
            },
        });
        main_lines.push(serde_json::to_string(&row).unwrap());

        *class_counts.entry(classification.class).or_insert(0) += 1;
        overall_warm.push(warm.med_ns);
        overall_cold.push(cold.med_ns);
        overall_brent.push(brent.med_ns);
        overall_brent_sim.push(brent_sim.med_ns);
        let agg = by_family.entry(path.family).or_insert_with(|| FamilyAgg {
            paths: 0,
            cold_med: Vec::new(),
            warm_med: Vec::new(),
            brent_med: Vec::new(),
        });
        agg.paths += 1;
        agg.cold_med.push(cold.med_ns);
        agg.warm_med.push(warm.med_ns);
        agg.brent_med.push(brent.med_ns);
    }

    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let main_text = format!("{}\n", main_lines.join("\n"));
    std::fs::write(&out_path, main_text).expect("write JSONL");

    let sum_warm_us: f64 = overall_warm.iter().map(|&n| n as f64 / 1000.0).sum();
    let sum_cold_us: f64 = overall_cold.iter().map(|&n| n as f64 / 1000.0).sum();
    let sum_brent_us: f64 = overall_brent.iter().map(|&n| n as f64 / 1000.0).sum();
    let warm_amortized_total_us = warmup_total_ms * 1000.0 + sum_warm_us;

    let families: Vec<Value> = PROFILES
        .iter()
        .filter_map(|f| by_family.get(f).map(|a| (f, a)))
        .map(|(f, a)| {
            let cold_med_us = median_u128(&a.cold_med) / 1000.0;
            let warm_med_us = median_u128(&a.warm_med) / 1000.0;
            let diff = cold_med_us - warm_med_us;
            let per_entry_us = warmup_per_entry_ns / 1000.0;
            let break_even = if diff > 0.0 {
                Some(per_entry_us / diff)
            } else {
                None
            };
            json!({
                "family": f,
                "paths": a.paths,
                "cold_med": dist_u128(&a.cold_med),
                "warm_med": dist_u128(&a.warm_med),
                "brent_med": dist_u128(&a.brent_med),
                "cold_med_us": cold_med_us,
                "warm_med_us": warm_med_us,
                "per_entry_warmup_us": per_entry_us,
                "break_even_paths_per_entry": break_even,
            })
        })
        .collect();

    let summary = json!({
        "corpus": {
            "pools": pool_count,
            "paths": path_count,
            "hops": HOPS,
            "profiles": PROFILES,
            "generation_ms": corpus_gen_ms,
        },
        "window": window,
        "reps": reps,
        "max_input_wei": max_input_wei.to_string(),
        "xatol": XATOL,
        "warmup": {
            "used_entries": unique_entries,
            "total_ns": warmup_total_ns,
            "total_ms": warmup_total_ms,
            "per_entry_median_us": warmup_entry_median_ns / 1000.0,
            "per_entry_mean_us": warmup_per_entry_ns / 1000.0,
            "entry_times": dist_u128(&warmup_times_ns),
        },
        "reuse_histogram": {
            "total_hop_occurrences": total_hop_occurrences,
            "unique_entries": unique_entries,
            "entries_possible": pool_count * 2,
            "min": reuse_min,
            "median": reuse_med,
            "max": reuse_max,
            "build_work_saved_pct": build_saved_pct,
        },
        "overall": {
            "mobius_warm_med": dist_u128(&overall_warm),
            "mobius_cold_med": dist_u128(&overall_cold),
            "brent_med": dist_u128(&overall_brent),
            "brent_helper_med": dist_u128(&overall_brent_sim),
        },
        "amortized_totals": {
            "mobius_warm_amortized_total_us": warm_amortized_total_us,
            "mobius_warm_paths_total_us": sum_warm_us,
            "warmup_total_us": warmup_total_ms * 1000.0,
            "mobius_cold_total_us": sum_cold_us,
            "brent_total_us": sum_brent_us,
        },
        "families": families,
        "classes": class_counts,
        "nondeterministic_paths": nondet_total,
        "out": out_path.to_string_lossy(),
    });
    println!("SUMMARY {}", serde_json::to_string(&summary).unwrap());

    if nondet_total > 0 {
        eprintln!(
            "WARNING: {nondet_total} path(s) produced per-rep nondeterministic solver outputs"
        );
    }
}

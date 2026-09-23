//! Refine-window under-sampling regression on wide/deep books.
//!
//! The A/B corpora (`examples/cl_mobius_vs_brent_ab.rs`,
//! `examples/cl_mobius_vs_brent_shared.rs`) surfaced paths where the
//! bounded-Brent helper's profit strictly exceeds the active-set walk's by
//! more than one wei. This test reconstructs the corpus generator locally
//! (the examples are separate compilation units) and pins the walk against
//! the same `ClPathSim` + bounded-Brent oracle for the stress families whose
//! deep books exposed the shortfall.
//!
//! The generator is a deliberate copy of the cold example's topology: the
//! XORSHIFT stream must be consumed identically for path ids to reconstruct.

#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::print_stdout,
    clippy::unreadable_literal
)]

use alloy::primitives::{U128, U256, U512};
use degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_pools::v3_state::{PoolTickCoverage, RegisterV3PoolParams, V3PoolState};
use degenbot_pools::TickInfo;
use hashbrown::HashMap;

use crate::bounded_brent::{minimize_scalar_bounded, DEFAULT_MAXFUN};
use crate::cl::{ClPathSim, IntV3TickRangeSequence};
use crate::runtime::SolveRuntimeConfig;

const SEED: u64 = 0x9E37_79B9_7F4A_7C15;
const MAX_INPUT_F64: f64 = 20_000e18;
const XATOL: f64 = 1.0;
const PER_FAMILY: usize = 43;
const FAMILIES: [&str; 7] = [
    "viz_uniform",
    "viz_decaying",
    "viz_growing",
    "viz_random",
    "stress_w",
    "stress_g",
    "stress_g2",
];

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

fn sqrt_at(tick: i32) -> U256 {
    U256::from(get_sqrt_ratio_at_tick_internal(tick).expect("tick within V3 bounds"))
}

fn snap(tick: i32, spacing: i32) -> i32 {
    tick.div_euclid(spacing) * spacing
}

fn range_bounds(anchor: i32, spacing: i32, zfo: bool, i: i32) -> (i32, i32) {
    if zfo {
        (anchor - (i + 1) * spacing, anchor - i * spacing)
    } else {
        (anchor + i * spacing, anchor + (i + 1) * spacing)
    }
}

fn range_sqrt(anchor: i32, spacing: i32, zfo: bool, i: i32) -> (U256, U256) {
    let (lower, upper) = range_bounds(anchor, spacing, zfo, i);
    if zfo {
        (sqrt_at(upper), sqrt_at(lower))
    } else {
        (sqrt_at(lower), sqrt_at(upper))
    }
}

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
    seq: IntV3TickRangeSequence,
}

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
    Hop { seq }
}

struct PathSpec {
    id: usize,
    family: &'static str,
    #[expect(dead_code)]
    spacings: [i32; 3],
    #[expect(dead_code)]
    fees: [u32; 3],
    #[expect(dead_code)]
    dirs: [bool; 3],
    ranges: [usize; 3],
    seqs: Vec<IntV3TickRangeSequence>,
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
    let mut hops = Vec::with_capacity(3);
    for i in 0..3 {
        let on_tick = ranges[i] >= 2 && rng.chance(0.5);
        let liqs = viz_liquidities(rng, profile, anchors[i], spacings[i], dirs[i], ranges[i]);
        hops.push(make_hop(
            anchors[i],
            spacings[i],
            fees[i],
            dirs[i],
            &liqs,
            on_tick,
        ));
    }
    PathSpec {
        id,
        family,
        spacings,
        fees,
        dirs,
        ranges,
        seqs: hops.into_iter().map(|h| h.seq).collect(),
    }
}

fn build_stress_path(family: &'static str, id: usize, rng: &mut Rng) -> PathSpec {
    let dirs = [true, false, true];
    let (spacings, ranges, fees, hop_liqs): ([i32; 3], [usize; 3], [u32; 3], [Vec<u128>; 3]) =
        match family {
            "stress_w" => {
                let sp = [pick(&[1000, 1300, 2000], rng), 60, pick(&[10, 20], rng)];
                let ranges = [1usize, 380, rng.range_usize(3, 5)];
                let fees = [500u32, 500, 500];
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
    let mut hops = Vec::with_capacity(3);
    for i in 0..3 {
        let on_tick = ranges[i] >= 2 && rng.chance(0.5);
        hops.push(make_hop(
            anchors[i],
            spacings[i],
            fees[i],
            dirs[i],
            &hop_liqs[i],
            on_tick,
        ));
    }
    PathSpec {
        id,
        family,
        spacings,
        fees,
        dirs,
        ranges,
        seqs: hops.into_iter().map(|h| h.seq).collect(),
    }
}

fn build_corpus() -> Vec<PathSpec> {
    let mut rng = Rng::new(SEED);
    let mut out = Vec::new();
    let mut id = 0usize;
    for family in FAMILIES {
        for _ in 0..PER_FAMILY {
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

fn u256_to_f64(v: U256) -> f64 {
    let limbs = v.as_limbs();
    let mut out = 0.0_f64;
    for i in (0..4).rev() {
        out = out * 18446744073709551616.0 + limbs[i] as f64;
    }
    out
}

fn floor_input(x: f64) -> U256 {
    if x.is_finite() && x >= 1.0 {
        U256::from(x.floor() as u128)
    } else {
        U256::ZERO
    }
}

/// The A/B harness's Brent-helper oracle: bounded Brent over `ClPathSim`.
fn brent_helper_run(sim: &ClPathSim) -> (U256, U256) {
    let mut objective = |x: f64| -> f64 {
        let xi = x.floor();
        let out = sim.output(floor_input(x)).final_output;
        -(u256_to_f64(out) - xi)
    };
    let r = minimize_scalar_bounded(&mut objective, (1.0, MAX_INPUT_F64), XATOL, DEFAULT_MAXFUN);
    let x_int = floor_input(r.x);
    let out = sim.output(x_int).final_output;
    (x_int, out.saturating_sub(x_int))
}

fn brent_helper_profit(sim: &ClPathSim) -> U256 {
    brent_helper_run(sim).1
}

/// The walk must not forfeit an actionable profit share to the Brent helper on
/// the wide/deep stress books: zero rows may exceed the A/B census's 2.5 %
/// actionable gap (`profit_delta_bps = 250`). The residual ties are wei-scale
/// staircase plateaus — bounded Brent and the walk land on neighbouring points
/// of an almost-flat top — so the regression guard also caps the worst residual
/// below 10 bps (1000 ppm), far under the pre-fix 2.6-13 % forfeitures.
#[test]
fn refine_window_matches_brent_helper_on_stress_books() {
    let cfg = SolveRuntimeConfig::default();
    let corpus = build_corpus();
    let mut actionable = Vec::new();
    let mut worst: Option<(usize, U256, U256, f64)> = None;
    let mut checked = 0usize;
    for path in &corpus {
        if !path.family.starts_with("stress_") {
            continue;
        }
        let refs: Vec<&IntV3TickRangeSequence> = path.seqs.iter().collect();
        let walk = crate::cl::derive_and_solve_cl_piecewise(&refs, &cfg);
        let (walk_x, walk_profit) = walk
            .result
            .as_ref()
            .map_or((U256::ZERO, U256::ZERO), |(x, p, _)| (*x, *p));
        let sim = ClPathSim::new(&refs, None);
        let helper = brent_helper_profit(&sim);
        checked += 1;
        if helper > walk_profit.saturating_add(U256::ONE) {
            let gap = helper - walk_profit;
            let bps = 10_000.0 * u256_to_f64(gap) / u256_to_f64(helper);
            worst = Some(match worst {
                Some(w) if w.3 >= bps => w,
                _ => (path.id, walk_profit, helper, bps),
            });
            if bps > 250.0 {
                actionable.push(format!(
                    "id={} family={} ranges={:?} walk_x={walk_x} walk_profit={walk_profit} helper_profit={helper} gap_bps={bps:.2} pieces={} refine_sims={}",
                    path.id, path.family, path.ranges, walk.stats.pieces, walk.stats.refine_sims,
                ));
            }
        }
    }
    assert!(
        actionable.is_empty(),
        "walk forfeited an actionable (>2.5%) profit share to the Brent helper on {checked} stress paths:\n{}",
        actionable.join("\n")
    );
    if let Some((id, wp, hp, bps)) = worst {
        assert!(
            bps < 10.0,
            "residual plateau tie on path {id} exceeds the 10 bps noise cap: walk={wp} helper={hp} gap_bps={bps}"
        );
        println!(
            "refine-undersample: {checked} stress paths, worst plateau tie id={id} gap_bps={bps:.6} (walk={wp} helper={hp})"
        );
    } else {
        println!("refine-undersample: {checked} stress paths match the Brent helper exactly");
    }
}

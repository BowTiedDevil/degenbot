#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::unwrap_used
)]
//! Gate-only benchmark: calls `path_profit_bound` on captured mixed-path
//! fixtures to isolate gate time from the decomposed solver time.

use alloy::primitives::U256;
use degenbot_math::v2::IntHopState;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::profit_envelope::{self, path_profit_bound, HopMath};
use serde_json::Value;

fn u256(s: &str) -> Result<U256, String> {
    s.trim().parse::<U256>().map_err(|e| e.to_string())
}

fn sf(v: &Value, k: &str) -> Result<String, String> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {k}"))
        .map(String::from)
}

fn clr(v: &Value) -> Result<IntV3TickRangeHop, String> {
    Ok(IntV3TickRangeHop {
        liquidity: sf(v, "liquidity")?
            .parse::<u128>()
            .map_err(|e| e.to_string())?,
        sqrt_price_x96: u256(&sf(v, "sqrt_price_x96")?)?,
        sqrt_price_lower_x96: u256(&sf(v, "sqrt_price_lower_x96")?)?,
        sqrt_price_upper_x96: u256(&sf(v, "sqrt_price_upper_x96")?)?,
        gamma_numer: v
            .get("gamma_numer")
            .and_then(Value::as_u64)
            .ok_or("gamma")?,
        fee_denom: v.get("fee_denom").and_then(Value::as_u64).ok_or("fee")?,
        zero_for_one: v
            .get("zero_for_one")
            .and_then(Value::as_bool)
            .ok_or("zfo")?,
        word_boundary_prices: Vec::new(),
    })
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/heavy_mixed_solve_captures.jsonl")
            .to_string_lossy()
            .into_owned()
    });
    let content = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("cannot read {path}: {e}");
        std::process::exit(2);
    });
    println!(
        "{:>8} {:>4} {:>6} {:>6} {:>10} {:>10} {:>10}",
        "path", "hops", "r1", "r2", "min_us", "med_us", "p95_us"
    );
    for line in content.lines().filter(|l| !l.trim().is_empty()) {
        let doc: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let pid: u64 = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let hops = match doc.get("hops").and_then(Value::as_array) {
            Some(a) => a.clone(),
            None => continue,
        };
        // Store owned values so HopMath borrows are valid for the gate call
        let mut v2_states: Vec<IntHopState> = Vec::new();
        let mut cl_seqs: Vec<IntV3TickRangeSequence> = Vec::new();
        let mut range_counts = Vec::new();
        let mut hop_order: Vec<bool> = Vec::new(); // true=V2, false=CL
        for hop in &hops {
            let kind = hop.get("kind").and_then(Value::as_str).unwrap_or("?");
            match kind {
                "V2" => {
                    let g = sf(hop, "gamma_numer").unwrap().parse::<u64>().unwrap();
                    let d = sf(hop, "fee_denom").unwrap().parse::<u64>().unwrap();
                    v2_states.push(IntHopState::new(
                        u256(&sf(hop, "reserve_in").unwrap()).unwrap(),
                        u256(&sf(hop, "reserve_out").unwrap()).unwrap(),
                        g,
                        d,
                    ));
                    hop_order.push(true);
                    range_counts.push(0);
                }
                "CL" => {
                    let ranges: Vec<_> = hop
                        .get("ranges")
                        .and_then(Value::as_array)
                        .unwrap()
                        .iter()
                        .map(clr)
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap();
                    range_counts.push(ranges.len());
                    cl_seqs.push(IntV3TickRangeSequence { ranges });
                    hop_order.push(false);
                }
                _ => {
                    hop_order.push(true);
                    range_counts.push(0);
                }
            }
        }
        // Build views: interleave V2 and CL references
        let mut vi = 0; // v2_states index
        let mut ci = 0; // cl_seqs index
        let mut views: Vec<Option<HopMath>> = Vec::with_capacity(hops.len());
        for &is_v2 in &hop_order {
            if is_v2 {
                views.push(Some(HopMath::V2(&v2_states[vi])));
                vi += 1;
            } else {
                views.push(Some(HopMath::Cl(&cl_seqs[ci])));
                ci += 1;
            }
        }
        let mut times = Vec::new();
        let mut prod_times = Vec::new();
        let mut s1_times = Vec::new();
        let mut hull_times = Vec::new();
        let mut reduce_times = Vec::new();
        let mut postprune_times = Vec::new();
        let mut sample_times = Vec::new();
        let mut derive_times = Vec::new();
        for _ in 0..9 {
            let t0 = std::time::Instant::now();
            let _ = path_profit_bound(&views);
            times.push(t0.elapsed().as_micros());
            let gs = degenbot_solvers::profit_envelope::take_last_gate_stats();
            prod_times.push(gs.product_ns / 1_000);
            s1_times.push(gs.prune_stage1_ns / 1_000);
            hull_times.push(gs.prune_hull_ns / 1_000);
            reduce_times.push(gs.duration_ns / 1_000);
            postprune_times.push(gs.postprune_reduce_ns / 1_000);
            sample_times.push(gs.sample_ns / 1_000);
            let (d, _, _) = degenbot_solvers::profit_envelope::take_gate_phases();
            derive_times.push(d / 1_000);
        }
        let m = |v: &Vec<u128>| {
            let mut t = v.clone();
            t.sort_unstable();
            t[t.len() / 2]
        };
        let n = times.len();
        let med = times[n / 2];
        let p95 = times[(n * 95) / 100];
        let r1 = range_counts.get(1).copied().unwrap_or(0);
        let r2 = range_counts.get(2).copied().unwrap_or(0);
        let n_hops = hops.len();
        println!(
            "{pid:>8} {n_hops:>4} {r1:>6} {r2:>6} gate={med:>8}/{p95:>8}us drv={:>6}us prod={:>6}us s1={:>6}us hull={:>6}us rdu={:>6}us smpl={:>6}us",
            m(&derive_times),
            m(&prod_times),
            m(&s1_times),
            m(&hull_times),
            m(&postprune_times),
            m(&sample_times),
        );
    }
    // Loop-16 gate census summary (DEGENBOT_GATE_CENSUS=1).
    let (s1_evals_ns, s1_sort_ns, s1_sweep_ns) = profit_envelope::take_gate_s1_split();
    let (h_red, h_sort, h_stack) = profit_envelope::take_gate_hull_split();
    println!(
        "[census] hull split: reduce={}us sort={}us stack={}us",
        h_red / 1_000,
        h_sort / 1_000,
        h_stack / 1_000
    );
    println!(
        "[census] s1 split: evals={}us sort={}us sweep={}us",
        s1_evals_ns / 1_000,
        s1_sort_ns / 1_000,
        s1_sweep_ns / 1_000
    );
    let census = profit_envelope::take_gate_census();
    if census.prune_calls > 0 {
        println!(
            "\n[census] prune_calls={} in_lines={} s1={} hull={} evals_upper={} sat_upper={}({:.1}%) boundaries={} pairs={}",
            census.prune_calls,
            census.in_lines,
            census.s1_survivors,
            census.hull_survivors,
            census.evals_upper,
            census.evals_saturated,
            100.0 * census.evals_saturated as f64 / census.evals_upper.max(1) as f64,
            census.boundaries,
            census.pairs
        );
        println!(
            "[census] in_buckets(<=8,<=64,<=256,<=1k,<=4k,>4k)={:?}",
            census.in_buckets
        );
        println!(
            "[census] hop_lines(<=8,<=64,<=256,<=1k,>1k)={:?}",
            census.hop_lines_buckets
        );
        println!(
            "[census] lines2(<=8,<=64,<=256,<=1k,>1k)={:?}",
            census.lines2_buckets
        );
    }
}

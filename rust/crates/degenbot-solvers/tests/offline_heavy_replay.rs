//! Offline heavy-path replay bench over live captures.
//! Reads the capture JSONL (env `DBENCH_CAPTURES`; default the capture-run
//! output in `logs/`) and reports per-path release-mode solve time + walk
//! stats. Diagnostic only — asserts nothing about goldens here.
#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::unnecessary_cast
)]

use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::mobius_v3_int::WalkStats;
use degenbot_solvers::profit_envelope::{path_profit_bound, GateDeps, HopMath};
use serde_json::Value;
use std::time::Instant;

fn u256(s: &str) -> U256 {
    s.trim().parse::<U256>().expect("valid U256")
}

fn parse_hop(v: &Value) -> Option<IntV3TickRangeSequence> {
    let arr = v.as_array()?;
    let mut ranges = Vec::with_capacity(arr.len());
    for item in arr {
        let liq: u128 = item.get("liquidity")?.as_str()?.parse().ok()?;
        let wbp: Vec<U256> = item
            .get("word_boundary_prices")?
            .as_array()?
            .iter()
            .filter_map(|w| w.as_str())
            .map(u256)
            .collect();
        ranges.push(IntV3TickRangeHop {
            liquidity: liq,
            sqrt_price_x96: u256(item.get("sqrt_price_x96")?.as_str()?),
            sqrt_price_lower_x96: u256(item.get("sqrt_price_lower_x96")?.as_str()?),
            sqrt_price_upper_x96: u256(item.get("sqrt_price_upper_x96")?.as_str()?),
            gamma_numer: item.get("gamma_numer")?.as_u64()?,
            fee_denom: item.get("fee_denom")?.as_u64()?,
            zero_for_one: item.get("zero_for_one")?.as_bool()?,
            word_boundary_prices: wbp,
        });
    }
    Some(IntV3TickRangeSequence { ranges })
}

#[test]
#[expect(clippy::too_many_lines)]
fn replay_captured_heavy_paths() {
    let path = std::env::var("DBENCH_CAPTURES")
        .unwrap_or_else(|_| "/workspaces/degenbot/logs/heavy_cl_captures_1.jsonl".to_string());
    let content = std::fs::read_to_string(&path).expect("captures readable");
    let max: usize = std::env::var("DBENCH_MAX")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let mut n = 0usize;
    let mut rows = Vec::new();
    for line in content.lines().filter(|l| !l.trim().is_empty()).take(max) {
        let doc: Value = serde_json::from_str(line).expect("capture JSON");
        let pid: u64 = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let hops = doc.get("hops").and_then(Value::as_array).expect("hops");
        let seqs: Vec<IntV3TickRangeSequence> = hops
            .iter()
            .map(parse_hop)
            .collect::<Option<Vec<_>>>()
            .expect("hops parse");
        let seq_refs: Vec<&IntV3TickRangeSequence> = seqs.iter().collect();
        // RED-1: small-K ranges must build word profiles after the threshold
        // widening. Until then the per-sim linear word walk is the driver.
        let small_k_profile = degenbot_solvers::mobius_v3_int::build_cl_word_profiles(&seqs[1]);
        let small_k_covered = seqs[1]
            .ranges
            .iter()
            .zip(small_k_profile.iter())
            .filter(|(r, _)| {
                r.word_boundary_prices.len() < 128 && !r.word_boundary_prices.is_empty()
            })
            .all(|(_, p)| p.is_some());
        assert!(
            small_k_covered,
            "sub-128-word-boundary ranges must carry a word profile"
        );
        let views: Vec<Option<HopMath<'_>>> = seq_refs
            .iter()
            .map(|s| Some(HopMath::cl_derived(s)))
            .collect();
        let gt0 = Instant::now();
        let _bound = path_profit_bound(&views, &GateDeps::offline());
        let gate_us = gt0.elapsed().as_micros();
        let gs = degenbot_solvers::profit_envelope::take_last_gate_stats();
        let (d_ns, c_ns, s_ns) = (gs.derive_ns, gs.compose_ns, gs.search_ns);
        let pairs = gs.pairs;
        let t0 = Instant::now();
        let outcome = degenbot_solvers::mobius_v3_int::solve_cl_derived(&seq_refs);
        let result = outcome.result;
        // Green net: profile-widened solve must reproduce the recorded golden
        // byte-for-byte (optimal_input + hop_outputs) for every captured path.
        if let Some(golden) = doc.get("golden").and_then(serde_json::Value::as_object) {
            if let Some(r) = &result {
                let go: U256 = golden["optimal_input"].as_str().unwrap().parse().unwrap();
                let gh: Vec<U256> = golden["hop_outputs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().parse().unwrap())
                    .collect();
                assert_eq!(&r.0, &go, "golden optimal_input diverges (pid {pid})");
                assert_eq!(&r.2, &gh, "golden hop_outputs diverge (pid {pid})");
            }
        }
        let us = t0.elapsed().as_micros();
        let ws: WalkStats = outcome.stats;
        if n < 2 {
            println!("raw-after[{n}] pid={pid} {ws:?}");
        }
        let total_ranges: usize = seqs.iter().map(|s| s.ranges.len()).sum();
        n += 1;
        rows.push((
            us,
            pid,
            total_ranges,
            ws.pieces,
            ws.sims,
            ws.word_steps,
            result.is_some(),
            gate_us,
            d_ns / 1_000,
            c_ns / 1_000,
            s_ns / 1_000,
            pairs,
        ));
    }
    rows.sort_unstable();
    println!("replayed {n} heavy paths (release)");
    let pick = |frac: f64| {
        let mut i = ((rows.len() as f64) * frac) as usize;
        i = i.min(rows.len() - 1);
        rows[i]
    };
    for (frac, label) in [(0.5, "p50"), (0.9, "p90"), (1.0, "max")] {
        let (us, pid, tr, pieces, sims, ws, some, gate_us, d_us, c_us, s_us, pairs) = pick(frac);
        println!(
            "{label}: {us:>7}us pid={pid} ranges={tr} pieces={pieces} sims={sims} word_steps={ws} profitable={some} | gate={gate_us}us derive={d_us}us compose={c_us}us search={s_us}us pairs={pairs}"
        );
    }
    let tot_gate: u128 = rows.iter().map(|r| r.7 as u128).sum();
    let tot_d: u128 = rows.iter().map(|r| r.8 as u128).sum();
    let tot_c: u128 = rows.iter().map(|r| r.9 as u128).sum();
    let tot_s: u128 = rows.iter().map(|r| r.10 as u128).sum();
    println!("gate totals: wall={tot_gate}us derive={tot_d}us compose={tot_c}us search={tot_s}us");
    let tot_sims: usize = rows.iter().map(|r| r.4).sum();
    let wall_us: u128 = rows.iter().map(|r| r.0 as u128).sum();
    println!(
        "total: {wall_us}ms wall for {n} paths | total sims {tot_sims} | sims/us={:.2}",
        tot_sims as f64 / wall_us as f64
    );
}

// Cache-lab golden gate (epic KIMRKS, task UR7CUX): on the 12-line full-range
// fixture, every capture epoch must honor the two-sided golden contract and,
// across a deterministic transition schedule, every catalog strategy must stay
// BYTE-EQUAL to the full-rebuild reference. CI-fast knobs: DRCLAB_GOLD_PATHS,
// DRCLAB_GOLD_TRANS.

#![expect(
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines
)]

use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::cl_cache::{strategy_catalog, CacheEvent, ClCacheStrategy, PreparedHop};
use degenbot_solvers::mobius_v3_int::int_solve_cl_path;
use serde_json::Value;

const PROFIT_EPS: u128 = 100_000;
const OVER_SHOOT_TOLERANCE_WEI: u128 = 8;

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

fn solve_prepared<S: ClCacheStrategy + ?Sized>(
    strategy: &mut S,
    seqs: &[IntV3TickRangeSequence],
    event: &CacheEvent,
    seq_refs: &[&IntV3TickRangeSequence],
) -> Option<(U256, U256, Vec<U256>)> {
    let prepared: Vec<PreparedHop> = strategy.refill(seqs, event);
    if prepared.is_empty() {
        return degenbot_solvers::mobius_v3_int::solve_cl_derived(seq_refs).result;
    }
    let prepared_hops: Vec<degenbot_solvers::mobius_v3_int::ClPrepared> = prepared
        .iter()
        .map(|(c, p)| degenbot_solvers::mobius_v3_int::ClPrepared {
            crossings: std::sync::Arc::clone(c),
            profiles: std::sync::Arc::clone(p),
        })
        .collect();
    int_solve_cl_path(seq_refs, &prepared_hops, None).result
}

fn price_move(seqs: &mut [IntV3TickRangeSequence], i: usize) {
    let r = &mut seqs[i].ranges[0];
    let price = r.sqrt_price_x96;
    let exit = if r.zero_for_one {
        r.sqrt_price_lower_x96
    } else {
        r.sqrt_price_upper_x96
    };
    let target = price.saturating_add(exit) / U256::from(2u64);
    r.sqrt_price_x96 = if target > price && target < exit {
        target
    } else {
        price.saturating_add(U256::from(1u64))
    };
}

fn liquidity_jitter(seqs: &mut [IntV3TickRangeSequence], i: usize, up: bool) -> usize {
    let seq = &mut seqs[i];
    let ri = seq.ranges.len() / 2;
    let liq = seq.ranges[ri].liquidity;
    seq.ranges[ri].liquidity = if up {
        liq.saturating_add(liq / 10)
    } else {
        liq.saturating_sub(liq / 10)
    };
    ri
}

fn window_slide(seqs: &mut [IntV3TickRangeSequence], i: usize) {
    let seq = &mut seqs[i];
    let old_lo = seq.ranges[0].sqrt_price_lower_x96;
    let old_hi = seq.ranges[0].sqrt_price_upper_x96;
    let zfo = seq.ranges[0].zero_for_one;
    let entry = if zfo { old_lo } else { old_hi };
    let mut ranges = seq.ranges.split_off(1);
    ranges[0].sqrt_price_x96 = entry;
    seq.ranges = ranges;
}

#[test]
fn golden_epochs_and_transitioned_epochs_stay_exact() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/heavy_cl_solve_captures.jsonl");
    let content = degenbot_solvers::capture_fixture::read_fixture(&fixture);
    let max_paths: usize = std::env::var("DRCLAB_GOLD_PATHS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let trans: usize = std::env::var("DRCLAB_GOLD_TRANS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    let mut catalog = strategy_catalog();
    let mut n_paths = 0usize;
    let mut n_golden_checked = 0usize;

    for (line_no, line) in content.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        if n_paths >= max_paths {
            break;
        }
        let doc: Value = serde_json::from_str(line).expect("capture JSON");
        let pid: u64 = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let hops = doc.get("hops").and_then(Value::as_array).expect("hops");
        let baseline: Vec<IntV3TickRangeSequence> = hops
            .iter()
            .map(parse_hop)
            .collect::<Option<Vec<_>>>()
            .expect("all hops parse");
        if baseline.len() < 2 {
            continue;
        }
        let mut seed: u64 = pid.wrapping_add(line_no as u64).wrapping_mul(0x9E37_79B9);
        let mut next = move || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            seed
        };

        // Golden epoch: strategies on the captured state + two-sided gate.
        let seq_refs: Vec<&IntV3TickRangeSequence> = baseline.iter().collect();
        let reference = degenbot_solvers::mobius_v3_int::solve_cl_derived(&seq_refs).result;
        for s in &mut catalog {
            let t = solve_prepared(s.as_mut(), &baseline, &CacheEvent::Fresh, &seq_refs);
            assert_eq!(
                t,
                reference,
                "path {pid} golden epoch diverges for {}",
                s.name()
            );
        }
        if let Some(golden) = doc.get("golden").and_then(Value::as_object) {
            let reference = reference.as_ref().unwrap_or_else(|| {
                panic!("golden present but reference solve is None on path {pid}")
            });
            let go: U256 = golden
                .get("optimal_input")
                .and_then(Value::as_str)
                .map(u256)
                .expect("golden optimal_input");
            let hop_out: Vec<U256> = golden
                .get("hop_outputs")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str()).map(u256).collect())
                .expect("golden hop_outputs");
            let gprof = hop_out
                .last()
                .copied()
                .unwrap_or(U256::ZERO)
                .saturating_sub(go);
            let rprof = reference
                .2
                .last()
                .copied()
                .unwrap_or(U256::ZERO)
                .saturating_sub(reference.0);
            assert!(
                gprof.saturating_sub(rprof) <= U256::from(PROFIT_EPS),
                "path {pid} under-shoots the recorded golden"
            );
            assert!(
                rprof.saturating_sub(gprof) <= U256::from(OVER_SHOOT_TOLERANCE_WEI),
                "path {pid} exceeds the recorded golden (phantom profit)"
            );
            n_golden_checked += 1;
        }

        // Transitioned epochs: every strategy must equal the reference byte-
        // for-byte after each synthetic state move.
        let mut seqs = baseline.clone();
        for t in 0..trans {
            let r = (next() % 4) as usize;
            let hop = (next() as usize) % seqs.len();
            let event = match r {
                0 => {
                    price_move(&mut seqs, hop);
                    CacheEvent::PriceMove { hop }
                }
                1 => {
                    let up = next() % 2 == 0;
                    let range = liquidity_jitter(&mut seqs, hop, up);
                    CacheEvent::Liquidity { hop, range }
                }
                2 if seqs[hop].ranges.len() > 2 => {
                    window_slide(&mut seqs, hop);
                    CacheEvent::TickCross { hop }
                }
                _ => {
                    seqs.clone_from(&baseline);
                    CacheEvent::Restore
                }
            };
            let refs2: Vec<&IntV3TickRangeSequence> = seqs.iter().collect();
            let reference = degenbot_solvers::mobius_v3_int::solve_cl_derived(&refs2).result;
            for s in &mut catalog {
                if s.name() == "S0_full_rebuild" {
                    let _ = s.refill(&seqs, &event); // keep counters honest
                    continue;
                }
                let cached = solve_prepared(s.as_mut(), &seqs, &event, &refs2);
                assert_eq!(
                    cached,
                    reference,
                    "path {pid} t={t} diverges for {} after {:?}",
                    s.name(),
                    event
                );
            }
        }
        n_paths += 1;
    }
    assert!(
        n_golden_checked >= 1,
        "fixture must contain at least one golden capture"
    );
}

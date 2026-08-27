//! Cache-lab driver (epic KIMRKS): replay a capture JSONL as a command of
//! deterministic pool-state transitions and solve every state through every
//! registered cache strategy, checking byte-equality against the full-rebuild
//! reference at each epoch and printing the rebuild-cost matrix.
//!
//! Usage:
//!   cargo run -p degenbot-solvers --example cl_cache_lab -- [<capture.jsonl>]
//!   DRCLAB_MAX_PATHS=4 DRCLAB_TRANS=8 ...
//!
//! Exit 1 if any strategy's solution diverges from the reference on any
//! transitioned state (exact accuracy — no tolerance).

#![expect(
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::too_many_lines
)]

extern crate degenbot_solvers;

use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::IntV3TickRangeSequence;
use degenbot_solvers::cl_cache::{strategy_catalog, CacheEvent, ClCacheStrategy, PreparedHop};
use degenbot_solvers::mobius_v3_int::int_solve_cl_path_cached;
use serde_json::Value;

fn u256(s: &str) -> Result<U256, String> {
    s.trim().parse::<U256>().map_err(|e| e.to_string())
}

fn str_of<'a>(item: &'a Value, k: &str) -> Result<&'a str, String> {
    item.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {k}"))
}

fn parse_hop(v: &Value) -> Result<IntV3TickRangeSequence, String> {
    let arr = v.as_array().ok_or("hop not an array")?;
    let mut ranges = Vec::with_capacity(arr.len());
    for item in arr {
        let liquidity: u128 = str_of(item, "liquidity")?
            .parse()
            .map_err(|e: std::num::ParseIntError| e.to_string())?;
        let wbp: Vec<U256> = item
            .get("word_boundary_prices")
            .and_then(Value::as_array)
            .ok_or("missing word_boundary_prices")?
            .iter()
            .map(|w| {
                w.as_str()
                    .ok_or_else(|| "wbp not a string".to_string())
                    .and_then(u256)
            })
            .collect::<Result<Vec<_>, String>>()?;
        ranges.push(degenbot_pools::int_v3_hop::IntV3TickRangeHop {
            liquidity,
            sqrt_price_x96: u256(str_of(item, "sqrt_price_x96")?)?,
            sqrt_price_lower_x96: u256(str_of(item, "sqrt_price_lower_x96")?)?,
            sqrt_price_upper_x96: u256(str_of(item, "sqrt_price_upper_x96")?)?,
            gamma_numer: item
                .get("gamma_numer")
                .and_then(Value::as_u64)
                .ok_or("missing gamma_numer")?,
            fee_denom: item
                .get("fee_denom")
                .and_then(Value::as_u64)
                .ok_or("missing fee_denom")?,
            zero_for_one: item
                .get("zero_for_one")
                .and_then(Value::as_bool)
                .ok_or("missing zero_for_one")?,
            word_boundary_prices: wbp,
        });
    }
    Ok(IntV3TickRangeSequence { ranges })
}

/// Solve through a strategy's prepared tables (production walk). Returns
/// None for the S0 sentinel (drive `int_solve_cl_path` directly).
fn solve_prepared<S: ClCacheStrategy + ?Sized>(
    strategy: &mut S,
    seqs: &[IntV3TickRangeSequence],
    event: &CacheEvent,
    seq_refs: &[&IntV3TickRangeSequence],
) -> Option<(U256, U256, Vec<U256>)> {
    let prepared: Vec<PreparedHop> = strategy.refill(seqs, event);
    if prepared.is_empty() {
        return degenbot_solvers::mobius_v3_int::int_solve_cl_path(seq_refs);
    }
    let crossings: Vec<&std::sync::Arc<degenbot_solvers::mobius_v3_int::ClCrossingTable>> =
        prepared.iter().map(|(c, _)| c).collect();
    let profiles: Vec<&std::sync::Arc<degenbot_solvers::mobius_v3_int::ClProfileTable>> =
        prepared.iter().map(|(_, p)| p).collect();
    int_solve_cl_path_cached(seq_refs, Some(&crossings), &profiles)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/heavy_cl_solve_captures.jsonl")
            .to_string_lossy()
            .into_owned()
    });
    let max_paths: usize = std::env::var("DRCLAB_MAX_PATHS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    let trans: usize = std::env::var("DRCLAB_TRANS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);

    let content = std::fs::read_to_string(&path).expect("capture fixture readable");
    let mut catalog = strategy_catalog();
    let mut total_mismatches: usize = 0;
    let mut n_paths = 0usize;
    // Class-indexed rebuild-delta matrix rows: [strategy][class], class 0-3 = price/liquidity/tick/restore.
    let mut class_agg: Vec<[u64; 4]> = vec![[0; 4]; catalog.len()];

    for (line_no, line) in content.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        if n_paths >= max_paths {
            break;
        }
        let doc: Value = serde_json::from_str(line).expect("capture line is JSON");
        let pid: u64 = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let baseline: Vec<IntV3TickRangeSequence> = {
            let hops = doc.get("hops").and_then(Value::as_array).expect("hops");
            let mut seqs = Vec::new();
            for h in hops {
                match parse_hop(h) {
                    Ok(s) => seqs.push(s),
                    Err(e) => {
                        eprintln!("line {line_no} path {pid}: skip ({e})");
                        break;
                    }
                }
            }
            seqs
        };
        if baseline.is_empty() {
            continue;
        }
        let mut seqs = baseline.clone();
        let mut seed: u64 = pid.wrapping_add(line_no as u64).wrapping_mul(0x9E37_79B9);
        let mut next = move || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            seed
        };
        let n_hops = seqs.len();
        let mut printable = Vec::with_capacity(catalog.len());
        for s in &mut catalog {
            printable.push(s.name());
        }
        println!(
            "path {pid}: hops={n_hops} range_lens={:?} transitions={trans} strategies={:?}",
            seqs.iter().map(|s| s.ranges.len()).collect::<Vec<_>>(),
            printable
        );
        let mut mismatches = vec![0u32; catalog.len()];

        for t in 0..trans {
            let r = (next() % 5) as usize;
            let hop = (next() as usize) % n_hops;
            let event = match r {
                0 => {
                    price_move(&mut seqs, hop);
                    CacheEvent::PriceMove { hop }
                }
                1 => {
                    price_move(&mut seqs, (hop + 1) % n_hops);
                    CacheEvent::PriceMove {
                        hop: (hop + 1) % n_hops,
                    }
                }
                2 => {
                    let up = next() % 2 == 0;
                    let range = liquidity_jitter(&mut seqs, hop, up);
                    CacheEvent::Liquidity { hop, range }
                }
                3 => {
                    if seqs[hop].ranges.len() > 2 {
                        window_slide(&mut seqs, hop);
                        CacheEvent::TickCross { hop }
                    } else {
                        price_move(&mut seqs, hop);
                        CacheEvent::PriceMove { hop }
                    }
                }
                _ => {
                    seqs.clone_from(&baseline);
                    CacheEvent::Restore
                }
            };
            let class: usize = match &event {
                CacheEvent::PriceMove { .. } => 0,
                CacheEvent::Liquidity { .. } => 1,
                CacheEvent::TickCross { .. } => 2,
                _ => 3,
            };
            let pre: Vec<degenbot_solvers::cl_cache::BuildCounters> =
                catalog.iter().map(|s| s.counters().clone()).collect();
            let seq_refs: Vec<&IntV3TickRangeSequence> = seqs.iter().collect();
            let reference = degenbot_solvers::mobius_v3_int::int_solve_cl_path(&seq_refs);
            for (si, s) in catalog.iter_mut().enumerate() {
                let cached = if si == 0 {
                    // S0 IS the reference; still run its refill so the
                    // rebuild counters stay comparable across the catalog.
                    let _ = s.refill(&seqs, &event);
                    reference.clone()
                } else {
                    solve_prepared(s.as_mut(), &seqs, &event, &seq_refs)
                };
                let name = s.name();
                if cached != reference {
                    mismatches[si] += 1;
                    total_mismatches += 1;
                    eprintln!(
                        "  DIVERGENCE path {pid} t={t} strat={name}: cached={cached:?} ref={reference:?}"
                    );
                }
            }
            for (si, s) in catalog.iter().enumerate() {
                let post = s.counters();
                class_agg[si][class] = class_agg[si][class]
                    .saturating_add(post.crossing_tables.saturating_sub(pre[si].crossing_tables));
            }
        }
        println!(
            "  path {pid} mismatches={:?}",
            mismatches
                .iter()
                .zip(&catalog)
                .map(|(m, s)| (s.name(), *m))
                .collect::<Vec<_>>()
        );
        n_paths += 1;
    }

    println!("---- class rebuild deltas ----");
    for (si, s) in catalog.iter().enumerate() {
        println!(
            "{} price={} liquidity={} tick={} restore={}",
            s.name(),
            class_agg[si][0],
            class_agg[si][1],
            class_agg[si][2],
            class_agg[si][3]
        );
    }
    println!("---- counters ----");
    for s in &catalog {
        let c = s.counters();
        println!(
            "{} crossing={} profiles={} seq_rebuilds={} partial={} solves={}",
            s.name(),
            c.crossing_tables,
            c.profile_tables,
            c.sequence_rebuilds,
            c.partial_rebuilds,
            c.solves
        );
    }
    println!(
        "----\nreplayed {n_paths} path(s) with {trans} transitions each | total divergences {total_mismatches}"
    );
    if total_mismatches > 0 {
        eprintln!("CACHE-LAB FAILED: exact accuracy violated");
        std::process::exit(1);
    }
}

/// Move hop `i`'s current-range sqrt price toward its exit boundary (midpoint
/// + 1 wei), staying strictly inside the range. Price-only event.
fn price_move(seqs: &mut [IntV3TickRangeSequence], i: usize) {
    let r = &mut seqs[i].ranges[0];
    let price = r.sqrt_price_x96;
    let exit = if r.zero_for_one {
        r.sqrt_price_lower_x96
    } else {
        r.sqrt_price_upper_x96
    };
    let mut target = price.saturating_add(exit) / U256::from(2u64);
    if target <= price || target >= exit {
        target = price.saturating_add(U256::from(1u64));
    }
    r.sqrt_price_x96 = target;
}

/// Bump liquidity of one range (a synthetic position change on the walk).
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

/// Slide the walked window past one crossed boundary: old range 0 is consumed,
/// the next range becomes current with its entry-boundary price.
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

#![expect(clippy::print_stdout, clippy::unwrap_used)]
//! Loop-12 (BY7BLS 4EG7P3): synthesize replayable corpus lines for the
//! giant-liquidity family shapes observed live (walk-heavy path 27817: ~400
//! piece hop with deep middle liquidity; gate-heavy paths 10760/15012/26030:
//! fat multi-hundred-to-low-thousands tick ranges feeding the envelope).
//! Emits TWO files matching the offline harness schemas:
//!   1. cl_solve_replay format:  {"block":..,"golden":null,"hops":[[range,..],..]}
//!   2. mixed_solve_replay / gate_bench format: {"path_id":..,"block":..,
//!      "n_hops":3,"hop_order":[false,..],"hops":[{"kind":"CL","ranges":[...]},..]}
//!
//! Usage: cargo run --release -p degenbot-solvers --example synth_corpus_gen
//! Geometry replicates the deep-late-liquidity fixtures that the production
//! walk solves correctly (optimum beyond the legacy enumeration prefix).

use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use serde_json::{json, Value};

fn sp_at(tick: i32) -> U256 {
    U256::from(degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(tick).unwrap())
}

fn multi_range(
    anchor_tick: i32,
    step: i32,
    zfo: bool,
    liquidities: &[u128],
) -> IntV3TickRangeSequence {
    let ranges: Vec<IntV3TickRangeHop> = liquidities
        .iter()
        .enumerate()
        .map(|(i, &liquidity)| {
            let i = i32::try_from(i).unwrap();
            let (tick_lo, tick_hi) = if zfo {
                (anchor_tick - (i + 1) * step, anchor_tick - i * step)
            } else {
                (anchor_tick + i * step, anchor_tick + (i + 1) * step)
            };
            let sqrt_price_x96 = if i == 0 {
                sp_at(anchor_tick)
            } else if zfo {
                sp_at(anchor_tick - i * step)
            } else {
                sp_at(anchor_tick + i * step)
            };
            IntV3TickRangeHop {
                liquidity,
                sqrt_price_x96,
                sqrt_price_lower_x96: sp_at(tick_lo),
                sqrt_price_upper_x96: sp_at(tick_hi),
                gamma_numer: 999_500,
                fee_denom: 1_000_000,
                zero_for_one: zfo,
                word_boundary_prices: Vec::new(),
            }
        })
        .collect();
    IntV3TickRangeSequence::new(ranges).unwrap()
}

fn cl_json(seq: &IntV3TickRangeSequence) -> Value {
    Value::Array(
        seq.ranges
            .iter()
            .map(|r| {
                json!({
                    "liquidity": r.liquidity.to_string(),
                    "sqrt_price_x96": r.sqrt_price_x96.to_string(),
                    "sqrt_price_lower_x96": r.sqrt_price_lower_x96.to_string(),
                    "sqrt_price_upper_x96": r.sqrt_price_upper_x96.to_string(),
                    "gamma_numer": r.gamma_numer,
                    "fee_denom": r.fee_denom,
                    "zero_for_one": r.zero_for_one,
                    "word_boundary_prices": [],
                })
            })
            .collect(),
    )
}

fn main() {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let g_range = |n: usize, liq: u128| vec![liq; n];
    // --- Family W (walk-heavy, path 27817 shape): 400 thin equal bars and
    // ONE deep-late 1e15 bar at the far end — the shallow-climb geometry that
    // makes the live 27817 walk visit ~320 pieces every solve.
    let mut l2: Vec<u128> = vec![1_000_000_000; 389];
    l2.push(1_000_000_000_000_000); // deep, LAST range
    let w_hop1 = multi_range(750, 1300, true, &[1_000_000_000_000_000]);
    let w_hop2 = multi_range(0, 60, false, &l2);
    let w_hop3 = multi_range(
        -200,
        10,
        true,
        &[5_000_000_000_000, 8_000_000_000_000, 3_000_000_000_000],
    );
    // --- Family G (gate bursts): fat middle crossing tables.
    let g_hop1 = multi_range(800, 2, true, &g_range(2_500, 1_000_000_000_000));
    let g_hop2 = multi_range(0, 2, false, &g_range(4_000, 2_000_000_000_000));
    let g_hop3 = multi_range(-800, 2, true, &g_range(2_500, 3_000_000_000_000));
    // --- Family G2 (mirror of the 10760 gate outlier: two intermediates).
    let g2_hop1 = multi_range(500, 3, true, &g_range(1_200, 1_000_000_000_000));
    let g2_hop2 = multi_range(-500, 3, false, &g_range(3_000, 2_000_000_000_000));
    let g2_hop3 = multi_range(0, 3, true, &g_range(1_500, 3_000_000_000_000));

    let w_cl: Value = json!({
        "block": 0u64,
        "golden": Value::Null,
        "hops": [cl_json(&w_hop1), cl_json(&w_hop2), cl_json(&w_hop3)]
    });
    let g_cl: Value = json!({
        "block": 1u64,
        "golden": Value::Null,
        "hops": [cl_json(&g_hop1), cl_json(&g_hop2), cl_json(&g_hop3)]
    });
    let g2_cl: Value = json!({
        "block": 2u64,
        "golden": Value::Null,
        "hops": [cl_json(&g2_hop1), cl_json(&g2_hop2), cl_json(&g2_hop3)]
    });
    let cl_heavy = base.join("synth_giant_cl.jsonl");
    std::fs::write(
        &cl_heavy,
        format!(
            "{w_cl}
{g_cl}
{g2_cl}
"
        ),
    )
    .unwrap();

    // mixed/replay format for gate_bench + mixed_solve_replay
    let mix = |pid: u64, block: u64, hops: [&IntV3TickRangeSequence; 3]| -> Value {
        let hop_objs: Vec<Value> = hops
            .iter()
            .map(|s| json!({ "kind": "CL", "ranges": cl_json(s) }))
            .collect();
        json!({
            "path_id": pid,
            "block": block,
            "n_hops": 3,
            "hop_order": [false, false, false],
            "hops": hop_objs
        })
    };
    let mm = format!(
        "{}
{}
{}
",
        mix(71_000, 0, [&w_hop1, &w_hop2, &w_hop3]),
        mix(71_001, 1, [&g_hop1, &g_hop2, &g_hop3]),
        mix(71_002, 2, [&g2_hop1, &g2_hop2, &g2_hop3]),
    );
    let mixed_heavy = base.join("synth_giant_mixed.jsonl");
    std::fs::write(&mixed_heavy, mm).unwrap();
    println!("wrote {} and {}", cl_heavy.display(), mixed_heavy.display());
}

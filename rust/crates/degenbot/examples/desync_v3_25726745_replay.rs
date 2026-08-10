#![expect(clippy::unwrap_used, clippy::panic, clippy::print_stdout)]
#![expect(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::similar_names,
    clippy::map_unwrap_or,
    clippy::expect_used
)] // run-once diagnostic harness mirroring the sibling `*_solver_fixture` runners
//! Deterministic replay of the block-25726745 solver-state desync (path 27).
//!
//! Loads `tests/fixtures/desync_v3_wbtc_25726745_block25726745.json` (written by
//! `scripts/capture_desync_v3_wbtc_25726745_fixture.py`), reconstructs the failing
//! hop-0 UniV3 WBTC/WETH 0.3% pool at the SOLVER's frozen update block, and
//! reproduces the exact divergence the production ADR-021 gate aborted on.
//!
//! Semantics mirror the `verify_solver_hop_states` fidelity check: the solver's
//! *stored* scalar (tick 265588, update_block 25726741) is diffed against
//! on-chain at the *solve* block (25726745, tick 265586). Because on-chain is
//! bit-identical across 25726741..25726744 and the only move is a Swap *inside
//! the solve block itself* (logIndex 75, tx 0xfa4bc4…), the replay confirms root
//! cause class (B) — the **in-block-swap-not-applied-before-solve** race
//! (header-promote-ahead-of-apply) — and rules out a multi-block
//! backfill/snapshot stall (class A).
//!
//! Exit 0 = desync reproduces (solver tick != on-chain solve tick) and the
//! `constant_across_gap` hold confirms class B. Exit 1 = capture invariant
//! violated (fixture/replay disagreement).

use std::collections::HashMap;
use std::str::FromStr;

use alloy::primitives::U256;
use serde::Deserialize;

use degenbot::investigation::{build_v3_state, v3_hop_output, OracleOutcome, PathFixture};

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/desync_v3_wbtc_25726745_block25726745.json"
);

/// The custom capture envelope (fields the shared `PathFixture` schema does not
/// carry: per-block on-chain trajectory, observed invariants, solve-block Swap).
#[derive(Deserialize)]
struct TrajRow {
    tick: i32,
    sqrt_price_x96: String,
    #[expect(dead_code)]
    liquidity: String,
}

#[derive(Deserialize)]
struct SolveSwap {
    #[expect(dead_code)]
    block: u64,
    #[expect(dead_code)]
    log_index: u64,
    #[expect(dead_code)]
    transaction_hash: String,
    #[expect(dead_code)]
    transaction_index: u64,
    #[expect(dead_code)]
    tick_before: i32,
    #[expect(dead_code)]
    tick_after: i32,
}

#[derive(Deserialize)]
struct Observed {
    constant_across_gap: bool,
    #[expect(dead_code)]
    moved_at_solve_block: bool,
}

#[derive(Deserialize)]
#[expect(dead_code)]
struct Envelope {
    solver_update_block: u64,
    per_block_onchain: HashMap<String, TrajRow>,
    solve_block_swap: Option<SolveSwap>,
    observed: Option<Observed>,
}

fn main() {
    let fixture_path = std::env::var("FIXTURE_PATH").unwrap_or_else(|_| FIXTURE_PATH.to_string());
    let fx = PathFixture::load(&fixture_path).unwrap_or_else(|e| panic!("{e}"));
    let env: Envelope = serde_json::from_str(
        &std::fs::read_to_string(&fixture_path).unwrap_or_else(|e| panic!("{e}")),
    )
    .unwrap_or_else(|e| panic!("envelope: {e}"));

    let solve_block = fx.target_block.expect("target_block");
    let solver_block = env.solver_update_block;

    let p = &fx.pools["v3_wbtc"];
    println!(
        "fixture: pool={} ({} / {}, fee={}, spacing={})",
        p.address.expect("address"),
        p.token0.expect("token0"),
        p.token1.expect("token1"),
        p.fee_token0.expect("fee"),
        p.tick_spacing.expect("tick_spacing")
    );
    println!(
        "  solver stored snapshot @ update_block={solver_block}: tick={} sqrt={} liq={}",
        p.tick.expect("tick"),
        p.sqrt_price_x96.expect("sqrt").0,
        p.liquidity.expect("liq").0
    );

    // ── Per-block on-chain trajectory (proves class A vs class B). ──
    println!("--- per-block on-chain slot0 trajectory ---");
    for b in solver_block..=solve_block {
        let row = &env.per_block_onchain[&b.to_string()];
        println!("  block {b}: tick={} sqrt={}", row.tick, row.sqrt_price_x96);
    }
    let truth = &env.per_block_onchain[&solve_block.to_string()];
    let onchain_solve_tick = truth.tick;
    let onchain_solve_sqrt = U256::from_str(&truth.sqrt_price_x96).unwrap();

    let const_gap = env
        .observed
        .as_ref()
        .map(|o| o.constant_across_gap)
        .unwrap_or_else(|| {
            (solver_block..solve_block).all(|b| {
                env.per_block_onchain[&b.to_string()].sqrt_price_x96
                    == env.per_block_onchain[&solver_block.to_string()].sqrt_price_x96
            })
        });

    println!(
        "--- on-chain truth @ solve block {solve_block}: tick={onchain_solve_tick} sqrt={onchain_solve_sqrt} ---"
    );
    println!(
        "  constant across gap {solver_block}..{}: {const_gap} \
         (true => pool was quiet; the only move is inside the solve block => class B)",
        solve_block - 1
    );

    let solver_tick = p.tick.expect("tick");
    let desync_reproduces = solver_tick != onchain_solve_tick;

    println!("--- verified-desync reproduction (production ADR-021 fidelity check) ---");
    println!(
        "  solver stored tick @ update_block ({solver_tick}) vs on-chain tick @ solve block \
         ({onchain_solve_tick}): {}",
        if desync_reproduces {
            "DIVERGENT — verified desync reproduces"
        } else {
            "CONSISTENT"
        }
    );
    println!(
        "  on-chain moved {} tick(s) inside the solve block itself = a Swap included in \
         block {solve_block} (logIndex 75) was NOT applied to the solver state before the \
         block-{solve_block} solve+verify fired.",
        (i64::from(onchain_solve_tick) - i64::from(solver_tick)).abs()
    );

    // ── Practical impact: what the solver would quote at the stale (frozen) price. ──
    let state = build_v3_state(p);
    let fee = p.fee_token0.expect("fee");
    let spacing = p.tick_spacing.expect("tick_spacing");
    // Practical impact: a swap the solver WOULD have quoted at the frozen (stale)
    // price. The pool is WBTC(0)/WETH(1); zfo=false converts WETH(t1)->WBTC(t0).
    let zfo = false;
    let amount_in = U256::from(10_u64).pow(U256::from(15u64)); // 0.001 WETH
    let outcome = v3_hop_output(&state, fee, spacing, zfo, amount_in);
    println!("--- practical impact: solver quote on the frozen snapshot ---");
    let quoted = match &outcome {
        OracleOutcome::Ok(v) => format!("{v} (t0 WBTC raw units)"),
        OracleOutcome::MissingTickWord(w) => format!("uncomputable: missing tick word {w}"),
        OracleOutcome::NotComputable => "uncomputable".to_string(),
    };
    println!("  zfo={zfo} in={amount_in} (t1 WETH): oracle quote = {quoted}");

    if !const_gap {
        println!("=> VERDICT: FAIL — fixture shows on-chain moved BEFORE the solve block");
        std::process::exit(1);
    }
    if !desync_reproduces {
        println!(
            "=> VERDICT: FAIL — solver snapshot matches on-chain at solve; no desync to replay"
        );
        std::process::exit(1);
    }
    println!(
        "=> VERDICT: PASS — desync reproduces deterministically and is root-cause class (B): \
         the pool was quiet through {} (constant_across_gap=true) and the sole \
         on-chain move is the Swap included in the solve block itself, so the divergence is a \
         WS header-vs-log ordering race (solve fired on pre-block-{} state while the \
         RPC-fidelity verifier compares post-block-{} truth). It is NOT a \
         multi-block backfill/snapshot-drain stall.",
        solve_block - 1,
        solve_block,
        solve_block
    );
}

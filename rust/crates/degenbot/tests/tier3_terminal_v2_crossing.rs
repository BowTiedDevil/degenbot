#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Tier-3 terminal-V2 crossing regression gate (ergo ASLM5N) — the
//! path182449 (block 25731019, v4v4v2) and path110302 (block 25711761,
//! v3v4v2) solver-parity samples that failed on-chain with `UniswapV2: K`.
//!
//! Incident: at the V2 CROSSING the production solver's terminal V2 hop
//! output was 1 wei of input worth above the byte-exact `getAmountOut` at the
//! delivered input — the upstream CL hop's twin-aligned FORWARD CLAMP reduced
//! the V2 hop's committed input without re-deriving the walk-frozen V2 output
//! (`clamp_cl_hop_capacity`'s V2 branch was a bare `continue`). The stale
//! prediction reached the executor, was encoded as the V2 exact-OUT
//! `amountOut`, and the exact-out swap needed 1 more input wei than the path
//! delivered → K reverts.
//!
//! This gate drives the committed captures through the PRODUCTION engine
//! (register → resolve → solve → clamp seam) and asserts the post-fix
//! invariants: every hop's `hop_outputs[i]` is byte-exact to the tier-3
//! oracle twin at the solver's own consumed input, and the terminal V2
//! hop's prediction round-trips through the exact-OUT inverse to NO MORE
//! than the delivered input (the on-chain no-shortfall invariant). Each test
//! also pins the fixture's recorded incident values so a corrupted capture
//! cannot silently vacate the gate. No RPC.
//!
//! Replaces the one-shot `examples/path*_solver_fixture.rs` harnesses
//! (deleted with their captures in 4635a163f, HAVRUW/SEG2PS); the two
//! captures were restored from that commit's parent for this gate.

use alloy::primitives::U256;
use degenbot::bot::solvers::arb_engine::ArbitrageEngine;
use degenbot::investigation::reconstruct::{
    build_v3_state, build_v4_state, register_v2, register_v3, register_v4, V2_DEFAULT_FEE,
};
use degenbot::investigation::{
    v2_get_amount_out, v3_hop_output, v4_hop_output, OracleOutcome, PathFixture, PoolData,
};
use degenbot_math::v2::IntHopState;
use degenbot_solvers::mixed::PoolHop;

const FIXTURE_182449: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path182449_v4v4v2_block25731019.json"
);
const FIXTURE_110302: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path110302_v3v4v2_block25711761.json"
);

/// The orientation-split V2 reserves for a hop with `zero_for_one`.
fn v2_reserves(v2: &PoolData, zero_for_one: bool) -> (U256, U256) {
    let r0 = v2.reserve0.as_ref().expect("reserve0").0;
    let r1 = v2.reserve1.as_ref().expect("reserve1").0;
    if zero_for_one {
        (U256::from(r0), U256::from(r1))
    } else {
        (U256::from(r1), U256::from(r0))
    }
}

/// The executor's exact-OUT encoding demands `getAmountIn(predicted)` input;
/// the path only delivers `consumed`. The incident breached this by exactly
/// 1 wei (`UniswapV2: K`).
fn assert_v2_no_shortfall(r_in: U256, r_out: U256, delivered: U256, predicted: U256) {
    let required = IntHopState::new(r_in, r_out, V2_DEFAULT_FEE.0, V2_DEFAULT_FEE.1)
        .swap_exact_out(predicted)
        .expect("V2 exact-out inverse computable");
    assert!(
        required <= delivered,
        "terminal V2 exact-out would need {required} input but only {delivered} is delivered \n\n(the path-182449/110302 `UniswapV2: K` class)"
    );
}

/// Every hop's solver output must equal its tier-3 oracle at the solver's
/// own consumed input — byte-exact, not approximate.
fn assert_oracle_equal(outcome: &OracleOutcome, solver_out: U256, hop: &str) {
    match outcome {
        OracleOutcome::Ok(o) => assert_eq!(
            *o, solver_out,
            "hop {hop} must be byte-exact to its oracle twin"
        ),
        other => panic!("hop {hop} oracle: {other:?}"),
    }
}

/// Fixture-sanity core shared by both tests: the capture still records the
/// 1-input-wei shortfall class (guards against a corrupted capture).
fn assert_recorded_incident(fx: &PathFixture) {
    let rec = &fx.recorded_solve;
    let hop = rec.v2_hop_index.expect("fixture pins the V2 hop index");
    let v2_in = rec.v2_input.as_ref().map(|a| a.0).unwrap_or_default();
    let v2_pred = rec.v2_predicted.as_ref().map(|a| a.0).unwrap_or_default();
    let v2_act = rec.v2_actual.as_ref().map(|a| a.0).unwrap_or_default();
    let (r_in, r_out) = v2_reserves(
        &fx.pools[fx.path[hop].pool.as_str()],
        fx.path[hop].zero_for_one,
    );
    assert_ne!(
        v2_pred, v2_act,
        "fixture must still record the over-prediction"
    );
    let required = IntHopState::new(r_in, r_out, V2_DEFAULT_FEE.0, V2_DEFAULT_FEE.1)
        .swap_exact_out(v2_pred)
        .expect("recorded prediction invertible");
    assert_eq!(
        required,
        v2_in + U256::from(1u64),
        "recorded incident must be exactly the 1-input-wei shortfall class"
    );
    assert_eq!(
        v2_act,
        v2_get_amount_out(v2_in, r_in, r_out, V2_DEFAULT_FEE)
    );
}

#[test]
fn v4v4v2_path182449_terminal_v2_is_byte_exact() {
    let fx = PathFixture::load(FIXTURE_182449).unwrap_or_else(|e| panic!("{e}"));
    assert_recorded_incident(&fx);

    let mut engine = ArbitrageEngine::new();
    let pid_a = register_v4(&mut engine.core().write(), &fx.pools["v4_a"])
        .unwrap_or_else(|e| panic!("{e}"));
    let pid_b = register_v4(&mut engine.core().write(), &fx.pools["v4_b"])
        .unwrap_or_else(|e| panic!("{e}"));
    let pid_c = register_v2(&mut engine.core().write(), &fx.pools["v2_c"])
        .unwrap_or_else(|e| panic!("{e}"));
    let hops: Vec<PoolHop> = fx
        .path
        .iter()
        .map(|h| match h.pool.as_str() {
            "v4_a" => PoolHop {
                pool_id: pid_a,
                zero_for_one: h.zero_for_one,
            },
            "v4_b" => PoolHop {
                pool_id: pid_b,
                zero_for_one: h.zero_for_one,
            },
            "v2_c" => PoolHop {
                pool_id: pid_c,
                zero_for_one: h.zero_for_one,
            },
            o => panic!("unknown pool {o}"),
        })
        .collect();

    let path_id = engine
        .register_and_solve_path(hops.clone())
        .expect("path registers");
    let (results, _) = engine.latest_results();
    let sr = results
        .get(&path_id)
        .cloned()
        .expect("production solver must solve the captured path");

    // Per-hop byte-exact oracle comparison at the solver's own consumed input.
    let v4a = build_v4_state(&fx.pools["v4_a"]);
    let v4b = build_v4_state(&fx.pools["v4_b"]);
    assert_oracle_equal(
        &v4_hop_output(
            &v4a,
            fx.pools["v4_a"].fee_currency0.unwrap(),
            fx.pools["v4_a"].tick_spacing.unwrap(),
            hops[0].zero_for_one,
            sr.consumed_inputs[0],
        ),
        sr.hop_outputs[0],
        "hop0 v4_a",
    );
    assert_oracle_equal(
        &v4_hop_output(
            &v4b,
            fx.pools["v4_b"].fee_currency0.unwrap(),
            fx.pools["v4_b"].tick_spacing.unwrap(),
            hops[1].zero_for_one,
            sr.consumed_inputs[1],
        ),
        sr.hop_outputs[1],
        "hop1 v4_b",
    );
    let v2 = &fx.pools["v2_c"];
    let (r_in, r_out) = v2_reserves(v2, hops[2].zero_for_one);
    assert_oracle_equal(
        &OracleOutcome::Ok(v2_get_amount_out(
            sr.consumed_inputs[2],
            r_in,
            r_out,
            V2_DEFAULT_FEE,
        )),
        sr.hop_outputs[2],
        "hop2 v2_c",
    );

    // The on-chain no-shortfall invariant at the terminal V2 hop.
    assert_v2_no_shortfall(r_in, r_out, sr.consumed_inputs[2], sr.hop_outputs[2]);
}

#[test]
fn v3v4v2_path110302_terminal_v2_is_byte_exact() {
    let fx = PathFixture::load(FIXTURE_110302).unwrap_or_else(|e| panic!("{e}"));
    assert_recorded_incident(&fx);

    let mut engine = ArbitrageEngine::new();
    let pid_a = register_v3(&mut engine.core().write(), &fx.pools["v3_0"])
        .unwrap_or_else(|e| panic!("{e}"));
    let pid_b =
        register_v4(&mut engine.core().write(), &fx.pools["v4"]).unwrap_or_else(|e| panic!("{e}"));
    let pid_c = register_v2(&mut engine.core().write(), &fx.pools["v2_2"])
        .unwrap_or_else(|e| panic!("{e}"));
    let hops: Vec<PoolHop> = fx
        .path
        .iter()
        .map(|h| match h.pool.as_str() {
            "v3_0" => PoolHop {
                pool_id: pid_a,
                zero_for_one: h.zero_for_one,
            },
            "v4" => PoolHop {
                pool_id: pid_b,
                zero_for_one: h.zero_for_one,
            },
            "v2_2" => PoolHop {
                pool_id: pid_c,
                zero_for_one: h.zero_for_one,
            },
            o => panic!("unknown pool {o}"),
        })
        .collect();

    let path_id = engine
        .register_and_solve_path(hops.clone())
        .expect("path registers");
    let (results, _) = engine.latest_results();
    let sr = results
        .get(&path_id)
        .cloned()
        .expect("production solver must solve the captured path");

    // Per-hop byte-exact oracle comparison at the solver's own consumed input.
    let v3 = build_v3_state(&fx.pools["v3_0"]);
    assert_oracle_equal(
        &v3_hop_output(
            &v3,
            fx.pools["v3_0"].fee_token0.unwrap(),
            fx.pools["v3_0"].tick_spacing.unwrap(),
            hops[0].zero_for_one,
            sr.consumed_inputs[0],
        ),
        sr.hop_outputs[0],
        "hop0 v3_0",
    );
    let v4 = build_v4_state(&fx.pools["v4"]);
    assert_oracle_equal(
        &v4_hop_output(
            &v4,
            fx.pools["v4"].fee_currency0.unwrap(),
            fx.pools["v4"].tick_spacing.unwrap(),
            hops[1].zero_for_one,
            sr.consumed_inputs[1],
        ),
        sr.hop_outputs[1],
        "hop1 v4",
    );
    let v2 = &fx.pools["v2_2"];
    let (r_in, r_out) = v2_reserves(v2, hops[2].zero_for_one);
    assert_oracle_equal(
        &OracleOutcome::Ok(v2_get_amount_out(
            sr.consumed_inputs[2],
            r_in,
            r_out,
            V2_DEFAULT_FEE,
        )),
        sr.hop_outputs[2],
        "hop2 v2_2",
    );

    // The on-chain no-shortfall invariant at the terminal V2 hop.
    assert_v2_no_shortfall(r_in, r_out, sr.consumed_inputs[2], sr.hop_outputs[2]);
}

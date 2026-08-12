#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::doc_markdown
)]
//! 6YUNQN derivation spike — prove the hybrid ShapeClass emitter (ADR-029 D4)
//! reproduces a family's command stream AND executes through the runtime
//! matrix with exact delta, without a hand-written adapter.
//!
//! For each representative V2/V3 2-hop family:
//!   1. derive the payload via `degenbot_executor::grammar_shape::derive_shape`
//!      (ShapeClass + per-protocol HopFacts — the new, rule-driven emitter),
//!   2. assert it is **byte-identical** to the proven hand-written grammar
//!      (`encode_cmd_stream`) — the strongest feasibility evidence,
//!   3. drive the *derived* bytes through the real cmd_executor and assert
//!      `Accepted` + exact `actual_delta == predicted_profit`
//!      (the runtime-fidelity gate, ADR-029 D5).

use alloy::primitives::{Address, U256};
use degenbot_executor::composers::{ComposerInputs, EncodeOptions};
use degenbot_simulation::harness::{assert_profitable, Harness, Hop, HopPool};

/// A protocol we derive.
#[derive(Clone, Copy, PartialEq)]
enum Prot {
    V2,
    V3,
    V4,
}

fn q96_one() -> U256 {
    U256::from(1u128) << 96
}
fn sqrt_x(x: u64) -> U256 {
    if x == 1 {
        q96_one()
    } else {
        let s = ((x as f64).sqrt() * 65536.0) as u64;
        q96_one() * U256::from(s) / U256::from(65536)
    }
}
fn liq() -> u128 {
    10u128.pow(22)
}

fn pool_for(h: &mut Harness, p: Prot, src: Address, dst: Address, mult: u64) -> HopPool {
    match p {
        Prot::V2 => {
            let r: u128 = 1_000_000_000_000;
            HopPool::V2(h.add_pool(src, dst, r, r * mult as u128).unwrap())
        }
        Prot::V3 => HopPool::V3(
            h.add_v3_pool(
                src,
                dst,
                3000,
                sqrt_x(mult),
                liq(),
                1_000_000_000_000,
                1_000_000_000_000,
            )
            .unwrap(),
        ),
        Prot::V4 => HopPool::V4(
            h.add_v4_pool(
                src,
                dst,
                3000,
                60,
                sqrt_x(mult),
                liq(),
                1_000_000_000_000,
                1_000_000_000_000,
            )
            .unwrap(),
        ),
    }
}

fn build_two_hop(h: &mut Harness, a: Prot, b: Prot, mult_b: u64) -> Vec<Hop> {
    let t = h.add_token().unwrap();
    vec![
        Hop {
            src: h.weth,
            dst: t,
            pool: pool_for(h, a, h.weth, t, 1),
        },
        Hop {
            src: t,
            dst: h.weth,
            pool: pool_for(h, b, t, h.weth, mult_b),
        },
    ]
}

fn run_spike(a: Prot, b: Prot, name: &str) {
    let mut h = Harness::new().unwrap();
    let hops = build_two_hop(&mut h, a, b, 3);
    let optimal_input = 100_000u128;

    let (path, hop_outputs, consumed) = h.path_and_amounts(&hops, optimal_input);
    let inputs = ComposerInputs {
        executor_address: h.executor,
        pool_manager_address: h.pool_manager,
        weth_address: h.weth,
        optimal_input,
        hop_outputs: &hop_outputs,
        consumed_inputs: &consumed,
        opts: EncodeOptions::default(),
    };

    // 1. Derive via the new ShapeClass emitter.
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("[{name}] derive_shape returned None"));
    // 2. Byte-parity vs the proven hand-written grammar (the strongest evidence).
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("[{name}] encode_path: {e}"));
    assert_eq!(
        derived, reference,
        "[{name}] derived bytes diverge from the proven hand-written grammar"
    );

    // 3. Execute the DERIVED bytes through the real executor; exact delta.
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 8_000_000)
        .unwrap_or_else(|e| panic!("[{name}] run_raw_payload: {e}"));
    assert_profitable(&result, 2, name);
    println!(
        "── {name}: derived==reference, executed, actual_delta={}",
        result.actual_weth_delta
    );
}

#[test]
fn derived_v2v3_executes_with_exact_delta() {
    run_spike(Prot::V2, Prot::V3, "v2_v3");
}

#[test]
fn derived_v3v2_executes_with_exact_delta() {
    run_spike(Prot::V3, Prot::V2, "v3_v2");
}

#[test]
fn derived_v3v3_executes_with_exact_delta() {
    run_spike(Prot::V3, Prot::V3, "v3_v3");
}

#[test]
fn derived_v2v2_executes_with_exact_delta() {
    run_spike(Prot::V2, Prot::V2, "v2_v2");
}

#[test]
fn derived_v4v4_executes_with_exact_delta() {
    run_spike(Prot::V4, Prot::V4, "v4_v4");
}

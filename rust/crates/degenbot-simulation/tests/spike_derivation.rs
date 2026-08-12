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

#[test]
fn derived_v4v3_executes_with_exact_delta() {
    run_spike(Prot::V4, Prot::V3, "v4_v3");
}

#[test]
fn derived_v3v4_executes_with_exact_delta() {
    run_spike(Prot::V3, Prot::V4, "v3_v4");
}

#[test]
fn derived_v4v2_executes_with_exact_delta() {
    run_spike(Prot::V4, Prot::V2, "v4_v2");
}

#[test]
fn derived_v2v4_executes_with_exact_delta() {
    run_spike(Prot::V2, Prot::V4, "v2_v4");
}

/// Native runtime proof (ergo WAYDTL (2)): a native v4_v4 path (NATIVE→t→
/// NATIVE — native at both path ends, ERC-20 mid, no wrap/unwrap) derived via
/// `derive_shape` must execute through the real cmd_executor with the
/// executor's **native** balance delta ≈ predicted profit. This is the
/// runtime gate that byte-parity alone can't prove for native flows: it
/// exercises native V4 pool funding (PoolManager native holdings), the
/// executor's native `TAKE` capture, and `set_native_balance`/native balance
/// reading in the harness.
#[test]
fn native_v4v4_derived_executes_with_exact_native_delta() {
    let mut h = Harness::new().unwrap();
    let t = h.add_token().unwrap();
    let native = Address::ZERO;

    // Pool A: NATIVE→t; Pool B: t→NATIVE. Both backed generously so the PM can
    // pay the captured native (and residual ERC-20) deltas.
    let p_a = HopPool::V4(
        h.add_v4_pool(
            native,
            t,
            3000,
            60,
            sqrt_x(1),
            liq(),
            1_000_000_000_000,
            1_000_000_000_000,
        )
        .unwrap(),
    );
    let p_b = HopPool::V4(
        h.add_v4_pool(
            t,
            native,
            3000,
            60,
            sqrt_x(3),
            liq(),
            1_000_000_000_000,
            1_000_000_000_000,
        )
        .unwrap(),
    );
    let hops = vec![
        Hop {
            src: native,
            dst: t,
            pool: p_a,
        },
        Hop {
            src: t,
            dst: native,
            pool: p_b,
        },
    ];
    let optimal_input = 100_000u128;
    let gas = 8_000_000;

    let (path, hop_outputs, consumed) = h.path_and_amounts(&hops, optimal_input);
    let predicted_profit = *hop_outputs.last().unwrap() as i128 - optimal_input as i128;
    assert!(predicted_profit > 0, "native path should be profitable");

    let inputs = ComposerInputs {
        executor_address: h.executor,
        pool_manager_address: h.pool_manager,
        weth_address: h.weth,
        optimal_input,
        hop_outputs: &hop_outputs,
        consumed_inputs: &consumed,
        opts: EncodeOptions::default(),
    };
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive_shape returned None for native v4_v4"));
    // Byte-parity vs production (the proven hand-written grammar).
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode_path: {e}"));
    assert_eq!(
        derived, reference,
        "native v4_v4 must match hand-written grammar"
    );

    // Fund the executor with the native seed (it is the caller paying hop 0's
    // native input) and execute the DERIVED bytes.
    h.fund(native, h.executor, optimal_input * 2).unwrap();
    let before = h.native_balance_of(h.executor).unwrap().to::<u128>() as i128;
    let outcome = h.execute_payload(&derived, gas).unwrap();
    assert!(
        outcome.executed(2),
        "native v4_v4 must execute through both V4 pools: {:?}",
        outcome
    );
    let after = h.native_balance_of(h.executor).unwrap().to::<u128>() as i128;
    let actual_delta = after - before;

    let tol = (predicted_profit.abs() / 1000).max(64);
    assert!(
        actual_delta > 0,
        "native v4_v4 should gain native, got delta {actual_delta}"
    );
    assert!(
        (actual_delta - predicted_profit).abs() <= tol,
        "native v4_v4 delta {actual_delta} diverges from predicted {predicted_profit} (tol {tol})"
    );
}

/// WRAP runtime proof (WETH_DEPOSIT): a V4→V2 path whose V4 pool is a
/// native-output pool. V4 outputs native, the derivation wraps it to WETH
/// (`WETH_DEPOSIT`) before funding the terminal V2 pool. Asserts the payload
/// executes through both pools and the executor's terminal ERC-20 (`t`) delta
/// equals the predicted terminal output.
#[test]
fn native_wrap_v4_v2_executes_with_terminal_delta() {
    let mut h = Harness::new().unwrap();
    let t = h.add_token().unwrap();
    let native = Address::ZERO;

    // Pool A: V4(WETH, native) — src=WETH=c0 -> zfo true, outputs native.
    let p_a = HopPool::V4(
        h.add_v4_pool(
            h.weth,
            native,
            3000,
            60,
            sqrt_x(1),
            liq(),
            1_000_000_000_000,
            1_000_000_000_000,
        )
        .unwrap(),
    );
    // Pool B: V2(WETH, t) — outer/terminal V2 converting the wrapped WETH->t.
    let p_b = HopPool::V2(
        h.add_pool(h.weth, t, 1_000_000_000_000, 1_000_000_000_000)
            .unwrap(),
    );

    let hops = vec![
        Hop {
            src: h.weth,
            dst: native,
            pool: p_a,
        },
        Hop {
            src: h.weth,
            dst: t,
            pool: p_b,
        },
    ];
    let optimal_input = 100_000u128;
    let gas = 8_000_000;

    let (path, hop_outputs, consumed) = h.path_and_amounts(&hops, optimal_input);
    let terminal = *hop_outputs.last().unwrap();

    let inputs = ComposerInputs {
        executor_address: h.executor,
        pool_manager_address: h.pool_manager,
        weth_address: h.weth,
        optimal_input,
        hop_outputs: &hop_outputs,
        consumed_inputs: &consumed,
        opts: EncodeOptions::default(),
    };
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive_shape returned None for wrap v4_v2"));

    // Provision: WETH seed (wrap input + V2 custody), V2 approve, t terminal read.
    h.fund(h.weth, h.executor, optimal_input * 2).unwrap();
    if let HopPool::V2(p) = &p_b {
        h.executor_approve_pair(*p).unwrap();
    }
    let t_before = h.balance_of(t, h.executor).unwrap().to::<u128>() as i128;
    let outcome = h.execute_payload(&derived, gas).unwrap();
    assert!(
        outcome.executed(2),
        "wrap v4_v2 must execute through both pools: {:?}",
        outcome
    );
    let t_after = h.balance_of(t, h.executor).unwrap().to::<u128>() as i128;
    let t_delta = t_after - t_before;
    let tol = (terminal / 1000) as i128;
    assert!(
        t_delta > 0 && (t_delta - terminal as i128).abs() <= tol,
        "wrap v4_v2 terminal t delta {t_delta} != predicted {terminal} (tol {tol})"
    );
}

/// UNWRAP runtime proof (WETH_WITHDRAW): a V3→V4 path whose V4 pool has a
/// native *input*. The V3 (outer flash) output is WETH; the derivation unwraps
/// it to native (`WETH_WITHDRAW`) to seed the V4 native input via settle; the
/// V4 output (an ERC-20 `u`) is the terminal profit. Asserts the payload
/// executes through both pools and the executor's terminal `u` delta equals
/// the predicted terminal output.
/// UNWRAP runtime proof (WETH_WITHDRAW), V3->V4 native-input variant: the
/// V3 output WETH is unwrapped (WETH_WITHDRAW) to seed the V4 native input; the
/// V4 output `u` is the terminal profit.
#[test]
fn native_unwrap_v3_v4_executes_with_terminal_delta() {
    let mut h = Harness::new().unwrap();
    let tok = h.add_token().unwrap();
    let u = h.add_token().unwrap();
    let native = Address::ZERO;

    // Pool A: V3(tok, WETH) — src=tok -> zfo true, outputs WETH (to unwrap).
    let p_a = HopPool::V3(
        h.add_v3_pool(
            tok,
            h.weth,
            3000,
            sqrt_x(1),
            liq(),
            1_000_000_000_000,
            1_000_000_000_000,
        )
        .unwrap(),
    );
    // Pool B: V4(u, native) — src=native=c1 -> zfo false (input native).
    let p_b = HopPool::V4(
        h.add_v4_pool(
            u,
            native,
            3000,
            60,
            sqrt_x(3),
            liq(),
            1_000_000_000_000,
            1_000_000_000_000,
        )
        .unwrap(),
    );

    let hops = vec![
        Hop {
            src: tok,
            dst: h.weth,
            pool: p_a,
        },
        Hop {
            src: native,
            dst: u,
            pool: p_b,
        },
    ];
    let optimal_input = 100_000u128;
    let gas = 8_000_000;

    let (path, hop_outputs, consumed) = h.path_and_amounts(&hops, optimal_input);
    let terminal = *hop_outputs.last().unwrap();

    let inputs = ComposerInputs {
        executor_address: h.executor,
        pool_manager_address: h.pool_manager,
        weth_address: h.weth,
        optimal_input,
        hop_outputs: &hop_outputs,
        consumed_inputs: &consumed,
        opts: EncodeOptions::default(),
    };
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive_shape returned None for unwrap v3_v4"));

    // Provision: V3 input seed (tok) + WETH (the executor holds V3's WETH
    // output to unwrap), terminal `u` measured.
    h.fund(tok, h.executor, optimal_input * 2).unwrap();
    h.fund(h.weth, h.executor, optimal_input * 2).unwrap();
    let u_before = h.balance_of(u, h.executor).unwrap().to::<u128>() as i128;
    let outcome = h.execute_payload(&derived, gas).unwrap();
    assert!(
        outcome.executed(2),
        "unwrap v3_v4 must execute through both pools: {:?}",
        outcome
    );
    let u_after = h.balance_of(u, h.executor).unwrap().to::<u128>() as i128;
    let u_delta = u_after - u_before;
    let tol = (terminal / 1000) as i128;
    assert!(
        u_delta > 0 && (u_delta - terminal as i128).abs() <= tol,
        "unwrap v3_v4 terminal u delta {u_delta} != predicted {terminal} (tol {tol})"
    );
}

/// UNWRAP runtime proof (WETH_WITHDRAW), pure-V4 unwrap-bridge variant: hop A
/// outputs WETH in the PM, the bridge takes it out + WETH_WITHDRAW -> native,
/// hop B (a native pool) consumes it; the `u` output is the terminal profit.
/// This was the case that root-caused the native `settle(value)` sign bug in
/// the harness PoolManager stub (it was crediting -value, never resolving the
/// native debt); it now executes with exact terminal delta.
#[test]
fn native_v4v4_unwrap_bridge_executes_with_terminal_delta() {
    let mut h = Harness::new().unwrap();
    let t0 = h.add_token().unwrap();
    let u = h.add_token().unwrap();
    let native = Address::ZERO;
    let p_a = HopPool::V4(
        h.add_v4_pool(
            t0,
            h.weth,
            3000,
            60,
            sqrt_x(1),
            liq(),
            1_000_000_000_000,
            1_000_000_000_000,
        )
        .unwrap(),
    );
    let p_b = HopPool::V4(
        h.add_v4_pool(
            native,
            u,
            3000,
            60,
            sqrt_x(3),
            liq(),
            1_000_000_000_000,
            1_000_000_000_000,
        )
        .unwrap(),
    );
    let hops = vec![
        Hop {
            src: t0,
            dst: h.weth,
            pool: p_a,
        },
        Hop {
            src: native,
            dst: u,
            pool: p_b,
        },
    ];
    let optimal_input = 100_000u128;
    let gas = 8_000_000;
    let (path, hop_outputs, consumed) = h.path_and_amounts(&hops, optimal_input);
    let terminal = *hop_outputs.last().unwrap();
    let inputs = ComposerInputs {
        executor_address: h.executor,
        pool_manager_address: h.pool_manager,
        weth_address: h.weth,
        optimal_input,
        hop_outputs: &hop_outputs,
        consumed_inputs: &consumed,
        opts: EncodeOptions::default(),
    };
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive_shape returned None for unwrap bridge"));
    h.fund(t0, h.executor, optimal_input * 2).unwrap();
    h.fund(h.weth, h.executor, optimal_input * 2).unwrap();
    let u_before = h.balance_of(u, h.executor).unwrap().to::<u128>() as i128;
    let outcome = h.execute_payload(&derived, gas).unwrap();
    assert!(
        outcome.executed(2),
        "unwrap-bridge v4_v4 must execute through both pools: {:?}",
        outcome
    );
    let u_after = h.balance_of(u, h.executor).unwrap().to::<u128>() as i128;
    let u_delta = u_after - u_before;
    let tol = (terminal / 1000) as i128;
    assert!(
        u_delta > 0 && (u_delta - terminal as i128).abs() <= tol,
        "unwrap-bridge terminal u delta {u_delta} != predicted {terminal} (tol {tol})"
    );
}

/// WETH-only 3-hop v4_v4_v4: derive -> byte-parity -> execute with exact WETH
/// delta (the 36-family matrix shape W->t1->t2->W).
#[test]
fn derived_v4v4v4_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V4(
                h.add_v4_pool(
                    h.weth,
                    t1,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t1,
                    t2,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t2,
                    h.weth,
                    3000,
                    60,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v4_v4_v4 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v4_v4_v4 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v4_v4_v4");
}

/// Native 3-hop v4_v4_v4 (NATIVE->t1->t2->NATIVE): the executor's native delta
/// must equal predicted profit (native pool funding + native capture).
#[test]
fn native_v4v4v4_derived_executes_with_exact_native_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let native = Address::ZERO;
    let hops = vec![
        Hop {
            src: native,
            dst: t1,
            pool: HopPool::V4(
                h.add_v4_pool(
                    native,
                    t1,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t1,
                    t2,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: native,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t2,
                    native,
                    3000,
                    60,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
    let optimal_input = 100_000u128;
    let gas = 40_000_000;
    let (path, hop_outputs, consumed) = h.path_and_amounts(&hops, optimal_input);
    let predicted = *hop_outputs.last().unwrap() as i128 - optimal_input as i128;
    let inputs = ComposerInputs {
        executor_address: h.executor,
        pool_manager_address: h.pool_manager,
        weth_address: h.weth,
        optimal_input,
        hop_outputs: &hop_outputs,
        consumed_inputs: &consumed,
        opts: EncodeOptions::default(),
    };
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive native v4_v4_v4 None"));
    h.fund(native, h.executor, optimal_input * 2).unwrap();
    let before = h.native_balance_of(h.executor).unwrap().to::<u128>() as i128;
    let outcome = h.execute_payload(&derived, gas).unwrap();
    assert!(
        outcome.executed(3),
        "native v4_v4_v4 must execute 3 pools: {outcome:?}"
    );
    let after = h.native_balance_of(h.executor).unwrap().to::<u128>() as i128;
    let actual = after - before;
    let tol = (predicted.abs() / 1000).max(64);
    assert!(
        actual > 0 && (actual - predicted).abs() <= tol,
        "native v4_v4_v4 delta {actual} != predicted {predicted} (tol {tol})"
    );
}

/// WETH-only 3-hop v4_v2_v2: derive -> byte-parity -> execute exact WETH delta.
#[test]
fn derived_v4v2v2_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V4(
                h.add_v4_pool(
                    h.weth,
                    t1,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V2(
                h.add_pool(t1, t2, 1_000_000_000_000, 1_000_000_000_000)
                    .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V2(
                h.add_pool(t2, h.weth, 1_000_000_000_000, 3_000_000_000_000)
                    .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v4_v2_v2 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v4_v2_v2 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v4_v2_v2");
}

/// WETH-only 3-hop v2_v2_v4 and v2_v3_v4 runtime (exact WETH delta).
#[test]
fn derived_v2v2v4_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V2(
                h.add_pool(h.weth, t1, 1_000_000_000_000, 1_000_000_000_000)
                    .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V2(
                h.add_pool(t1, t2, 1_000_000_000_000, 1_000_000_000_000)
                    .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t2,
                    h.weth,
                    3000,
                    60,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v2_v2_v4 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v2_v2_v4 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v2_v2_v4");
}

#[test]
fn derived_v2v3v4_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V2(
                h.add_pool(h.weth, t1, 1_000_000_000_000, 1_000_000_000_000)
                    .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V3(
                h.add_v3_pool(
                    t1,
                    t2,
                    3000,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t2,
                    h.weth,
                    3000,
                    60,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v2_v3_v4 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v2_v3_v4 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v2_v3_v4");
}

#[test]
fn derived_v3v2v4_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V3(
                h.add_v3_pool(
                    h.weth,
                    t1,
                    3000,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V2(
                h.add_pool(t1, t2, 1_000_000_000_000, 1_000_000_000_000)
                    .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t2,
                    h.weth,
                    3000,
                    60,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v3_v2_v4 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v3_v2_v4 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v3_v2_v4");
}

#[test]
fn derived_v3v3v4_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V3(
                h.add_v3_pool(
                    h.weth,
                    t1,
                    3000,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V3(
                h.add_v3_pool(
                    t1,
                    t2,
                    3000,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t2,
                    h.weth,
                    3000,
                    60,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v3_v3_v4 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v3_v3_v4 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v3_v3_v4");
}

#[test]
fn derived_v2v4v2_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V2(
                h.add_pool(h.weth, t1, 1_000_000_000_000, 1_000_000_000_000)
                    .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t1,
                    t2,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V2(
                h.add_pool(t2, h.weth, 1_000_000_000_000, 3_000_000_000_000)
                    .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v2_v4_v2 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v2_v4_v2 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v2_v4_v2");
}

#[test]
fn derived_v2v4v3_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V2(
                h.add_pool(h.weth, t1, 1_000_000_000_000, 1_000_000_000_000)
                    .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t1,
                    t2,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V3(
                h.add_v3_pool(
                    t2,
                    h.weth,
                    3000,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v2_v4_v3 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v2_v4_v3 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v2_v4_v3");
}

#[test]
fn derived_v3v4v2_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V3(
                h.add_v3_pool(
                    h.weth,
                    t1,
                    3000,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t1,
                    t2,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V2(
                h.add_pool(t2, h.weth, 1_000_000_000_000, 3_000_000_000_000)
                    .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v3_v4_v2 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v3_v4_v2 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v3_v4_v2");
}

#[test]
fn derived_v3v4v3_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V3(
                h.add_v3_pool(
                    h.weth,
                    t1,
                    3000,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t1,
                    t2,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V3(
                h.add_v3_pool(
                    t2,
                    h.weth,
                    3000,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v3_v4_v3 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v3_v4_v3 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v3_v4_v3");
}

#[test]
fn derived_v2v4v4_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V2(
                h.add_pool(h.weth, t1, 1_000_000_000_000, 1_000_000_000_000)
                    .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t1,
                    t2,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t2,
                    h.weth,
                    3000,
                    60,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v2_v4_v4 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v2_v4_v4 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v2_v4_v4");
}

#[test]
fn derived_v3v4v4_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V3(
                h.add_v3_pool(
                    h.weth,
                    t1,
                    3000,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t1,
                    t2,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t2,
                    h.weth,
                    3000,
                    60,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v3_v4_v4 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v3_v4_v4 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v3_v4_v4");
}

#[test]
fn derived_v4v4v2_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V4(
                h.add_v4_pool(
                    h.weth,
                    t1,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t1,
                    t2,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V2(
                h.add_pool(t2, h.weth, 1_000_000_000_000, 3_000_000_000_000)
                    .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v4_v4_v2 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v4_v4_v2 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v4_v4_v2");
}

#[test]
fn derived_v4v4v3_executes_with_exact_delta() {
    let mut h = Harness::new().unwrap();
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let hops = vec![
        Hop {
            src: h.weth,
            dst: t1,
            pool: HopPool::V4(
                h.add_v4_pool(
                    h.weth,
                    t1,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V4(
                h.add_v4_pool(
                    t1,
                    t2,
                    3000,
                    60,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
        Hop {
            src: t2,
            dst: h.weth,
            pool: HopPool::V3(
                h.add_v3_pool(
                    t2,
                    h.weth,
                    3000,
                    sqrt_x(3),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        },
    ];
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive v4_v4_v3 None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "v4_v4_v3 derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, "v4_v4_v3");
}

fn run_v4lead(
    h: &mut Harness,
    tail_kind: &str,
    tail_zfo: bool,
    reserve3: u64,
    name: &str,
) -> Vec<Hop> {
    let t1 = h.add_token().unwrap();
    let t2 = h.add_token().unwrap();
    let mut hops = vec![Hop {
        src: h.weth,
        dst: t1,
        pool: HopPool::V4(
            h.add_v4_pool(
                h.weth,
                t1,
                3000,
                60,
                sqrt_x(1),
                liq(),
                1_000_000_000_000,
                1_000_000_000_000,
            )
            .unwrap(),
        ),
    }];
    match tail_kind {
        "v2" => hops.push(Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V2(
                h.add_pool(t1, t2, 1_000_000_000_000, 1_000_000_000_000)
                    .unwrap(),
            ),
        }),
        "v3" => hops.push(Hop {
            src: t1,
            dst: t2,
            pool: HopPool::V3(
                h.add_v3_pool(
                    t1,
                    t2,
                    3000,
                    sqrt_x(1),
                    liq(),
                    1_000_000_000_000,
                    1_000_000_000_000,
                )
                .unwrap(),
            ),
        }),
        _ => unreachable!(),
    }
    let _ = tail_zfo;
    let _ = reserve3;
    let _ = name;
    hops
}
fn assert_derived_executes(h: &mut Harness, hops: Vec<Hop>, name: &str) {
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
    let derived = degenbot_executor::grammar_shape::derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive {name} None"));
    let reference = h
        .encode_path(&path, optimal_input, &hop_outputs)
        .unwrap_or_else(|e| panic!("encode: {e}"));
    assert_eq!(derived, reference, "{name} derived != hand-written");
    let result = h
        .run_raw_payload(&hops, &derived, optimal_input, 40_000_000)
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_profitable(&result, 3, name);
}
#[test]
fn derived_v4v2v3_executes() {
    let mut h = Harness::new().unwrap();
    let mut hops = run_v4lead(&mut h, "v2", true, 0, "");
    let t2 = hops[1].dst;
    hops.push(Hop {
        src: t2,
        dst: h.weth,
        pool: HopPool::V3(
            h.add_v3_pool(
                t2,
                h.weth,
                3000,
                sqrt_x(3),
                liq(),
                1_000_000_000_000,
                1_000_000_000_000,
            )
            .unwrap(),
        ),
    });
    assert_derived_executes(&mut h, hops, "v4_v2_v3");
}
#[test]
fn derived_v4v2v4_executes() {
    let mut h = Harness::new().unwrap();
    let mut hops = run_v4lead(&mut h, "v2", true, 0, "");
    let t2 = hops[1].dst;
    hops.push(Hop {
        src: t2,
        dst: h.weth,
        pool: HopPool::V4(
            h.add_v4_pool(
                t2,
                h.weth,
                3000,
                60,
                sqrt_x(3),
                liq(),
                1_000_000_000_000,
                1_000_000_000_000,
            )
            .unwrap(),
        ),
    });
    assert_derived_executes(&mut h, hops, "v4_v2_v4");
}
#[test]
fn derived_v4v3v2_executes() {
    let mut h = Harness::new().unwrap();
    let mut hops = run_v4lead(&mut h, "v3", true, 0, "");
    let t2 = hops[1].dst;
    hops.push(Hop {
        src: t2,
        dst: h.weth,
        pool: HopPool::V2(
            h.add_pool(t2, h.weth, 1_000_000_000_000, 3_000_000_000_000)
                .unwrap(),
        ),
    });
    assert_derived_executes(&mut h, hops, "v4_v3_v2");
}
#[test]
fn derived_v4v3v3_executes() {
    let mut h = Harness::new().unwrap();
    let mut hops = run_v4lead(&mut h, "v3", true, 0, "");
    let t2 = hops[1].dst;
    hops.push(Hop {
        src: t2,
        dst: h.weth,
        pool: HopPool::V3(
            h.add_v3_pool(
                t2,
                h.weth,
                3000,
                sqrt_x(3),
                liq(),
                1_000_000_000_000,
                1_000_000_000_000,
            )
            .unwrap(),
        ),
    });
    assert_derived_executes(&mut h, hops, "v4_v3_v3");
}
#[test]
fn derived_v4v3v4_executes() {
    let mut h = Harness::new().unwrap();
    let mut hops = run_v4lead(&mut h, "v3", true, 0, "");
    let t2 = hops[1].dst;
    hops.push(Hop {
        src: t2,
        dst: h.weth,
        pool: HopPool::V4(
            h.add_v4_pool(
                t2,
                h.weth,
                3000,
                60,
                sqrt_x(3),
                liq(),
                1_000_000_000_000,
                1_000_000_000_000,
            )
            .unwrap(),
        ),
    });
    assert_derived_executes(&mut h, hops, "v4_v3_v4");
}

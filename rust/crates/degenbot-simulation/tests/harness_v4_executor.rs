#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::doc_markdown
)]
//! Executor grammar harness — V4-involving permutations (UQOAHA), the funding-
//! topology slice. Adds the minimal `PoolManager` stub and proves the pure-V4
//! family (`v4_v4`) drives the executor's V4 command surface end-to-end:
//! `v4_unlock` → `swap` (exact-input deltas) → `take_delta` → `settle_all`.
use alloy::primitives::{Address, U256};
use degenbot_executor::composers::{HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo};
use degenbot_simulation::harness::v3_amount_out;

fn price_one() -> U256 {
    U256::from(1u128) << 96
}

fn v2_out(reserve_in: u128, reserve_out: u128, amount_in: u128) -> u128 {
    use degenbot_v2_math::IntHopState;
    IntHopState::new(U256::from(reserve_in), U256::from(reserve_out), 997, 1000)
        .swap(U256::from(amount_in))
        .expect("bounded v2 getAmountOut")
        .to::<u128>()
}

fn amt_out(reserve_in: u128, reserve_out: u128, amount_in: u128) -> u128 {
    use degenbot_v2_math::IntHopState;
    IntHopState::new(U256::from(reserve_in), U256::from(reserve_out), 997, 1000)
        .swap(U256::from(amount_in))
        .expect("bounded v2 getAmountOut")
        .to::<u128>()
}

fn v2_zfo(pool: degenbot_simulation::harness::V2Pool, src: Address) -> bool {
    src == pool.token0
}

fn reserve_of(pool: degenbot_simulation::harness::V2Pool, token: Address) -> u128 {
    if token == pool.token0 {
        pool.reserve0
    } else {
        pool.reserve1
    }
}

#[test]
fn v4_v4_two_pool_path_executes() {
    let mut h = degenbot_simulation::harness::Harness::new().unwrap();
    let i = h.add_token().unwrap();
    let fee = 3000u32; // 0.3%
    let liq = 10u128.pow(22);
    // Pool A: WETH -> I (token0=WETH, token1=I), price 1.
    let a = h
        .add_v4_pool(
            h.weth,
            i,
            fee,
            60,
            price_one(),
            liq,
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    // Pool B: I -> WETH (token0=I, token1=WETH), price ~1.02 — better WETH for I.
    let b_sqrt = price_one() * U256::from(101u64) / U256::from(100u64);
    let b = h
        .add_v4_pool(
            i,
            h.weth,
            fee,
            60,
            b_sqrt,
            liq,
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();

    let optimal_input = 100_000u128;
    // hop A: sell WETH(token0)->I(token1), exact-in optimal.
    let mid = v3_amount_out(a.sqrt_price, a.liquidity, optimal_input, true, a.fee);
    // hop B: sell I(token0)->WETH(token1), exact-in `mid`.
    let weth_out = v3_amount_out(b.sqrt_price, b.liquidity, mid, true, b.fee);
    let hop_outputs = [mid, weth_out];
    assert!(
        weth_out > optimal_input,
        "must be profitable (got weth_out={weth_out} vs optimal={optimal_input})"
    );

    h.fund(h.weth, h.executor, optimal_input).unwrap();

    let path = PathInfo::new(vec![
        HopInfo::V4(V4HopInfo {
            pool_manager_address: h.pool_manager,
            pool_id_hex: "0x0".into(),
            currency0_address: a.currency0,
            currency1_address: a.currency1,
            fee: a.fee,
            tick_spacing: a.tick_spacing,
            hook_address: Address::ZERO,
            zfo: true,
        }),
        HopInfo::V4(V4HopInfo {
            pool_manager_address: h.pool_manager,
            pool_id_hex: "0x0".into(),
            currency0_address: b.currency0,
            currency1_address: b.currency1,
            fee: b.fee,
            tick_spacing: b.tick_spacing,
            hook_address: Address::ZERO,
            zfo: true,
        }),
    ]);
    let outcome = h
        .run_path(&path, optimal_input, &hop_outputs, 8_000_000)
        .unwrap();
    println!("V4-V4 outcome: {outcome:?}  mid={mid} weth_out={weth_out} optimal={optimal_input}");
    assert!(
        outcome.executed(2),
        "V4-V4 payload must execute (reach both pools): {outcome:?}"
    );
}

#[test]
fn v4_v2_v4_entry_terminal_v2_executes() {
    let mut h = degenbot_simulation::harness::Harness::new().unwrap();
    let f = h.add_token().unwrap();
    let fee = 3000u32;
    let liq = 10u128.pow(22);
    // V4 entry: WETH -> F (token0=WETH, token1=F) at price 1 — cheap forward.
    let a = h
        .add_v4_pool(
            h.weth,
            f,
            fee,
            60,
            price_one(),
            liq,
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    let optimal_input = 100_000u128;
    let forward_out = v3_amount_out(a.sqrt_price, a.liquidity, optimal_input, true, a.fee);

    // V2 exit: sell F -> WETH. Reserves are large so the 99.7k trade sees
    // only ~0.01% price impact (billion-scale).
    let pair = h.add_pool(f, h.weth, 1_000_000_000, 1_020_000_000).unwrap();
    let v2_in = reserve_of(pair, f);
    let v2_out_tok = reserve_of(pair, h.weth);
    let v2_out = amt_out(v2_in, v2_out_tok, forward_out);
    let hop_outputs = [forward_out, v2_out];
    assert!(
        v2_out > optimal_input,
        "must be profitable (v2_out={v2_out} vs optimal={optimal_input})"
    );

    // Executor self-funds the WETH it repays the V4 input delta.
    h.fund(h.weth, h.executor, optimal_input).unwrap();

    let v4_zfo = true; // WETH(token0)->F(token1) on the V4 pool
    let path = PathInfo::new(vec![
        HopInfo::V4(V4HopInfo {
            pool_manager_address: h.pool_manager,
            pool_id_hex: "0x0".into(),
            currency0_address: a.currency0,
            currency1_address: a.currency1,
            fee: a.fee,
            tick_spacing: a.tick_spacing,
            hook_address: Address::ZERO,
            zfo: v4_zfo,
        }),
        HopInfo::V2(V2HopInfo {
            pool_address: pair.pair,
            token0_address: pair.token0,
            token1_address: pair.token1,
            zfo: v2_zfo(pair, f),
            fee: 30,
        }),
    ]);
    let outcome = h
        .run_path(&path, optimal_input, &hop_outputs, 40_000_000)
        .unwrap();
    println!(
        "V4-V2 outcome: {outcome:?} forward_out={forward_out} v2_out={v2_out} optimal={optimal_input}"
    );
    assert!(
        outcome.executed(2),
        "V4-V2 payload must execute (reach both pools): {outcome:?}"
    );
}

#[test]
fn v2_v4_two_pool_path_executes() {
    let mut h = degenbot_simulation::harness::Harness::new().unwrap();
    let t = h.add_token().unwrap();
    // V2 pool: WETH -> T (token0=WETH, token1=T).
    let p0 = h.add_pool(h.weth, t, 1_000_000, 2_000_000).unwrap();
    // V4 pool: T -> WETH (token0=T, token1=WETH), price 1.
    let a = h
        .add_v4_pool(
            t,
            h.weth,
            3000,
            60,
            price_one(),
            10u128.pow(22),
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    let optimal_input = 100_000u128; // WETH into the V2 hop
    let ho0 = v2_out(reserve_of(p0, h.weth), reserve_of(p0, t), optimal_input); // T -> V4 input
    let weth_out = v3_amount_out(a.sqrt_price, a.liquidity, ho0, true, a.fee); // V4 exact-in -> WETH
    let hop_outputs = [ho0, weth_out];
    assert!(
        weth_out > optimal_input,
        "must be profitable (weth_out={weth_out} vs optimal={optimal_input})"
    );

    // WETH buffer (V2 input + flash), T buffer (V4 sync/transfer), approve pair.
    h.fund(h.weth, h.executor, optimal_input * 2).unwrap();
    h.fund(t, h.executor, ho0 * 2).unwrap();
    h.executor_approve_pair(p0).unwrap();

    let path = PathInfo::new(vec![
        HopInfo::V2(V2HopInfo {
            pool_address: p0.pair,
            token0_address: p0.token0,
            token1_address: p0.token1,
            fee: 30,
            zfo: v2_zfo(p0, h.weth),
        }),
        HopInfo::V4(V4HopInfo {
            pool_manager_address: h.pool_manager,
            pool_id_hex: "0x0".into(),
            currency0_address: a.currency0,
            currency1_address: a.currency1,
            fee: a.fee,
            tick_spacing: a.tick_spacing,
            hook_address: Address::ZERO,
            zfo: true,
        }),
    ]);
    let outcome = h
        .run_path(&path, optimal_input, &hop_outputs, 8_000_000)
        .unwrap();
    println!("V2-V4 outcome: {outcome:?}  ho0={ho0} weth_out={weth_out} optimal={optimal_input}");
    assert!(
        outcome.executed(2),
        "V2-V4 payload must execute (reach both pools): {outcome:?}"
    );
}

#[test]
fn v3_v4_two_pool_path_executes() {
    let mut h = degenbot_simulation::harness::Harness::new().unwrap();
    let t = h.add_token().unwrap();
    // V3 pool: WETH -> T (token0=WETH, token1=T), price 1.
    let p0 = h
        .add_v3_pool(
            h.weth,
            t,
            3000,
            price_one(),
            10u128.pow(22),
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    // V4 pool: T -> WETH (token0=T, token1=WETH), price ~1.02.
    let a_sqrt = price_one() * U256::from(101u64) / U256::from(100u64);
    let a = h
        .add_v4_pool(
            t,
            h.weth,
            3000,
            60,
            a_sqrt,
            10u128.pow(22),
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    let optimal_input = 100_000u128; // WETH into the V3 hop
    let ho0 = v3_amount_out(p0.sqrt_price, p0.liquidity, optimal_input, true, p0.fee); // T -> V4 input
    let weth_out = v3_amount_out(a.sqrt_price, a.liquidity, ho0, true, a.fee);
    let hop_outputs = [ho0, weth_out];
    assert!(
        weth_out > optimal_input,
        "must be profitable (weth_out={weth_out} vs optimal={optimal_input})"
    );

    h.fund(h.weth, h.executor, optimal_input * 2).unwrap();
    h.fund(t, h.executor, ho0 * 2).unwrap();

    let path = PathInfo::new(vec![
        HopInfo::V3(V3HopInfo {
            pool_address: p0.pool,
            token0_address: p0.token0,
            token1_address: p0.token1,
            fee: p0.fee,
            zfo: true, // weth(token0) -> t(token1)
        }),
        HopInfo::V4(V4HopInfo {
            pool_manager_address: h.pool_manager,
            pool_id_hex: "0x0".into(),
            currency0_address: a.currency0,
            currency1_address: a.currency1,
            fee: a.fee,
            tick_spacing: a.tick_spacing,
            hook_address: Address::ZERO,
            zfo: true,
        }),
    ]);
    let outcome = h
        .run_path(&path, optimal_input, &hop_outputs, 8_000_000)
        .unwrap();
    println!("V3-V4 outcome: {outcome:?}  ho0={ho0} weth_out={weth_out} optimal={optimal_input}");
    assert!(
        outcome.executed(2),
        "V3-V4 payload must execute (reach both pools): {outcome:?}"
    );
}

#[test]
fn v4_v3_two_pool_path_executes() {
    let mut h = degenbot_simulation::harness::Harness::new().unwrap();
    let t = h.add_token().unwrap();
    // V4 pool: WETH -> T (token0=WETH, token1=T), price 1.
    let a = h
        .add_v4_pool(
            h.weth,
            t,
            3000,
            60,
            price_one(),
            10u128.pow(22),
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    // V3 pool: T -> WETH (token0=T, token1=WETH), price ~1.02.
    let p1_sqrt = price_one() * U256::from(101u64) / U256::from(100u64);
    let p1 = h
        .add_v3_pool(
            t,
            h.weth,
            3000,
            p1_sqrt,
            10u128.pow(22),
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    let optimal_input = 100_000u128; // WETH into the V4 entry hop
    let ho0 = v3_amount_out(a.sqrt_price, a.liquidity, optimal_input, true, a.fee); // T -> V3 input
    let v3_out = v3_amount_out(p1.sqrt_price, p1.liquidity, ho0, true, p1.fee);
    let hop_outputs = [ho0, v3_out];
    assert!(
        v3_out > optimal_input,
        "must be profitable (v3_out={v3_out} vs optimal={optimal_input})"
    );

    h.fund(h.weth, h.executor, optimal_input * 2).unwrap();
    h.fund(t, h.executor, ho0 * 2).unwrap();

    let path = PathInfo::new(vec![
        HopInfo::V4(V4HopInfo {
            pool_manager_address: h.pool_manager,
            pool_id_hex: "0x0".into(),
            currency0_address: a.currency0,
            currency1_address: a.currency1,
            fee: a.fee,
            tick_spacing: a.tick_spacing,
            hook_address: Address::ZERO,
            zfo: true,
        }),
        HopInfo::V3(V3HopInfo {
            pool_address: p1.pool,
            token0_address: p1.token0,
            token1_address: p1.token1,
            fee: p1.fee,
            zfo: true, // t(token0) -> weth(token1)
        }),
    ]);
    let outcome = h
        .run_path(&path, optimal_input, &hop_outputs, 8_000_000)
        .unwrap();
    println!("V4-V3 outcome: {outcome:?}  ho0={ho0} v3_out={v3_out} optimal={optimal_input}");
    assert!(
        outcome.executed(2),
        "V4-V3 payload must execute (reach both pools): {outcome:?}"
    );
}

// ── 3-hop funding-topology conversion families ──────────────────────────────
// These are the deferred conversions (v2_v3_v2, v2_v4_v2, v3_v2_v4) plus the
// v3_v4_v2 sibling — the explicit purpose of this harness: prove the composed
// payload executes over the real executor without a command-decode / flash-
// repay failure. WETH -> T1 -> T2 -> WETH through the family's pool mix.

#[test]
fn v2_v3_v2_three_pool_path_executes() {
    let mut h = degenbot_simulation::harness::Harness::new().unwrap();
    let (t1, t2) = (h.add_token().unwrap(), h.add_token().unwrap());
    // pool0 V2: WETH -> T1 (balanced ~1x)
    let p0 = h
        .add_pool(h.weth, t1, 1_000_000_000, 1_000_000_000)
        .unwrap();
    // pool1 V3: T1 -> T2, priced 1.05x (captures the profit)
    let mid = price_one() * U256::from(105u64) / U256::from(100u64);
    let p1 = h
        .add_v3_pool(
            t1,
            t2,
            3000,
            mid,
            10u128.pow(22),
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    // pool2 V2: T2 -> WETH (terminal, balanced ~1x)
    let p2 = h
        .add_pool(t2, h.weth, 1_000_000_000, 1_000_000_000)
        .unwrap();

    let optimal_input = 100_000u128;
    let ho0 = v2_out(reserve_of(p0, h.weth), reserve_of(p0, t1), optimal_input); // WETH->T1
    let ho1 = v3_amount_out(p1.sqrt_price, p1.liquidity, ho0, true, p1.fee); // T1->T2
    let out = v2_out(reserve_of(p2, t2), reserve_of(p2, h.weth), ho1); // T2->WETH
    let hop_outputs = [ho0, ho1, out];
    assert!(
        out > optimal_input,
        "must be profitable (out={out} vs optimal={optimal_input})"
    );

    h.fund(h.weth, h.executor, optimal_input * 2).unwrap();
    h.fund(t1, h.executor, ho0 * 2).unwrap();
    h.fund(t2, h.executor, ho1 * 2).unwrap();
    h.executor_approve_pair(p0).unwrap();
    h.executor_approve_pair(p2).unwrap();

    let path = PathInfo::new(vec![
        HopInfo::V2(V2HopInfo {
            pool_address: p0.pair,
            token0_address: p0.token0,
            token1_address: p0.token1,
            fee: 30,
            zfo: v2_zfo(p0, h.weth),
        }),
        HopInfo::V3(V3HopInfo {
            pool_address: p1.pool,
            token0_address: p1.token0,
            token1_address: p1.token1,
            fee: p1.fee,
            zfo: true,
        }),
        HopInfo::V2(V2HopInfo {
            pool_address: p2.pair,
            token0_address: p2.token0,
            token1_address: p2.token1,
            fee: 30,
            zfo: v2_zfo(p2, t2),
        }),
    ]);
    let outcome = h
        .run_path(&path, optimal_input, &hop_outputs, 8_000_000)
        .unwrap();
    println!(
        "V2-V3-V2 outcome: {outcome:?}  ho0={ho0} ho1={ho1} out={out} optimal={optimal_input}"
    );
    assert!(
        outcome.executed(3),
        "V2-V3-V2 payload must execute: {outcome:?}"
    );
}

#[test]
fn v2_v4_v2_three_pool_path_executes() {
    let mut h = degenbot_simulation::harness::Harness::new().unwrap();
    let (t1, t2) = (h.add_token().unwrap(), h.add_token().unwrap());
    // pool0 V2: WETH -> T1 (balanced ~1x)
    let p0 = h
        .add_pool(h.weth, t1, 1_000_000_000, 1_000_000_000)
        .unwrap();
    // pool1 V4: T1 -> T2, priced 1.05x (captures the profit)
    let mid = price_one() * U256::from(105u64) / U256::from(100u64);
    let a = h
        .add_v4_pool(
            t1,
            t2,
            3000,
            60,
            mid,
            10u128.pow(22),
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    // pool2 V2: T2 -> WETH (terminal, balanced ~1x)
    let p2 = h
        .add_pool(t2, h.weth, 1_000_000_000, 1_000_000_000)
        .unwrap();

    let optimal_input = 100_000u128;
    let ho0 = v2_out(reserve_of(p0, h.weth), reserve_of(p0, t1), optimal_input);
    let ho1 = v3_amount_out(a.sqrt_price, a.liquidity, ho0, true, a.fee);
    let out = v2_out(reserve_of(p2, t2), reserve_of(p2, h.weth), ho1);
    let hop_outputs = [ho0, ho1, out];
    assert!(out > optimal_input, "must be profitable (out={out})");

    h.fund(h.weth, h.executor, optimal_input * 2).unwrap();
    h.fund(t1, h.executor, ho0 * 2).unwrap();
    h.fund(t2, h.executor, ho1 * 2).unwrap();
    h.executor_approve_pair(p0).unwrap();
    h.executor_approve_pair(p2).unwrap();

    let path = PathInfo::new(vec![
        HopInfo::V2(V2HopInfo {
            pool_address: p0.pair,
            token0_address: p0.token0,
            token1_address: p0.token1,
            fee: 30,
            zfo: v2_zfo(p0, h.weth),
        }),
        HopInfo::V4(V4HopInfo {
            pool_manager_address: h.pool_manager,
            pool_id_hex: "0x0".into(),
            currency0_address: a.currency0,
            currency1_address: a.currency1,
            fee: a.fee,
            tick_spacing: a.tick_spacing,
            hook_address: Address::ZERO,
            zfo: true,
        }),
        HopInfo::V2(V2HopInfo {
            pool_address: p2.pair,
            token0_address: p2.token0,
            token1_address: p2.token1,
            fee: 30,
            zfo: v2_zfo(p2, t2),
        }),
    ]);
    let outcome = h
        .run_path(&path, optimal_input, &hop_outputs, 8_000_000)
        .unwrap();
    println!(
        "V2-V4-V2 outcome: {outcome:?}  ho0={ho0} ho1={ho1} out={out} optimal={optimal_input}"
    );
    assert!(
        outcome.executed(3),
        "V2-V4-V2 payload must execute: {outcome:?}"
    );
}

#[test]
fn v3_v2_v4_three_pool_path_executes() {
    let mut h = degenbot_simulation::harness::Harness::new().unwrap();
    let (t1, t2) = (h.add_token().unwrap(), h.add_token().unwrap());
    // pool0 V3: WETH -> T1 (price 1)
    let p0 = h
        .add_v3_pool(
            h.weth,
            t1,
            3000,
            price_one(),
            10u128.pow(22),
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    // pool1 V2: T1 -> T2 (balanced ~1x)
    let p1 = h.add_pool(t1, t2, 1_000_000_000, 1_000_000_000).unwrap();
    // pool2 V4: T2 -> WETH, priced 1.05x (captures the profit)
    let out_sqrt = price_one() * U256::from(105u64) / U256::from(100u64);
    let a = h
        .add_v4_pool(
            t2,
            h.weth,
            3000,
            60,
            out_sqrt,
            10u128.pow(22),
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();

    let optimal_input = 100_000u128;
    let ho0 = v3_amount_out(p0.sqrt_price, p0.liquidity, optimal_input, true, p0.fee); // WETH->T1
    let ho1 = v2_out(reserve_of(p1, t1), reserve_of(p1, t2), ho0); // T1->T2
    let out = v3_amount_out(a.sqrt_price, a.liquidity, ho1, true, a.fee); // T2->WETH
    let hop_outputs = [ho0, ho1, out];
    assert!(out > optimal_input, "must be profitable (out={out})");

    h.fund(h.weth, h.executor, optimal_input * 2).unwrap();
    h.fund(t1, h.executor, ho0 * 2).unwrap();
    h.fund(t2, h.executor, ho1 * 2).unwrap();
    h.executor_approve_pair(p1).unwrap();

    let path = PathInfo::new(vec![
        HopInfo::V3(V3HopInfo {
            pool_address: p0.pool,
            token0_address: p0.token0,
            token1_address: p0.token1,
            fee: p0.fee,
            zfo: true,
        }),
        HopInfo::V2(V2HopInfo {
            pool_address: p1.pair,
            token0_address: p1.token0,
            token1_address: p1.token1,
            fee: 30,
            zfo: v2_zfo(p1, t1),
        }),
        HopInfo::V4(V4HopInfo {
            pool_manager_address: h.pool_manager,
            pool_id_hex: "0x0".into(),
            currency0_address: a.currency0,
            currency1_address: a.currency1,
            fee: a.fee,
            tick_spacing: a.tick_spacing,
            hook_address: Address::ZERO,
            zfo: true,
        }),
    ]);
    let outcome = h
        .run_path(&path, optimal_input, &hop_outputs, 8_000_000)
        .unwrap();
    println!(
        "V3-V2-V4 outcome: {outcome:?}  ho0={ho0} ho1={ho1} out={out} optimal={optimal_input}"
    );
    assert!(
        outcome.executed(3),
        "V3-V2-V4 payload must execute: {outcome:?}"
    );
}

#[test]
fn v3_v4_v2_three_pool_path_executes() {
    let mut h = degenbot_simulation::harness::Harness::new().unwrap();
    let (t1, t2) = (h.add_token().unwrap(), h.add_token().unwrap());
    // pool0 V3: WETH -> T1 (price 1)
    let p0 = h
        .add_v3_pool(
            h.weth,
            t1,
            3000,
            price_one(),
            10u128.pow(22),
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    // pool1 V4: T1 -> T2, priced 1.05x (captures the profit)
    let mid = price_one() * U256::from(105u64) / U256::from(100u64);
    let a = h
        .add_v4_pool(
            t1,
            t2,
            3000,
            60,
            mid,
            10u128.pow(22),
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    // pool2 V2: T2 -> WETH (terminal, balanced ~1x)
    let p2 = h
        .add_pool(t2, h.weth, 1_000_000_000, 1_000_000_000)
        .unwrap();

    let optimal_input = 100_000u128;
    let ho0 = v3_amount_out(p0.sqrt_price, p0.liquidity, optimal_input, true, p0.fee);
    let ho1 = v3_amount_out(a.sqrt_price, a.liquidity, ho0, true, a.fee);
    let out = v2_out(reserve_of(p2, t2), reserve_of(p2, h.weth), ho1);
    let hop_outputs = [ho0, ho1, out];
    assert!(out > optimal_input, "must be profitable (out={out})");

    h.fund(h.weth, h.executor, optimal_input * 2).unwrap();
    h.fund(t1, h.executor, ho0 * 2).unwrap();
    h.fund(t2, h.executor, ho1 * 2).unwrap();
    h.executor_approve_pair(p2).unwrap();

    let path = PathInfo::new(vec![
        HopInfo::V3(V3HopInfo {
            pool_address: p0.pool,
            token0_address: p0.token0,
            token1_address: p0.token1,
            fee: p0.fee,
            zfo: true,
        }),
        HopInfo::V4(V4HopInfo {
            pool_manager_address: h.pool_manager,
            pool_id_hex: "0x0".into(),
            currency0_address: a.currency0,
            currency1_address: a.currency1,
            fee: a.fee,
            tick_spacing: a.tick_spacing,
            hook_address: Address::ZERO,
            zfo: true,
        }),
        HopInfo::V2(V2HopInfo {
            pool_address: p2.pair,
            token0_address: p2.token0,
            token1_address: p2.token1,
            fee: 30,
            zfo: v2_zfo(p2, t2),
        }),
    ]);
    let outcome = h
        .run_path(&path, optimal_input, &hop_outputs, 8_000_000)
        .unwrap();
    println!(
        "V3-V4-V2 outcome: {outcome:?}  ho0={ho0} ho1={ho1} out={out} optimal={optimal_input}"
    );
    assert!(
        outcome.executed(3),
        "V3-V4-V2 payload must execute: {outcome:?}"
    );
}

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
use degenbot_executor::composers::{HopInfo, PathInfo, V2HopInfo, V4HopInfo};
use degenbot_simulation::harness::v3_amount_out;

fn price_one() -> U256 {
    U256::from(1u128) << 96
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

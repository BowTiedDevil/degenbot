#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::doc_markdown
)]
//! Executor grammar harness — V3-involving permutations (UQOAHA), the next
//! matrix slice after the all-V2 scaffold.
//!
//! Adds the minimal `PoolV3` stub and proves the mixed 2-hop families the
//! funding-topology conversions depend on: `v2_v3` and `v3_v2`. These exercise
//! the executor's `V3_SWAP_COMPACT` exact-input path + the `uniswapV3SwapCallback`
//! custody (the flash/repay ordering — the very risk byte-parity can't see).

use alloy::primitives::{Address, U256};
use degenbot_executor::composers::{HopInfo, PathInfo, V2HopInfo, V3HopInfo};
use degenbot_simulation::harness::{v3_amount_out, Harness, V2Pool};

fn v2_out(r_in: u128, r_out: u128, amount_in: u128) -> u128 {
    use degenbot_v2_math::IntHopState;
    IntHopState::new(U256::from(r_in), U256::from(r_out), 997, 1000)
        .swap(U256::from(amount_in))
        .unwrap()
        .to::<u128>()
}
fn zfo_v2(pool: V2Pool, src: Address) -> bool {
    src == pool.token0
}

/// price = 1 (token1 per token0), Q64.96.
fn price_one() -> U256 {
    U256::from(1u128) << 96
}

#[test]
fn v2_v3_two_pool_path_executes() {
    let mut h = Harness::new().unwrap();
    let t = h.add_token().unwrap();
    // V2 pool0: WETH -> T (forward-out).
    let p0 = h.add_pool(h.weth, t, 1_000_000, 2_000_000).unwrap();
    // V3 pool1: T -> WETH (token0=T, token1=WETH), price 1.
    let p1 = h
        .add_v3_pool(
            t,
            h.weth,
            3000,
            price_one(),
            10u128.pow(22),
            1_000_000_000,
            1_000_000_000,
        )
        .unwrap();
    let optimal_input = 100_000u128; // WETH
                                     // ho0 = V2 exact-out: WETH -> T on pool0. It becomes the V3 exact-in.
    let ho0 = v2_out(reserve_of(&p0, h.weth), reserve_of(&p0, t), optimal_input);
    // V3 output (WETH) must be >= optimal for the final WETH return transfer.
    let v3_out = v3_amount_out(p1.sqrt_price, p1.liquidity, ho0, true, p1.fee);
    let hop_outputs = [ho0, v3_out]; // ho1 is informational; not routed forward

    // Fund the executor with a WETH buffer: the V2 compact's `getAmountIn`
    // can round up +1 wei over the by-the-book optimal, so an exact balance
    // underflows the Token's checked `-=` (Panic 0x11). A margin avoids the
    // 1-wei shortfall class; the V3 leg returns >= optimal to cover the final
    // WETH return. Also give a T margin in case a pull lands above ho0.
    h.fund(h.weth, h.executor, optimal_input * 2).unwrap();
    h.fund(t, h.executor, ho0 * 2).unwrap();
    h.executor_approve_pair(p0).unwrap();

    let path = PathInfo::new(vec![
        HopInfo::V2(V2HopInfo {
            pool_address: p0.pair,
            token0_address: p0.token0,
            token1_address: p0.token1,
            fee: 30,
            zfo: zfo_v2(p0, h.weth),
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
        .run_path(&path, optimal_input, &hop_outputs, 5_000_000)
        .unwrap();
    println!("V2-V3 outcome: {outcome:?}  ho0={ho0} v3_out={v3_out} optimal={optimal_input}");
    assert!(
        v3_out >= optimal_input,
        "test must be set up so the V3 leg returns >= optimal (got {v3_out})"
    );
    assert!(
        outcome.executed(2),
        "V2-V3 payload must execute (reach both pools): {outcome:?}"
    );
}

#[test]
fn v3_v2_two_pool_path_executes() {
    let mut h = Harness::new().unwrap();
    let t = h.add_token().unwrap();
    // V3 pool0: WETH -> T (token0=WETH, token1=T), selling WETH (token0) for T.
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
    // V2 pool1: T -> WETH (terminal calc).
    let p1 = h.add_pool(t, h.weth, 2_000_000, 1_000_000).unwrap();
    let optimal_input = 100_000u128; // WETH
                                     // ho0 = the V3 exact-in output (forward_out), which funds the V2 calc input.
    let ho0 = v3_amount_out(p0.sqrt_price, p0.liquidity, optimal_input, true, p0.fee);
    let hop_outputs = [ho0, 0];

    // WETH buffer for the executor (the V3 exact-in + any rounding), and a T
    // buffer so the terminal V2 calc's `transferFrom` pull can't underflow.
    h.fund(h.weth, h.executor, optimal_input * 2).unwrap();
    h.fund(t, h.executor, ho0 * 2).unwrap();
    h.executor_approve_pair(p1).unwrap();

    let path = PathInfo::new(vec![
        HopInfo::V3(V3HopInfo {
            pool_address: p0.pool,
            token0_address: p0.token0,
            token1_address: p0.token1,
            fee: p0.fee,
            zfo: true, // weth(token0) -> t(token1)
        }),
        HopInfo::V2(V2HopInfo {
            pool_address: p1.pair,
            token0_address: p1.token0,
            token1_address: p1.token1,
            fee: 30,
            zfo: zfo_v2(p1, t),
        }),
    ]);
    let outcome = h
        .run_path(&path, optimal_input, &hop_outputs, 5_000_000)
        .unwrap();
    println!("V3-V2 outcome: {outcome:?}  ho0={ho0} optimal={optimal_input}");
    assert!(
        outcome.executed(2),
        "V3-V2 payload must execute (reach both pools): {outcome:?}"
    );
}

fn reserve_of(pool: &V2Pool, tok: Address) -> u128 {
    if tok == pool.token0 {
        pool.reserve0
    } else {
        pool.reserve1
    }
}

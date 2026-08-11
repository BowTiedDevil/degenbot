#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::doc_markdown
)]
//! Executor grammar harness — first matrix slice (UQOAHA).
//!
//! We prove the harness scaffold on the **unchanged** all-V2 permutations
//! FIRST (the user directive: "start simple with V2-V2, V2-V2-V2, then extend
//! to all permutations once we understand the broad shape"). All-V2 any-N
//! routes through `encode_all_v2`/`all_v2_walk` (first-hop compact + terminal
//! `V2_SWAP_CALC`) — already-safe encoding — so a GREEN here proves the
//! harness is faithful, not that a bug was fixed.
//!
//! For each case: deploy the real `cmd_executor` + synthesized V2 pools and
//! tokens into a fresh `CacheDB<EmptyDB>`, seed reserves (mint+sync), fund the
//! executor's WETH flash + approvals, encode the path through the production
//! `encode_cmd_stream`, and drive `executor.execute(payload,0x0)`. Assert the
//! payload EXECUTES (reaches every pool, no `UniswapV2: K`) — the exact
//! runtime property byte-parity cannot see.

use alloy::primitives::{Address, U256};
use degenbot_simulation::harness::{Harness, V2Pool};

/// Byte-consistent V2 0.3% getAmountOut via the engine math (mirrors the
/// tier-3 V2 oracle), so `hop_outputs[i]` round-trips the seeded reserves.
fn amt_out(reserve_in: u128, reserve_out: u128, amount_in: u128) -> u128 {
    use degenbot_v2_math::IntHopState;
    IntHopState::new(U256::from(reserve_in), U256::from(reserve_out), 997, 1000)
        .swap(U256::from(amount_in))
        .expect("bounded v2 getAmountOut")
        .to::<u128>()
}

/// zero_for_one relative to the pair's sorted token0.
fn zfo(pool: V2Pool, src: Address) -> bool {
    src == pool.token0
}
/// Seeded reserve of the hop's input token in `pool`.
fn reserve_in(pool: V2Pool, src: Address) -> u128 {
    if src == pool.token0 {
        pool.reserve0
    } else {
        pool.reserve1
    }
}
/// Seeded reserve of the hop's output token in `pool`.
fn reserve_out(pool: V2Pool, dst: Address) -> u128 {
    if dst == pool.token0 {
        pool.reserve0
    } else {
        pool.reserve1
    }
}
fn idx_of(h: &mut Harness, pool: V2Pool) -> usize {
    h.pools.iter().position(|p| p.pair == pool.pair).unwrap()
}

/// Build: WETH + USDC + DAI tokens; pool0 = WETH/USDC, pool1 = USDC/DAI.
fn two_pool_env(r_weth: u128, r_usdc: u128, r_dai: u128) -> (Harness, V2Pool, V2Pool) {
    let mut h = Harness::new().expect("harness builds");
    let usdc = h.add_token().expect("usdc");
    let dai = h.add_token().expect("dai");
    let p0 = h.add_pool(h.weth, usdc, r_weth, r_usdc).expect("pool0");
    let p1 = h.add_pool(usdc, dai, r_usdc, r_dai).expect("pool1");
    (h, p0, p1)
}

#[test]
fn v2_v2_two_pool_path_executes() {
    // `all_v2_walk` always returns WETH to the first pool (the flash currency),
    // so a valid V2-V2 loop is two pools on the SAME pair in inverse directions:
    // WETH -> USDC on pool0 (USDC-rich), USDC -> WETH on pool1 (WETH-rich).
    let mut h = Harness::new().expect("harness builds");
    let usdc = h.add_token().expect("usdc");
    let p0 = h.add_pool(h.weth, usdc, 1_000_000, 2_000_000).unwrap(); // WETH 1M, USDC 2M
    let p1 = h.add_pool(h.weth, usdc, 2_000_000, 1_000_000).unwrap(); // WETH 2M, USDC 1M
    let i0 = idx_of(&mut h, p0);
    let i1 = idx_of(&mut h, p1);

    let optimal_input = 100_000u128;
    let out0 = amt_out(reserve_in(p0, h.weth), reserve_out(p0, usdc), optimal_input); // WETH->USDC
    let out1 = amt_out(reserve_in(p1, usdc), reserve_out(p1, h.weth), out0); // USDC->WETH
    let hop_outputs = [out0, out1];

    h.fund(h.weth, h.executor, optimal_input).unwrap();
    for p in [p0, p1] {
        h.executor_approve_pair(p).unwrap();
    }

    let zfos = [zfo(p0, h.weth), zfo(p1, usdc)];
    let outcome = h
        .run_v2_path(&[i0, i1], &zfos, optimal_input, &hop_outputs, 5_000_000)
        .unwrap();
    println!("V2-V2 outcome: {outcome:?}  hop_outputs={hop_outputs:?} optimal={optimal_input}");
    assert!(
        outcome.executed(2),
        "V2-V2 payload must execute (reach both pools): {outcome:?}"
    );
}

#[test]
fn v2_v2_v2_three_pool_path_executes() {
    let (mut h, p0, p1) = two_pool_env(1_000_000, 2_000_000, 1_000_000);
    let usdc = if p0.token0 == h.weth {
        p0.token1
    } else {
        p0.token0
    };
    let dai = usdc_of_other(p1, usdc);
    // pool2 = DAI/WETH closes the WETH loop.
    let p2 = h.add_pool(dai, h.weth, 1_000_000, 1_000_000).unwrap();
    let i0 = idx_of(&mut h, p0);
    let i1 = idx_of(&mut h, p1);
    let i2 = idx_of(&mut h, p2);

    let optimal_input = 100_000u128;
    let out0 = amt_out(reserve_in(p0, h.weth), reserve_out(p0, usdc), optimal_input);
    let out1 = amt_out(reserve_in(p1, usdc), reserve_out(p1, dai), out0);
    let out2 = amt_out(reserve_in(p2, dai), reserve_out(p2, h.weth), out1);
    let hop_outputs = [out0, out1, out2];

    h.fund(h.weth, h.executor, optimal_input).unwrap();
    for p in [p0, p1, p2] {
        h.executor_approve_pair(p).unwrap();
    }

    let zfos = [zfo(p0, h.weth), zfo(p1, usdc), zfo(p2, dai)];
    let outcome = h
        .run_v2_path(&[i0, i1, i2], &zfos, optimal_input, &hop_outputs, 5_000_000)
        .unwrap();
    println!("V2-V2-V2 outcome: {outcome:?}  hop_outputs={hop_outputs:?} optimal={optimal_input}");
    assert!(
        outcome.executed(3),
        "V2-V2-V2 payload must execute (reach all three pools): {outcome:?}"
    );
}

/// The token in `pool` that is NOT `known` (i.e. the other side of the pool).
fn usdc_of_other(pool: V2Pool, known: Address) -> Address {
    if pool.token0 == known {
        pool.token1
    } else {
        pool.token0
    }
}

//! Prototype test: shared Pool handle for Uniswap V3.

use alloy::primitives::{Address, B256, I256, U128, U256};
use degenbot_pools::registry::PoolEntry;
use degenbot_pools::v3_state::{PoolTickCoverage, RegisterV3PoolParams, V3PoolState};
use degenbot_pools::TickInfo;
use degenbot_pools::{ConcentratedLiquidityVariant, Identity, Pool, Structure};
use std::collections::HashMap;

fn make_v3_pool(liquidity: u128) -> PoolEntry {
    let liq_u128 = U128::from(liquidity);
    let liquidity_net = I256::try_from(i128::try_from(liquidity).unwrap()).unwrap();
    let mut tick_data = HashMap::new();
    tick_data.insert(
        -60,
        TickInfo {
            liquidity_gross: liq_u128,
            liquidity_net,
            block: 0,
        },
    );
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: liq_u128,
            liquidity_net: -liquidity_net,
            block: 0,
        },
    );

    let params = RegisterV3PoolParams {
        address: Address::from([0x11u8; 20]),
        token0: Address::from([0xAAu8; 20]),
        token1: Address::from([0xBBu8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::from([0x22u8; 20]),
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity,
        tick: 0,
        tick_data,
        update_block: 100,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        deployer: Address::from([0x22u8; 20]),
        init_hash: B256::default(),
    };
    let (identity, state) = V3PoolState::from_params(params, 8);
    PoolEntry::V3(identity, state)
}

#[test]
fn v3_pool_handle_exposes_structure_identity_cl_view_and_swap() {
    let entry = make_v3_pool(1_000_000u128);

    let pool = Pool::new(&entry);
    assert_eq!(pool.structure(), Structure::ConcentratedLiquidity);
    assert!(matches!(
        pool.identity(),
        Identity::ConcentratedLiquidity {
            variant: ConcentratedLiquidityVariant::UniswapV3,
        }
    ));

    let cl = pool.concentrated_liquidity().expect("CL view");
    assert_eq!(cl.fee(), 3000);
    assert_eq!(cl.tick_spacing(), 60);
    assert_eq!(cl.liquidity(), 1_000_000);
    assert_eq!(cl.tick(), 0);
    assert_eq!(cl.sqrt_price_x96(), U256::from(1u128) << 96);
    assert_eq!(cl.token0(), Address::from([0xAAu8; 20]));
    assert_eq!(cl.token1(), Address::from([0xBBu8; 20]));

    let out = pool
        .calculate_tokens_out(true, U256::from(1_000))
        .expect("computable");
    assert!(out > U256::ZERO);
}

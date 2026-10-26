//! Prototype test: shared Pool handle for Uniswap V4.

#![expect(clippy::expect_used)]
use alloy::primitives::{Address, U256};
use degenbot_decoders::v4_swap_decoder::V4PoolId;
use degenbot_pools::sim::registry::PoolEntry;
use degenbot_pools::state::v3_state::PoolTickCoverage;
use degenbot_pools::state::v4_state::{RegisterV4PoolParams, V4PoolKey, V4PoolState};
use degenbot_pools::TickInfo;
use degenbot_pools::{ConcentratedLiquidityVariant, Identity, Pool, Structure};
use hashbrown::HashMap;

fn make_v4_pool(liquidity: u128) -> PoolEntry {
    let liq_u128 = alloy::primitives::U128::from(liquidity);
    let liquidity_net = i128::try_from(liquidity).unwrap_or(0);
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

    let params = RegisterV4PoolParams {
        pool_manager: Address::from([0x11u8; 20]),
        pool_id: V4PoolId::default(),
        pool_key: V4PoolKey {
            currency0: Address::from([0xAAu8; 20]),
            currency1: Address::from([0xBBu8; 20]),
            fee: 3000,
            tick_spacing: 60,
            hooks: Address::default(),
        },
        state_view: Address::ZERO,
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity,
        tick: 0,
        tick_data,
        update_block: 100,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        known_bitmap_words: Vec::new(),
    };
    let (identity, state) = V4PoolState::from_params(params, 8);
    PoolEntry::V4(Box::new((identity, state)))
}

#[test]
fn v4_pool_handle_exposes_structure_identity_concentrated_liquidity_view_and_swap() {
    let entry = make_v4_pool(1_000_000u128);

    let pool = Pool::new(&entry, 1);
    assert_eq!(pool.structure(), Structure::ConcentratedLiquidity);
    assert!(matches!(
        pool.identity(),
        Identity::ConcentratedLiquidity {
            variant: ConcentratedLiquidityVariant::UniswapV4,
            ..
        }
    ));

    let concentrated_liquidity = pool
        .concentrated_liquidity()
        .expect("concentrated liquidity view");
    assert_eq!(concentrated_liquidity.fee(), 3000);
    assert_eq!(concentrated_liquidity.tick_spacing(), 60);
    assert_eq!(concentrated_liquidity.liquidity(), 1_000_000);
    assert_eq!(concentrated_liquidity.tick(), 0);
    assert_eq!(
        concentrated_liquidity.sqrt_price_x96(),
        U256::from(1u128) << 96
    );
    assert_eq!(concentrated_liquidity.token0(), Address::from([0xAAu8; 20]));
    assert_eq!(concentrated_liquidity.token1(), Address::from([0xBBu8; 20]));

    let out = pool
        .calculate_tokens_out(true, U256::from(1_000))
        .expect("computable");
    assert!(out > U256::ZERO);
}

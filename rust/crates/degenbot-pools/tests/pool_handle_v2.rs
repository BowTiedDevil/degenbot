//! Prototype test: shared Pool handle for V2 (RED → GREEN).

use alloy::primitives::{aliases::U112, Address, B256, U256};
use degenbot_pools::registry::PoolEntry;
use degenbot_pools::state_history::{ReorgJournal, V2BlockDelta};
use degenbot_pools::v2_state::{V2PoolIdentity, V2PoolState};
use degenbot_pools::{Identity, Pool, ReservePairVariant, Structure};
use degenbot_uniswap::dex_identity::DexVariant;

fn make_v2_pool(reserve0: u128, reserve1: u128) -> PoolEntry {
    let identity = V2PoolIdentity {
        address: Address::from([0x11u8; 20]),
        token0: Address::from([0xAAu8; 20]),
        token1: Address::from([0xBBu8; 20]),
        fee_token0: (997, 1000),
        fee_token1: (997, 1000),
        factory: Address::from([0x22u8; 20]),
        deployer: Address::from([0x22u8; 20]),
        init_hash: B256::default(),
        variant: DexVariant::UniswapV2,
        stable_swap: false,
        fee_denominator: None,
    };
    let state = V2PoolState {
        reserve0: U112::from(reserve0),
        reserve1: U112::from(reserve1),
        update_block: 100,
        journal: ReorgJournal::<V2BlockDelta>::new(8),
    };
    PoolEntry::V2(identity, state)
}

#[test]
fn v2_pool_handle_exposes_structure_identity_and_swap() {
    let entry = make_v2_pool(1_000_000_000, 2_000_000_000);

    let pool = Pool::new(&entry);
    assert_eq!(pool.structure(), Structure::ReservePair);
    assert!(matches!(
        pool.identity(),
        Identity::ReservePair {
            variant: ReservePairVariant::UniswapV2,
        }
    ));

    let rp = pool.reserve_pair().expect("reserve pair");
    assert_eq!(rp.reserve0(), U256::from(1_000_000_000));
    assert_eq!(rp.reserve1(), U256::from(2_000_000_000));

    let out = pool
        .calculate_tokens_out(true, U256::from(1_000_000))
        .expect("computable");
    assert!(out > U256::ZERO);
}

//! Prototype test: shared Pool handle for Aerodrome V2.

use alloy::primitives::{aliases::U112, Address, U256};
use degenbot_pools::aerodrome_v2_state::{AerodromeV2PoolState, RegisterAerodromeV2PoolParams};
use degenbot_pools::registry::PoolEntry;
use degenbot_pools::{Identity, Pool, ReservePairVariant, Structure};
use degenbot_uniswap::dex_identity::DexVariant;

fn make_aerodrome_v2_pool(reserve0: u128, reserve1: u128, stable: bool) -> PoolEntry {
    let params = RegisterAerodromeV2PoolParams {
        address: Address::from([0x11u8; 20]),
        token0: Address::from([0xAAu8; 20]),
        token1: Address::from([0xBBu8; 20]),
        factory: Address::from([0x22u8; 20]),
        variant: DexVariant::AerodromeV2Volatile,
        stable,
        fee: (3, 10_000),
        reserve0: U112::from(reserve0),
        reserve1: U112::from(reserve1),
        update_block: 100,
    };
    let (identity, state) = AerodromeV2PoolState::from_params(&params, 8);
    PoolEntry::AerodromeV2(identity, state)
}

#[test]
fn aerodrome_pool_handle_exposes_structure_identity_and_reserve_pair() {
    let entry = make_aerodrome_v2_pool(1_000_000_000, 2_000_000_000, false);

    let pool = Pool::new(&entry);
    assert_eq!(pool.structure(), Structure::ReservePair);
    assert!(matches!(
        pool.identity(),
        Identity::ReservePair {
            variant: ReservePairVariant::AerodromeV2 { stable: false },
        }
    ));

    let rp = pool.reserve_pair().expect("reserve pair");
    assert_eq!(rp.reserve0(), U256::from(1_000_000_000));
    assert_eq!(rp.reserve1(), U256::from(2_000_000_000));
    assert_eq!(rp.token0(), Address::from([0xAAu8; 20]));
    assert_eq!(rp.token1(), Address::from([0xBBu8; 20]));

    // Aerodrome volatile swap math dispatches through the Rust solidly math
    // (`calc_exact_in_volatile`) — pure constant-product math, no decimals
    // needed.
    let out = pool
        .calculate_tokens_out(true, U256::from(1_000_000))
        .expect("computable");
    assert!(out > U256::ZERO);
}

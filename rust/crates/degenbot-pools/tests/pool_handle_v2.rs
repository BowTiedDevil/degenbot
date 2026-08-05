//! Prototype test: shared Pool handle for V2 (RED → GREEN).

use alloy::primitives::{aliases::U112, Address, B256, U256};
use degenbot_pools::registry::PoolEntry;
use degenbot_pools::state_history::{ReorgJournal, V2BlockDelta};
use degenbot_pools::v2_state::{V2PoolIdentity, V2PoolState};
use degenbot_pools::{Identity, Pool, ReservePairVariant, Structure};
use degenbot_uniswap::dex_identity::DexVariant;

fn make_v2_pool(factory: Address, reserve0: u128, reserve1: u128) -> PoolEntry {
    let identity = V2PoolIdentity {
        address: Address::from([0x11u8; 20]),
        token0: Address::from([0xAAu8; 20]),
        token1: Address::from([0xBBu8; 20]),
        fee_token0: (997, 1000),
        fee_token1: (997, 1000),
        factory,
        deployer: factory,
        init_hash: B256::default(),
        variant: DexVariant::UniswapV2,
        stable_swap: false,
        fee_denominator: None,
    };
    let state = V2PoolState {
        reserve0: U112::from(reserve0),
        reserve1: U112::from(reserve1),
        update_block: 100,
        state_nonce: 0,
        journal: ReorgJournal::<V2BlockDelta>::new(8),
    };
    PoolEntry::V2(identity, state)
}

#[test]
fn v2_pool_handle_exposes_structure_identity_and_swap() {
    let entry = make_v2_pool(Address::from([0x22u8; 20]), 1_000_000_000, 2_000_000_000);

    let pool = Pool::new(&entry, 1);
    assert_eq!(pool.structure(), Structure::ReservePair);
    assert!(matches!(
        pool.identity(),
        Identity::ReservePair {
            variant: ReservePairVariant::UniswapV2,
            ..
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

/// Aerodrome-style DEX-name resolution: a V2 pool whose `(chain_id, factory)`
/// matches a known deployment resolves the DEX name on `Identity::dex`
/// (QHGN2E). Uses the `SushiSwap` V2 mainnet factory (chain 1) → `SushiSwap`.
#[test]
fn v2_resolves_sushiswap_dex_name_from_known_deployment() {
    // SushiSwap V2 mainnet factory.
    let sushi_factory =
        Address::parse_checksummed("0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac", None).unwrap();
    let entry = make_v2_pool(sushi_factory, 1_000_000, 2_000_000);

    let pool = Pool::new(&entry, 1);
    let identity = pool.identity();
    assert_eq!(
        identity,
        Identity::ReservePair {
            variant: ReservePairVariant::UniswapV2,
            dex: Some(degenbot_uniswap::dex_identity::DexName::SushiSwap),
        }
    );
}

/// Unknown deployment degrades gracefully: a factory not in `deployments.json`
/// yields `dex: None` (generic variant), never an error (QHGN2E).
#[test]
fn v2_unknown_deployment_resolves_none_dex() {
    // Synthetic factory absent from deployments.json.
    let unknown =
        Address::parse_checksummed("0x1111111111111111111111111111111111111111", None).unwrap();
    let entry = make_v2_pool(unknown, 1_000_000, 2_000_000);

    let pool = Pool::new(&entry, 1);
    let identity = pool.identity();
    assert_eq!(
        identity,
        Identity::ReservePair {
            variant: ReservePairVariant::UniswapV2,
            dex: None,
        }
    );
}

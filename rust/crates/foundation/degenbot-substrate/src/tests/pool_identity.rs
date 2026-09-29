//! Core-derived address/pool identity — `BotState::pool_id_for_identity`.
//!
//! A driver that holds a pool's IDENTITY (a family tag plus an address, or a
//! V4 `(PoolManager, pool_id)` pair) needs the id the engine hops on. The
//! answer is derived from the registration tables `BotState` already owns, so
//! no driver keeps a second pool-id map that can disagree with the state
//! owner. These tests pin that derivation: the right id for a registered
//! identity, `None` for a family that is not what is registered there, and
//! `None` for an unregistered identity — plus the unregister case, which must
//! withdraw the derived id rather than leave a stale one behind.

use super::*;

use crate::session_registry::PoolIdentity;
use degenbot_decoders::v4_swap_decoder::V4PoolId;

use super::session_registry::v4_params;

fn v3_params(address: Address) -> RegisterV3PoolParams {
    RegisterV3PoolParams {
        address,
        token0: make_token0(),
        token1: make_token1(),
        fee: 3_000,
        tick_spacing: 60,
        factory: make_factory(),
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
        ..Default::default()
    }
}

fn aerodrome_params(address: Address) -> RegisterAerodromeV2PoolParams {
    RegisterAerodromeV2PoolParams {
        address,
        token0: make_token0(),
        token1: make_token1(),
        reserve0: U112::from(1000),
        reserve1: U112::from(2000),
        factory: make_factory(),
        variant: degenbot_uniswap::dex_identity::DexVariant::AerodromeV2Volatile,
        fee: FEE_03,
        token0_decimals: 18,
        token1_decimals: 18,
        update_block: 0,
        stable: false,
    }
}

/// The address-keyed families answer from the address index: registering a
/// pool is what makes its identity resolvable, and the resolved id is the
/// one `BotState` allocated.
#[test]
fn address_keyed_identities_resolve_to_the_registered_pool_id() {
    let mut state = BotState::new();
    let v2_id = state
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");
    let v3_address = Address::from([0x11; 20]);
    let v3_id = state
        .register_v3_pool(&v3_params(v3_address))
        .expect("test setup: V3 registration");
    let aero_address = Address::from([0x22; 20]);
    let aero_id = state.register_aerodrome_pool(&aerodrome_params(aero_address));

    assert_eq!(
        state.pool_id_for_identity(&PoolIdentity::v2(make_pool_addr())),
        Some(v2_id)
    );
    assert_eq!(
        state.pool_id_for_identity(&PoolIdentity::v3(v3_address)),
        Some(v3_id)
    );
    assert_eq!(
        state.pool_id_for_identity(&PoolIdentity::AerodromeV2(aero_address)),
        Some(aero_id)
    );
    assert_eq!(
        state.pool_id_for_identity(&PoolIdentity::Curve(make_pool_addr())),
        None,
        "an unregistered family at a registered address has no id"
    );
    assert_eq!(
        state.pool_id_for_identity(&PoolIdentity::v2(Address::from([0x33; 20]))),
        None,
        "an unregistered address has no id"
    );
}

/// A V4 pool is keyed by its `(PoolManager, pool_id)` pair, never by the
/// manager address alone: one manager hosts many pools, and a query for the
/// wrong pair — or naming the pair as if it were an address-keyed identity —
/// resolves to nothing.
#[test]
fn v4_identity_resolves_by_the_manager_and_pool_id_pair() {
    let mut state = BotState::new();
    let manager = Address::from([0x44; 20]);
    let pool_id = V4PoolId::from([0x55; 32]);
    let other_pool_id = V4PoolId::from([0x66; 32]);
    let registered = state
        .register_v4_pool(&v4_params(manager, pool_id))
        .expect("test setup: V4 registration");

    assert_eq!(
        state.pool_id_for_identity(&PoolIdentity::v4(manager, pool_id)),
        Some(registered)
    );
    assert_eq!(
        state.pool_id_for_identity(&PoolIdentity::v4(manager, other_pool_id)),
        None,
        "the same manager with a different pool id is a different pool"
    );
    assert_eq!(
        state.pool_id_for_identity(&PoolIdentity::v3(manager)),
        None,
        "a PoolManager address is not a pool address"
    );
}

/// The derived id is the live registration, not a snapshot of it: an
/// unregistration withdraws the answer, so a driver cannot hop on a retired
/// id.
#[test]
fn unregistration_withdraws_the_derived_identity() {
    let mut state = BotState::new();
    state
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");
    assert!(state
        .pool_id_for_identity(&PoolIdentity::v2(make_pool_addr()))
        .is_some());

    state.unregister_pool(make_pool_addr(), None);

    assert_eq!(
        state.pool_id_for_identity(&PoolIdentity::v2(make_pool_addr())),
        None
    );
}

//! The pool table's invariant suite — written once, holding for every family.
//!
//! Before the table owned identity, each registration arm carried its own copy
//! of "bump the id, insert the entry, bind the key" and its own duplicate
//! policy, and the per-family tests pinned each copy separately: a fix in one
//! arm said nothing about the other six. These tests state the invariants at
//! the ONE place that owns them and drive every family through it, so an arm
//! that stops going through the table fails here.
//!
//! Four invariants, one test each: the key/index/entry bijection, monotonic
//! ids with no reuse, the one duplicate refusal, and the quarantine transition
//! that dirties the pool's state nonce.

use super::*;
use crate::pool_table::PoolClaim;
use crate::registration_door::{PoolRegistration, RegistrationVerdict};
use crate::PoolLookup;
use degenbot_decoders::v4_swap_decoder::V4PoolId;
use degenbot_pools::RegisteredPoolFamily;

// ---------------------------------------------------------------------------
// One registration per family: the request, the identity it claims, its family.
// ---------------------------------------------------------------------------

fn v2_registration(address: Address) -> PoolRegistration {
    PoolRegistration::V2(Box::new(RegisterV2PoolParams {
        address,
        token0: make_token0(),
        token1: make_token1(),
        reserve0: U112::from(1_000),
        reserve1: U112::from(2_000),
        fee_token0: FEE_03,
        fee_token1: FEE_03,
        factory: make_factory(),
        update_block: 0,
        variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
        stable_swap: false,
        fee_denominator: None,
        ..Default::default()
    }))
}

fn v3_registration(address: Address) -> PoolRegistration {
    PoolRegistration::V3(Box::new(RegisterV3PoolParams {
        address,
        token0: make_token0(),
        token1: make_token1(),
        fee: 3_000,
        tick_spacing: 60,
        factory: make_factory(),
        sqrt_price_x96: U256::from(1u64) << 96,
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 4,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
        ..Default::default()
    }))
}

const V4_MANAGER: Address = Address::new([0x44; 20]);
const V4_POOL_ID: V4PoolId = [0xee; 32];

fn v4_registration() -> PoolRegistration {
    PoolRegistration::V4(Box::new(RegisterV4PoolParams {
        pool_manager: V4_MANAGER,
        pool_id: V4_POOL_ID,
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::from([0x01; 20]),
            fee: 500,
            tick_spacing: 10,
            hooks: Address::ZERO,
        },
        state_view: Address::ZERO,
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 6,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
        known_bitmap_words: Vec::new(),
    }))
}

fn curve_params(address: Address) -> RegisterCurvePoolParams {
    RegisterCurvePoolParams {
        address,
        tokens: vec![Address::ZERO, Address::from([0x01; 20])],
        a_coefficient: 100,
        a_precision: 100,
        fee: 4_000_000,
        admin_fee: 0,
        rate_multipliers: vec![U256::from(1u64), U256::from(1u64)],
        balances: vec![U256::from(1_000_000u64), U256::from(1_000_000u64)],
        update_block: 3,
        swap_style: 1,
        lending_rate_style: 1,
        d_variant: 1,
        y_variant: 1,
        yd_variant: 1,
        base_pool: None,
        initial_a_coefficient: None,
        future_a_coefficient: None,
        initial_a_coefficient_time: None,
        future_a_coefficient_time: None,
        create_timestamp: None,
        fee_gamma: None,
        mid_fee: None,
        offpeg_fee_multiplier: None,
        out_fee: None,
        gamma: None,
        lp_token: None,
        use_lending: vec![false, false],
        precision_multipliers: vec![U256::from(1u64), U256::from(1u64)],
        tokens_underlying: None,
        metapool_rate_style: 1,
        metapool_underlying_style: 1,
        data_provider: None,
    }
}

fn balancer_weighted_params(address: Address) -> RegisterBalancerWeightedPoolParams {
    RegisterBalancerWeightedPoolParams {
        address,
        vault: Address::from([0xa0; 20]),
        pool_id: [0x11; 32],
        tokens: vec![Address::ZERO, Address::from([0x01; 20])],
        weights: vec![
            U256::from(5_000_000_000_000_000_000u128),
            U256::from(5_000_000_000_000_000_000u128),
        ],
        scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
        swap_fee: 1_000_000_000_000_000,
        pow_version: 2,
        balances: vec![U256::from(1_000_000u64), U256::from(1_000_000u64)],
        update_block: 5,
    }
}

fn balancer_stable_params(address: Address) -> RegisterBalancerStablePoolParams {
    RegisterBalancerStablePoolParams {
        address,
        vault: Address::from([0xb0; 20]),
        pool_id: [0x22; 32],
        tokens: vec![Address::ZERO, Address::from([0x01; 20])],
        amp: 100,
        scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
        swap_fee: 1_000_000_000_000_000,
        bpt_idx: None,
        invariant_version: 2,
        balances: vec![U256::from(1_000_000u64), U256::from(1_000_000u64)],
        update_block: 4,
        rate_provider: None,
    }
}

fn aerodrome_params(address: Address) -> RegisterAerodromeV2PoolParams {
    RegisterAerodromeV2PoolParams {
        address,
        token0: make_token0(),
        token1: make_token1(),
        factory: make_factory(),
        variant: degenbot_uniswap::dex_identity::DexVariant::AerodromeV2Volatile,
        fee: FEE_03,
        token0_decimals: 18,
        token1_decimals: 18,
        reserve0: U112::from(1_000),
        reserve1: U112::from(2_000),
        update_block: 2,
        stable: false,
    }
}

/// One registration per family with the identity it claims, at a contract
/// address distinct from its siblings' (so all seven can share one table).
fn every_family() -> Vec<(PoolRegistration, PoolLookup, RegisteredPoolFamily)> {
    let curve_address = Address::from([0xc0; 20]);
    let weighted_address = Address::from([0xb1; 20]);
    let stable_address = Address::from([0xb2; 20]);
    let aerodrome_address = Address::from([0xae; 20]);
    vec![
        (
            v2_registration(make_pool_addr()),
            PoolLookup::from_address(make_pool_addr()),
            RegisteredPoolFamily::V2,
        ),
        (
            v3_registration(Address::from([0x33; 20])),
            PoolLookup::from_address(Address::from([0x33; 20])),
            RegisteredPoolFamily::V3,
        ),
        (
            v4_registration(),
            PoolLookup::V4 {
                pool_manager: V4_MANAGER,
                pool_id: V4_POOL_ID,
            },
            RegisteredPoolFamily::V4,
        ),
        (
            PoolRegistration::Curve(Box::new(curve_params(curve_address))),
            PoolLookup::from_address(curve_address),
            RegisteredPoolFamily::Curve,
        ),
        (
            PoolRegistration::BalancerWeighted(Box::new(balancer_weighted_params(
                weighted_address,
            ))),
            PoolLookup::from_address(weighted_address),
            RegisteredPoolFamily::BalancerWeighted,
        ),
        (
            PoolRegistration::BalancerStable(Box::new(balancer_stable_params(stable_address))),
            PoolLookup::from_address(stable_address),
            RegisteredPoolFamily::BalancerStable,
        ),
        (
            PoolRegistration::AerodromeV2(Box::new(aerodrome_params(aerodrome_address))),
            PoolLookup::from_address(aerodrome_address),
            RegisteredPoolFamily::AerodromeV2,
        ),
    ]
}

/// Register one family's pool through the door, asserting the verdict registers.
fn register_one(state: &mut BotState, registration: PoolRegistration) -> u64 {
    let verdict = state.register_pool(registration);
    verdict
        .pool_id()
        .unwrap_or_else(|| panic!("a first registration must register, got {verdict:?}"))
}

// ---------------------------------------------------------------------------
// 1. The bijection: one key, one id, one entry — in one step.
// ---------------------------------------------------------------------------

#[test]
fn every_family_binds_its_key_entry_and_id_in_one_step() {
    for (registration, key, family) in every_family() {
        let mut state = BotState::new();
        let pool_id = register_one(&mut state, registration);

        assert_eq!(state.pools.len(), 1, "{family:?}: one entry");
        assert_eq!(
            state.pools.pool_id_for(&key),
            Some(pool_id),
            "{family:?}: the claimed key resolves to the allocated id"
        );
        assert_eq!(
            state.pools.get(pool_id).map(PoolEntry::registered_family),
            Some(family),
            "{family:?}: the entry is registered under the claimed family"
        );

        // The id index and both key indexes agree with the entry table: every
        // index value names a registered entry, and every entry is reachable
        // from the key it was claimed under.
        assert!(state
            .pools
            .address_index()
            .all(|(_, &id)| { state.pools.contains(id) }));
        assert!(state
            .pools
            .v4_entries()
            .all(|(_, &id)| state.pools.contains(id)));
        assert_eq!(
            state.pools.address_index().count() + state.pools.v4_count(),
            state.pools.len(),
            "{family:?}: every entry owns exactly one key"
        );
        assert_eq!(
            state.pools.keys().count(),
            state.pools.len(),
            "{family:?}: ids and entries are one table"
        );

        // Releasing the key retires the entry with it (the bijection holds in
        // the other direction too).
        assert_eq!(state.pools.unregister(&key), Some(pool_id));
        assert!(!state.pools.contains(pool_id), "{family:?}: entry retired");
        assert_eq!(
            state.pools.pool_id_for(&key),
            None,
            "{family:?}: key binding retired"
        );
        assert_eq!(state.pools.len(), 0, "{family:?}: no orphan entry");
    }
}

// ---------------------------------------------------------------------------
// 2. Monotonic ids: allocation counts up, a retired id is never reused.
// ---------------------------------------------------------------------------

#[test]
fn ids_are_monotonic_and_a_retired_id_is_never_reused() {
    let mut state = BotState::new();
    let mut ids = Vec::new();
    for (registration, _, family) in every_family() {
        let pool_id = register_one(&mut state, registration);
        assert!(
            ids.last().is_none_or(|highest| pool_id > *highest),
            "{family:?}: allocation is monotonic"
        );
        ids.push(pool_id);
    }

    let first = ids[0];
    let key = PoolLookup::from_address(make_pool_addr());
    assert_eq!(state.pools.unregister(&key), Some(first));

    let fresh = register_one(
        &mut state,
        PoolRegistration::Curve(Box::new(curve_params(Address::from([0xc1; 20])))),
    );
    assert_ne!(fresh, first, "a retired id is never handed out again");
    assert!(
        fresh > *ids.last().expect("ids were allocated"),
        "the counter does not rewind"
    );
}

// ---------------------------------------------------------------------------
// 3. The one duplicate refusal, for every family.
// ---------------------------------------------------------------------------

#[test]
fn the_table_refuses_every_family_duplicate_the_same_way() {
    for (registration, key, family) in every_family() {
        let mut state = BotState::new();
        let pool_id = register_one(&mut state, registration.clone());

        let RegistrationVerdict::Refused {
            family: refused_family,
            refusal,
        } = state.register_pool(registration)
        else {
            panic!("{family:?}: a duplicate must refuse");
        };
        assert_eq!(refused_family, family, "{family:?}: the refused family");
        let RegisterRefusal::AlreadyRegistered {
            address,
            pool_key,
            detail: _,
        } = refusal
        else {
            panic!("{family:?}: a duplicate refuses as AlreadyRegistered, got {refusal:?}");
        };
        assert_eq!(
            address,
            match key {
                PoolLookup::Address(address) => Some(address),
                PoolLookup::V4 { .. } => None,
            },
            "{family:?}: the refusal names the claimed identity"
        );
        assert_eq!(
            pool_key,
            match key {
                PoolLookup::Address(_) => None,
                PoolLookup::V4 {
                    pool_manager,
                    pool_id,
                } => Some((pool_manager, pool_id)),
            },
            "{family:?}: the refusal names the claimed V4 key"
        );
        assert_eq!(state.pools.len(), 1, "{family:?}: nothing was inserted");
        assert_eq!(
            state.pools.pool_id_for(&key),
            Some(pool_id),
            "{family:?}: the registration of record is untouched"
        );
    }
}

/// The arms whose registration used to `assert!`-panic on a duplicate answer the
/// same typed refusal now — the table refuses before any arm can panic.
#[test]
fn the_panic_shaped_arms_answer_the_typed_refusal() {
    let mut state = BotState::new();
    let curve = curve_params(Address::from([0xc2; 20]));
    let weighted = balancer_weighted_params(Address::from([0xb3; 20]));
    let stable = balancer_stable_params(Address::from([0xb4; 20]));

    state
        .register_curve_pool(&curve)
        .expect("first Curve registration");
    state
        .register_balancer_weighted_pool(&weighted)
        .expect("first Balancer weighted registration");
    state
        .register_balancer_stable_pool(&stable)
        .expect("first Balancer stable registration");

    for refusal in [
        state.register_curve_pool(&curve).unwrap_err(),
        state
            .register_balancer_weighted_pool(&weighted)
            .unwrap_err(),
        state.register_balancer_stable_pool(&stable).unwrap_err(),
    ] {
        assert!(
            matches!(refusal, RegisterRefusal::AlreadyRegistered { .. }),
            "a duplicate answers AlreadyRegistered, got {refusal:?}"
        );
    }
    assert_eq!(state.pools.len(), 3, "no arm inserted a second pool");
}

/// The table's own refusal and the direct arm's refusal are one policy: an arm
/// cannot answer a duplicate differently from the table it registers through.
#[test]
fn an_arm_claim_is_the_tables_claim() {
    let mut state = BotState::new();
    let address = Address::from([0xc3; 20]);
    let params = curve_params(address);
    let pool_id = state
        .register_curve_pool(&params)
        .expect("first Curve registration");

    let claim = PoolClaim::new(
        PoolLookup::from_address(address),
        RegisteredPoolFamily::Curve,
    );
    assert_eq!(state.pools.pool_id_for(&claim.key), Some(pool_id));
}

// ---------------------------------------------------------------------------
// 4. The quarantine transition: one body, both directions, nonce bumped.
// ---------------------------------------------------------------------------

#[test]
fn the_quarantine_transition_dirties_the_nonce_for_every_family() {
    for (registration, _, family) in every_family() {
        let mut state = BotState::new();
        let pool_id = register_one(&mut state, registration);

        assert!(
            !state.pools.quarantine(u64::MAX),
            "{family:?}: unknown pool"
        );
        assert!(!state.pools.is_quarantined(pool_id));

        let registered = state.pool_state_nonce(pool_id);
        assert!(
            state.pools.quarantine(pool_id),
            "{family:?}: first transition"
        );
        assert!(state.pools.is_quarantined(pool_id));
        assert_eq!(state.pools.quarantined_count(), 1, "{family:?}: depth");
        let quarantined = state.pool_state_nonce(pool_id);
        assert_ne!(
            quarantined, registered,
            "{family:?}: quarantine dirties the nonce"
        );

        assert!(
            !state.pools.quarantine(pool_id),
            "{family:?}: re-quarantine is a no-op"
        );
        assert_eq!(
            state.pool_state_nonce(pool_id),
            quarantined,
            "{family:?}: a no-op transition does not dirty the nonce"
        );

        assert!(
            state.pools.release(pool_id),
            "{family:?}: release transition"
        );
        assert!(!state.pools.is_quarantined(pool_id));
        assert_eq!(state.pools.quarantined_count(), 0, "{family:?}: depth");
        assert_ne!(
            state.pool_state_nonce(pool_id),
            quarantined,
            "{family:?}: release dirties the nonce too, so a stale quarantined \
             projection cannot stick"
        );
        assert!(!state.pools.release(pool_id), "{family:?}: double release");
    }
}

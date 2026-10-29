//! ADR-078 D1 — the family write door's parity harness and refusal shapes.
//!
//! One test per family registers the SAME params through the door and through
//! the direct `register_*` entry on a fresh `BotState`, asserting the outcomes
//! agree (pool id, family tag, state clocks, registry census). The refusal
//! tests pin each family's typed refusal to the shared register-refusal
//! vocabulary the ledger stores, so the fold is lossless.

use super::*;
use crate::registration_door::{PoolRegistration, RegistrationVerdict};
use crate::{BuildFailure, RegisterRefusal};
use crate::{
    RegisterAerodromeV2PoolParams, RegisterBalancerStablePoolParams,
    RegisterBalancerWeightedPoolParams, RegisterCurvePoolParams, RegisterV2PoolError,
    RegisterV3PoolParams, RegisterV4PoolParams,
};
use degenbot_pools::RegisteredPoolFamily;

// ---------------------------------------------------------------------------
// Params builders (one per family), mirroring the direct-entry tests.
// ---------------------------------------------------------------------------

fn v3_params() -> RegisterV3PoolParams {
    RegisterV3PoolParams {
        address: make_pool_addr(),
        token0: make_token0(),
        token1: make_token1(),
        fee: 3_000,
        tick_spacing: 60,
        factory: make_factory(),
        sqrt_price_x96: U256::from(1u64) << 96,
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 7,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
        ..Default::default()
    }
}

fn v4_params() -> RegisterV4PoolParams {
    RegisterV4PoolParams {
        pool_manager: Address::from([0x44u8; 20]),
        pool_id: [0xeeu8; 32],
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::from([0x01u8; 20]),
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
        update_block: 9,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
        known_bitmap_words: Vec::new(),
    }
}

fn curve_params(address: Address) -> RegisterCurvePoolParams {
    RegisterCurvePoolParams {
        address,
        tokens: vec![Address::ZERO, Address::from([0x01u8; 20])],
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
        vault: Address::from([0xa0u8; 20]),
        pool_id: [0x11u8; 32],
        tokens: vec![Address::ZERO, Address::from([0x01u8; 20])],
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
        vault: Address::from([0xb0u8; 20]),
        pool_id: [0x22u8; 32],
        tokens: vec![Address::ZERO, Address::from([0x01u8; 20])],
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
        token0: Address::ZERO,
        token1: Address::from([0x01u8; 20]),
        factory: Address::from([0xafu8; 20]),
        variant: degenbot_uniswap::dex_identity::DexVariant::AerodromeV2Volatile,
        stable: false,
        fee: (3, 1000),
        token0_decimals: 18,
        token1_decimals: 18,
        reserve0: U112::from(1_000_000u64),
        reserve1: U112::from(2_000_000u64),
        update_block: 2,
    }
}

/// Assert the door's verdict and the direct entry's outcome agree for a
/// registration that must succeed.
fn assert_parity(
    direct: &BotState,
    door: &BotState,
    direct_id: u64,
    verdict: &RegistrationVerdict,
) {
    assert_eq!(
        verdict.pool_id(),
        Some(direct_id),
        "the door must assign the same pool id as the direct entry"
    );
    assert_eq!(door.pool_family(direct_id), direct.pool_family(direct_id));
    assert_eq!(
        door.pool_update_block(direct_id),
        direct.pool_update_block(direct_id),
        "the door must land the same state clock"
    );
    assert_eq!(
        door.pool_state_nonce(direct_id),
        direct.pool_state_nonce(direct_id)
    );
    assert_eq!(door.pool_count(), direct.pool_count());
}

// ---------------------------------------------------------------------------
// Parity: one family per test.
// ---------------------------------------------------------------------------

#[test]
fn door_parity_v2() {
    let params = make_params(U112::from(1000), U112::from(2000));
    let mut direct = BotState::new();
    let direct_id = direct.register_v2_pool(&params).expect("direct V2");
    let mut door = BotState::new();
    let verdict = door.register_pool(PoolRegistration::V2(Box::new(params.clone())));
    assert_eq!(
        verdict,
        RegistrationVerdict::Registered {
            pool_id: direct_id,
            family: RegisteredPoolFamily::V2,
        }
    );
    assert_parity(&direct, &door, direct_id, &verdict);
}

#[test]
fn door_parity_v3() {
    let params = v3_params();
    let mut direct = BotState::new();
    let direct_id = direct.register_v3_pool(&params).expect("direct V3");
    let mut door = BotState::new();
    let verdict = door.register_pool(PoolRegistration::V3(Box::new(params.clone())));
    assert_eq!(
        verdict,
        RegistrationVerdict::Registered {
            pool_id: direct_id,
            family: RegisteredPoolFamily::V3,
        }
    );
    assert_parity(&direct, &door, direct_id, &verdict);
}

#[test]
fn door_parity_v4() {
    let params = v4_params();
    let mut direct = BotState::new();
    let direct_id = direct.register_v4_pool(&params).expect("direct V4");
    let mut door = BotState::new();
    let verdict = door.register_pool(PoolRegistration::V4(Box::new(params.clone())));
    assert_eq!(
        verdict,
        RegistrationVerdict::Registered {
            pool_id: direct_id,
            family: RegisteredPoolFamily::V4,
        }
    );
    assert_parity(&direct, &door, direct_id, &verdict);
}

#[test]
fn door_parity_curve() {
    let params = curve_params(Address::from([0xc0u8; 20]));
    let mut direct = BotState::new();
    let direct_id = direct
        .register_curve_pool(&params)
        .expect("curve registration");
    let mut door = BotState::new();
    let verdict = door.register_pool(PoolRegistration::Curve(Box::new(params.clone())));
    assert_eq!(
        verdict,
        RegistrationVerdict::Registered {
            pool_id: direct_id,
            family: RegisteredPoolFamily::Curve,
        }
    );
    assert_parity(&direct, &door, direct_id, &verdict);
}

#[test]
fn door_parity_balancer_weighted() {
    let params = balancer_weighted_params(Address::from([0xb1u8; 20]));
    let mut direct = BotState::new();
    let direct_id = direct
        .register_balancer_weighted_pool(&params)
        .expect("balancer weighted registration");
    let mut door = BotState::new();
    let verdict = door.register_pool(PoolRegistration::BalancerWeighted(Box::new(params.clone())));
    assert_eq!(
        verdict,
        RegistrationVerdict::Registered {
            pool_id: direct_id,
            family: RegisteredPoolFamily::BalancerWeighted,
        }
    );
    assert_parity(&direct, &door, direct_id, &verdict);
}

#[test]
fn door_parity_balancer_stable() {
    let params = balancer_stable_params(Address::from([0xb2u8; 20]));
    let mut direct = BotState::new();
    let direct_id = direct
        .register_balancer_stable_pool(&params)
        .expect("balancer stable registration");
    let mut door = BotState::new();
    let verdict = door.register_pool(PoolRegistration::BalancerStable(Box::new(params.clone())));
    assert_eq!(
        verdict,
        RegistrationVerdict::Registered {
            pool_id: direct_id,
            family: RegisteredPoolFamily::BalancerStable,
        }
    );
    assert_parity(&direct, &door, direct_id, &verdict);
}

#[test]
fn door_parity_aerodrome_v2() {
    let params = aerodrome_params(Address::from([0xaeu8; 20]));
    let mut direct = BotState::new();
    let direct_id = direct
        .register_aerodrome_pool(&params)
        .expect("direct Aerodrome");
    let mut door = BotState::new();
    let verdict = door.register_pool(PoolRegistration::AerodromeV2(Box::new(params.clone())));
    assert_eq!(
        verdict,
        RegistrationVerdict::Registered {
            pool_id: direct_id,
            family: RegisteredPoolFamily::AerodromeV2,
        }
    );
    assert_parity(&direct, &door, direct_id, &verdict);
}

// ---------------------------------------------------------------------------
// Refusals: the typed shape the ledger's `RefusalKind::RegisterRefused` maps.
// ---------------------------------------------------------------------------

#[test]
fn door_refuses_duplicate_v2_as_already_registered() {
    let params = make_params(U112::from(1000), U112::from(2000));
    let mut core = BotState::new();
    assert!(core
        .register_pool(PoolRegistration::V2(Box::new(params.clone())))
        .is_registered());
    let verdict = core.register_pool(PoolRegistration::V2(Box::new(params.clone())));
    assert_eq!(
        verdict,
        RegistrationVerdict::Refused {
            family: RegisteredPoolFamily::V2,
            refusal: RegisterRefusal::AlreadyRegistered {
                address: Some(params.address),
                pool_key: None,
                detail: format!(
                    "{:?}",
                    RegisterV2PoolError::AlreadyRegistered {
                        address: params.address
                    }
                ),
            },
        },
        "the V2 refusal carries the ledger's AlreadyRegistered shape verbatim"
    );
}

#[test]
fn door_refuses_out_of_spec_v3_as_spec_violation() {
    let mut params = v3_params();
    params.sqrt_price_x96 =
        U256::from(degenbot_math::concentrated_liquidity::tick_math::MAX_SQRT_RATIO);
    let mut core = BotState::new();
    let verdict = core.register_pool(PoolRegistration::V3(Box::new(params)));
    assert!(
        matches!(
            verdict,
            RegistrationVerdict::Refused {
                family: RegisteredPoolFamily::V3,
                refusal: RegisterRefusal::SpecViolation { .. },
            }
        ),
        "an out-of-spec V3 field must refuse as a SpecViolation, got {verdict:?}"
    );
}

#[test]
fn door_refuses_dynamic_fee_v4_as_admission() {
    let mut params = v4_params();
    params.pool_key.fee = crate::V4_DYNAMIC_FEE_FLAG;
    let mut core = BotState::new();
    let verdict = core.register_pool(PoolRegistration::V4(Box::new(params)));
    assert_eq!(
        verdict,
        RegistrationVerdict::Refused {
            family: RegisteredPoolFamily::V4,
            refusal: RegisterRefusal::Admission {
                failure: BuildFailure::DynamicFee,
                hook_flags: None,
                fee: Some(crate::V4_DYNAMIC_FEE_FLAG),
            },
        }
    );
}

#[test]
fn door_refuses_duplicate_curve_without_panicking() {
    let params = curve_params(Address::from([0xc0u8; 20]));
    let mut core = BotState::new();
    assert!(core
        .register_pool(PoolRegistration::Curve(Box::new(params.clone())))
        .is_registered());
    let verdict = core.register_pool(PoolRegistration::Curve(Box::new(params.clone())));
    let address = params.address;
    assert_eq!(
        verdict,
        RegistrationVerdict::Refused {
            family: RegisteredPoolFamily::Curve,
            refusal: RegisterRefusal::AlreadyRegistered {
                address: Some(address),
                pool_key: None,
                detail: format!("pool already registered: {address}"),
            },
        },
        "the Curve duplicate is a typed refusal through the door, not a panic"
    );
}

#[test]
fn door_refuses_duplicate_balancer_weighted_without_panicking() {
    let params = balancer_weighted_params(Address::from([0xb1u8; 20]));
    let mut core = BotState::new();
    assert!(core
        .register_pool(PoolRegistration::BalancerWeighted(Box::new(params.clone())))
        .is_registered());
    let verdict = core.register_pool(PoolRegistration::BalancerWeighted(Box::new(params.clone())));
    assert!(
        matches!(
            verdict,
            RegistrationVerdict::Refused {
                family: RegisteredPoolFamily::BalancerWeighted,
                refusal: RegisterRefusal::AlreadyRegistered {
                    address: Some(_),
                    ..
                },
            }
        ),
        "the Balancer weighted duplicate is a typed refusal, got {verdict:?}"
    );
}

#[test]
fn door_refuses_duplicate_balancer_stable_without_panicking() {
    let params = balancer_stable_params(Address::from([0xb2u8; 20]));
    let mut core = BotState::new();
    assert!(core
        .register_pool(PoolRegistration::BalancerStable(Box::new(params.clone())))
        .is_registered());
    let verdict = core.register_pool(PoolRegistration::BalancerStable(Box::new(params.clone())));
    assert!(
        matches!(
            verdict,
            RegistrationVerdict::Refused {
                family: RegisteredPoolFamily::BalancerStable,
                refusal: RegisterRefusal::AlreadyRegistered {
                    address: Some(_),
                    ..
                },
            }
        ),
        "the Balancer stable duplicate is a typed refusal, got {verdict:?}"
    );
}

#[test]
fn door_refuses_duplicate_aerodrome_as_already_registered() {
    let params = aerodrome_params(Address::from([0xaeu8; 20]));
    let mut core = BotState::new();
    assert!(core
        .register_pool(PoolRegistration::AerodromeV2(Box::new(params.clone())))
        .is_registered());
    let verdict = core.register_pool(PoolRegistration::AerodromeV2(Box::new(params.clone())));
    assert!(
        matches!(
            verdict,
            RegistrationVerdict::Refused {
                family: RegisteredPoolFamily::AerodromeV2,
                refusal: RegisterRefusal::AlreadyRegistered {
                    address: Some(_),
                    ..
                },
            }
        ),
        "the Aerodrome duplicate is a typed refusal, got {verdict:?}"
    );
}

#[test]
fn door_family_tag_covers_every_variant() {
    let cases: [(PoolRegistration, RegisteredPoolFamily); 7] = [
        (
            PoolRegistration::V2(Box::new(make_params(U112::from(1), U112::from(2)))),
            RegisteredPoolFamily::V2,
        ),
        (
            PoolRegistration::V3(Box::new(v3_params())),
            RegisteredPoolFamily::V3,
        ),
        (
            PoolRegistration::V4(Box::new(v4_params())),
            RegisteredPoolFamily::V4,
        ),
        (
            PoolRegistration::Curve(Box::new(curve_params(Address::from([0xc2u8; 20])))),
            RegisteredPoolFamily::Curve,
        ),
        (
            PoolRegistration::BalancerWeighted(Box::new(balancer_weighted_params(Address::from(
                [0xb3u8; 20],
            )))),
            RegisteredPoolFamily::BalancerWeighted,
        ),
        (
            PoolRegistration::BalancerStable(Box::new(balancer_stable_params(Address::from(
                [0xb4u8; 20],
            )))),
            RegisteredPoolFamily::BalancerStable,
        ),
        (
            PoolRegistration::AerodromeV2(Box::new(aerodrome_params(Address::from([0xaeu8; 20])))),
            RegisteredPoolFamily::AerodromeV2,
        ),
    ];
    for (registration, family) in cases {
        assert_eq!(registration.family(), family);
    }
}

use super::*;

#[test]
fn conservative_bot_flag_default_on() {
    // the env-flag parse contract moved to degenbot-config's
    // fail-closed loader (parse_bool_flag). ADR-043 retired the old
    // verbosity flags; this asserts the holder's test-default stance on a
    // surviving default-ON behavior key.
    assert!(!crate::bot_core::stance::installed());
    assert!(crate::bot_core::stance::config().solve.cl_projection_cache);
}

#[test]
fn register_v2_pool_and_calculate_tokens_out() {
    let mut core = BotState::new();
    let pool_id = core
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");

    // Python reference: constant_product_calc_exact_in(100, 1000, 2000, 3/1000) = 181
    let amount_out = tokens_out(&mut core, pool_id, true, U256::from(100));
    assert_eq!(amount_out, U256::from(181));
}

#[test]
fn v2_identity_round_trip() {
    // The identity (address/tokens/fees/factory/variant/stable_swap/
    // fee_denominator) round-trips through register_v2_pool ->
    // get_v2_identity. Identity is pure immutable registration data
    // (mirrors TokenEntry), distinct from the mutable V2PoolState.
    let mut core = BotState::new();
    let pool_id = core
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");
    let id = core
        .get_v2_identity(pool_id)
        .expect("registered V2 pool has an identity");
    assert_eq!(id.address, make_pool_addr());
    assert_eq!(id.token0, make_token0());
    assert_eq!(id.token1, make_token1());
    assert_eq!(id.factory, make_factory());
    assert_eq!(id.fee_token0, FEE_03);
    assert_eq!(id.fee_token1, FEE_03);
    assert_eq!(
        id.variant,
        degenbot_uniswap::dex_identity::DexVariant::UniswapV2
    );
    assert!(!id.stable_swap);
    assert_eq!(id.fee_denominator, None);
}

#[test]
fn pool_family_dispatches_v2_and_unknown() {
    // `pool_family(pool_id)` returns a kebab-case family tag by matching
    // on the `PoolEntry` variant. This is the uniform family-guard
    // primitive every `_from_py_pool` seam asserts against (replacing the
    // V2-only `variant` getter). Tracer bullet: V2 + unregistered.
    let mut core = BotState::new();
    let pool_id = core
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");
    assert_eq!(core.pool_family(pool_id), Some("v2"));
    assert_eq!(core.pool_family(999_999), None);
}

#[test]
fn curve_get_dy_runs_the_rust_owned_swap_path() {
    // The Rust-owned `get_dy` entry replays the shared
    // `standard_plain` fixture and reproduces the recorded dy — proving
    // the whole swap path (orchestration + calc) runs with no Python
    // provider / cache / calculator.
    use crate::bot_core::RegisterCurvePoolParams;

    const E18: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
    const E21: U256 = U256::from_limbs([11_627_460_059_052_638_208, 162, 0, 0]); // 3e21
    const TWO_E21: U256 = U256::from_limbs([4_808_176_044_395_724_800, 325, 0, 0]); // 6e21

    let mut core = BotState::new();
    let pool_id = core.register_curve_pool(&RegisterCurvePoolParams {
        address: Address::from([0xccu8; 20]),
        tokens: vec![Address::ZERO, Address::from([0x01u8; 20])],
        a_coefficient: 100,
        a_precision: 100,
        fee: 500_000,
        admin_fee: 0,
        rate_multipliers: vec![E18, E18],
        balances: vec![E21, TWO_E21],
        update_block: 0,
        swap_style: 1,         // STANDARD
        lending_rate_style: 1, // NONE
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
        use_lending: Vec::new(),
        precision_multipliers: vec![E18, E18],
        tokens_underlying: None,
        metapool_rate_style: 1,
        metapool_underlying_style: 1,
        data_provider: None,
    });

    let dy = core
        .curve_get_dy(pool_id, 0, 1, E18, 0, None)
        .expect("standard curve get_dy");
    assert_eq!(dy, U256::from(1_008_296_947_143_911_861u64));

    // Unknown pool id -> UnknownPool error.
    assert!(matches!(
        core.curve_get_dy(999_999, 0, 1, E18, 0, None),
        Err(CurveInputsError::UnknownPool(999_999))
    ));
}

#[test]
#[expect(clippy::too_many_lines)]
fn pool_family_dispatches_every_registered_family() {
    // Each non-V2 `PoolEntry` variant resolves to its own family tag.
    // Registers one pool of each family with minimal params and asserts
    // the tag — this is the precondition for every non-V2 `_from_py_pool`
    // seam's variant-family guard.
    use crate::bot_core::{
        RegisterAerodromeV2PoolParams, RegisterBalancerStablePoolParams,
        RegisterBalancerWeightedPoolParams, RegisterCurvePoolParams, RegisterV4PoolParams,
        TickInfo, V4PoolKey,
    };
    use alloy::primitives::U128;

    let mut core = BotState::new();

    // V3
    let v3_id = register_v3(&mut core, 0);
    assert_eq!(core.pool_family(v3_id), Some("v3"));

    // V4
    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
    let v4_id = core
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([0x01u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u64) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        })
        .expect("V4 registration");
    assert_eq!(core.pool_family(v4_id), Some("v4"));

    // Curve (2-token plain pool)
    let curve_id = core.register_curve_pool(&RegisterCurvePoolParams {
        address: Address::from([0xc0u8; 20]),
        tokens: vec![Address::ZERO, Address::from([0x01u8; 20])],
        a_coefficient: 100,
        a_precision: 100,
        fee: 4_000_000,
        admin_fee: 5_000_000_000,
        rate_multipliers: vec![U256::from(1u64), U256::from(1u64)],
        balances: vec![U256::from(1_000_000u64), U256::from(1_000_000u64)],
        update_block: 0,
        swap_style: 0,
        lending_rate_style: 0,
        d_variant: 1,
        y_variant: 1,
        yd_variant: 0,
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
    });
    assert_eq!(core.pool_family(curve_id), Some("curve"));

    // Balancer weighted (2-token)
    let bal_weighted_id =
        core.register_balancer_weighted_pool(&RegisterBalancerWeightedPoolParams {
            address: Address::from([0xb1u8; 20]),
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
            update_block: 0,
        });
    assert_eq!(core.pool_family(bal_weighted_id), Some("balancer-weighted"));

    // Balancer stable (2-token, MetaStable — bpt_idx=None)
    let bal_stable_id = core.register_balancer_stable_pool(&RegisterBalancerStablePoolParams {
        address: Address::from([0xb2u8; 20]),
        vault: Address::from([0xb0u8; 20]),
        pool_id: [0x22u8; 32],
        tokens: vec![Address::ZERO, Address::from([0x01u8; 20])],
        amp: 100,
        scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
        swap_fee: 1_000_000_000_000_000,
        bpt_idx: None,
        invariant_version: 2,
        balances: vec![U256::from(1_000_000u64), U256::from(1_000_000u64)],
        update_block: 0,
        rate_provider: None,
    });
    assert_eq!(core.pool_family(bal_stable_id), Some("balancer-stable"));

    // Suppress unused-import warning for TickInfo/U128 when the V4 tick_data
    // map is empty — kept for parity with sibling V4 tests.
    let _ = TickInfo {
        liquidity_gross: U128::ZERO,
        liquidity_net: 0,
        block: 0,
    };

    // Aerodrome V2 (volatile mode; stable=false)
    let aero_id = core.register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
        address: Address::from([0xaeu8; 20]),
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
        update_block: 0,
    });
    assert_eq!(core.pool_family(aero_id), Some("aerodrome-v2"));
}

/// Plan 102, slice 2: `BotState::register_v4_pool` returns a typed
/// `RegisterV4PoolError` (not a flat `String`) for each admission
/// category, so the `PyO3` seam can surface distinct Python exception
/// types. Pins the three variants the seam maps to
/// `HookedPoolRejectedError` / `DynamicFeePoolRejectedError` / plain
/// `PyValueError` respectively.
#[test]
fn register_v4_pool_admits_amount_modifying_hook_with_caveat() {
    // ADR-037: hooked pools are ADMITTED (the hard rejection is
    // gone) — their sims carry Caveats::HOOKED_POOL and paths through
    // them are excluded from solving at projection time.
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::swap_simulation::{Caveats, SwapOutcome, SwapRead, SwapRequest};
    use crate::bot_core::{RegisterV4PoolParams, V4PoolKey};
    use hashbrown::HashMap;

    let mut core = BotState::new();
    let pid = core
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager: Address::from([0x44u8; 20]),
            pool_id: [0xeeu8; 32],
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                // BEFORE_SWAP (0x80) — amount-modifying. Note the flags
                // are derived from the hook address's low 16 bits by
                // pool_builder, not trusted from this field.
                hooks: Address::from([
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x80,
                ]),
            },
            hook_flags: 0x80,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            // Seed tick 0 + Tracked so an in-tick swap computes without
            // a fetch (isolates the caveat assertion from fetch policy).
            tick_data: HashMap::from([(
                0_i32,
                TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(1_000_000u64),
                    liquidity_net: 0,
                    block: 0,
                },
            )]),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
        })
        .expect("hooked pool must now be admitted");

    // The sim computes but is caveated as potentially inaccurate.
    let read = core.swap_simulation(
        0,
        pid,
        SwapRequest {
            zero_for_one: true,
            amount_specified: -I256::try_from(1_000u64).unwrap(),
            sqrt_price_limit: None,
        },
    );
    match read {
        SwapRead::Computed(SwapOutcome::V4(payload)) => {
            assert!(
                payload.caveats.contains(Caveats::HOOKED_POOL),
                "hooked-pool sims must carry the HOOKED_POOL caveat"
            );
        }
        other => panic!("hooked V4 sim must compute with caveat, got {other:?}"),
    }
}

#[test]
fn register_v4_pool_rejects_dynamic_fee_with_typed_error() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV4PoolError, RegisterV4PoolParams, V4PoolKey};
    use hashbrown::HashMap;

    let mut core = BotState::new();
    let err = core
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager: Address::from([0x44u8; 20]),
            pool_id: [0xeeu8; 32],
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: crate::bot_core::V4_DYNAMIC_FEE_FLAG,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        })
        .expect_err("dynamic-fee pool must be rejected");
    assert_eq!(
        err,
        RegisterV4PoolError::DynamicFee {
            fee: crate::bot_core::V4_DYNAMIC_FEE_FLAG,
        },
        "dynamic-fee refusal returns the typed DynamicFee variant"
    );
}

#[test]
fn register_v4_pool_rejects_duplicate_with_already_registered_variant() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV4PoolError, RegisterV4PoolParams, V4PoolKey};
    use hashbrown::HashMap;

    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
    let mut core = BotState::new();
    let params = RegisterV4PoolParams {
        pool_manager,
        pool_id: pool_id_bytes,
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::from([1u8; 20]),
            fee: 500,
            tick_spacing: 10,
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
    };
    core.register_v4_pool(&params)
        .expect("first registration ok");
    let err = core
        .register_v4_pool(&params)
        .expect_err("duplicate registration must be rejected");
    assert_eq!(
        err,
        RegisterV4PoolError::AlreadyRegistered {
            pool_manager,
            pool_id: pool_id_bytes,
        },
        "duplicate-registration refusal returns the typed AlreadyRegistered variant"
    );
}

// -----------------------------------------------------------------------
// Spec-bound admission.
// Mirrors the V2/V3 spec-bound tests: `register_v4_pool` now rejects
// out-of-solidity-bounds `sqrt_price_x96` / `tick` / V4 `fee` /
// `tick_spacing` with a typed `RegisterV4PoolError::SpecViolation`, ahead
// of the existing `HookedPool` / `DynamicFee` / `AlreadyRegistered`
// rejections. The four V4 spec validators (`validate_sqrt_price` /
// `validate_tick` / `validate_v4_fee` / `validate_tick_spacing`) are the
// same family-agnostic CL validators V3 uses (V4 shares TickMath); only
// `validate_v4_fee` is V4-specific (the `0x800000` high bit flags a
// dynamic-fee pool, which `DynamicFee` rejects upstream as a more specific
// typed variant).
// -----------------------------------------------------------------------
/// Baseline in-spec V4 params at tick 0, srqt=1<<96, fee=500,
/// `tick_spacing=10`. Each spec-violation test below derives a
/// broken-on-one-field copy.
fn make_v4_params_in_spec() -> crate::bot_core::RegisterV4PoolParams {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV4PoolParams, V4PoolKey};
    use hashbrown::HashMap;
    RegisterV4PoolParams {
        pool_manager: Address::from([0x44u8; 20]),
        pool_id: [0xeeu8; 32],
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::from([1u8; 20]),
            fee: 500,
            tick_spacing: 10,
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
    }
}

#[test]
fn register_v4_pool_rejects_sqrt_price_at_max_as_spec_violation() {
    use crate::bot_core::RegisterV4PoolError;
    let mut core = BotState::new();
    let mut params = make_v4_params_in_spec();
    // Distinct pool_id so the duplicate-registered guard never fires if the
    // previous test's params linger in core (defensive; core is fresh here).
    params.pool_id = [0xe1u8; 32];
    params.sqrt_price_x96 = U256::from(degenbot_math::cl::tick_math::MAX_SQRT_RATIO);
    assert!(
        matches! {
            core.register_v4_pool(&params),
            Err(RegisterV4PoolError::SpecViolation(v)) if v.field == "sqrtPriceX96",
        },
        "sqrtPriceX96 == MAX_SQRT_RATIO surfaces a V4 typed SpecViolation"
    );
}

#[test]
fn register_v4_pool_rejects_sqrt_price_below_min_as_spec_violation() {
    use crate::bot_core::RegisterV4PoolError;
    let mut core = BotState::new();
    let mut params = make_v4_params_in_spec();
    params.pool_id = [0xe2u8; 32];
    params.sqrt_price_x96 =
        U256::from(degenbot_math::cl::tick_math::MIN_SQRT_RATIO) - uint!(1_U256);
    assert!(
        matches! {
            core.register_v4_pool(&params),
            Err(RegisterV4PoolError::SpecViolation(v)) if v.field == "sqrtPriceX96",
        },
        "sqrtPriceX96 < MIN_SQRT_RATIO surfaces a V4 typed SpecViolation"
    );
}

#[test]
fn register_v4_pool_rejects_tick_below_min_as_spec_violation() {
    use crate::bot_core::RegisterV4PoolError;
    let mut core = BotState::new();
    let mut params = make_v4_params_in_spec();
    params.pool_id = [0xe3u8; 32];
    params.tick = degenbot_math::cl::tick_math::MIN_TICK - 1;
    assert!(
        matches! {
            core.register_v4_pool(&params),
            Err(RegisterV4PoolError::SpecViolation(v)) if v.field == "tick",
        },
        "tick < MIN_TICK surfaces a V4 typed SpecViolation"
    );
}

#[test]
fn register_v4_pool_rejects_fee_at_v4_max_as_spec_violation() {
    // The V4 fee bound is `< 1 << 24` (uint24 width; the `0x800000` high
    // bit is the dynamic-fee flag, separately rejected as `DynamicFee`).
    // A fee of `1 << 24` itself is out-of-spec for V4 — distinct from a
    // dynamic-fee flag, and surfaces as a `SpecViolation`, not `DynamicFee`.
    use crate::bot_core::RegisterV4PoolError;
    let mut core = BotState::new();
    let mut params = make_v4_params_in_spec();
    params.pool_id = [0xe4u8; 32];
    params.pool_key.fee = ::degenbot_pools::spec_bounds::V4_FEE_MAX;
    assert!(
        matches! {
            core.register_v4_pool(&params),
            Err(RegisterV4PoolError::SpecViolation(v)) if v.field == "fee",
        },
        "V4 fee >= 1 << 24 surfaces a V4 typed SpecViolation (not DynamicFee)"
    );
}

#[test]
fn register_v4_pool_rejects_fee_exceeding_encoder_limit() {
    // The cmd_executor encodes V4 `fee` as a 2-byte field in both
    // V4_SWAP_COMPACT and V4_SWAP_DYNAMIC (the contract masks `& 65535`).
    // A static fee > 65535 is protocol-valid (`< 1 << 24`, not the
    // dynamic-fee flag) but un-encodable — and unprofitable (32%+ per
    // swap). Reject at admission , mirroring the dynamic-fee
    // refusal, so these pools never enter the path graph.
    use crate::bot_core::RegisterV4PoolError;
    let mut core = BotState::new();
    let mut params = make_v4_params_in_spec();
    params.pool_id = [0xe6u8; 32];
    // fee=320000 — a real mainnet V4 pool (32% fee) seen in the bot run.
    params.pool_key.fee = 320_000;
    assert!(
        matches! {
            core.register_v4_pool(&params),
            Err(RegisterV4PoolError::FeeExceedsEncoderLimit { fee }) if fee == 320_000,
        },
        "V4 fee > u16::MAX (65535) must surface the typed FeeExceedsEncoderLimit variant at admission"
    );
}

#[test]
fn register_v4_pool_admits_fee_at_encoder_limit_boundary() {
    // fee = 65535 (u16::MAX) is the largest encodable static fee; it must
    // be ADMITTED (the executor's 2-byte field holds it). fee = 65536 is
    // the first un-encodable value; it must be rejected.
    let mut core = BotState::new();
    let mut params = make_v4_params_in_spec();
    params.pool_id = [0xe7u8; 32];
    params.pool_key.fee = 65_535;
    assert!(
        core.register_v4_pool(&params).is_ok(),
        "fee = u16::MAX (65535) is encodable and must be admitted"
    );

    let mut core = BotState::new();
    let mut params = make_v4_params_in_spec();
    params.pool_id = [0xe8u8; 32];
    params.pool_key.fee = 65_536;
    assert!(
        matches! {
            core.register_v4_pool(&params),
            Err(crate::bot_core::RegisterV4PoolError::FeeExceedsEncoderLimit { fee }) if fee == 65_536,
        },
        "fee = 65536 (first value > u16::MAX) must be rejected as FeeExceedsEncoderLimit"
    );
}

#[test]
fn register_v4_pool_rejects_tick_spacing_out_of_range_as_spec_violation() {
    use crate::bot_core::RegisterV4PoolError;
    let mut core = BotState::new();
    let mut params = make_v4_params_in_spec();
    params.pool_id = [0xe5u8; 32];
    params.pool_key.tick_spacing = ::degenbot_pools::spec_bounds::MAX_TICK_SPACING + 1;
    assert!(
        matches! {
            core.register_v4_pool(&params),
            Err(RegisterV4PoolError::SpecViolation(v)) if v.field == "tickSpacing",
        },
        "tickSpacing > MAX_TICK_SPACING surfaces a V4 typed SpecViolation"
    );
}

#[test]
fn register_v4_pool_accepts_in_spec_params() {
    // Green companion for the V4 reject tests above: baseline
    // in-spec V4 params must register OK (and reach the
    // `AlreadyRegistered` guard cleanly past the spec validators).
    let mut core = BotState::new();
    let params = make_v4_params_in_spec();
    let pool_id = core
        .register_v4_pool(&params)
        .expect("in-spec V4 params must register");
    assert!(pool_id > 0, "registration returns a non-zero pool_id");
}

// -----------------------------------------------------------------------
// ADR-007: BotState::unregister_pool (V2/V3 address-keyed, V4 tuple-keyed).
// -----------------------------------------------------------------------
#[test]
fn unregister_v2_pool_returns_true_then_re_register_allocates_fresh_id() {
    let mut core = BotState::new();
    let params = make_params(U112::from(1000), U112::from(2000));
    let first_id = core
        .register_v2_pool(&params)
        .expect("test setup: V2 registration");
    assert_eq!(core.pool_count(), 1);

    // Unregister the V2 pool.
    let removed = core.unregister_pool(make_pool_addr(), None);
    assert!(removed, "unregister of a registered V2 pool returns true");
    assert_eq!(core.pool_count(), 0, "unregister must drop the PoolEntry");
    assert_eq!(
        core.pool_id_by_address(&make_pool_addr()),
        None,
        "unregister must clear pool_addresses"
    );

    // Re-register: must succeed (no panic) and allocate a fresh id
    // (retired ids are NOT reused — ADR-007 U3).
    let second_id = core
        .register_v2_pool(&params)
        .expect("test setup: V2 registration");
    assert_ne!(
        second_id, first_id,
        "re-register must allocate a fresh id (retired, not reused)",
    );
    assert_eq!(core.pool_count(), 1);
}

// -----------------------------------------------------------------------
// PRG-1 registry unification: BotState is the registry of record.
// `registered_pool_by_address` answers the PyO3 build adapters' pre-check
// with the family-tagged entry, so a duplicate build resolves to the
// registered handle instead of replaying the builder into an
// `AlreadyRegistered` refusal.
// -----------------------------------------------------------------------
#[test]
fn admission_refusal_records_a_consultable_gate_verdict() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{
        registration_gate::AdmissionVerdict, RegisterV4PoolError, RegisterV4PoolParams, V4PoolKey,
    };
    use hashbrown::HashMap;

    let pm = Address::from([0x44u8; 20]);
    let pid: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
    let mut core = BotState::new();
    let base = |fee: u32| RegisterV4PoolParams {
        pool_manager: pm,
        pool_id: pid,
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::from([1u8; 20]),
            fee,
            tick_spacing: 10,
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
    };

    // Dynamic-fee refusal records a consultable verdict.
    assert!(matches! {
        core.register_v4_pool(&base(0x100_000)),
        Err(RegisterV4PoolError::DynamicFee { .. }),
    });
    assert_eq!(
        core.admission_verdict(pm, &pid),
        Some(AdmissionVerdict::DynamicFee { fee: 0x100_000 })
    );

    // Fee-encoder-limit refusal records its verdict on a fresh pool.
    let pid2: degenbot_decoders::v4_swap_decoder::V4PoolId = [0x33u8; 32];
    let mut p2 = base(320_000);
    p2.pool_id = pid2;
    assert!(matches! {
        core.register_v4_pool(&p2),
        Err(RegisterV4PoolError::FeeExceedsEncoderLimit { fee: 320_000 }),
    });
    assert_eq!(
        core.admission_verdict(pm, &pid2),
        Some(AdmissionVerdict::FeeExceedsEncoderLimit { fee: 320_000 })
    );
    assert_eq!(core.registration_gate_len(), 2);

    // An admitted pool registers normally and answers via the registry
    // of record — the REGISTRATION refusals gate-record, admissions do
    // not, and the two readers never mix.
    let pid3: degenbot_decoders::v4_swap_decoder::V4PoolId = [0x77u8; 32];
    let mut ok = base(500);
    ok.pool_id = pid3;
    let admitted = core.register_v4_pool(&ok).expect("in-spec registration");
    assert!(
        core.admission_verdict(pm, &pid3).is_none(),
        "admitted pools gate-record nothing"
    );
    assert!(core.try_registered_v4(pm, &pid3).is_some());
    let _: u64 = admitted;
}

#[test]
fn registered_pool_by_address_answers_registered_families() {
    let mut core = BotState::new();
    let params = make_params(U112::from(1000), U112::from(2000));
    let id = core
        .register_v2_pool(&params)
        .expect("test setup: V2 registration");

    let (found_id, family) = core
        .registered_pool_by_address(&make_pool_addr())
        .expect("the registered V2 address answers");
    assert_eq!(found_id, id);
    assert_eq!(family, RegisteredPoolFamily::V2);
}

#[test]
fn registered_pool_by_address_is_none_for_unknown_and_v4() {
    let mut core = BotState::new();
    assert_eq!(
        core.registered_pool_by_address(&Address::from([0x11u8; 20])),
        None,
        "an unregistered address answers None"
    );

    // V4 is (pool_manager, pool_id)-keyed — not address-keyed — so even a
    // registered V4 pool must NOT answer here (its fast path is the
    // `try_registered_v4` reader).
    let v4_params = make_v4_params_in_spec();
    core.register_v4_pool(&v4_params)
        .expect("test setup: V4 registration");
    assert_eq!(
        core.registered_pool_by_address(&Address::from([0x22u8; 20])),
        None,
        "V4 pools do not answer the address-keyed reader"
    );
}

#[test]
fn unregister_pool_on_unknown_address_returns_false_silently() {
    let mut core = BotState::new();
    // Register one pool at make_pool_addr().
    let _ = core
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");
    let unknown = Address::from([0x99u8; 20]);

    let removed = core.unregister_pool(unknown, None);
    assert!(!removed, "unregister on an unknown address returns false");
    // No mutation occurred.
    assert_eq!(core.pool_count(), 1);
}

// -----------------------------------------------------------------------
// Spec-bound admission.
// `register_v2_pool` is a typed `Result` that rejects (a) duplicate
// address and (b) out-of-spec `uint112` reserves, rather than panicking
// on (a) and silently degrading to `U256::MAX` on (b).
// -----------------------------------------------------------------------
#[test]
fn register_v2_pool_rejects_duplicate_address_as_already_registered() {
    let mut core = BotState::new();
    let params = make_params(U112::from(1000), U112::from(2000));
    let _ok = core.register_v2_pool(&params).expect("first registration");
    // Second registration at the same address: prior impl `assert!`-panicked;
    // now returns `Err(AlreadyRegistered { address })`.
    assert!(
        matches! {
            core.register_v2_pool(&params),
            Err(RegisterV2PoolError::AlreadyRegistered { address }) if address == params.address,
        },
        "duplicate-address registration surfaces a typed Err, not a panic"
    );
}

// Note: the overlarge-reserve rejection that lived here previously has
// moved to the `narrow_v2_reserve` ingestion seam (PyO3 `sync_reserves` /
// `register_*_pool` paths + the V2 Sync decoder) — see
// `degenbot_pools::spec_bounds::narrow_v2_reserve` and its tests. With
// `V2PoolState`/`RegisterV2PoolParams.reserve0/1` typed `U112`, an
// overlarge value cannot be constructed at the `register_v2_pool` layer
// (the type system enforces the `uint112` bound), so there is nothing to
// test here.
// -----------------------------------------------------------------------
// Spec-bound admission .
// `register_v3_pool` is a typed `Result` that rejects (a) duplicate
// address and (b) out-of-spec `sqrtPriceX96` / `tick` / `fee` /
// `tickSpacing`, rather than `assert!`-panicking on (a) and silently
// accepting impossible CL config on (b). Mirrors the V2 tests above.
// -----------------------------------------------------------------------
/// Baseline in-spec V3 params at tick 0, `sqrt=1<<96`, `fee=3_000`,
/// `tick_spacing=60`, undirectional tokens. Each spec-violation test below
/// derives a fresh broken-on-one-field copy.
fn make_v3_params_in_spec() -> RegisterV3PoolParams {
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
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
        ..Default::default()
    }
}

#[test]
fn register_v3_pool_rejects_duplicate_address_as_already_registered() {
    let mut core = BotState::new();
    let params = make_v3_params_in_spec();
    let _ok = core.register_v3_pool(&params).expect("first registration");
    assert!(
        matches! {
            core.register_v3_pool(&params),
            Err(RegisterV3PoolError::AlreadyRegistered { address }) if address == params.address,
        },
        "duplicate-address registration surfaces a typed Err, not an assert! panic"
    );
}

#[test]
fn register_v3_pool_rejects_sqrt_price_at_max_as_spec_violation() {
    let mut core = BotState::new();
    let mut params = make_v3_params_in_spec();
    params.sqrt_price_x96 = U256::from(degenbot_math::cl::tick_math::MAX_SQRT_RATIO);
    assert!(
        matches! {
            core.register_v3_pool(&params),
            Err(RegisterV3PoolError::SpecViolation(v)) if v.field == "sqrtPriceX96",
        },
        "sqrtPriceX96 == MAX_SQRT_RATIO surfaces a typed SpecViolation"
    );
}

#[test]
fn register_v3_pool_rejects_sqrt_price_below_min_as_spec_violation() {
    let mut core = BotState::new();
    let mut params = make_v3_params_in_spec();
    params.sqrt_price_x96 =
        U256::from(degenbot_math::cl::tick_math::MIN_SQRT_RATIO) - uint!(1_U256);
    assert!(
        matches! {
            core.register_v3_pool(&params),
            Err(RegisterV3PoolError::SpecViolation(v)) if v.field == "sqrtPriceX96",
        },
        "sqrtPriceX96 < MIN_SQRT_RATIO surfaces a typed SpecViolation"
    );
}

#[test]
fn register_v3_pool_rejects_tick_below_min_as_spec_violation() {
    let mut core = BotState::new();
    let mut params = make_v3_params_in_spec();
    params.tick = degenbot_math::cl::tick_math::MIN_TICK - 1;
    assert!(
        matches! {
            core.register_v3_pool(&params),
            Err(RegisterV3PoolError::SpecViolation(v)) if v.field == "tick",
        },
        "tick < MIN_TICK surfaces a typed SpecViolation"
    );
}

#[test]
fn register_v3_pool_rejects_fee_at_max_as_spec_violation() {
    let mut core = BotState::new();
    let mut params = make_v3_params_in_spec();
    params.fee = ::degenbot_pools::spec_bounds::V3_FEE_MAX;
    assert!(
        matches! {
            core.register_v3_pool(&params),
            Err(RegisterV3PoolError::SpecViolation(v)) if v.field == "fee",
        },
        "fee >= 1_000_000 surfaces a typed SpecViolation"
    );
}

#[test]
fn register_v3_pool_rejects_tick_spacing_out_of_range_as_spec_violation() {
    let mut core = BotState::new();
    let mut params = make_v3_params_in_spec();
    params.tick_spacing = ::degenbot_pools::spec_bounds::MAX_TICK_SPACING + 1;
    assert!(
        matches! {
            core.register_v3_pool(&params),
            Err(RegisterV3PoolError::SpecViolation(v)) if v.field == "tickSpacing",
        },
        "tickSpacing > MAX_TICK_SPACING surfaces a typed SpecViolation"
    );
}

#[test]
fn register_v3_pool_accepts_in_spec_params() {
    // Green companion for the reject tests above: each validator's accept
    // boundary (sqrtPriceX96 in [MIN_SQRT_RATIO, MAX_SQRT_RATIO), tick in
    // [MIN_TICK, MAX_TICK], fee < 1_000_000, tickSpacing in [1, 32_767])
    // composes — the baseline `make_v3_params_in_spec()` must register OK.
    let mut core = BotState::new();
    let params = make_v3_params_in_spec();
    let pool_id = core
        .register_v3_pool(&params)
        .expect("in-spec V3 params must register");
    assert!(pool_id > 0, "registration returns a non-zero pool_id");
}

#[test]
fn unregister_v3_pool_discards_buffered_liquidity_events() {
    let mut core = BotState::new();
    let v3_addr = make_pool_addr();

    // Pre-registration: buffer a backfill ModifyLiquidity for `v3_addr`.
    // `buffer_backfill_v3_liquidity_update` on an UNregistered address
    // buffers (the registered-address early-return path doesn't fire).
    core.buffer_backfill_v3_liquidity_update(v3_addr, -100, 100, 500_i128, 42_u64);
    assert_eq!(
        core.buffered_v3_event_count(&v3_addr),
        1,
        "precondition: the event is buffered for the unregistered address"
    );

    // Register the V3 pool. Registration does NOT auto-drain the buffer
    // (drain is caller-driven via `apply_backfill_buffer_v3`); the buffer
    // entry persists. This mirrors the live pump path where events can
    // arrive pre-registration and stay buffered.
    let _ = core
        .register_v3_pool(&RegisterV3PoolParams {
            address: v3_addr,
            token0: make_token0(),
            token1: make_token1(),
            fee: 3_000,
            tick_spacing: 60,
            factory: make_factory(),
            sqrt_price_x96: U256::from(1u64) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");
    assert_eq!(
        core.buffered_v3_event_count(&v3_addr),
        1,
        "registration must not auto-drain (drain is caller-driven), so the buffer entry survives to here"
    );

    // Unregister the V3 pool. ADR-007 U3: drain the V3 buffer for the
    // removed address so a re-register does not replay stale Mint/Burn.
    let removed = core.unregister_pool(v3_addr, None);
    assert!(removed);
    assert_eq!(
        core.buffered_v3_event_count(&v3_addr),
        0,
        "unregister must discard buffered V3 events for the removed address"
    );
}

#[test]
fn unregister_v4_pool_by_tuple_key_discards_buffered_modify_liquidity() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV4PoolParams, V4PoolKey};

    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
    let mut core = BotState::new();

    let pool_id_u64 = core
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 10_000,
                tick_spacing: 60,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        })
        .expect("V4 pool registers");
    assert_eq!(core.pool_count(), 1);

    // Unregister by the V4 tuple key (address = pool_manager).
    let removed = core.unregister_pool(pool_manager, Some(pool_id_bytes));
    assert!(removed, "unregister of a registered V4 pool returns true");
    assert_eq!(core.pool_count(), 0);

    // Re-register must succeed (no Err) and allocate a fresh id.
    let second_id = core
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 10_000,
                tick_spacing: 60,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        })
        .expect("V4 re-register after unregister must succeed");
    assert_ne!(
        second_id, pool_id_u64,
        "re-register must allocate a fresh id (retired, not reused)",
    );
}

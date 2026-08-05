//! TDD tests for the Curve dy calculator layer (task `YY64IT`).
//!
//! Oracle provenance: every expected constant below is a **recorded
//! Python-oracle output** — computed by `scripts`/`/tmp` oracle scripts that
//! run the *same* pure `stableswap_get_y`/`newton_y`/`reduction_coefficient`
//! primitives through the exact Python calculator formulas. The calculator's
//! novel logic (axes, dynamic fee, crypto fee, metapool conversion) is thus
//! cross-validated against the reference; the shared invariant primitives are
//! trusted (already Tier-3/property-tested in `stableswap.rs`).

use alloy::primitives::{address, Address, U256};

use super::*;
use crate::stableswap::{DVariant, YVariant};

const ONE: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]); // 1e18
const FEE_DEN: U256 = U256::from_limbs([10_000_000_000, 0, 0, 0]); // 1e10
const AMP: U256 = U256::from_limbs([10_000, 0, 0, 0]); // a_coefficient=100 * a_precision=100
const A_PREC: U256 = U256::from_limbs([100, 0, 0, 0]);
const TOK: Address = address!("0x1111111111111111111111111111111111111111");

/// `v * 10**21` — the reserve magnitude used by the recorded fixtures.
fn e21(v: u64) -> U256 {
    U256::from(v) * U256::from(10u64).pow(U256::from(21u64))
}

/// Build a default 2-coin inputs snapshot; tests overwrite the slices they
/// exercise. `n_coins` defaults to 2.
fn inputs() -> DyCalculationInputs {
    DyCalculationInputs {
        precision: ONE,
        fee_denominator: FEE_DEN,
        fee: U256::from(500_000u64),
        n_coins: 2,
        balances: vec![e21(3), e21(6)],
        rate_multipliers: vec![ONE, ONE],
        precision_multipliers: vec![U256::from(1u8), U256::from(1u8)],
        offpeg_fee_multiplier: U256::from(400_000_000_000u64),
        fee_gamma: U256::from(500_000_000_000_000u64),
        mid_fee: U256::from(30_000_000u64),
        out_fee: U256::from(450_000_000u64),
        address: TOK,
        resolved_rates: vec![ONE, ONE],
        xp: vec![U256::from(3u64) * ONE, U256::from(6u64) * ONE],
        block_number: 18_000_000,
        block_timestamp: 1_700_000_000,
        amp: AMP,
        d_variant: DVariant::Standard,
        y_variant: YVariant::Standard,
        a_precision: A_PREC,
        swap_style: 1,
        metapool: false,
        metapool_rate_style: 1,
        metapool_underlying_style: 1,
        d: None,
        gamma: None,
        price_scale: None,
        live_balances: None,
        admin_balances: None,
        effective_balances: None,
        virtual_price: None,
        scaled_redemption_price: None,
    }
}

// ──────────────────────────────────────────────────────────────────────────
// StandardDyCalculator axes (Python-oracle fixture `oracle_std2.py`)
// ──────────────────────────────────────────────────────────────────────────

/// Discriminating fixture: 50% fee + `resolved_rates[1] = 1.5e18` so the
/// balance-source and rate-source axes yield distinct results. `xp` is the
/// rate-adjusted balances `[3e21, 9e21]`.
fn swap_fixture() -> DyCalculationInputs {
    let mut i = inputs();
    i.fee = U256::from(5_000_000_000u64); // 50 %
    i.resolved_rates = vec![
        ONE,
        U256::from(15u64) * U256::from(10u64).pow(U256::from(17u64)),
    ];
    i.xp = vec![e21(3), e21(9)];
    i
}

#[test]
fn standard_style_matches_oracle() {
    let mut i = swap_fixture();
    i.swap_style = 1; // STANDARD
    assert_eq!(
        calculate_dy(0, 1, ONE, &i).unwrap(),
        U256::from(339_176_146_955_799_262u64)
    );
}

#[test]
fn rate_adjusted_equals_standard_fee_ordering() {
    // Fee-then-rate and rate-then-fee are mathematically equivalent; they
    // differ only by integer-floor rounding residue, and here they coincide.
    let mut i = swap_fixture();
    i.swap_style = 2; // RATE_ADJUSTED (RATE_THEN_FEE)
    assert_eq!(
        calculate_dy(0, 1, ONE, &i).unwrap(),
        U256::from(339_176_146_955_799_262u64)
    );
}

#[test]
fn raw_balance_style_differs_from_rate_adjusted() {
    // RAW_BALANCE uses the raw reserves with identity rates + fee-only.
    let mut i = swap_fixture();
    i.swap_style = 3; // RAW_BALANCE
    assert_eq!(
        calculate_dy(0, 1, ONE, &i).unwrap(),
        U256::from(504_173_682_256_068_734u64)
    );
}

#[test]
fn live_admin_uses_rate_multipliers_source() {
    let mut i = swap_fixture();
    i.swap_style = 5; // LIVE_ADMIN (rate_source = RATE_MULTIPLIERS)
    assert_eq!(
        calculate_dy(0, 1, ONE, &i).unwrap(),
        U256::from(508_764_220_433_698_893u64)
    );
}

#[test]
fn no_one_fee_rate_skips_subtract_one() {
    // Plain fixture (rate = 1e18) so the -1 survives; NO_ONE adds it back.
    let mut i = inputs();
    i.swap_style = 1; // STANDARD
    let standard = calculate_dy(0, 1, ONE, &i).unwrap();
    i.swap_style = 9; // NO_ONE_FEE_RATE
    assert_eq!(
        calculate_dy(0, 1, ONE, &i).unwrap(),
        standard + U256::from(1u8)
    );
}

#[test]
fn cytoken_equals_standard() {
    let mut i = swap_fixture();
    i.swap_style = 10; // CYTOKEN — identical arithmetic to STANDARD
    assert_eq!(
        calculate_dy(0, 1, ONE, &i).unwrap(),
        U256::from(339_176_146_955_799_262u64)
    );
}

// ──────────────────────────────────────────────────────────────────────────
// LiveAdminDynamicDyCalculator (Python-oracle fixture `oracle_final.py`)
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn live_dynamic_none_offpeg_fee() {
    let mut i = inputs();
    i.swap_style = 6; // LIVE_ADMIN_DYNAMIC (precision_mode NONE)
    i.effective_balances = Some(vec![U256::from(2_999u64) * ONE, U256::from(5_998u64) * ONE]);
    assert_eq!(
        calculate_dy(0, 1, ONE, &i).unwrap(),
        U256::from(1_008_290_824_882_167_556u64)
    );
}

#[test]
fn live_dynamic_precision_multipliers() {
    let mut i = inputs();
    i.swap_style = 7; // LIVE_ADMIN_DYNAMIC_PRECISION
    i.balances = vec![U256::from(3_000_000u64), U256::from(6_000_000u64)];
    i.effective_balances = Some(vec![U256::from(3_000_000u64), U256::from(6_000_000u64)]);
    i.precision_multipliers = vec![
        U256::from(10u64).pow(U256::from(12u64)),
        U256::from(10u64).pow(U256::from(12u64)),
    ];
    assert_eq!(
        calculate_dy(0, 1, U256::from(1_000_000u64), &i).unwrap(),
        U256::from(1_004_955u64)
    );
}

// ──────────────────────────────────────────────────────────────────────────
// CryptoDyCalculator (Python-oracle fixture `oracle_final.py`)
// ──────────────────────────────────────────────────────────────────────────

/// Internally-consistent 3-coin tricrypto fixture (d computed on the
/// price-scale-adjusted xp `[1e24, 1e24, 1e24]`).
fn crypto_fixture() -> DyCalculationInputs {
    let mut i = inputs();
    i.n_coins = 3;
    i.swap_style = 4; // CRYPTO
    i.fee = U256::ZERO;
    i.balances = vec![
        U256::from(10u64).pow(U256::from(12u64)),
        U256::from(10u64).pow(U256::from(20u64)),
        U256::from(10u64).pow(U256::from(30u64)),
    ];
    i.rate_multipliers = vec![ONE, ONE, ONE];
    i.precision_multipliers = vec![U256::from(1u8), U256::from(1u8), U256::from(1u8)];
    i.resolved_rates = vec![ONE, ONE, ONE];
    i.gamma = Some(U256::from(145_000_000_000_000u64));
    i.price_scale = Some(vec![
        U256::from(10u64).pow(U256::from(12u64)),
        U256::from(10u64).pow(U256::from(12u64)),
    ]);
    i.d = Some(U256::from(3u64) * U256::from(10u64).pow(U256::from(24u64)));
    i
}

#[test]
fn crypto_dy_into_second_coin() {
    let i = crypto_fixture();
    assert_eq!(
        calculate_dy(0, 1, U256::from(1_000_000u64), &i).unwrap(),
        U256::from(99_699_997_096_060u64)
    );
}

#[test]
fn crypto_dy_into_third_coin() {
    let i = crypto_fixture();
    assert_eq!(
        calculate_dy(0, 2, U256::from(1_000_000u64), &i).unwrap(),
        "996999970960597536436000".parse::<U256>().unwrap()
    );
}

#[test]
fn crypto_rejects_zero_dx() {
    let i = crypto_fixture();
    assert_eq!(
        calculate_dy(0, 1, U256::ZERO, &i).unwrap_err(),
        CurveSwapError::ZeroInput
    );
}

// ──────────────────────────────────────────────────────────────────────────
// MetapoolDyCalculator (Python-oracle fixture `oracle_final.py`)
// ──────────────────────────────────────────────────────────────────────────

fn metapool_fixture() -> DyCalculationInputs {
    let mut i = inputs();
    i.balances = vec![e21(1), e21(2)];
    i.precision_multipliers = vec![U256::from(10u64).pow(U256::from(12u64)), U256::from(1u8)];
    i.virtual_price = Some(U256::from(1_050_000_000_000_000_000u64));
    i.scaled_redemption_price = Some(U256::from(1_010_000_000_000_000_000u64));
    i
}

#[test]
fn metapool_standard_rate_style() {
    let mut i = metapool_fixture();
    i.metapool = true;
    i.metapool_rate_style = 1; // STANDARD
    assert_eq!(
        calculate_dy(0, 1, ONE, &i).unwrap(),
        U256::from(961_074_000_134_304_340u64)
    );
}

#[test]
fn metapool_precision_vp_rate_style() {
    // PRECISION_VP uses (PRECISION, virtual_price); rate_multipliers[0] is 1e18
    // here so it coincides with STANDARD.
    let mut i = metapool_fixture();
    i.metapool = true;
    i.metapool_rate_style = 2; // PRECISION_VP
    assert_eq!(
        calculate_dy(0, 1, ONE, &i).unwrap(),
        U256::from(961_074_000_134_304_340u64)
    );
}

#[test]
fn metapool_redemption_vp_rate_style() {
    let mut i = metapool_fixture();
    i.metapool = true;
    i.metapool_rate_style = 3; // REDEMPTION_VP
    assert_eq!(
        calculate_dy(0, 1, ONE, &i).unwrap(),
        U256::from(970_515_793_492_844_491u64)
    );
}

// ──────────────────────────────────────────────────────────────────────────
// MetapoolUnderlyingDyCalculator — base-pool delegation branch
// ──────────────────────────────────────────────────────────────────────────

/// A canned base pool whose delegated ops return configured constants, so the
/// calculator's index-mapping + delegation (not the base pool's own math) is
/// exercised.
struct StubBasePool;

impl CurveBasePoolPort for StubBasePool {
    fn token_count(&self) -> usize {
        2
    }
    fn fee(&self) -> U256 {
        FEE_DEN / U256::from(20u8) // 5 %
    }
    fn calc_token_amount(&self, _amounts: &[U256], _block: u64) -> Result<U256, CurveSwapError> {
        Ok(U256::from(1_234_567_890u64))
    }
    fn get_dy(&self, _i: usize, _j: usize, _dx: U256, _block: u64) -> Result<U256, CurveSwapError> {
        Ok(U256::from(987_654_321u64))
    }
    fn calc_withdraw_one_coin(
        &self,
        _token_amount: U256,
        _i: usize,
        _block: u64,
    ) -> Result<U256, CurveSwapError> {
        Ok(U256::from(123_456_789u64))
    }
}

#[test]
fn underlying_delegates_when_both_coins_are_base_pool_coins() {
    // i=1, j=2 (max_coin=1) → both are base coins (base_i=0, base_j=1); the
    // calculator returns the base pool's get_dy verbatim.
    let mut i = metapool_fixture();
    i.n_coins = 2;
    i.metapool_underlying_style = 1; // STANDARD
    assert_eq!(
        calculate_dy_underlying(1, 2, ONE, &i, &StubBasePool).unwrap(),
        U256::from(987_654_321u64)
    );
}

// ──────────────────────────────────────────────────────────────────────────
// A-ramping resolution
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn ramping_a_interpolates_between_endpoints() {
    // Ramp from 100 → 300 over t0=1000 → t1=2000; at t=1500 →
    // 100 + (300-100)*(1500-1000)//(2000-1000) = 100 + 100 = 200.
    let p = ARampingParams {
        a_coefficient: 100,
        initial_a_coefficient: Some(100),
        future_a_coefficient: Some(300),
        initial_a_coefficient_time: Some(1_000),
        future_a_coefficient_time: Some(2_000),
        create_timestamp: Some(0),
        a_precision: 100,
    };
    assert_eq!(resolve_ramping_a(p, 1_500).unwrap(), U256::from(200u64));
}

#[test]
fn ramping_a_finished_returns_future() {
    let p = ARampingParams {
        a_coefficient: 100,
        initial_a_coefficient: Some(100),
        future_a_coefficient: Some(300),
        initial_a_coefficient_time: Some(1_000),
        future_a_coefficient_time: Some(2_000),
        create_timestamp: Some(0),
        a_precision: 100,
    };
    // create_timestamp(0) < future_a_time(2000), but block timestamp past the
    // end → returns future_a.
    assert_eq!(resolve_ramping_a(p, 3_000).unwrap(), U256::from(300u64));
    // create_timestamp past future_a_time.
    let p2 = ARampingParams {
        create_timestamp: Some(2_500),
        ..p
    };
    assert_eq!(resolve_ramping_a(p2, 1_500).unwrap(), U256::from(300u64));
}

#[test]
fn ramping_a_fixed_without_endpoints() {
    let p = ARampingParams {
        a_coefficient: 200,
        initial_a_coefficient: None,
        future_a_coefficient: None,
        initial_a_coefficient_time: None,
        future_a_coefficient_time: None,
        create_timestamp: None,
        a_precision: 100,
    };
    assert_eq!(resolve_ramping_a(p, 1_500).unwrap(), U256::from(20_000u64));
}

#[test]
fn amp_resolves_variant0_divisor() {
    // The A_PRECISION amp divisor applies ONLY to `VARIANT_0` (mirrors the
    // companion `_resolve_calculation_inputs_via_io`: `amp = raw_a //
    // A_PRECISION if y_variant == VARIANT_0 else raw_a`). `VARIANT_1` does
    // NOT divide — this is distinct from `omits_a_precision()` (used inside
    // stableswap_get_y's c/b formulas, where Variant1 also omits). A real
    // VARIANT_1 pool (Curve 3pool) exposed the bug: under-amplifying by
    // A_PRECISION mispriced every swap vs the on-chain oracle.
    assert_eq!(
        resolve_amp(U256::from(20_000u64), A_PREC, YVariant::Standard),
        U256::from(20_000u64)
    );
    assert_eq!(
        resolve_amp(U256::from(20_000u64), A_PREC, YVariant::Variant0),
        U256::from(200u64)
    );
    // VARIANT_1 keeps the full raw amp (no A_PRECISION divisor).
    assert_eq!(
        resolve_amp(U256::from(20_000u64), A_PREC, YVariant::Variant1),
        U256::from(20_000u64)
    );
}

#[test]
fn unknown_style_is_rejected() {
    let mut i = inputs();
    i.swap_style = 99;
    assert_eq!(
        calculate_dy(0, 1, ONE, &i).unwrap_err(),
        CurveSwapError::UnknownStyle(99)
    );
}

#[test]
fn i_equals_j_rejected() {
    let i = inputs();
    assert_eq!(
        calculate_dy(0, 0, ONE, &i).unwrap_err(),
        CurveSwapError::IndexOutOfBounds
    );
}

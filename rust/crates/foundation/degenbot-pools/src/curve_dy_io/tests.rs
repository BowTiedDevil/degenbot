//! TDD for the Curve `get_dy` I/O orchestration (`resolve_dy_inputs`).
//!
//! Probes each `SwapStyle` branch against the pure `calculate_dy`, asserting the
//! resolved snapshot carries the same inputs the Python companion
//! `_resolve_calculation_inputs_via_io` produces (rates / xp / amp / crypto /
//! live-admin effective balances / metapool prices), then that `calculate_dy`
//! reproduces the recorded dy for the shared `standard_plain` fixture shape.

#![expect(clippy::expect_used)]
use std::collections::HashMap;

use alloy::primitives::{Address, U256};
use degenbot_math::curve::calculate_dy;

use super::{
    check_metapool_liquidity, check_pool_liquidity, resolve_dy_inputs, BASE_CACHE_EXPIRES,
};
use crate::curve_data_provider::{CurveDataProvider, CurveDataProviderError};
use crate::curve_state::CurvePoolIdentity;

const E18: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
const E21: U256 = U256::from_limbs([11_627_460_059_052_638_208, 162, 0, 0]);
const TWO_E21: U256 = U256::from_limbs([4_808_176_044_395_724_800, 325, 0, 0]);
const TOK0: Address = Address::ZERO;
const TOK1: Address = Address::with_last_byte(1);
const BASE: Address = Address::with_last_byte(2);

fn identity(swap_style: u8, lending_rate_style: u8, metapool: bool) -> CurvePoolIdentity {
    CurvePoolIdentity {
        address: Address::ZERO,
        tokens: vec![TOK0, TOK1].into(),
        a_coefficient: 100,
        a_precision: 100,
        fee: 500_000,
        admin_fee: 0,
        rate_multipliers: vec![E18, E18].into(),
        swap_style,
        lending_rate_style,
        d_variant: 1,
        y_variant: 1,
        yd_variant: 1,
        base_pool: if metapool { Some(BASE) } else { None },
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
        use_lending: Box::default(),
        precision_multipliers: vec![E18, E18].into(),
        tokens_underlying: None,
        metapool_rate_style: 1,
        metapool_underlying_style: 1,
    }
}

/// Test provider: every method returns an error unless the test set the
/// corresponding field (so a branch that wrongly touches I/O fails, and
/// optional reads like `redemption_price` degrade to `None`).
#[derive(Debug)]
struct StubProvider {
    block_timestamp: u64,
    lending_rates: Option<Vec<U256>>,
    virtual_price: Option<U256>,
    redemption_price: Option<U256>,
    d: Option<U256>,
    gamma: Option<U256>,
    price_scale: Option<Vec<U256>>,
    token_balances: HashMap<Address, U256>,
    admin_balances: Option<Vec<U256>>,
    base_cache_updated: Option<u64>,
    base_virtual_price: Option<U256>,
}

fn missing() -> CurveDataProviderError {
    CurveDataProviderError::Unsupported
}

impl CurveDataProvider for StubProvider {
    fn block_number(&self) -> Result<u64, CurveDataProviderError> {
        Ok(0)
    }
    fn block_timestamp(&self, _b: u64) -> Result<u64, CurveDataProviderError> {
        Ok(self.block_timestamp)
    }
    fn token_balance(
        &self,
        token: Address,
        _holder: Address,
        _b: u64,
    ) -> Result<U256, CurveDataProviderError> {
        self.token_balances.get(&token).copied().ok_or_else(missing)
    }
    fn token_total_supply(&self, _t: Address, _b: u64) -> Result<U256, CurveDataProviderError> {
        Err(missing())
    }
    fn lending_rates(&self, _b: u64) -> Result<Vec<U256>, CurveDataProviderError> {
        self.lending_rates.clone().ok_or_else(missing)
    }
    fn d(&self, _b: u64) -> Result<U256, CurveDataProviderError> {
        self.d.ok_or_else(missing)
    }
    fn gamma(&self, _b: u64) -> Result<U256, CurveDataProviderError> {
        self.gamma.ok_or_else(missing)
    }
    fn price_scale(&self, _b: u64) -> Result<Vec<U256>, CurveDataProviderError> {
        self.price_scale.clone().ok_or_else(missing)
    }
    fn admin_balances(&self, _b: u64) -> Result<Vec<U256>, CurveDataProviderError> {
        self.admin_balances.clone().ok_or_else(missing)
    }
    fn redemption_price(&self, _b: u64) -> Result<U256, CurveDataProviderError> {
        self.redemption_price.ok_or_else(missing)
    }
    fn base_cache_updated(&self, _b: u64) -> Result<u64, CurveDataProviderError> {
        self.base_cache_updated.ok_or_else(missing)
    }
    fn base_virtual_price(&self, _b: u64) -> Result<U256, CurveDataProviderError> {
        self.base_virtual_price.ok_or_else(missing)
    }
    fn virtual_price(&self, _b: u64) -> Result<U256, CurveDataProviderError> {
        self.virtual_price.ok_or_else(missing)
    }
}

#[test]
fn standard_plain_no_provider_resolves_and_matches_recorded_dy() {
    // A plain STANDARD pool needs no provider I/O — `None` still resolves.
    let inputs = resolve_dy_inputs(&identity(1, 1, false), &[E21, TWO_E21], None, 1, None)
        .expect("plain standard resolves without a provider");

    assert!(!inputs.metapool);
    assert_eq!(inputs.swap_style, 1);
    assert_eq!(inputs.n_coins, 2);
    // xp = rate * balance // PRECISION
    assert_eq!(inputs.xp, vec![E21, TWO_E21]);
    // amp = a_coefficient * A_PRECISION = 100 * 100
    assert_eq!(inputs.amp, U256::from(10_000_u64));
    assert_eq!(inputs.resolved_rates, vec![E18, E18]);

    // Feeding it to the pure calc reproduces the shared `standard_plain` dy.
    let dy = calculate_dy(0, 1, E18, &inputs).expect("standard dy");
    assert_eq!(dy, U256::from(1_008_296_947_143_911_861_u64));
}

#[test]
fn override_balances_replace_state() {
    let inputs = resolve_dy_inputs(
        &identity(1, 1, false),
        &[E21, TWO_E21],
        None,
        1,
        Some(&[E18, E18]),
    )
    .expect("override resolves");
    assert_eq!(inputs.balances, vec![E18, E18]);
    assert_eq!(inputs.xp, vec![E18, E18]);
}

#[test]
fn lending_pool_reads_rates_from_provider() {
    let provider = StubProvider {
        block_timestamp: 0,
        lending_rates: Some(vec![E18, E18 + E18 / U256::from(2)]),
        base_cache_updated: None,
        base_virtual_price: None,
        virtual_price: None,
        redemption_price: None,
        d: None,
        gamma: None,
        price_scale: None,
        token_balances: HashMap::new(),
        admin_balances: None,
    };
    // lending_rate_style = 2 (CTOKEN, != NONE)
    let inputs = resolve_dy_inputs(
        &identity(1, 2, false),
        &[E21, TWO_E21],
        Some(&provider),
        1,
        None,
    )
    .expect("lending resolves");
    assert_eq!(
        inputs.resolved_rates,
        vec![E18, E18 + E18 / U256::from(2)],
        "rates come from the provider, not rate_multipliers"
    );
    assert_eq!(inputs.xp[1], TWO_E21 * (E18 + E18 / U256::from(2)) / E18);
}

#[test]
fn lending_pool_requires_a_provider() {
    let err = resolve_dy_inputs(&identity(1, 2, false), &[E21, TWO_E21], None, 1, None)
        .expect_err("lending without a provider must fail");
    assert!(matches!(
        err,
        crate::curve_dy_io::CurveInputsError::NoProvider(_)
    ));
}

#[test]
fn crypto_pool_reads_d_gamma_price_scale() {
    let mut tb = HashMap::new();
    tb.insert(TOK0, E21);
    tb.insert(TOK1, TWO_E21);
    let provider = StubProvider {
        block_timestamp: 1_700_000_000,
        lending_rates: None,
        virtual_price: None,
        redemption_price: None,
        d: Some(U256::from(123u64)),
        gamma: Some(U256::from(7_000_000_000_u64)),
        price_scale: Some(vec![E18, E18]),
        base_cache_updated: None,
        base_virtual_price: None,
        token_balances: tb,
        admin_balances: None,
    };
    // swap_style = 4 (CRYPTO)
    let inputs = resolve_dy_inputs(
        &identity(4, 1, false),
        &[E21, TWO_E21],
        Some(&provider),
        1,
        None,
    )
    .expect("crypto resolves");
    assert_eq!(inputs.d, Some(U256::from(123u64)));
    assert_eq!(inputs.gamma, Some(U256::from(7_000_000_000_u64)));
    assert_eq!(inputs.price_scale, Some(vec![E18, E18]));
    assert_eq!(inputs.block_timestamp, 1_700_000_000);
}

#[test]
fn live_admin_uses_effective_balances() {
    let mut tb = HashMap::new();
    tb.insert(TOK0, U256::from(4_000_000_000_000_000_000_000u128)); // 4e21 live
    tb.insert(TOK1, U256::from(8_000_000_000_000_000_000_000u128)); // 8e21 live
    let provider = StubProvider {
        block_timestamp: 0,
        lending_rates: None,
        virtual_price: None,
        redemption_price: None,
        d: None,
        gamma: None,
        price_scale: None,
        token_balances: tb,
        admin_balances: Some(vec![E21, TWO_E21]),
        base_cache_updated: None,
        base_virtual_price: None,
    };
    // swap_style = 5 (LIVE_ADMIN)
    let inputs = resolve_dy_inputs(
        &identity(5, 1, false),
        &[E21, TWO_E21],
        Some(&provider),
        1,
        None,
    )
    .expect("live-admin resolves");
    let expected_effective = vec![
        U256::from_limbs([3_875_820_019_684_212_736, 54, 0, 0]),
        U256::from_limbs([7_751_640_039_368_425_472, 108, 0, 0]),
    ];
    assert_eq!(
        inputs.effective_balances.as_deref(),
        Some(expected_effective.as_slice())
    );
    assert_eq!(
        inputs.balances, expected_effective,
        "live-admin swaps balances for effective balances"
    );
    assert_eq!(
        inputs.xp, expected_effective,
        "xp uses the effective balances with resolved rates"
    );
}

#[test]
fn metapool_reads_virtual_and_redemption_prices() {
    let provider = StubProvider {
        block_timestamp: 0,
        lending_rates: None,
        virtual_price: Some(E18 + E18 / U256::from(20)), // 1.05e18
        redemption_price: Some(E18),
        // The provider serves no base-cache metadata, so the base rate
        // degrades to the live read (contract `_vp_rate()` with an expired
        // cache).
        base_cache_updated: None,
        base_virtual_price: None,
        d: None,
        gamma: None,
        price_scale: None,
        token_balances: HashMap::new(),
        admin_balances: None,
    };
    let inputs = resolve_dy_inputs(
        &identity(1, 1, true),
        &[E21, TWO_E21],
        Some(&provider),
        1,
        None,
    )
    .expect("metapool resolves");
    assert!(inputs.metapool);
    assert_eq!(inputs.virtual_price, Some(E18 + E18 / U256::from(20)));
    assert_eq!(inputs.scaled_redemption_price, Some(E18));
}

#[test]
fn metapool_without_redemption_degrades_to_none() {
    let provider = StubProvider {
        block_timestamp: 0,
        lending_rates: None,
        virtual_price: Some(E18),
        redemption_price: None, // not supported -> must degrade, not fail
        base_cache_updated: None,
        base_virtual_price: None,
        d: None,
        gamma: None,
        price_scale: None,
        token_balances: HashMap::new(),
        admin_balances: None,
    };
    let inputs = resolve_dy_inputs(
        &identity(1, 1, true),
        &[E21, TWO_E21],
        Some(&provider),
        1,
        None,
    )
    .expect("metapool resolves");
    assert_eq!(inputs.scaled_redemption_price, None);
}

#[test]
fn metapool_requires_a_provider() {
    let err = resolve_dy_inputs(&identity(1, 1, true), &[E21, TWO_E21], None, 1, None)
        .expect_err("metapool without a provider must fail");
    assert!(matches!(
        err,
        crate::curve_dy_io::CurveInputsError::NoProvider(_)
    ));
}

#[test]
fn ramping_a_interpolates_with_timestamp() {
    let mut ident = identity(1, 1, false);
    ident.initial_a_coefficient = Some(100);
    ident.future_a_coefficient = Some(200);
    ident.initial_a_coefficient_time = Some(1_000);
    ident.future_a_coefficient_time = Some(2_000);
    ident.create_timestamp = Some(500);
    ident.a_precision = 100;
    let provider = StubProvider {
        block_timestamp: 1_500, // midpoint of the ramp
        base_cache_updated: None,
        base_virtual_price: None,
        lending_rates: None,
        virtual_price: None,
        redemption_price: None,
        d: None,
        gamma: None,
        price_scale: None,
        token_balances: HashMap::new(),
        admin_balances: None,
    };
    let inputs = resolve_dy_inputs(&ident, &[E21, TWO_E21], Some(&provider), 1, None)
        .expect("ramp resolves");
    // scaled A at the midpoint = 100 + (200-100)*(1500-1000)/(2000-1000) = 150
    // (the ramping A is already the full amplified A, matching the companion
    // `_a` which returns the interpolated value directly, not x A_PRECISION)
    assert_eq!(inputs.amp, U256::from(150_u64));
}

#[test]
fn pool_liquidity_passes_when_every_balance_is_nonzero() {
    assert!(check_pool_liquidity(&[E21, TWO_E21]).is_ok());
}

#[test]
fn pool_liquidity_rejects_a_zero_balance() {
    let err = check_pool_liquidity(&[E21, U256::ZERO]).expect_err("a zero balance must refuse");
    assert!(matches!(
        err,
        crate::curve_dy_io::CurveInputsError::NoLiquidity
    ));
}

#[test]
fn metapool_liquidity_passes_when_both_sides_have_balances() {
    assert!(check_metapool_liquidity(&[E21, TWO_E21], &[E21, E21]).is_ok());
}

#[test]
fn metapool_liquidity_rejects_a_zero_metapool_balance() {
    let err = check_metapool_liquidity(&[U256::ZERO, TWO_E21], &[E21, E21])
        .expect_err("a zero metapool balance must refuse");
    assert!(matches!(
        err,
        crate::curve_dy_io::CurveInputsError::NoLiquidity
    ));
}

#[test]
fn metapool_liquidity_rejects_a_zero_base_balance() {
    let err = check_metapool_liquidity(&[E21, TWO_E21], &[U256::ZERO, E21])
        .expect_err("a zero base-pool balance must refuse");
    assert!(matches!(
        err,
        crate::curve_dy_io::CurveInputsError::NoLiquidity
    ));
}

// ──────────────────────────────────────────────────────────────────────────
// Metapool base-rate source (`_vp_rate()`), pinned to the observed GUSD/3Crv
// state at block 26_204_126: the base cache was last updated at
// `BASE_CACHE_TS`, the cached base vp lags the live base vp by 21_035 wei,
// and the contract prices off the cached value while the cache is fresh.
// ──────────────────────────────────────────────────────────────────────────

const BASE_CACHE_TS: u64 = 1_762_000_000;
const GUSD_CACHED_BASE_VP: U256 = U256::from_limbs([1_039_823_717_425_653_995, 0, 0, 0]);
const GUSD_LIVE_BASE_VP: U256 = U256::from_limbs([1_039_823_717_425_675_030, 0, 0, 0]);

/// A metapool provider offering both base-rate candidates: the cached base vp
/// (as `base_virtual_price`) and the live base-pool vp (as `virtual_price`),
/// so a resolved snapshot names which one the swap path chose.
fn metapool_provider(block_timestamp: u64, base_cache_updated: Option<u64>) -> StubProvider {
    StubProvider {
        block_timestamp,
        lending_rates: None,
        virtual_price: Some(GUSD_LIVE_BASE_VP),
        redemption_price: None,
        d: None,
        gamma: None,
        price_scale: None,
        token_balances: HashMap::new(),
        admin_balances: None,
        base_cache_updated,
        base_virtual_price: Some(GUSD_CACHED_BASE_VP),
    }
}

fn resolve_metapool(provider: &StubProvider) -> degenbot_math::curve::DyCalculationInputs {
    resolve_dy_inputs(
        &identity(1, 1, true),
        &[E21, TWO_E21],
        Some(provider),
        1,
        None,
    )
    .expect("metapool resolves")
}

#[test]
fn metapool_uses_cached_base_virtual_price_while_cache_fresh() {
    let provider = metapool_provider(BASE_CACHE_TS + 300, Some(BASE_CACHE_TS));
    assert_eq!(
        resolve_metapool(&provider).virtual_price,
        Some(GUSD_CACHED_BASE_VP),
        "a fresh base cache prices off `base_virtual_price()`, not the live vp"
    );
}

#[test]
fn metapool_uses_live_base_virtual_price_after_cache_expiry() {
    let provider = metapool_provider(BASE_CACHE_TS + BASE_CACHE_EXPIRES + 1, Some(BASE_CACHE_TS));
    assert_eq!(
        resolve_metapool(&provider).virtual_price,
        Some(GUSD_LIVE_BASE_VP),
        "an expired base cache refetches the base pool's live vp"
    );
}

#[test]
fn metapool_cache_expiry_boundary_is_inclusive() {
    // `block.timestamp <= base_cache_updated + BASE_CACHE_EXPIRES` is fresh.
    let provider = metapool_provider(BASE_CACHE_TS + BASE_CACHE_EXPIRES, Some(BASE_CACHE_TS));
    assert_eq!(
        resolve_metapool(&provider).virtual_price,
        Some(GUSD_CACHED_BASE_VP),
        "the expiry comparison is inclusive at bcu + BASE_CACHE_EXPIRES"
    );
    // One second past the boundary flips to the live read.
    let expired = metapool_provider(BASE_CACHE_TS + BASE_CACHE_EXPIRES + 1, Some(BASE_CACHE_TS));
    assert_eq!(
        resolve_metapool(&expired).virtual_price,
        Some(GUSD_LIVE_BASE_VP)
    );
}

#[test]
fn metapool_cache_updated_ahead_of_timestamp_stays_cached() {
    // An observed state carries `base_cache_updated > block_timestamp`; the
    // expiry arithmetic saturates rather than overflowing.
    let provider = metapool_provider(BASE_CACHE_TS, Some(BASE_CACHE_TS + 5));
    assert_eq!(
        resolve_metapool(&provider).virtual_price,
        Some(GUSD_CACHED_BASE_VP)
    );
}

//! Curve `get_dy` I/O orchestration (epic `TV72EG`, task `BPEM4V`).
//!
//! The Rust twin of the Python companion's `_resolve_calculation_inputs_via_io`
//! (+ the metapool resolver). Given a pool's immutable `CurvePoolIdentity`,
//! its current balances, and the stored I/O provider, it pre-resolves a
//! [`DyCalculationInputs`] snapshot so the pure `degenbot_curve_math::calculate_dy`
//! (which does no I/O) can run. This is what lets a swap path run with **no
//! Python provider / cache / calculator in the graph** — the companion's
//! orchestration is retired once the Rust entry (GW2) drives this.
//!
//! The provider is `Option`: plain pools with no lending/metapool/crypto /
//! live-admin need no I/O beyond balances, so a `None` provider still yields a
//! valid standard snapshot. Styles that require a provider (`lending_rates` /
//! crypto / live-admin / metapool) return `CurveInputsError::NoProvider` when
//! absent, mirroring the Python `MissingCurveData` guard.

use alloy::primitives::U256;
use degenbot_curve_math::{
    resolve_amp, resolve_ramping_a, ARampingParams, DVariant, DyCalculationInputs, SwapStyle,
    YVariant,
};

use crate::curve_data_provider::{CurveDataProvider, CurveDataProviderError};
use crate::curve_state::CurvePoolIdentity;

/// The pool `PRECISION` (18 decimals).
const PRECISION: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
/// The swap-fee denominator (1e10).
const FEE_DENOMINATOR: U256 = U256::from_limbs([10_000_000_000, 0, 0, 0]);

/// `LendingRateStyle` `NONE` discriminant (1-based `auto()`, matches Python).
const LENDING_NONE: u8 = 1;

/// Errors from resolving the dy-calculation inputs.
#[derive(Debug)]
pub enum CurveInputsError {
    /// A swap style / metapool needs provider I/O but no provider is stored.
    NoProvider(&'static str),
    /// A provider fetch failed (RPC / length / unsupported).
    Provider(CurveDataProviderError),
    /// A swap-math (A-ramping) error.
    Swap(degenbot_curve_math::CurveSwapError),
    /// A length mismatch in zipped rate/balance/coin arrays.
    LengthMismatch(&'static str),
}

/// Compute `xp = rate * balance // PRECISION` (strict-zip, matches the
/// companion `_xp`).
fn xp(rates: &[U256], balances: &[U256]) -> Result<Vec<U256>, CurveInputsError> {
    if rates.len() != balances.len() {
        return Err(CurveInputsError::LengthMismatch("rates/balances"));
    }
    Ok(rates
        .iter()
        .zip(balances)
        .map(|(r, b)| *r * *b / PRECISION)
        .collect())
}

/// Resolve the lending rates: `rate_multipliers` directly for `NONE`, else a
/// `lending_rates(block)` provider fetch (matches the companion
/// `_resolve_rates`).
fn resolve_rates(
    identity: &CurvePoolIdentity,
    provider: Option<&dyn CurveDataProvider>,
    block_number: u64,
) -> Result<Vec<U256>, CurveInputsError> {
    if identity.lending_rate_style == LENDING_NONE {
        Ok(identity.rate_multipliers.clone())
    } else {
        let p = provider.ok_or(CurveInputsError::NoProvider("lending_rate"))?;
        p.lending_rates(block_number)
            .map_err(CurveInputsError::Provider)
    }
}

/// Pre-resolve a [`DyCalculationInputs`] snapshot for a Curve pool from its
/// immutable identity + current balances + the stored provider.
///
/// `override_balances` swaps the balance source (the companion's
/// `override_state.balances`); otherwise the pool's current balances are used.
///
/// # Errors
///
/// Returns [`CurveInputsError::NoProvider`] when a style/metapool needs a
/// provider that isn't stored, [`CurveInputsError::Provider`] on a fetch
/// failure, and [`CurveInputsError::LengthMismatch`] on mismatched arrays.
#[allow(clippy::too_many_lines)] // cohesive I/O orchestration ported 1:1 from the companion
pub fn resolve_dy_inputs(
    identity: &CurvePoolIdentity,
    balances: &[U256],
    provider: Option<&dyn CurveDataProvider>,
    block_number: u64,
    override_balances: Option<&[U256]>,
) -> Result<DyCalculationInputs, CurveInputsError> {
    let pool_balances: &[U256] = override_balances.unwrap_or(balances);
    if pool_balances.len() != identity.rate_multipliers.len() {
        return Err(CurveInputsError::LengthMismatch(
            "balances/rate_multipliers",
        ));
    }

    let block_timestamp = match provider {
        Some(p) => p.block_timestamp(block_number).unwrap_or(0),
        None => 0,
    };

    // Amplification: ramping A interpolation, then the YVariant0 A_PRECISION
    // divisor (mirrors the companion `_a` + `amp` resolution).
    let raw_amp = resolve_ramping_a(
        ARampingParams {
            a_coefficient: identity.a_coefficient,
            initial_a_coefficient: identity.initial_a_coefficient,
            future_a_coefficient: identity.future_a_coefficient,
            initial_a_coefficient_time: identity.initial_a_coefficient_time,
            future_a_coefficient_time: identity.future_a_coefficient_time,
            create_timestamp: identity.create_timestamp,
            a_precision: u32::try_from(identity.a_precision).unwrap_or(0),
        },
        block_timestamp,
    )
    .map_err(CurveInputsError::Swap)?;
    let y_variant = YVariant::try_from_u8(identity.y_variant).unwrap_or(YVariant::Standard);
    let amp = resolve_amp(raw_amp, U256::from(identity.a_precision), y_variant);

    let resolved_rates = resolve_rates(identity, provider, block_number)?;
    let xp_v = xp(&resolved_rates, pool_balances)?;

    let swap_style = SwapStyle::try_from_u8(identity.swap_style).unwrap_or(SwapStyle::Standard);
    let metapool = identity.base_pool.is_some();

    let mut inputs = DyCalculationInputs {
        precision: PRECISION,
        fee_denominator: FEE_DENOMINATOR,
        fee: U256::from(identity.fee),
        n_coins: pool_balances.len(),
        balances: pool_balances.to_vec(),
        rate_multipliers: identity.rate_multipliers.clone(),
        precision_multipliers: identity.precision_multipliers.clone(),
        offpeg_fee_multiplier: U256::from(identity.offpeg_fee_multiplier.unwrap_or_default()),
        fee_gamma: U256::from(identity.fee_gamma.unwrap_or_default()),
        mid_fee: U256::from(identity.mid_fee.unwrap_or_default()),
        out_fee: U256::from(identity.out_fee.unwrap_or_default()),
        address: identity.address,
        resolved_rates: resolved_rates.clone(),
        xp: xp_v,
        block_number,
        block_timestamp,
        amp,
        d_variant: DVariant::try_from_u8(identity.d_variant).unwrap_or(DVariant::Standard),
        y_variant,
        a_precision: U256::from(identity.a_precision),
        swap_style: identity.swap_style,
        metapool,
        metapool_rate_style: identity.metapool_rate_style,
        metapool_underlying_style: identity.metapool_underlying_style,
        d: None,
        gamma: None,
        price_scale: None,
        live_balances: None,
        admin_balances: None,
        effective_balances: None,
        virtual_price: None,
        scaled_redemption_price: None,
    };

    // Metapool virtual/redemption prices.
    if metapool {
        let p = provider.ok_or(CurveInputsError::NoProvider("metapool virtual_price"))?;
        inputs.virtual_price = Some(
            p.virtual_price(block_number)
                .map_err(CurveInputsError::Provider)?,
        );
        inputs.scaled_redemption_price = p.redemption_price(block_number).ok();
    }

    // Crypto d/gamma/price_scale.
    if swap_style == SwapStyle::Crypto {
        let p = provider.ok_or(CurveInputsError::NoProvider("crypto"))?;
        inputs.d = Some(p.d(block_number).map_err(CurveInputsError::Provider)?);
        inputs.gamma = Some(p.gamma(block_number).map_err(CurveInputsError::Provider)?);
        inputs.price_scale = Some(
            p.price_scale(block_number)
                .map_err(CurveInputsError::Provider)?,
        );
    }

    // Live-admin balances -> effective balances (and Oracle rate re-resolve).
    if matches!(
        swap_style,
        SwapStyle::LiveAdmin
            | SwapStyle::LiveAdminDynamic
            | SwapStyle::LiveAdminDynamicPrecision
            | SwapStyle::LiveAdminOracle
    ) {
        let p = provider.ok_or(CurveInputsError::NoProvider("live_admin"))?;
        let n = pool_balances.len();
        let mut live = Vec::with_capacity(n);
        for &tok in &identity.tokens {
            live.push(
                p.token_balance(tok, identity.address, block_number)
                    .map_err(CurveInputsError::Provider)?,
            );
        }
        let admin = p
            .admin_balances(block_number)
            .map_err(CurveInputsError::Provider)?;
        if admin.len() != n {
            return Err(CurveInputsError::LengthMismatch("admin_balances"));
        }
        let mut effective = Vec::with_capacity(n);
        for (l, a) in live.iter().zip(&admin) {
            effective.push(l.checked_sub(*a).ok_or(CurveInputsError::LengthMismatch(
                "admin_balance exceeds live",
            ))?);
        }

        // LIVE_ADMIN_ORACLE re-resolves rates from the pool's rate multipliers;
        // other live-admin styles reuse the already-resolved rates.
        let oracle_rates = if swap_style == SwapStyle::LiveAdminOracle {
            resolve_rates(identity, Some(p), block_number)?
        } else {
            resolved_rates.clone()
        };
        let oracle_xp = xp(&oracle_rates, &effective)?;

        inputs.live_balances = Some(live);
        inputs.admin_balances = Some(admin);
        inputs.effective_balances = Some(effective.clone());
        inputs.balances = effective;
        inputs.resolved_rates = oracle_rates;
        inputs.xp = oracle_xp;
    }

    Ok(inputs)
}

#[cfg(test)]
mod tests;

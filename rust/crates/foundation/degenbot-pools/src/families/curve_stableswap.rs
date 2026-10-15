//! The Curve stableswap family read module (ADR-073 D1) — the foundation-layer
//! home of everything the Curve stableswap family contributes to being read
//! and resolved.
//!
//! GLOSSARY.md "Family read module": this tree owns the state slice (the
//! I/O-free `crate::curve_state` types, ADR-001), the swap-read arm (the
//! standard-stableswap exact-input read; the non-`STANDARD` swap styles keep
//! their pre-port `Ok(ZERO)` sentinel — see the read door's docs), the resolve
//! projection as a pure function of the family state slice, and the
//! family-local apply/get/calc behavior over that slice. It reads NO state
//! owner and consumes NO solver kernel — the hop is solver vocabulary, so the
//! wrapper stays at the dispatch row (the substrate resolve row does the
//! registry lookup and wraps the solvers crate's `ResolvedHop`).
//!
//! # The four intent doors (ADR-073 D4)
//!
//! - [`capture_hop`] — the resolve projection: one fresh projection of one
//!   hop, refused rather than defaulted;
//! - [`read_swap`] — the swap-read arm, ALL variants (the standard
//!   stableswap exact-input read plus the not-yet-ported swap-style sentinel)
//!   behind one entry;
//! - [`apply_event`] — one buffered balance event onto one slice;
//! - [`observe`] — family-local GET behavior over one slice.
//!
//! The family-local calc doors ([`get_dy`], [`calc_token_amount`],
//! [`calc_withdraw_one_coin`]) are the pure-over-slice halves of the
//! orchestration rows in `degenbot-substrate` (GLOSSARY "CL orchestration
//! capability" pattern): the state owner keeps the registry lookups, the
//! base-pool port, and the live-state bookkeeping; this module owns the
//! family behavior.
//!
//! # The gate-bounds contract is NOT re-litigated here
//!
//! Curve stableswap keeps the native reserve cap ALONE in the solve-side
//! profit gate (no entry tangent — the leaf's out-curve was measured
//! non-concave; see
//! `docs/architecture/stableswap-gate-bounds-and-solve-method-evaluation.md`
//! and the solver-side family module `degenbot-solvers` curve lines). Nothing
//! in this read module touches envelope lines.

use alloy::primitives::U256;

use degenbot_math::curve::resolve_ramping_a;

use crate::curve_dy_io::{check_pool_liquidity, resolve_dy_inputs, CurveInputsError};
use crate::state_history::BalanceVectorPoolState as _;
use crate::v3_state::SimulateSwapError;

use super::balance_vector::{BalanceVectorEvent, Observe, Observed};

// Curve stableswap structural constants (Vyper contract literal values).
/// Curve native precision scale (1e18).
pub(crate) const CURVE_PRECISION: u64 = 1_000_000_000_000_000_000;
/// Curve fee denominator (1e10).
pub(crate) const CURVE_FEE_DENOMINATOR: u64 = 10_000_000_000;

/// The state slice + its registration DTOs — the family's read surface lives
/// in `crate::curve_state` (I/O-free per ADR-001) and is re-exported here so
/// the family tree presents the whole read story under one path.
pub use crate::curve_state::{CurvePoolIdentity, CurvePoolState, RegisterCurvePoolParams};

/// Why [`capture_hop`] refused to project a hop — the projection is refused,
/// never defaulted. The dispatch row maps these onto its own invalidation
/// vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureRefusal {
    /// Fewer than two coins — no pairwise hop can be formed.
    TooFewTokens,
    /// A variant byte (`y_variant` / `d_variant`) decoded to nothing.
    UnknownVariant,
    /// A pairwise index fell outside the rate-adjusted coin list.
    OutOfRange,
}

/// The Curve stableswap hop projection — everything one Curve hop contributes
/// to a solve, derived purely from the family state slice. The substrate
/// resolve row wraps this into the solver crate's hop vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurveStableswapHop {
    /// `amp = a_coefficient * A_PRECISION` (Curve convention).
    pub amp: U256,
    /// `A_PRECISION` (100 for standard pools).
    pub a_precision: U256,
    /// Rate-adjusted balances `xp[i] = balances[i] * rate_multipliers[i] / PRECISION`.
    /// Pre-computed at resolve time.
    pub xp: Vec<U256>,
    /// Token index of the input.
    pub token_index_in: usize,
    /// Token index of the output.
    pub token_index_out: usize,
    /// Number of coins (== `xp.len()`).
    pub n_coins: U256,
    /// Swap fee in `FEE_DENOMINATOR = 1e10` units.
    pub fee: U256,
    /// `FEE_DENOMINATOR = 10^10`.
    pub fee_denom: U256,
    /// `PRECISION = 10^18`.
    pub precision: U256,
    /// Rate multiplier for the input token.
    pub rate_multiplier_in: U256,
    /// Rate multiplier for the output token.
    pub rate_multiplier_out: U256,
    /// y-iteration variant.
    pub y_variant: degenbot_math::curve::stableswap::YVariant,
    /// d-iteration variant.
    pub d_variant: degenbot_math::curve::stableswap::DVariant,
}

/// The capture door — the resolve projection: one fresh projection of one
/// Curve stableswap hop, refused rather than defaulted. Pure over the family
/// state slice; the dispatch row does the registry lookup and the
/// solver-vocabulary wrap.
///
/// # Errors
///
/// [`CaptureRefusal`] — see the type's docs for the reasons.
pub fn capture_hop(
    state: &CurvePoolState,
    identity: &CurvePoolIdentity,
    zero_for_one: bool,
) -> Result<CurveStableswapHop, CaptureRefusal> {
    if identity.tokens.len() < 2 {
        return Err(CaptureRefusal::TooFewTokens);
    }
    let (token_index_in, token_index_out) = if zero_for_one { (0, 1) } else { (1, 0) };
    // Curve constants (degenbot_math::curve owns the convention).
    let precision = U256::from(CURVE_PRECISION);
    let fee_denom = U256::from(CURVE_FEE_DENOMINATOR);
    let a_precision = U256::from(identity.a_precision);
    let amp = U256::from(identity.a_coefficient).saturating_mul(a_precision);
    let n_coins = U256::from(identity.tokens.len() as u64);
    // Build rate-adjusted XP: xp[i] = balances[i] * rate_multipliers[i] / X_PRECISION
    let xp: Vec<U256> = state
        .balances
        .iter()
        .zip(identity.rate_multipliers.iter())
        .map(|(b, rm)| b.saturating_mul(*rm) / precision)
        .collect();
    if token_index_in >= xp.len() || token_index_out >= xp.len() {
        return Err(CaptureRefusal::OutOfRange);
    }
    let y_variant = degenbot_math::curve::stableswap::YVariant::try_from_u8(identity.y_variant)
        .ok_or(CaptureRefusal::UnknownVariant)?;
    let d_variant = degenbot_math::curve::stableswap::DVariant::try_from_u8(identity.d_variant)
        .ok_or(CaptureRefusal::UnknownVariant)?;
    Ok(CurveStableswapHop {
        amp,
        a_precision,
        xp,
        token_index_in,
        token_index_out,
        n_coins,
        fee: U256::from(identity.fee),
        fee_denom,
        precision,
        rate_multiplier_in: identity.rate_multipliers[token_index_in],
        rate_multiplier_out: identity.rate_multipliers[token_index_out],
        y_variant,
        d_variant,
    })
}

/// The swap-read door — ALL variants behind one entry. The standard
/// stableswap exact-input read (rate-adjusted `xp`, `get_y`, fee + rate
/// descale, `ConversionStyle.FEE_THEN_RATE`) plus the pre-port sentinel for
/// the non-`STANDARD` swap styles (crypto, live-admin, metapool `get_dy`
/// base-pool dispatch — the Python companions keep doing their own math via
/// `swap_fn` for those, so the Rust core returns `Ok(ZERO)`).
///
/// # Errors
///
/// [`SimulateSwapError::NotComputable`] on arithmetic overflow / invariant
/// violation / non-positive amount. (The family carries no sparse-map miss —
/// a balance-vector slice has no tick words to miss.)
pub fn read_swap(
    state: &CurvePoolState,
    identity: &CurvePoolIdentity,
    zero_for_one: bool,
    amount_in: U256,
) -> Result<U256, SimulateSwapError> {
    // Only the standard stableswap path is ported; advertise the missing ones
    // as the pre-port sentinel (matches the long-standing `Ok(U256::ZERO)` arm).
    if identity.swap_style != 1 {
        return Ok(U256::ZERO);
    }
    if amount_in.is_zero() {
        return Ok(U256::ZERO);
    }
    let n_coins = identity.n_coins();
    if n_coins < 2 {
        return Ok(U256::ZERO);
    }
    // 2-token structural convention: zero_for_one → coin (0, 1), else (1, 0).
    let (coin_in, coin_out) = if zero_for_one {
        (0usize, 1usize)
    } else {
        (1usize, 0usize)
    };

    let a_precision = U256::from(identity.a_precision);
    let n_u = U256::from(n_coins);

    // Resolve A. Python `_a()` returns `a_coefficient * A_PRECISION`; the
    // stableswap `amp` is that raw product for the standard path (only
    // `VARIANT_0` divides by A_PRECISION).
    let amp = U256::from(identity.a_coefficient)
        .checked_mul(a_precision)
        .ok_or(SimulateSwapError::NotComputable)?;

    // Rate-adjust balances into xp (Python: `rate * balance // PRECISION`).
    let xp: Vec<U256> = state
        .balances
        .iter()
        .zip(&identity.rate_multipliers)
        .map(|(&balance, &rate_mult)| {
            rate_mult
                .checked_mul(balance)
                .ok_or(SimulateSwapError::NotComputable)
                .map(|v| v / U256::from(CURVE_PRECISION))
        })
        .collect::<Result<_, _>>()?;
    if xp.len() != n_coins {
        return Ok(U256::ZERO);
    }

    // x = xp[coin_in] + (dx * rates[coin_in] // PRECISION)
    let dx_scaled = amount_in
        .checked_mul(identity.rate_multipliers[coin_in])
        .ok_or(SimulateSwapError::NotComputable)?
        / U256::from(CURVE_PRECISION);
    let x = xp[coin_in]
        .checked_add(dx_scaled)
        .ok_or(SimulateSwapError::NotComputable)?;

    // Variant dispatch: map the opaque u8 discriminants (1-based auto(); the
    // standard path defaults to STANDARD when an unrecognised/zero value is
    // carried, as the slice fixtures do).
    let y_variant = degenbot_math::curve::stableswap::YVariant::try_from_u8(identity.y_variant)
        .unwrap_or(degenbot_math::curve::stableswap::YVariant::Standard);
    let d_variant = degenbot_math::curve::stableswap::DVariant::try_from_u8(identity.d_variant)
        .unwrap_or(degenbot_math::curve::stableswap::DVariant::Standard);

    let y = degenbot_math::curve::stableswap::stableswap_get_y(
        coin_in,
        coin_out,
        x,
        &xp,
        amp,
        n_u,
        a_precision,
        y_variant,
        d_variant,
    )
    .map_err(|_| SimulateSwapError::NotComputable)?;

    // dy = xp[coin_out] - y - 1
    let one = U256::from(1u8);
    let raw_dy = xp[coin_out]
        .checked_sub(y)
        .ok_or(SimulateSwapError::NotComputable)?;
    let raw_dy = if raw_dy.is_zero() {
        raw_dy
    } else {
        raw_dy
            .checked_sub(one)
            .ok_or(SimulateSwapError::NotComputable)?
    };

    // fee = fee * dy // FEE_DENOMINATOR; return (dy - fee) * PRECISION // rate_out
    let fee = U256::from(identity.fee)
        .checked_mul(raw_dy)
        .ok_or(SimulateSwapError::NotComputable)?
        / U256::from(CURVE_FEE_DENOMINATOR);
    let dy_less_fee = raw_dy
        .checked_sub(fee)
        .ok_or(SimulateSwapError::NotComputable)?;
    let out = dy_less_fee
        .checked_mul(U256::from(CURVE_PRECISION))
        .ok_or(SimulateSwapError::NotComputable)?
        / identity.rate_multipliers[coin_out];
    Ok(out)
}

/// The apply door — one buffered balance event (a Curve `Exchange`) onto one
/// slice. The journal + clock discipline lives on the slice itself
/// ([`crate::state_history::BalanceVectorPoolState::apply_balance_update`],
/// ADR-017 D1); the door is the family's named apply arm. The neutral
/// `apply_balance_update_by_pool_id` orchestration row stays on the
/// family-blind trait projection (`PoolEntry::as_balance_vector_mut`) — a
/// per-family match there would reintroduce the arms ADR-017 D1 collapsed.
pub fn apply_event(state: &mut CurvePoolState, event: BalanceVectorEvent) {
    state.apply_balance_update(event.balances, event.block_number);
}

/// The observe door — family-local GET behavior over one slice.
#[must_use]
pub fn observe(state: &CurvePoolState, probe: Observe) -> Observed {
    match probe {
        Observe::UpdateBlock => Observed::UpdateBlock(state.update_block),
        Observe::StateNonce => Observed::StateNonce(state.state_nonce),
        Observe::TokenCount => Observed::TokenCount(state.balances.len()),
    }
}

/// The family's `get_dy` calc door — the pure-over-slice half of the
/// orchestration row. Resolves the dy-calculation snapshot from the pool's
/// identity + balances + stored provider via [`resolve_dy_inputs`], then runs
/// the pure [`degenbot_math::curve::calculate_dy`]. The contract's
/// zero-balance refusal is evaluated where the balance vector is read.
///
/// # Errors
///
/// [`CurveInputsError`] — the liquidity, input-resolution, and swap-math
/// errors propagate unchanged from the orchestration row this door serves.
pub fn get_dy(
    state: &CurvePoolState,
    identity: &CurvePoolIdentity,
    i: usize,
    j: usize,
    dx: U256,
    block_number: u64,
    override_balances: Option<&[U256]>,
) -> Result<U256, CurveInputsError> {
    check_pool_liquidity(&state.balances)?;
    let provider = state.data_provider.as_deref();
    let inputs = resolve_dy_inputs(
        identity,
        &state.balances,
        provider,
        block_number,
        override_balances,
    )?;
    degenbot_math::curve::calculate_dy(i, j, dx, &inputs).map_err(CurveInputsError::Swap)
}

/// The family's `calc_token_amount` calc door — the pure-over-slice half of
/// the orchestration row (the twin of the companion `calc_token_amount`).
/// Computes `D` before and after the balance change and scales by the LP
/// total supply (fetched via the stored provider).
///
/// # Errors
///
/// [`CurveInputsError`] — see [`get_dy`].
pub fn calc_token_amount(
    state: &CurvePoolState,
    identity: &CurvePoolIdentity,
    amounts: &[U256],
    deposit: bool,
    block_number: u64,
) -> Result<U256, CurveInputsError> {
    let n = identity.n_coins();
    if amounts.len() != n {
        return Err(CurveInputsError::LengthMismatch("amounts/coins"));
    }
    let provider = state.data_provider.as_deref();
    let timestamp = curve_block_timestamp(identity, provider, block_number)?;
    let amp = resolve_ramping_a(curve_ramping_params(identity), timestamp)?;
    let a_precision = U256::from(identity.a_precision);
    let d_variant = degenbot_math::curve::DVariant::try_from_u8(identity.d_variant)
        .ok_or(degenbot_math::curve::CurveSwapError::UnknownStyle(99))?;
    let n_u = U256::from(n);

    let d_0 = degenbot_math::curve::stableswap_get_d(
        &curve_base_xp(identity, &state.balances)?,
        amp,
        n_u,
        a_precision,
        d_variant,
    )?;

    let mut pool_balances = state.balances.to_vec();
    for (b, a) in pool_balances.iter_mut().zip(amounts) {
        *b = if deposit { *b + *a } else { *b - *a };
    }
    let d_1 = degenbot_math::curve::stableswap_get_d(
        &curve_base_xp(identity, &pool_balances)?,
        amp,
        n_u,
        a_precision,
        d_variant,
    )?;

    let token_amount = curve_total_supply(identity, provider, block_number)?;
    let diff = if deposit { d_1 - d_0 } else { d_0 - d_1 };
    Ok(diff * token_amount / d_0)
}

/// The family's `calc_withdraw_one_coin` calc door — the pure-over-slice half
/// of the orchestration row. Returns the single coin-`i` output `dy`.
///
/// # Errors
///
/// [`CurveInputsError`] — see [`get_dy`].
pub fn calc_withdraw_one_coin(
    state: &CurvePoolState,
    identity: &CurvePoolIdentity,
    token_amount: U256,
    i: usize,
    block_number: u64,
) -> Result<U256, CurveInputsError> {
    let n = identity.n_coins();
    if i >= n {
        return Err(CurveInputsError::Swap(
            degenbot_math::curve::CurveSwapError::IndexOutOfBounds,
        ));
    }
    let provider = state.data_provider.as_deref();
    let timestamp = curve_block_timestamp(identity, provider, block_number)?;
    let amp = resolve_ramping_a(curve_ramping_params(identity), timestamp)?;
    let a_precision = U256::from(identity.a_precision);
    let d_variant = degenbot_math::curve::DVariant::try_from_u8(identity.d_variant)
        .ok_or(degenbot_math::curve::CurveSwapError::UnknownStyle(99))?;
    let yd_variant = degenbot_math::curve::YDVariant::try_from_u8(identity.yd_variant)
        .ok_or(degenbot_math::curve::CurveSwapError::UnknownStyle(99))?;
    let n_u = U256::from(n);
    let precisions = &identity.precision_multipliers;

    let xp = curve_base_xp(identity, &state.balances)?;
    let d_0 = degenbot_math::curve::stableswap_get_d(&xp, amp, n_u, a_precision, d_variant)?;
    let total_supply = curve_total_supply(identity, provider, block_number)?;
    let d_1 = d_0 - token_amount * d_0 / total_supply;
    let new_y =
        degenbot_math::curve::stableswap_get_y_d(amp, i, &xp, d_1, n_u, a_precision, yd_variant)?;
    let raw_dy_0 = (xp[i] - new_y) / precisions[i];

    let n_u64 = n as u64;
    let mut xp_reduced = xp.clone();
    let fee = identity.fee * n_u64 / (4 * (n_u64 - 1));
    let fee_u = U256::from(fee);
    for (j, x) in xp_reduced.iter_mut().enumerate() {
        let dx_expected = if j == i {
            xp[j] * d_1 / d_0 - new_y
        } else {
            xp[j] - xp[j] * d_1 / d_0
        };
        *x -= fee_u * dx_expected / U256::from(CURVE_FEE_DENOMINATOR);
    }

    let dy = xp_reduced[i]
        - degenbot_math::curve::stableswap_get_y_d(
            amp,
            i,
            &xp_reduced,
            d_1,
            n_u,
            a_precision,
            yd_variant,
        )?;
    let _ = raw_dy_0; // kept only to mirror the companion's tuple shape; not returned
    Ok((dy - U256::from(1u8)) / precisions[i])
}

/// Compute the base pool's `xp = rate * balance // PRECISION` from its
/// immutable `rate_multipliers` + a balance vector (twin of the companion
/// `_xp`).
fn curve_base_xp(
    identity: &CurvePoolIdentity,
    balances: &[U256],
) -> Result<Vec<U256>, CurveInputsError> {
    if identity.rate_multipliers.len() != balances.len() {
        return Err(CurveInputsError::LengthMismatch("rates/balances"));
    }
    Ok(identity
        .rate_multipliers
        .iter()
        .zip(balances)
        .map(|(r, b)| *r * *b / U256::from(CURVE_PRECISION))
        .collect())
}

/// Build the `ARampingParams` the `resolve_ramping_a` twin reads, from a
/// pool's immutable A-ramping identity fields.
fn curve_ramping_params(identity: &CurvePoolIdentity) -> degenbot_math::curve::ARampingParams {
    degenbot_math::curve::ARampingParams {
        a_coefficient: identity.a_coefficient,
        initial_a_coefficient: identity.initial_a_coefficient,
        future_a_coefficient: identity.future_a_coefficient,
        initial_a_coefficient_time: identity.initial_a_coefficient_time,
        future_a_coefficient_time: identity.future_a_coefficient_time,
        create_timestamp: identity.create_timestamp,
        a_precision: u32::try_from(identity.a_precision).unwrap_or(0),
    }
}

/// Resolve the block timestamp for A-ramping. Only a pool that actually ramps
/// (has end-time set) needs a provider block-timestamp fetch; plain pools
/// return `0` and `resolve_ramping_a` ignores it.
fn curve_block_timestamp(
    identity: &CurvePoolIdentity,
    provider: Option<&dyn crate::curve_data_provider::CurveDataProvider>,
    block_number: u64,
) -> Result<u64, CurveInputsError> {
    if identity.future_a_coefficient_time.is_none() {
        return Ok(0);
    }
    let p = provider.ok_or(CurveInputsError::NoProvider("block_timestamp"))?;
    p.block_timestamp(block_number)
        .map_err(CurveInputsError::Provider)
}

/// Fetch a pool's LP-token total supply via its stored provider (`lp_token`
/// falling back to the pool token when the LP IS the pool token).
fn curve_total_supply(
    identity: &CurvePoolIdentity,
    provider: Option<&dyn crate::curve_data_provider::CurveDataProvider>,
    block_number: u64,
) -> Result<U256, CurveInputsError> {
    let p = provider.ok_or(CurveInputsError::NoProvider("token_total_supply"))?;
    let lp = identity.lp_token.unwrap_or(identity.tokens[0]);
    p.token_total_supply(lp, block_number)
        .map_err(CurveInputsError::Provider)
}

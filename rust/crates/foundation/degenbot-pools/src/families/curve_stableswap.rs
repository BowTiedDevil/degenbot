//! The Curve stableswap family read module (ADR-073 D1) — the foundation-layer
//! home of everything the Curve stableswap family contributes to being read
//! and resolved.
//!
//! GLOSSARY.md "Family read module": this tree owns the state slice (the
//! I/O-free `crate::curve_state` types, ADR-001), the swap-read arm (the
//! standard-stableswap exact-input read; the non-`STANDARD` swap styles
//! answer the typed [`SimulateSwapError::NotComputable`] refusal — a zero
//! output always means a real zero-output swap; see the read door's docs)),
//! the resolve
//! projection as a pure function of the family state slice, and the
//! family-local apply/get/calc behavior over that slice. It reads NO state
//! owner and consumes NO solver kernel — the hop is solver vocabulary, so the
//! wrapper stays at the dispatch row (the substrate resolve row does the
//! registry lookup and wraps the solver's `ResolvedHop`).
//!
//! # The four intent doors (ADR-073 D4)
//!
//! - [`capture_hop`] — the resolve projection: one fresh projection of one
//!   hop, refused rather than defaulted;
//! - [`read_swap`] — the swap-read arm, ALL variants (the standard
//!   stableswap exact-input read plus the typed non-`STANDARD`-style
//!   refusal) behind one entry;
//! - [`apply_event`] — one buffered balance event onto one slice;
//! - [`observe`] — family-local GET behavior over one slice.
//!
//! The family-local calc doors ([`get_dy`], [`calc_token_amount`],
//! [`calc_withdraw_one_coin`]) are the pure-over-slice halves of the
//! orchestration rows in `degenbot-substrate` (GLOSSARY "concentrated liquidity orchestration
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
//! and this file's nested `solve` module curve lines). Nothing
//! in this read module touches envelope lines.

use alloy::primitives::U256;

use degenbot_math::curve::resolve_ramping_a;

use crate::curve_dy_io::{check_pool_liquidity, resolve_dy_inputs, CurveInputsError};
use crate::state_history::BalanceVectorPoolState as _;
use crate::v3_state::SimulateSwapError;

use super::balance_vector::BalanceVectorEvent;
use super::door::{FamilyApply, Observe, Observed, ReadOutcome, SwapReadRequest};

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
/// resolve row wraps this into the solve tree's hop vocabulary.
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
/// descale, `ConversionStyle.FEE_THEN_RATE`) plus the typed refusal for the
/// non-`STANDARD` swap styles (crypto, live-admin, metapool `get_dy`
/// base-pool dispatch). Those styles' `get_dy` math is owned by the
/// calculator path (`curve_dy_calculator`), not this read arm — this door
/// answers typed refusals for non-computable cases, so a zero output must
/// mean a real zero-output swap; the typed refusal says "not this door's
/// computation" instead. The companion's Curve read path consumes
/// `curve_get_dy`, never this door, and no consumer distinguishes a zero
/// output from the refusal.
///
/// # Errors
///
/// [`SimulateSwapError::NotComputable`] on arithmetic overflow / invariant
/// violation / non-positive amount, and on the degenerate-structure shapes
/// (`n_coins < 2`, `xp.len() != n_coins`) — both answer the typed refusal,
/// never a zero output: the companion's Curve read path consumes
/// `curve_get_dy`, never this door, and no consumer distinguishes a zero
/// output from a refusal.
/// (The family carries no sparse-map miss — a balance-vector slice has no
/// tick words to miss.)
#[must_use]
pub fn read_swap(
    state: &CurvePoolState,
    identity: &CurvePoolIdentity,
    request: SwapReadRequest<'_>,
) -> ReadOutcome {
    // The family answers only the exact-input read; every other variant of
    // the shared request vocabulary (the pair reads, the concentrated liquidity canonical
    // request) is a typed refusal.
    let SwapReadRequest::ExactIn {
        zero_for_one,
        amount_in,
    } = request
    else {
        return ReadOutcome::NotComputable;
    };
    ReadOutcome::simple(read_standard_swap(state, identity, zero_for_one, amount_in))
}

/// The standard stableswap exact-input read arm behind [`read_swap`].
fn read_standard_swap(
    state: &CurvePoolState,
    identity: &CurvePoolIdentity,
    zero_for_one: bool,
    amount_in: U256,
) -> Result<U256, SimulateSwapError> {
    // Only the standard stableswap path is ported; the other styles answer the
    // typed refusal (`curve_get_dy` / `calculate_dy` are the Rust surfaces
    // that DO compute those styles).
    if identity.swap_style != 1 {
        return Err(SimulateSwapError::NotComputable);
    }
    if amount_in.is_zero() {
        return Ok(U256::ZERO);
    }
    let n_coins = identity.n_coins();
    // Degenerate structure: fewer than two coins admits no pairwise hop —
    // a typed refusal, never a zero output that reads as a real swap result.
    if n_coins < 2 {
        return Err(SimulateSwapError::NotComputable);
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
    // Degenerate structure: the rate-adjusted balance vector does not cover
    // the coin set — a typed refusal, never a zero output.
    if xp.len() != n_coins {
        return Err(SimulateSwapError::NotComputable);
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
impl FamilyApply for CurvePoolState {
    type Event = BalanceVectorEvent;

    fn apply_event(&mut self, event: Self::Event) {
        self.apply_balance_update(event.balances, event.block_number);
    }
}

/// The observe door — family-local GET behavior over one slice. Every probe
/// this family carries is answerable over the slice; a probe another family
/// carries is a caller bug (the door refuses to answer it rather than
/// defaulting).
///
/// # Panics
///
/// Only on a foreign probe (never constructible by a correct caller — the
/// same backstop posture as the compiler-enforced event/slice pairing).
#[must_use]
pub fn observe(state: &CurvePoolState, probe: Observe) -> Observed {
    match probe {
        Observe::UpdateBlock => Observed::UpdateBlock(state.update_block),
        Observe::StateNonce => Observed::StateNonce(state.state_nonce),
        Observe::TokenCount => Observed::TokenCount(state.balances.len()),
        _ => unreachable!("a balance-vector slice answers only the vector probes"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve_state::CurvePoolIdentity;

    // A non-STANDARD Curve swap style answers the TYPED refusal at the read
    // door, never a zero output (the companion's Curve read path consumes
    // curve_get_dy, which still computes those styles through the calculator,
    // and no consumer distinguishes a zero output from the refusal).
    #[test]
    fn non_standard_swap_style_answers_the_typed_refusal_not_a_zero() {
        let identity = CurvePoolIdentity {
            address: alloy::primitives::Address::ZERO,
            tokens: vec![alloy::primitives::Address::new([0x21u8; 20]); 2].into(),
            a_coefficient: 100,
            a_precision: 100,
            fee: 4_000_000,
            admin_fee: 0,
            rate_multipliers: vec![U256::from(CURVE_PRECISION); 2].into(),
            swap_style: 4, // CRYPTO - a non-STANDARD style
            lending_rate_style: 0,
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
            use_lending: Box::default(),
            precision_multipliers: vec![U256::from(CURVE_PRECISION); 2].into(),
            tokens_underlying: None,
            metapool_rate_style: 1,
            metapool_underlying_style: 1,
        };
        let state = CurvePoolState {
            balances: vec![U256::from(1_000_000u64), U256::from(1_000_000u64)].into(),
            update_block: 1,
            state_nonce: 1,
            journal: crate::state_history::ReorgJournal::new(8),
            data_provider: None,
        };
        let verdict = read_swap(
            &state,
            &identity,
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(1_000u64),
            },
        );
        assert!(
            matches!(verdict, ReadOutcome::NotComputable),
            "a non-STANDARD style answers the typed refusal, got {verdict:?}"
        );
    }

    // The degenerate-structure pins: `n_coins < 2` and
    // `xp.len() != n_coins` answer the TYPED refusal, never an
    // `Ok(U256::ZERO)` output that reads as a real zero-output swap. A
    // one-coin registration IS admission-reachable (the Curve row validates
    // only vector-length AGREEMENT), so the refusal is live behavior.

    fn degenerate_identity(n_tokens: usize) -> CurvePoolIdentity {
        CurvePoolIdentity {
            address: alloy::primitives::Address::ZERO,
            tokens: vec![alloy::primitives::Address::new([0x21u8; 20]); n_tokens].into(),
            a_coefficient: 100,
            a_precision: 100,
            fee: 4_000_000,
            admin_fee: 0,
            rate_multipliers: vec![U256::from(CURVE_PRECISION); n_tokens].into(),
            swap_style: 1,
            lending_rate_style: 0,
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
            use_lending: Box::default(),
            precision_multipliers: vec![U256::from(CURVE_PRECISION); n_tokens].into(),
            tokens_underlying: None,
            metapool_rate_style: 1,
            metapool_underlying_style: 1,
        }
    }

    fn degenerate_state(n_balances: usize) -> CurvePoolState {
        CurvePoolState {
            balances: vec![U256::from(1_000_000u64); n_balances].into(),
            update_block: 1,
            state_nonce: 1,
            journal: crate::state_history::ReorgJournal::new(8),
            data_provider: None,
        }
    }

    #[test]
    fn fewer_than_two_coins_answers_the_typed_refusal_not_a_zero() {
        let identity = degenerate_identity(1);
        let state = degenerate_state(1);
        let verdict = read_swap(
            &state,
            &identity,
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(1_000u64),
            },
        );
        assert!(
            matches!(verdict, ReadOutcome::NotComputable),
            "n_coins < 2 answers the typed refusal, got {verdict:?}"
        );
    }

    #[test]
    fn rate_adjusted_vector_shorter_than_the_coin_set_answers_the_typed_refusal() {
        // xp is built by zipping balances with rate multipliers; a one-element
        // balance vector against a two-coin identity leaves `xp.len() != n_coins`.
        let identity = degenerate_identity(2);
        let state = degenerate_state(1);
        let verdict = read_swap(
            &state,
            &identity,
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(1_000u64),
            },
        );
        assert!(
            matches!(verdict, ReadOutcome::NotComputable),
            "xp.len() != n_coins answers the typed refusal, got {verdict:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The curve_stableswap family's solve entry (ADR-073 endgame): the solve half re-homed from
// the dissolved solver crate — the family story is ONE file again.
// ---------------------------------------------------------------------------
pub(crate) mod solve {
    //! The Curve stableswap family module (ADR-071 D1).
    //!
    //! GLOSSARY.md "Family module": the solve tree's single module owning
    //! everything the Curve stableswap hop family contributes to a solve and its
    //! bound.
    //!
    //! What lives here:
    //! - the family's resolved state slice ([`CurveStableswapHopState`]);
    //! - the family's gate projection ([`curve_to_math`]) - Curve to
    //!   `HopMath::CurveStableswap`;
    //! - the family's path-level solve entry ([`solve_curve_path_int`]) and its
    //!   dispatch-row adapter ([`solve_curve_entry`]), built on the shared
    //!   golden-section template that stays family-blind in `mixed::solve`;
    //! - the V2-equivalent projection ([`curve_hop_v2_equiv`]) that template's
    //!   Mobius precheck consumes;
    //! - the simulate triple ([`simulate_curve_path`] / `_outputs` /
    //!   [`simulate_curve_hop`]) over the `degenbot-math` Curve stableswap leaf;
    //! - the family's envelope lines ([`curve_hop_lines`]): the NATIVE reserve cap
    //!   ALONE. The entry-tangent shape was MEASURED non-concave against the real
    //!   leaf (see [`curve_stableswap_lines`]), so an entry tangent would under-cut
    //!   a real leaf and skip a profitable path.

    use alloy::primitives::{aliases::I512, U256, U512};

    use degenbot_math::curve::stableswap::{DVariant, YVariant};

    use crate::solve::mixed::solve::solve_golden_section_path_int;
    use crate::solve::mixed::{ResolvedHop, ResolvedMixedPath, SolvePathResult};
    use crate::solve::profit_envelope::{
        GateDeps, GateStats, HopMath, Line, PathBoundLines, PrefixCache,
    };
    use crate::solve::runtime::SolveRuntimeConfig;

    /// Resolved state for a Curve stableswap pool hop, ready for the curve
    /// solve branch.
    ///
    /// Carries everything needed to simulate a swap: the rate-adjusted XP
    /// array, the input/output token indices, amp (with `A_PRECISION` baked in),
    /// fee, and the y/d variant discriminators.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct CurveStableswapHopState {
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
        pub y_variant: YVariant,
        /// d-iteration variant.
        pub d_variant: DVariant,
    }

    /// The Curve family's gate projection: the entry tangent was MEASURED
    /// non-concave against the real leaf (see `curve_stableswap_lines`), so the
    /// family keeps the sound native reserve cap — carried through its own hop
    /// view.
    pub(crate) fn curve_to_math(hop: &ResolvedHop) -> Option<HopMath<'_>> {
        match hop {
            ResolvedHop::CurveStableswap { state } => {
                if state.rate_multiplier_out.is_zero() {
                    return None;
                }
                // Only the fields the cap derivation and the family label read:
                // the rest of the resolved state stays on `CurveStableswapHopState`
                // for `simulate_curve_hop`, but the gate view does not carry it.
                Some(HopMath::CurveStableswap {
                    amp: state.amp,
                    xp: state.xp.as_slice(),
                    token_index_in: state.token_index_in,
                    token_index_out: state.token_index_out,
                    precision: state.precision,
                    rate_multiplier_out: state.rate_multiplier_out,
                })
            }
            _ => None,
        }
    }

    /// The Curve family's dispatch-row adapter for its single-argument path entry
    /// (`SolveFn`): wraps [`solve_curve_path_int`] and reports no walk telemetry
    /// (the golden-section arm does not walk).
    pub(crate) fn solve_curve_entry(
        resolved: &ResolvedMixedPath,
        _gate: &GateDeps<'_>,
        _walk_env: Option<&PathBoundLines>,
    ) -> crate::solve::mixed::SolveArmOutcome {
        crate::solve::mixed::SolveArmOutcome::walked(
            solve_curve_path_int(resolved),
            crate::solve::concentrated_liquidity::WalkStats::default(),
        )
    }

    /// Curve stableswap solve — the shared golden-section template
    /// (`solve_golden_section_path_int`) over the Curve stableswap math leaf
    /// (`stableswap_get_y` + fee/rate conversion).
    fn solve_curve_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
        use degenbot_math::v2::IntHopState;

        let hops = &resolved.hops;

        // V2-equivalent IntHopState per hop: Curve hops use xp[idx_in]/xp[idx_out]
        // + the retained fee fraction; V2 hops pass through unchanged.
        let v2_equiv: Vec<IntHopState> = hops
            .iter()
            .map(curve_hop_v2_equiv)
            .collect::<Option<Vec<_>>>()?;

        solve_golden_section_path_int(&v2_equiv, &|x| simulate_curve_path(x, hops), &|x| {
            simulate_curve_path_outputs(x, hops)
        })
    }

    /// V2-equivalent [`IntHopState`] for a Curve stableswap hop.
    fn curve_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_math::v2::IntHopState> {
        use degenbot_math::v2::IntHopState;
        match hop {
            ResolvedHop::V2 { state } => Some(state.clone()),
            ResolvedHop::CurveStableswap { state } => {
                // Curve fee is fee/fee_denom; gamma = 1 - fee/fee_denom.
                // IntHopState wants gamma_numer/fee_denom.
                let gn: u64 = state.fee_denom.saturating_sub(state.fee).try_into().ok()?;
                let fd: u64 = state.fee_denom.try_into().ok()?;
                Some(IntHopState::new(
                    state.xp[state.token_index_in],
                    state.xp[state.token_index_out],
                    gn,
                    fd,
                ))
            }
            _ => None,
        }
    }

    /// Simulate the full path and return the final output.
    fn simulate_curve_path(x: U256, hops: &[ResolvedHop]) -> U256 {
        let outputs = simulate_curve_path_outputs(x, hops);
        outputs.last().copied().unwrap_or(U256::ZERO)
    }

    /// Simulate the full path and return each hop's output.
    fn simulate_curve_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
        let mut amount = x;
        let mut outputs = Vec::with_capacity(hops.len());
        for hop in hops {
            if amount.is_zero() {
                outputs.push(U256::ZERO);
                continue;
            }
            let out = match hop {
                ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
                ResolvedHop::CurveStableswap { state } => simulate_curve_hop(amount, state),
                _ => U256::ZERO,
            };
            outputs.push(out);
            amount = out;
        }
        outputs
    }

    /// Simulate a single Curve stableswap hop (rate-adjusted XP, `get_y`,
    /// fee+rate conversion).
    fn simulate_curve_hop(amount_in: U256, hop: &CurveStableswapHopState) -> U256 {
        use degenbot_math::curve::stableswap::stableswap_get_y;
        if amount_in.is_zero() {
            return U256::ZERO;
        }
        let i = hop.token_index_in;
        let j = hop.token_index_out;
        // Rate-adjust the input: x = xp[i] + dx * rate_in / PRECISION
        let x = hop.xp[i]
            .saturating_add(amount_in.saturating_mul(hop.rate_multiplier_in) / hop.precision);
        let Ok(y) = stableswap_get_y(
            i,
            j,
            x,
            &hop.xp,
            hop.amp,
            hop.n_coins,
            hop.a_precision,
            hop.y_variant,
            hop.d_variant,
        ) else {
            return U256::ZERO;
        };
        // dy = xp[j] - y - 1
        let dy_raw = hop.xp[j].saturating_sub(y).saturating_sub(U256::from(1u64));
        // Fee on dy, then rate convert: (dy - fee) * PRECISION / rate_out
        let fee = dy_raw.saturating_mul(hop.fee) / hop.fee_denom;
        let dy_after_fee = dy_raw.saturating_sub(fee);
        // Rate-convert from XP-space to native token space
        dy_after_fee.saturating_mul(hop.precision) / hop.rate_multiplier_out
    }

    /// The Curve family's line builder: the native reserve cap alone — its entry
    /// tangent was measured non-concave against the real leaf (see
    /// `curve_stableswap_lines`).
    pub(crate) fn curve_hop_lines(
        hop: &HopMath<'_>,
        _cfg: &SolveRuntimeConfig,
        _fan_cache: Option<(&PrefixCache, u64)>,
        _stats: &mut GateStats,
    ) -> Option<(Vec<Line>, U256)> {
        let HopMath::CurveStableswap {
            xp,
            token_index_out,
            precision,
            rate_multiplier_out,
            ..
        } = hop
        else {
            return None;
        };
        curve_stableswap_lines(xp, *token_index_out, *precision, *rate_multiplier_out)
    }

    /// Build the line set for a Curve stableswap hop: `[flat]` — the NATIVE reserve
    /// cap alone, by a VERIFIED non-concavity finding.
    ///
    /// The entry-tangent shape was evaluated against the real
    /// `degenbot_math::curve::stableswap` leaves (`stableswap_get_y` over all
    /// `DVariant`/`YVariant` pairs) and REJECTED: the leaf's y-iteration floors
    /// (`div`) and can settle a unit below the invariant's root, so
    /// `dy_raw = xp_out − y − 1` reports more than the ideal curve. The measured
    /// overshoot is 1–2 native units for the `Standard` y-iteration (extreme
    /// amplification, nearly-empty co-coin), and grows to ~1e24 native units — the
    /// pool's whole output reserve, at a 1-wei input — when a
    /// `Variant0`/`Variant1` y-iteration (which omits the `A_PRECISION` divisor)
    /// is paired with a `Standard` D-step. A tangent that under-cuts a real leaf is
    /// an unsound skip, so the family keeps the sound cap. The counterexamples are
    /// pinned in `tests/profit_envelope_tests.rs`.
    fn curve_stableswap_lines(
        xp: &[U256],
        token_index_out: usize,
        precision: U256,
        rate_multiplier_out: U256,
    ) -> Option<(Vec<Line>, U256)> {
        if rate_multiplier_out.is_zero() {
            return None;
        }
        let xp_out = *xp.get(token_index_out)?;
        // Native output reserve = `xp[out]·PRECISION/rate_out`.
        let reserve_out = xp_out.saturating_mul(precision) / rate_multiplier_out;
        if reserve_out.is_zero() {
            return None;
        }
        let flat = Line {
            a: I512::try_from(U512::from(reserve_out)).ok()?,
            b: I512::ZERO,
            c: I512::ONE,
        };
        Some((vec![flat], reserve_out))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        use proptest::prelude::*;

        use crate::solve::profit_envelope::path_output_bound_at;

        proptest! {
            /// Curve stableswap: the reserve cap (`xp[out]·PRECISION/rate_out` = the
            /// native output-token reserve) must dominate the real Curve leaf output
            /// for every amount — guards the rate-multiplier unit conversion.
            #[test]
            fn m6776w_curve_reserve_cap_dominates_real_leaf(
                bal_in in 1_000_000u64..1_000_000_000_000u64,
                bal_out in 1_000_000u64..1_000_000_000_000u64,
                x in 0u64..1_000_000_000_000u64,
                token_in in 0u8..2u8,
            ) {
                use degenbot_math::curve::stableswap::{DVariant, YVariant};
                let precision = U256::from(1_000_000_000_000_000_000u64); // 1e18
                // rate multiplier for a 6-decimal token = 10^30 (10^(18+12)).
                let rate = U256::from(1_000_000_000u64)
                    * U256::from(1_000_000_000u64)
                    * U256::from(1_000_000_000_000u64); // 1e9 * 1e9 * 1e12 = 1e30
                let (i, j) = if token_in == 0 { (0usize, 1usize) } else { (1, 0) };
                let balances = [
                    if i == 0 { bal_in } else { bal_out },
                    if i == 1 { bal_in } else { bal_out },
                ];
                // xp[k] = balance[k] * rate / precision
                let xp: Vec<U256> = balances
                    .iter()
                    .map(|&b| U256::from(b).saturating_mul(rate) / precision)
                    .collect();
                // Sanity: the caller-side cap formula xp[j]*precision/rate_out
                // equals the native reserve (exact: 1e30/1e18=1e12, no rounding).
                let caller_cap = xp[j].saturating_mul(precision) / rate;
                prop_assert!(
                    caller_cap == U256::from(balances[j]),
                    "curve cap unit mismatch: caller {caller_cap} != reserve {}",
                    U256::from(balances[j])
                );
                let hop = HopMath::ReserveCap {
                    reserve_out: U256::from(balances[j]),
                };
                let bound = path_output_bound_at(&[Some(hop)], &U256::from(x), &crate::solve::runtime::SolveRuntimeConfig::default())
                    .unwrap_or(U256::ZERO);
                let state = crate::solve::mixed::CurveStableswapHopState {
                    amp: U256::from(1000u64),
                    a_precision: U256::from(100u64),
                    xp: xp.clone(),
                    token_index_in: i,
                    token_index_out: j,
                    n_coins: U256::from(2u64),
                    fee: U256::from(4_000_000u64),
                    fee_denom: U256::from(10_000_000_000u64),
                    precision,
                    rate_multiplier_in: rate,
                    rate_multiplier_out: rate,
                    y_variant: YVariant::Standard,
                    d_variant: DVariant::Standard,
                };
                let true_out = simulate_curve_hop(U256::from(x), &state);
                if x > 0 && true_out.is_zero() {
                    return Ok(()); // leaf rejected the input; not soundness-relevant
                }
                prop_assert!(
                    bound >= true_out,
                    "curve cap {bound} < true {true_out} (bin={bal_in},bout={bal_out},x={x},t={token_in})"
                );
            }
        }
    }
}

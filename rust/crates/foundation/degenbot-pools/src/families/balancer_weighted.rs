//! The Balancer-weighted family read module (ADR-073 D1) — the
//! foundation-layer home of everything the Balancer V2 weighted family
//! contributes to being read and resolved.
//!
//! GLOSSARY.md "Family read module": this tree owns the state slice (the
//! I/O-free `crate::balancer_weighted_state` types, ADR-001), the swap-read
//! arm (every variant — the token0/1 exact-input row, the N-token pair
//! exact-in, and the pair exact-out `GIVEN_OUT` read — behind one entry), the
//! resolve projection as a pure function of the family state slice, and the
//! family-local apply/observe behavior over that slice. It reads NO state
//! owner and consumes NO solver kernel — the hop is solver vocabulary, so the
//! wrapper stays at the dispatch row (the substrate resolve row does the
//! registry lookup and wraps the solvers crate's `ResolvedHop`).
//!
//! # The four intent doors (ADR-073 D4)
//!
//! - [`capture_hop`] — the resolve projection: one fresh projection of one
//!   hop, refused rather than defaulted;
//! - [`read_swap`] — the swap-read arm, ALL variants behind one entry;
//! - [`apply_event`] — one buffered balance event onto one slice;
//! - [`observe`] — family-local GET behavior over one slice.
//!
//! GUARDRAIL (load-bearing, do not fuse): this file and the Balancer-stable
//! twin ([`super::balancer_stable`]) deliberately share NO projection or
//! swap-read code — the weighted invariant (`calc_out_given_in` over
//! balances/weights with `mulDown` upscaling) and the stable invariant
//! (pre-computed `D`, BPT-skipped balances) are different leaves with
//! different override surfaces (weighted overrides balances only; stable
//! overrides balances AND scaling factors — `MetaStable` rates). Sharing no
//! code between the two files IS the enforcement, not a header comment. What
//! the two families DO share is only the family-blind event/probe vocabulary
//! in [`super::balance_vector`] (ADR-059 D6).

use alloy::primitives::U256;

use degenbot_math::balancer::fixed_point::{div_down, div_up, mul_down};
use degenbot_math::balancer::weighted_math;
use degenbot_math::balancer::PowVersion;

use crate::state_history::BalanceVectorPoolState as _;
use crate::v3_state::SimulateSwapError;

use super::balance_vector::{BalanceVectorEvent, Observe, Observed};

/// The state slice + its registration DTOs — the family's read surface lives
/// in `crate::balancer_weighted_state` (I/O-free per ADR-001) and is
/// re-exported here so the family tree presents the whole read story under
/// one path.
pub use crate::balancer_weighted_state::{
    BalancerWeightedPoolIdentity, BalancerWeightedPoolState, RegisterBalancerWeightedPoolParams,
};

/// Why [`capture_hop`] refused to project a hop — the projection is refused,
/// never defaulted. The dispatch row maps these onto its own invalidation
/// vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureRefusal {
    /// Fewer than two tokens — no pairwise hop can be formed.
    TooFewTokens,
    /// The registered `pow_version` byte decoded to nothing.
    UnknownVariant,
}

/// The Balancer-weighted hop projection — everything one weighted hop
/// contributes to a solve, derived purely from the family state slice. The
/// substrate resolve row wraps this into the solver crate's hop vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalancerWeightedHop {
    /// Balance of the input token (upscaled to 18-decimal fixed-point).
    pub balance_in: U256,
    /// Balance of the output token (upscaled to 18-decimal fixed-point).
    pub balance_out: U256,
    /// Normalized weight of the input token (18-decimal, e.g. 5e17 for 50%).
    pub weight_in: U256,
    /// Normalized weight of the output token (18-decimal).
    pub weight_out: U256,
    /// Swap fee as a fraction of `ONE = 1e18` (e.g. 3e15 for 0.3%).
    pub swap_fee: U256,
    /// `PowVersion` discriminator — selects `FixedPoint::pow` fast paths.
    pub pow_version: PowVersion,
    /// Scaling factor for the input token (`10^(18 - decimals_in)`).
    pub scaling_factor_in: U256,
    /// Scaling factor for the output token (`10^(18 - decimals_out)`).
    pub scaling_factor_out: U256,
}

/// The capture door — the resolve projection: one fresh projection of one
/// Balancer-weighted hop, refused rather than defaulted. Pure over the family
/// state slice; the dispatch row does the registry lookup and the
/// solver-vocabulary wrap.
///
/// N-token pool: `zero_for_one` selects token[0]→token[1] (i=0, j=1) or
/// token[1]→token[0] (i=1, j=0). The engine only handles the pairwise (0/1)
/// case; N>2 pair selection is a Python-side concern (`BalancerPairView`)
/// that fixes the pair before registration.
///
/// # Errors
///
/// [`CaptureRefusal`] — see the type's docs for the reasons.
pub fn capture_hop(
    state: &BalancerWeightedPoolState,
    identity: &BalancerWeightedPoolIdentity,
    zero_for_one: bool,
) -> Result<BalancerWeightedHop, CaptureRefusal> {
    if identity.n_tokens() < 2 {
        return Err(CaptureRefusal::TooFewTokens);
    }
    let (balance_in, balance_out, weight_in, weight_out, scaling_factor_in, scaling_factor_out) =
        if zero_for_one {
            (
                state.balances[0].saturating_mul(identity.scaling_factors[0]),
                state.balances[1].saturating_mul(identity.scaling_factors[1]),
                identity.weights[0],
                identity.weights[1],
                identity.scaling_factors[0],
                identity.scaling_factors[1],
            )
        } else {
            (
                state.balances[1].saturating_mul(identity.scaling_factors[1]),
                state.balances[0].saturating_mul(identity.scaling_factors[0]),
                identity.weights[1],
                identity.weights[0],
                identity.scaling_factors[1],
                identity.scaling_factors[0],
            )
        };
    let pow_version =
        PowVersion::from_u8(identity.pow_version).ok_or(CaptureRefusal::UnknownVariant)?; // Unknown pow_version → invalid
    Ok(BalancerWeightedHop {
        balance_in,
        balance_out,
        weight_in,
        weight_out,
        swap_fee: U256::from(identity.swap_fee),
        pow_version,
        scaling_factor_in,
        scaling_factor_out,
    })
}

/// The swap-read request: EVERY weighted read variant, keyed by direction and
/// token selection. The token0/1 exact-input row, the N-token pair reads, and
/// the `GIVEN_OUT` arm all enter through [`read_swap`].
#[derive(Clone, Copy, Debug)]
pub enum SwapReadRequest<'a> {
    /// Exact-input swap over the token0/token1 pair (the seat engine's hop
    /// universe) — `zero_for_one` selects the direction.
    ExactIn {
        /// Direction of the swap (token0 → token1 when `true`).
        zero_for_one: bool,
        /// The input amount (native decimals).
        amount_in: U256,
    },
    /// Exact-input swap across an explicit token pair (the standalone-driver
    /// N-token surface). Overrides carry the caller's view of the balances.
    PairExactIn {
        /// Input token index.
        idx_in: usize,
        /// Output token index.
        idx_out: usize,
        /// The input amount (native decimals).
        amount_in: U256,
        /// Replacement balances (caller's view); `None` reads the slice.
        override_balances: Option<&'a [U256]>,
    },
    /// Exact-output swap across an explicit token pair (the `GIVEN_OUT` arm).
    /// Overrides carry the caller's view of the balances.
    PairExactOut {
        /// Input token index.
        idx_in: usize,
        /// Output token index.
        idx_out: usize,
        /// The requested output amount (native decimals).
        amount_out: U256,
        /// Replacement balances (caller's view); `None` reads the slice.
        override_balances: Option<&'a [U256]>,
    },
}

/// The swap-read door — ALL variants behind one entry.
///
/// # Errors
///
/// [`SimulateSwapError::NotComputable`] for an index out of range,
/// `idx_in == idx_out`, an override-shape mismatch, an on-chain ratio breach,
/// or an arithmetic overflow (mirroring the pipe-line's `SafeMath` revert
/// contract). The family carries no sparse-map miss — a balance-vector slice
/// has no tick words to miss.
pub fn read_swap(
    state: &BalancerWeightedPoolState,
    identity: &BalancerWeightedPoolIdentity,
    request: SwapReadRequest<'_>,
) -> Result<U256, SimulateSwapError> {
    match request {
        SwapReadRequest::ExactIn {
            zero_for_one,
            amount_in,
        } => {
            let (idx_in, idx_out) = if zero_for_one { (0, 1) } else { (1, 0) };
            pair_exact_in(state, identity, idx_in, idx_out, amount_in, None)
        }
        SwapReadRequest::PairExactIn {
            idx_in,
            idx_out,
            amount_in,
            override_balances,
        } => pair_exact_in(
            state,
            identity,
            idx_in,
            idx_out,
            amount_in,
            override_balances,
        ),
        SwapReadRequest::PairExactOut {
            idx_in,
            idx_out,
            amount_out,
            override_balances,
        } => pair_exact_out(
            state,
            identity,
            idx_in,
            idx_out,
            amount_out,
            override_balances,
        ),
    }
}

/// Balancer V2 weighted exact-input swap across an explicit token pair.
/// Mirrors the Vault's `_swapMinimalInfoGivenIn`: upscale balances + amount
/// (mulDown), subtract swap fee (mulUp on the scaled amount), compute,
/// downscale output (divDown).
///
/// The token0/1 row generalizes to N-token pools (the weighted math only
/// reads the in/out pair, so any index selection is the same 2-token
/// computation).
///
/// # Errors
/// `NotComputable` for an index out of range, `idx_in == idx_out`, the
/// on-chain `MAX_IN_RATIO` breach, or an arithmetic overflow (mirroring the
/// pipe-line's `SafeMath` revert contract).
fn pair_exact_in(
    state: &BalancerWeightedPoolState,
    identity: &BalancerWeightedPoolIdentity,
    idx_in: usize,
    idx_out: usize,
    amount_in: U256,
    override_balances: Option<&[U256]>,
) -> Result<U256, SimulateSwapError> {
    if idx_in >= identity.tokens.len() || idx_out >= identity.tokens.len() || idx_in == idx_out {
        return Err(SimulateSwapError::NotComputable);
    }
    let balances: &[U256] = match override_balances {
        Some(ob) if ob.len() == identity.tokens.len() => ob,
        Some(_) => return Err(SimulateSwapError::NotComputable),
        None => &state.balances,
    };
    if amount_in.is_zero() {
        return Ok(U256::ZERO);
    }
    let sf_in = identity.scaling_factors[idx_in];
    let sf_out = identity.scaling_factors[idx_out];
    // Fee is subtracted from the RAW amount, then upscaled (matches the
    // Python companion `BalancerV2Pool.calculate_tokens_out_from_tokens_in`).
    let amount_in_less_fee =
        weighted_math::subtract_swap_fee_amount(amount_in, U256::from(identity.swap_fee))
            .map_err(|_| SimulateSwapError::NotComputable)?;
    let scaled_balance_in =
        mul_down(balances[idx_in], sf_in).map_err(|_| SimulateSwapError::NotComputable)?;
    let scaled_balance_out =
        mul_down(balances[idx_out], sf_out).map_err(|_| SimulateSwapError::NotComputable)?;
    let scaled_amount_in =
        mul_down(amount_in_less_fee, sf_in).map_err(|_| SimulateSwapError::NotComputable)?;
    // Respect the registered pow revision (V1 = 2021-era general path, V2 =
    // fast paths): the revision changes last-digit rounding, so a pow_version
    // != V2 registration must not be computed with V2 semantics.
    let pow_version =
        PowVersion::from_u8(identity.pow_version).ok_or(SimulateSwapError::NotComputable)?;
    let scaled_amount_out = weighted_math::calc_out_given_in(
        scaled_balance_in,
        identity.weights[idx_in],
        scaled_balance_out,
        identity.weights[idx_out],
        scaled_amount_in,
        pow_version,
    )
    .map_err(|_| SimulateSwapError::NotComputable)?;
    let amount_out =
        div_down(scaled_amount_out, sf_out).map_err(|_| SimulateSwapError::NotComputable)?;
    Ok(amount_out)
}

/// Balancer V2 weighted exact-output across an explicit token pair.
///
/// Mirrors the python companion `calculate_tokens_in_from_tokens_out`:
/// upscale the out amount (`mulDown`), `calc_in_given_out` in scaled space,
/// downscale UP, then add the fee LAST (div-up over the complement).
///
/// # Errors
/// `NotComputable` for an index/shape or `MAX_OUT_RATIO` breach, or an
/// arithmetic overflow (mirroring the on-chain `SafeMath` revert contract).
fn pair_exact_out(
    state: &BalancerWeightedPoolState,
    identity: &BalancerWeightedPoolIdentity,
    idx_in: usize,
    idx_out: usize,
    amount_out: U256,
    override_balances: Option<&[U256]>,
) -> Result<U256, SimulateSwapError> {
    if idx_in >= identity.tokens.len() || idx_out >= identity.tokens.len() || idx_in == idx_out {
        return Err(SimulateSwapError::NotComputable);
    }
    let balances: &[U256] = match override_balances {
        Some(ob) if ob.len() == identity.tokens.len() => ob,
        Some(_) => return Err(SimulateSwapError::NotComputable),
        None => &state.balances,
    };
    if amount_out.is_zero() {
        return Ok(U256::ZERO);
    }
    let sf_in = identity.scaling_factors[idx_in];
    let sf_out = identity.scaling_factors[idx_out];
    let swap_fee = U256::from(identity.swap_fee);
    let scaled_balance_in =
        mul_down(balances[idx_in], sf_in).map_err(|_| SimulateSwapError::NotComputable)?;
    let scaled_balance_out =
        mul_down(balances[idx_out], sf_out).map_err(|_| SimulateSwapError::NotComputable)?;
    let scaled_amount_out =
        mul_down(amount_out, sf_out).map_err(|_| SimulateSwapError::NotComputable)?;
    let scaled_amount_in = weighted_math::calc_in_given_out(
        scaled_balance_in,
        identity.weights[idx_in],
        scaled_balance_out,
        identity.weights[idx_out],
        scaled_amount_out,
        PowVersion::from_u8(identity.pow_version).ok_or(SimulateSwapError::NotComputable)?,
    )
    .map_err(|_| SimulateSwapError::NotComputable)?;
    // Downscale UP first, then add the fee - matching the Solidity
    // onSwap GIVEN_OUT path (python `_downscale_up` then `addSwapFeeAmount`).
    let in_raw = div_up(scaled_amount_in, sf_in).map_err(|_| SimulateSwapError::NotComputable)?;
    weighted_math::add_swap_fee_amount(in_raw, swap_fee)
        .map_err(|_| SimulateSwapError::NotComputable)
}

/// The apply door — one buffered balance event (a Balancer Vault
/// `PoolBalanceChanged`) onto one slice. The journal + clock discipline lives
/// on the slice itself
/// ([`crate::state_history::BalanceVectorPoolState::apply_balance_update`],
/// ADR-017 D1); the door is the family's named apply arm. The neutral
/// `apply_balance_update_by_pool_id` orchestration row stays on the
/// family-blind trait projection (`PoolEntry::as_balance_vector_mut`) — a
/// per-family match there would reintroduce the arms ADR-017 D1 collapsed.
pub fn apply_event(state: &mut BalancerWeightedPoolState, event: BalanceVectorEvent) {
    state.apply_balance_update(event.balances, event.block_number);
}

/// The observe door — family-local GET behavior over one slice.
#[must_use]
pub fn observe(state: &BalancerWeightedPoolState, probe: Observe) -> Observed {
    match probe {
        Observe::UpdateBlock => Observed::UpdateBlock(state.update_block),
        Observe::StateNonce => Observed::StateNonce(state.state_nonce),
        Observe::TokenCount => Observed::TokenCount(state.balances.len()),
    }
}

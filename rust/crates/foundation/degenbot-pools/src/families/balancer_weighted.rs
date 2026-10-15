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
//! registry lookup and wraps the solver's `ResolvedHop`).
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

use super::balance_vector::BalanceVectorEvent;
use super::door::{FamilyApply, Observe, Observed, ReadOutcome, SwapReadRequest};

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

/// The swap-read door — ALL variants behind one entry.
///
/// # Errors
///
/// [`SimulateSwapError::NotComputable`] for an index out of range,
/// `idx_in == idx_out`, an override-shape mismatch, an on-chain ratio breach,
/// or an arithmetic overflow (mirroring the pipe-line's `SafeMath` revert
/// contract). The family carries no sparse-map miss — a balance-vector slice
/// has no tick words to miss.
#[must_use]
pub fn read_swap(
    state: &BalancerWeightedPoolState,
    identity: &BalancerWeightedPoolIdentity,
    request: SwapReadRequest<'_>,
) -> ReadOutcome {
    let read = match request {
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
            override_scaling_factors: None,
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
            override_scaling_factors: None,
        } => pair_exact_out(
            state,
            identity,
            idx_in,
            idx_out,
            amount_out,
            override_balances,
        ),
        // A variant this family doesn't answer is a typed refusal: an
        // exact-output read over the token0/1 pair, the CL canonical
        // request, and the stable family's scaling-factor override bit (a
        // weighted pool's scaling factors are registration-immutable
        // identity fields — there is no caller view to override, so a
        // `Some` is refused rather than silently ignored).
        _ => return ReadOutcome::NotComputable,
    };
    ReadOutcome::simple(read)
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
impl FamilyApply for BalancerWeightedPoolState {
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
pub fn observe(state: &BalancerWeightedPoolState, probe: Observe) -> Observed {
    match probe {
        Observe::UpdateBlock => Observed::UpdateBlock(state.update_block),
        Observe::StateNonce => Observed::StateNonce(state.state_nonce),
        Observe::TokenCount => Observed::TokenCount(state.balances.len()),
        _ => unreachable!("a balance-vector slice answers only the vector probes"),
    }
}

// ---------------------------------------------------------------------------
// The balancer_weighted family's solve entry (ADR-073 endgame): the solve half re-homed from
// the dissolved solver crate — the family story is ONE file again.
// ---------------------------------------------------------------------------
pub(crate) mod solve {
    //! The Balancer-weighted family module (ADR-071 D1).
    //!
    //! GLOSSARY.md "Family module": the solver crate's single module owning
    //! everything the Balancer V2 weighted hop family contributes to a solve and
    //! its bound.
    //!
    //! What lives here:
    //! - the family's resolved state slice ([`BalancerWeightedHopState`]);
    //! - the family's gate projection ([`weighted_to_math`]) - weighted to
    //!   `HopMath::Weighted`;
    //! - the family's path-level solve entry ([`solve_balancer_weighted_path_int`])
    //!   and its dispatch-row adapter ([`solve_balancer_weighted_entry`]), built on
    //!   the shared golden-section template that stays family-blind in `mixed::solve`;
    //! - the V2-equivalent projection ([`balancer_weighted_hop_v2_equiv`]) that
    //!   template's Mobius precheck consumes;
    //! - the simulate triple ([`simulate_balancer_weighted_path`] / `_outputs` /
    //!   [`simulate_balancer_weighted_hop`]) over the `degenbot-balancer-math`
    //!   weighted leaf;
    //! - the family's envelope lines ([`weighted_hop_lines`]) - the concave entry
    //!   tangent plus the asymptotic reserve cap.

    use alloy::primitives::{aliases::I512, U256, U512};

    use degenbot_math::balancer::constants::PowVersion;

    use crate::solve::mixed::solve::solve_golden_section_path_int;
    use crate::solve::mixed::{ResolvedHop, ResolvedMixedPath, SolvePathResult};
    use crate::solve::profit_envelope::{
        GateDeps, GateStats, HopMath, Line, PathBoundLines, PrefixCache,
    };
    use crate::solve::runtime::SolveRuntimeConfig;

    /// Resolved state for a Balancer V2 weighted pool hop, ready for the
    /// weighted solve branch. Carries everything the
    /// `degenbot-balancer-math::weighted_math::calc_out_given_in` leaf needs.
    ///
    /// The fee is stored as a fraction of `ONE = 1e18` (the Balancer fixed-point
    /// convention): `amount_in_after_fee = amount_in - amount_in * swap_fee / ONE`.
    /// The `PowVersion` selects `FixedPoint::pow` fast paths (V1 general / V2
    /// fast paths for y ∈ {1, 2, 4}).
    ///
    /// For the V2-equivalent Möbius precheck, `gamma_numer` / `fee_denom` give
    /// the retained fraction `gamma_numer / fee_denom` = `(ONE - swap_fee) / ONE`
    /// scaled to a `u64` pair (the `IntHopState` fee convention).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct BalancerWeightedHopState {
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
        /// The input amount (in native decimals) is multiplied by this before
        /// passing to the math leaf, and the math result is divided by the
        /// output's scaling factor to downscale back to native decimals.
        pub scaling_factor_in: U256,
        /// Scaling factor for the output token (`10^(18 - decimals_out)`).
        pub scaling_factor_out: U256,
    }

    /// The Balancer-weighted family's gate projection: the concave weighted-pool
    /// curve → entry-rate tangent dominates globally (sound) + the reserve
    /// asymptote caps it.
    pub(crate) fn weighted_to_math(hop: &ResolvedHop) -> Option<HopMath<'_>> {
        match hop {
            ResolvedHop::BalancerWeighted { state } => Some(HopMath::Weighted {
                balance_in: state.balance_in,
                balance_out: state.balance_out,
                weight_in: state.weight_in,
                weight_out: state.weight_out,
                scaling_in: state.scaling_factor_in,
                scaling_out: state.scaling_factor_out,
            }),
            _ => None,
        }
    }

    /// The Balancer-weighted family's dispatch-row adapter for its
    /// single-argument path entry (`SolveFn`): wraps
    /// [`solve_balancer_weighted_path_int`] and reports no walk telemetry (the
    /// golden-section arm does not walk).
    pub(crate) fn solve_balancer_weighted_entry(
        resolved: &ResolvedMixedPath,
        _gate: &GateDeps<'_>,
        _walk_env: Option<&PathBoundLines>,
    ) -> crate::solve::mixed::SolveArmOutcome {
        crate::solve::mixed::SolveArmOutcome::walked(
            solve_balancer_weighted_path_int(resolved),
            crate::solve::concentrated_liquidity::WalkStats::default(),
        )
    }

    /// Solve an all-V2-or-Balancer-weighted (no CL hops, ≥1 weighted) path with
    /// the shared golden-section template (`solve_golden_section_path_int`).
    ///
    /// Per-hop simulation calls
    /// `degenbot_math::balancer::weighted_math::calc_out_given_in`. The
    /// V2-equivalent precheck treats each weighted hop as a constant-product
    /// hop — exact for 50/50 pools, approximate for other weights (good enough
    /// for bracket narrowing).
    fn solve_balancer_weighted_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
        use degenbot_math::v2::IntHopState;

        let hops = &resolved.hops;
        if hops.len() < 2 {
            return None;
        }

        // V2-equivalent IntHopState per hop: weighted hops use
        // balance_in/balance_out + the retained fee fraction; V2 hops pass
        // through unchanged.
        let v2_equiv: Vec<IntHopState> = hops
            .iter()
            .map(balancer_weighted_hop_v2_equiv)
            .collect::<Option<Vec<_>>>()?;

        solve_golden_section_path_int(
            &v2_equiv,
            &|x| simulate_balancer_weighted_path(x, hops),
            &|x| simulate_balancer_weighted_path_outputs(x, hops),
        )
    }

    /// Build the V2-equivalent [`IntHopState`] for a hop in a Balancer
    /// weighted solve path. Weighted hops use the raw balances as reserves
    /// and convert the fee (`swap_fee` as fraction of 1e18 → retained fraction
    /// `gamma_numer/fee_denom`). V2 hops pass through unchanged. Returns `None`
    /// for any non-V2/non-weighted hop.
    fn balancer_weighted_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_math::v2::IntHopState> {
        use degenbot_math::v2::IntHopState;
        match hop {
            ResolvedHop::V2 { state } => Some(state.clone()),
            ResolvedHop::BalancerWeighted { state } => {
                // swap_fee is the fee fraction as parts of 1e18.
                // gamma = 1e18 - swap_fee; IntHopState wants gamma_numer/fee_denom.
                let one_e18 = U256::from(10u64).pow(U256::from(18u64));
                let gamma = one_e18.saturating_sub(state.swap_fee);
                let gn: u64 = gamma.try_into().ok()?;
                let fd: u64 = one_e18.try_into().ok()?;
                Some(IntHopState::new(
                    state.balance_in,
                    state.balance_out,
                    gn,
                    fd,
                ))
            }
            _ => None,
        }
    }

    /// Simulate a Balancer-weighted-or-V2 path, returning only the final output.
    #[must_use]
    pub(crate) fn simulate_balancer_weighted_path(x: U256, hops: &[ResolvedHop]) -> U256 {
        simulate_balancer_weighted_path_outputs(x, hops)
            .last()
            .copied()
            .unwrap_or(U256::ZERO)
    }

    /// Simulate a Balancer-weighted-or-V2 path, returning per-hop outputs.
    fn simulate_balancer_weighted_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
        let mut amount = x;
        let mut outputs = Vec::with_capacity(hops.len());
        for hop in hops {
            if amount.is_zero() {
                outputs.push(U256::ZERO);
                continue;
            }
            let out = match hop {
                ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
                ResolvedHop::BalancerWeighted { state } => {
                    simulate_balancer_weighted_hop(amount, state)
                }
                _ => U256::ZERO,
            };
            outputs.push(out);
            amount = out;
        }
        outputs
    }

    /// Evaluate a single Balancer weighted hop's output via
    /// `degenbot_math::balancer::weighted_math::calc_out_given_in`. The swap
    /// fee is subtracted from the input first (Balancer convention:
    /// `amount_in_after_fee = amount_in - mul_up(amount_in, swap_fee)`),
    /// then the invariant math runs on the after-fee amount. Returns
    /// `U256::ZERO` on overflow / error (matching the defensive fallback in
    /// `mixed::families::solidly::simulate_solidly_hop`).
    fn simulate_balancer_weighted_hop(amount_in: U256, hop: &BalancerWeightedHopState) -> U256 {
        use degenbot_math::balancer::fixed_point::mul_up;
        use degenbot_math::balancer::weighted_math::calc_out_given_in;
        if amount_in.is_zero() {
            return U256::ZERO;
        }
        // Upscale input to 18-decimal fixed-point (plain integer multiply,
        // NOT mul_down — scaling factors are integer multipliers, not FXP).
        let amount_in_upscaled = amount_in.saturating_mul(hop.scaling_factor_in);
        // Subtract swap fee: amount_in_after_fee = amount_in - mul_up(amount_in, swap_fee).
        let Ok(fee_amount) = mul_up(amount_in_upscaled, hop.swap_fee) else {
            return U256::ZERO;
        };
        let amount_in_after_fee = amount_in_upscaled.saturating_sub(fee_amount);
        let Ok(amount_out_upscaled) = calc_out_given_in(
            hop.balance_in,
            hop.weight_in,
            hop.balance_out,
            hop.weight_out,
            amount_in_after_fee,
            hop.pow_version,
        ) else {
            return U256::ZERO;
        };
        // Downscale output from 18-decimal to native decimals (plain integer
        // division, NOT div_down — matches Python's `amount // scaling_factor`).
        if hop.scaling_factor_out.is_zero() {
            return U256::ZERO;
        }
        amount_out_upscaled / hop.scaling_factor_out
    }

    /// The Balancer-weighted family's line builder: the concave entry tangent plus
    /// the asymptotic reserve cap.
    pub(crate) fn weighted_hop_lines(
        hop: &HopMath<'_>,
        _cfg: &SolveRuntimeConfig,
        _fan_cache: Option<(&PrefixCache, u64)>,
        _stats: &mut GateStats,
    ) -> Option<(Vec<Line>, U256)> {
        let HopMath::Weighted {
            balance_in,
            balance_out,
            weight_in,
            weight_out,
            scaling_in,
            scaling_out,
        } = hop
        else {
            return None;
        };
        weighted_lines(
            *balance_in,
            *balance_out,
            *weight_in,
            *weight_out,
            *scaling_in,
            *scaling_out,
        )
    }

    /// Build the line set for a Balancer V2 WEIGHTED hop: `[rise, flat]`.
    ///
    /// The rise slope (native units) is
    /// `B_out·sf_in·w_in/(B_in·w_out·sf_out)`, computed in `U512` to avoid
    /// overflow and then narrowed to `I512`; the flat cap is the asymptotic
    /// reserve `B_out/sf_out`.
    fn weighted_lines(
        balance_in: U256,
        balance_out: U256,
        weight_in: U256,
        weight_out: U256,
        scaling_in: U256,
        scaling_out: U256,
    ) -> Option<(Vec<Line>, U256)> {
        if balance_in.is_zero()
            || balance_out.is_zero()
            || weight_out.is_zero()
            || scaling_out.is_zero()
        {
            return None;
        }
        let slope_num = U512::from(balance_out)
            .saturating_mul(U512::from(scaling_in))
            .saturating_mul(U512::from(weight_in));
        let slope_den = U512::from(balance_in)
            .saturating_mul(U512::from(weight_out))
            .saturating_mul(U512::from(scaling_out));
        if slope_den.is_zero() {
            return None;
        }
        let rise = Line {
            a: I512::ZERO,
            b: I512::try_from(slope_num).ok()?,
            c: I512::try_from(slope_den).ok()?,
        };
        // Cap (native) = B_out / sf_out (the asymptotic reserve).
        let cap = balance_out / scaling_out;
        let flat = Line {
            a: I512::try_from(U512::from(cap)).ok()?,
            b: I512::ZERO,
            c: I512::ONE,
        };
        Some((vec![rise, flat], cap))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        use proptest::prelude::*;

        use crate::solve::profit_envelope::path_output_bound_at;
        use degenbot_math::balancer::constants::ONE;

        proptest! {
            /// Balancer weighted: the concave tangent (entry marginal rate) + the
            /// reserve asymptote must dominate `calc_out_given_in` for every amount.
            #[test]
            fn m6776w_balancer_weighted_bound_dominates_real_leaf(
                b_in in 1u64..1_000_000_000u64,
                b_out in 1u64..1_000_000_000u64,
                w_in_pct in 10u32..90u32,
                x in 0u64..1_000_000_000u64,
            ) {
                let weight_in = U256::from(w_in_pct) * ONE / U256::from(100u32);
                let weight_out = ONE - weight_in;
                let state = crate::solve::mixed::BalancerWeightedHopState {
                    balance_in: U256::from(b_in),
                    balance_out: U256::from(b_out),
                    weight_in,
                    weight_out,
                    swap_fee: U256::from(3u64) * ONE / U256::from(1000u64),
                    pow_version: degenbot_math::balancer::constants::PowVersion::V2,
                    scaling_factor_in: U256::from(1u64),
                    scaling_factor_out: U256::from(1u64),
                };
                let hop = HopMath::Weighted {
                    balance_in: state.balance_in,
                    balance_out: state.balance_out,
                    weight_in: state.weight_in,
                    weight_out: state.weight_out,
                    scaling_in: state.scaling_factor_in,
                    scaling_out: state.scaling_factor_out,
                };
                let bound = path_output_bound_at(&[Some(hop)], &U256::from(x), &crate::solve::runtime::SolveRuntimeConfig::default())
                    .unwrap_or(U256::ZERO);
                let true_out = simulate_balancer_weighted_hop(U256::from(x), &state);
                // Skip cases the math leaf itself rejects (returns 0 with x>0):
                // those are not soundness-relevant.
                if x > 0 && true_out.is_zero() {
                    return Ok(());
                }
                prop_assert!(
                    bound >= true_out,
                    "weighted bound {bound} < true {true_out} (bin={b_in},bout={b_out},w={w_in_pct},x={x})"
                );
            }
        }
    }
}

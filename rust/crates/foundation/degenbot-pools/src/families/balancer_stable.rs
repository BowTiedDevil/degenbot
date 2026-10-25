//! The Balancer-stable family read module (ADR-073 D1) — the foundation-layer
//! home of everything the Balancer V2 stable (`StableSwap` invariant) family
//! contributes to being read and resolved.
//!
//! GLOSSARY.md "Family read module": this tree owns the state slice (the
//! I/O-free `crate::balancer_stable_state` types, ADR-001), the swap-read arm
//! (every variant — the token0/1 exact-input row, the N-token pair exact-in,
//! and the pair exact-out `GIVEN_OUT` read — behind one entry), the resolve
//! projection as a pure function of the family state slice, and the
//! family-local apply/observe behavior over that slice. It reads NO state
//! owner and consumes NO solver kernel — the hop is solver vocabulary, so the
//! wrapper stays at the dispatch row (the substrate resolve row does the
//! registry lookup and wraps the solver's `ResolvedHop`).
//!
//! # The four intent doors (ADR-073 D4)
//!
//! - [`capture_hop`] — the resolve projection: one fresh projection of one
//!   hop (BPT-skip + 18-decimal upscale + pre-computed invariant), refused
//!   rather than defaulted;
//! - [`read_swap`] — the swap-read arm, ALL variants behind one entry;
//! - [`apply_event`] — one buffered balance event onto one slice;
//! - [`observe`] — family-local GET behavior over one slice.
//!
//! GUARDRAIL (load-bearing, do not fuse): this file and the Balancer-weighted
//! twin ([`super::balancer_weighted`]) deliberately share NO projection or
//! swap-read code — the stable invariant runs on BPT-skipped balances with a
//! pre-computed `D` and carries the `MetaStable` scaling-factor override
//! surface the weighted family does not have. Sharing no code between the two
//! files IS the enforcement, not a header comment. What the two families DO
//! share is only the family-blind event/probe vocabulary in
//! [`super::balance_vector`] (ADR-059 D6).

use alloy::primitives::U256;

use degenbot_math::balancer::fixed_point::{div_down, div_up, mul_down};
use degenbot_math::balancer::stable_math;
use degenbot_math::balancer::weighted_math;

use crate::state_history::BalanceVectorPoolState as _;
use crate::v3_state::SimulateSwapError;

use super::balance_vector::BalanceVectorEvent;
use super::door::{FamilyApply, Observe, Observed, ReadOutcome, SwapReadRequest};

/// The state slice + its registration DTOs — the family's read surface lives
/// in `crate::balancer_stable_state` (I/O-free per ADR-001) and is re-exported
/// here so the family tree presents the whole read story under one path.
pub use crate::balancer_stable_state::{
    BalancerStablePoolIdentity, BalancerStablePoolState, RegisterBalancerStablePoolParams,
};

/// Why [`capture_hop`] refused to project a hop — the projection is refused,
/// never defaulted. The dispatch row maps these onto its own invalidation
/// vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureRefusal {
    /// Fewer than two tokens — no pairwise hop can be formed.
    TooFewTokens,
    /// A pairwise index fell outside the BPT-skipped balance list.
    OutOfRange,
    /// The stable invariant calculation errored.
    InvariantError,
}

/// The Balancer-stable hop projection — everything one stable hop contributes
/// to a solve, derived purely from the family state slice. The substrate
/// resolve row wraps this into the solve tree's hop vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalancerStableHop {
    /// Amplification coefficient `amp` (deployed convention, includes
    /// `AMP_PRECISION=1000`).
    pub amp: U256,
    /// BPT-skipped upscaled balances (all non-BPT tokens).
    pub balances: Vec<U256>,
    /// Token index of the input (in the BPT-skipped list).
    pub token_index_in: usize,
    /// Token index of the output (in the BPT-skipped list).
    pub token_index_out: usize,
    /// Pre-computed invariant `D` (via `calculate_invariant` or
    /// `calculate_invariant_deployed`).
    pub invariant: U256,
    /// Swap fee as a fraction of `ONE = 1e18`.
    pub swap_fee: U256,
    /// Scaling factor for the input token (`10^(18 - decimals_in)`).
    pub scaling_factor_in: U256,
    /// Scaling factor for the output token.
    pub scaling_factor_out: U256,
}

/// The capture door — the resolve projection: one fresh projection of one
/// Balancer-stable hop, refused rather than defaulted. Pure over the family
/// state slice; the dispatch row does the registry lookup and the
/// solver-vocabulary wrap.
///
/// Pairwise (0/1) hop over the BPT-skipped, 18-decimal-upscaled balances with
/// the pre-computed `invariant` (one-shot under the core lock at resolve
/// time — never recomputed per search iteration).
///
/// # Errors
///
/// [`CaptureRefusal`] — see the type's docs for the reasons.
pub fn capture_hop(
    state: &BalancerStablePoolState,
    identity: &BalancerStablePoolIdentity,
    zero_for_one: bool,
) -> Result<BalancerStableHop, CaptureRefusal> {
    if identity.n_tokens() < 2 {
        return Err(CaptureRefusal::TooFewTokens);
    }
    let (raw_idx_in, raw_idx_out) = if zero_for_one { (0, 1) } else { (1, 0) };
    let skip_bpt = |idx: usize| -> usize {
        match identity.bpt_idx {
            Some(bpt) if idx >= bpt => idx - 1,
            _ => idx,
        }
    };
    let token_index_in = skip_bpt(raw_idx_in);
    let token_index_out = skip_bpt(raw_idx_out);
    let upscaled_balances: Vec<U256> = {
        let mut ub = Vec::with_capacity(identity.n_tokens());
        for (i, &bal) in state.balances.iter().enumerate() {
            if identity.bpt_idx.is_some_and(|bpt| bpt == i) {
                continue;
            }
            ub.push(bal.saturating_mul(identity.scaling_factors[i]));
        }
        ub
    };
    if token_index_in >= upscaled_balances.len() || token_index_out >= upscaled_balances.len() {
        return Err(CaptureRefusal::OutOfRange);
    }
    let amp_u256 = U256::from(identity.amp);
    let invariant = if identity.invariant_version == 1 {
        stable_math::calculate_invariant(amp_u256, &upscaled_balances)
    } else {
        stable_math::calculate_invariant_deployed(amp_u256, &upscaled_balances, true)
    };
    let invariant = invariant.map_err(|_| CaptureRefusal::InvariantError)?;
    Ok(BalancerStableHop {
        amp: amp_u256,
        balances: upscaled_balances,
        token_index_in,
        token_index_out,
        invariant,
        swap_fee: U256::from(identity.swap_fee),
        scaling_factor_in: identity.scaling_factors[raw_idx_in],
        scaling_factor_out: identity.scaling_factors[raw_idx_out],
    })
}

/// The swap-read door — ALL variants behind one entry.
///
/// # Errors
///
/// [`SimulateSwapError::NotComputable`] for an index/shape breach or an
/// arithmetic overflow (mirroring the on-chain `SafeMath` revert contract).
/// The family carries no sparse-map miss — a balance-vector slice has no tick
/// words to miss.
#[must_use]
pub fn read_swap(
    state: &BalancerStablePoolState,
    identity: &BalancerStablePoolIdentity,
    request: SwapReadRequest<'_>,
) -> ReadOutcome {
    let read = match request {
        SwapReadRequest::ExactIn {
            zero_for_one,
            amount_in,
        } => {
            let (idx_in, idx_out) = if zero_for_one { (0, 1) } else { (1, 0) };
            pair_exact_in(state, identity, idx_in, idx_out, amount_in, None, None)
        }
        SwapReadRequest::PairExactIn {
            idx_in,
            idx_out,
            amount_in,
            override_balances,
            override_scaling_factors,
        } => pair_exact_in(
            state,
            identity,
            idx_in,
            idx_out,
            amount_in,
            override_balances,
            override_scaling_factors,
        ),
        SwapReadRequest::PairExactOut {
            idx_in,
            idx_out,
            amount_out,
            override_balances,
            override_scaling_factors,
        } => pair_exact_out(
            state,
            identity,
            idx_in,
            idx_out,
            amount_out,
            override_balances,
            override_scaling_factors,
        ),
        // A variant this family doesn't answer (an exact-output read over the
        // token0/1 pair, the concentrated liquidity canonical request) is a typed refusal.
        _ => return ReadOutcome::NotComputable,
    };
    ReadOutcome::simple(read)
}

/// Balancer V2 stable exact-input swap across an explicit token pair.
///
/// Mirrors the python companion `BalancerV2StablePool.calculate_tokens_out_from_tokens_in`:
///   1. Subtract swap fee from the RAW amount.
///   2. Upscale balances (drop BPT for `ComposableStable` pools) + amount.
///   3. Compute invariant per `invariant_version` (V1 roundDown `D_P`,
///      V2 roundUp `P_D`).
///   4. `calc_out_given_in` in scaled space with BPT-skipped indices.
///   5. Downscale the output (`divDown`).
///
/// # Errors
/// `NotComputable` for an index/shape breach or an arithmetic overflow
/// (mirroring the on-chain `SafeMath` revert contract).
fn pair_exact_in(
    state: &BalancerStablePoolState,
    identity: &BalancerStablePoolIdentity,
    idx_in: usize,
    idx_out: usize,
    amount_in: U256,
    override_balances: Option<&[U256]>,
    override_scaling_factors: Option<&[U256]>,
) -> Result<U256, SimulateSwapError> {
    if idx_in >= identity.tokens.len() || idx_out >= identity.tokens.len() || idx_in == idx_out {
        return Err(SimulateSwapError::NotComputable);
    }
    let balances: &[U256] = match override_balances {
        Some(ob) if ob.len() == identity.tokens.len() => ob,
        Some(_) => return Err(SimulateSwapError::NotComputable),
        None => &state.balances,
    };
    let scaling_factors: &[U256] = match override_scaling_factors {
        Some(osf) if osf.len() == identity.tokens.len() => osf,
        Some(_) => return Err(SimulateSwapError::NotComputable),
        None => &identity.scaling_factors,
    };
    if amount_in.is_zero() {
        return Ok(U256::ZERO);
    }
    let sf_in = scaling_factors[idx_in];
    let sf_out = scaling_factors[idx_out];
    // Step 1: subtract fee from the raw amount.
    let amount_in_less_fee =
        weighted_math::subtract_swap_fee_amount(amount_in, U256::from(identity.swap_fee))
            .map_err(|_| SimulateSwapError::NotComputable)?;
    // Step 2: upscale balances + amount; drop BPT for ComposableStable pools.
    let upscaled_balances: Vec<U256> = balances
        .iter()
        .zip(scaling_factors.iter())
        .map(|(&b, &sf)| mul_down(b, sf).map_err(|_| SimulateSwapError::NotComputable))
        .collect::<Result<_, _>>()?;
    let scaled_amount_in =
        mul_down(amount_in_less_fee, sf_in).map_err(|_| SimulateSwapError::NotComputable)?;
    // Adjust indices/balances to skip the BPT token (ComposableStable).
    let (inv_balances, adj_in, adj_out) =
        skip_bpt(&upscaled_balances, identity.bpt_idx, idx_in, idx_out);
    // Step 3: invariant per deployed version (V1 always-roundDown D_P,
    // V2 P_D with round_up).
    let amp = U256::from(identity.amp);
    let invariant = if identity.invariant_version == 1 {
        stable_math::calculate_invariant(amp, &inv_balances)
    } else {
        stable_math::calculate_invariant_deployed(amp, &inv_balances, true)
    }
    .map_err(|_| SimulateSwapError::NotComputable)?;
    // Step 4: outGivenIn in scaled space.
    let scaled_amount_out = stable_math::calc_out_given_in(
        amp,
        &inv_balances,
        adj_in,
        adj_out,
        scaled_amount_in,
        invariant,
    )
    .map_err(|_| SimulateSwapError::NotComputable)?;
    // Step 5: downscale.
    let amount_out =
        div_down(scaled_amount_out, sf_out).map_err(|_| SimulateSwapError::NotComputable)?;
    Ok(amount_out)
}

/// Balancer V2 stable exact-output across an explicit token pair.
///
/// Mirrors the python companion `calculate_tokens_in_from_tokens_out`:
/// upscale balances + the out amount (`mulDown`), BPT-skip, the registered
/// invariant revision, `calc_in_given_out` in scaled space, downscale UP,
/// add the fee LAST.
///
/// # Errors
/// `NotComputable` for an index/shape breach or an arithmetic overflow
/// (mirroring the on-chain `SafeMath` revert contract).
fn pair_exact_out(
    state: &BalancerStablePoolState,
    identity: &BalancerStablePoolIdentity,
    idx_in: usize,
    idx_out: usize,
    amount_out: U256,
    override_balances: Option<&[U256]>,
    override_scaling_factors: Option<&[U256]>,
) -> Result<U256, SimulateSwapError> {
    if idx_in >= identity.tokens.len() || idx_out >= identity.tokens.len() || idx_in == idx_out {
        return Err(SimulateSwapError::NotComputable);
    }
    let balances: &[U256] = match override_balances {
        Some(ob) if ob.len() == identity.tokens.len() => ob,
        Some(_) => return Err(SimulateSwapError::NotComputable),
        None => &state.balances,
    };
    let scaling_factors: &[U256] = match override_scaling_factors {
        Some(osf) if osf.len() == identity.tokens.len() => osf,
        Some(_) => return Err(SimulateSwapError::NotComputable),
        None => &identity.scaling_factors,
    };
    if amount_out.is_zero() {
        return Ok(U256::ZERO);
    }
    let sf_in = scaling_factors[idx_in];
    let sf_out = scaling_factors[idx_out];
    let swap_fee = U256::from(identity.swap_fee);
    let upscaled_balances: Vec<U256> = balances
        .iter()
        .zip(scaling_factors.iter())
        .map(|(&b, &sf)| mul_down(b, sf).map_err(|_| SimulateSwapError::NotComputable))
        .collect::<Result<_, _>>()?;
    let scaled_amount_out =
        mul_down(amount_out, sf_out).map_err(|_| SimulateSwapError::NotComputable)?;
    let (inv_balances, adj_in, adj_out) =
        skip_bpt(&upscaled_balances, identity.bpt_idx, idx_in, idx_out);
    let amp = U256::from(identity.amp);
    let invariant = if identity.invariant_version == 1 {
        stable_math::calculate_invariant(amp, &inv_balances)
    } else {
        stable_math::calculate_invariant_deployed(amp, &inv_balances, true)
    }
    .map_err(|_| SimulateSwapError::NotComputable)?;
    let scaled_amount_in = stable_math::calc_in_given_out(
        amp,
        &inv_balances,
        adj_in,
        adj_out,
        scaled_amount_out,
        invariant,
    )
    .map_err(|_| SimulateSwapError::NotComputable)?;
    let in_raw = div_up(scaled_amount_in, sf_in).map_err(|_| SimulateSwapError::NotComputable)?;
    weighted_math::add_swap_fee_amount(in_raw, swap_fee)
        .map_err(|_| SimulateSwapError::NotComputable)
}

/// Drop the BPT entry and rebase in/out indices to the compressed list.
/// Mirrors `BalancerV2StablePool._skip_bpt_index` / `_non_bpt_indices`.
fn skip_bpt(
    balances: &[U256],
    bpt_idx: Option<usize>,
    idx_in: usize,
    idx_out: usize,
) -> (Vec<U256>, usize, usize) {
    match bpt_idx {
        None => (balances.to_vec(), idx_in, idx_out),
        Some(b) => {
            let adj = |i: usize| if i > b { i - 1 } else { i };
            let v: Vec<U256> = balances
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != b)
                .map(|(_, &x)| x)
                .collect();
            (v, adj(idx_in), adj(idx_out))
        }
    }
}

/// The apply door — one buffered balance event (a Balancer Vault
/// `PoolBalanceChanged`) onto one slice. The journal + clock discipline lives
/// on the slice itself
/// ([`crate::state_history::BalanceVectorPoolState::apply_balance_update`],
/// ADR-017 D1); the door is the family's named apply arm. The neutral
/// `apply_balance_update_by_pool_id` orchestration row stays on the
/// family-blind trait projection (`PoolEntry::as_balance_vector_mut`) — a
/// per-family match there would reintroduce the arms ADR-017 D1 collapsed.
impl FamilyApply for BalancerStablePoolState {
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
pub fn observe(state: &BalancerStablePoolState, probe: Observe) -> Observed {
    match probe {
        Observe::UpdateBlock => Observed::UpdateBlock(state.update_block),
        Observe::StateNonce => Observed::StateNonce(state.state_nonce),
        Observe::TokenCount => Observed::TokenCount(state.balances.len()),
        _ => unreachable!("a balance-vector slice answers only the vector probes"),
    }
}

#[cfg(test)]
mod tests {
    use super::skip_bpt;
    use alloy::primitives::U256;

    // Distinct magnitudes per token so a mis-rebase swapping one balance for
    // another is caught.
    fn b(i: u64) -> U256 {
        U256::from(i)
    }

    /// the `MetaStable` pass-through. `bpt_idx = None` leaves balances +
    /// indices untouched — the `ComposableStable` rebase machinery is a no-op.
    #[test]
    fn skip_bpt_metastable_none_is_passthrough() {
        let balances = [b(10), b(20)];
        let (v, idx_in, idx_out) = skip_bpt(&balances, None, 0, 1);
        assert_eq!(
            v,
            vec![b(10), b(20)],
            "Metastable balances must be unchanged"
        );
        assert_eq!(
            (idx_in, idx_out),
            (0, 1),
            "Metastable indices must be unchanged"
        );
    }

    /// `ComposableStable` `bpt_idx = Some(2)` (BPT at the END). The BPT
    /// is dropped and neither swap index is past `bpt_idx`, so `adj_in`/`adj_out`
    /// do NOT rebase — this is the BPT-drop branch exercisable end-to-end via
    /// the family read door (covered by the parity fixture in
    /// `rust/crates/facade/degenbot/tests/parity_balancer_stable_swap.rs`). Pinned here
    /// at the unit level so the BPT-drop + no-rebase combination is directly
    /// attributable.
    #[test]
    fn skip_bpt_composable_bpt_at_end_drops_bpt_no_rebase() {
        // [token0=10, token1=20, BPT=999] → drop index 2 → [10, 20].
        let balances = [b(10), b(20), b(999)];
        let (v, idx_in, idx_out) = skip_bpt(&balances, Some(2), 0, 1);
        assert_eq!(v, vec![b(10), b(20)], "BPT (index 2) must be dropped");
        assert_eq!(
            (idx_in, idx_out),
            (0, 1),
            "indices below bpt_idx must not rebase"
        );
    }

    /// `ComposableStable` `bpt_idx = Some(1)` (BPT in the MIDDLE), swap
    /// `token0 → token2` (`idx_in=0`, `idx_out=2`). `idx_out` is PAST `bpt_idx`, so
    /// it rebases to `2 - 1 = 1`; `idx_in` (0, below `bpt_idx`) stays `0`. The BPT
    /// at index 1 is dropped, leaving [token0, token2] with `adj_in=0`, `adj_out=1`.
    ///
    /// This is the index-rebase branch the end-to-end token0/1 row
    /// CANNOT reach (the dispatch is `zero_for_one`-based and hardcodes token
    /// positions `0 ↔ 1`, so a `bpt_idx = 1` pool would swap token0 ↔ BPT —
    /// not a valid asset-pair swap). It is the core correctness claim of the
    /// full end-to-end scenario and is pinned directly here so the rebase is
    /// verified independently of the dispatch limitation. The end-to-end wiring
    /// of arbitrary (`idx_in`, `idx_out`) through the pair read door is the broader
    /// multi-token-API extension.
    #[test]
    fn skip_bpt_composable_bpt_in_middle_rebases_index_past_bpt() {
        // [token0=10, BPT=999, token2=30] → drop index 1 → [10, 30].
        let balances = [b(10), b(999), b(30)];
        let (v, idx_in, idx_out) = skip_bpt(&balances, Some(1), 0, 2);
        assert_eq!(v, vec![b(10), b(30)], "BPT (index 1) must be dropped");
        assert_eq!(idx_in, 0, "idx_in (0, below bpt_idx=1) must not rebase");
        assert_eq!(
            idx_out, 1,
            "idx_out (2, past bpt_idx=1) must rebase to 2-1=1"
        );
    }

    /// symmetric rebase — `idx_in` past `bpt_idx`, `idx_out` below.
    /// `bpt_idx = Some(1)`, swap `token2 → token0` (`idx_in=2`, `idx_out=0`).
    /// Confirms the rebase applies to EITHER side, not just `idx_out`.
    #[test]
    fn skip_bpt_composable_rebases_idx_in_past_bpt() {
        let balances = [b(10), b(999), b(30)];
        let (v, idx_in, idx_out) = skip_bpt(&balances, Some(1), 2, 0);
        assert_eq!(v, vec![b(10), b(30)], "BPT (index 1) must be dropped");
        assert_eq!(idx_in, 1, "idx_in (2, past bpt_idx=1) must rebase to 2-1=1");
        assert_eq!(idx_out, 0, "idx_out (0, below bpt_idx=1) must not rebase");
    }

    /// both indices past `bpt_idx` (`bpt_idx = Some(0)`, BPT at start,
    /// swap `token1 → token2`). Both rebase by -1. Confirms the rebase is
    /// applied uniformly to both sides when both are past the BPT.
    #[test]
    fn skip_bpt_composable_rebases_both_indices_past_bpt() {
        // [BPT=999, token1=20, token2=30] → drop index 0 → [20, 30].
        let balances = [b(999), b(20), b(30)];
        let (v, idx_in, idx_out) = skip_bpt(&balances, Some(0), 1, 2);
        assert_eq!(v, vec![b(20), b(30)], "BPT (index 0) must be dropped");
        assert_eq!(
            (idx_in, idx_out),
            (0, 1),
            "both past bpt_idx=0 rebase by -1"
        );
    }
}

// ---------------------------------------------------------------------------
// The balancer_stable family's solve entry (ADR-073 endgame): the solve half re-homed from
// the dissolved solver crate — the family story is ONE file again.
// ---------------------------------------------------------------------------
pub(crate) mod solve {
    //! The Balancer-stable family module (ADR-071 D1).
    //!
    //! GLOSSARY.md "Family module": the solve tree's single module owning
    //! everything the Balancer V2 stable (`StableSwap` invariant) hop family
    //! contributes to a solve and its bound.
    //!
    //! What lives here:
    //! - the family's resolved state slice ([`BalancerStableHopState`]);
    //! - the family's gate projection ([`stable_to_math`]) - balancer-stable to
    //!   `HopMath::BalancerStable`;
    //! - the family's path-level solve entry ([`solve_balancer_stable_path_int`])
    //!   and its dispatch-row adapter ([`solve_balancer_stable_entry`]), built on
    //!   the shared golden-section template that stays family-blind in `mixed::solve`;
    //! - the V2-equivalent projection ([`balancer_stable_hop_v2_equiv`]) that
    //!   template's Mobius precheck consumes;
    //! - the simulate triple ([`simulate_balancer_stable_path`] / `_outputs` /
    //!   [`simulate_balancer_stable_hop`]) over the `degenbot-balancer-math`
    //!   stable leaf;
    //! - the family's envelope lines ([`balancer_stable_hop_lines`]): the entry
    //!   tangent wherever the closed-form curvature test ([`stableswap_concave_at`])
    //!   holds, beside the flat native reserve cap.

    use alloy::primitives::{aliases::I512, U256, U512};

    use crate::solve::gate::{GateDeps, GateStats, HopMath, Line, PathBoundLines, PrefixCache};
    use crate::solve::runtime::SolveRuntimeConfig;
    use crate::solve::solve_golden_section_path_int;
    use crate::solve::{ResolvedHop, ResolvedMixedPath, SolvePathResult};

    /// Resolved state for a Balancer V2 stable pool hop (`StableSwap` invariant),
    /// ready for the stable solve branch.
    ///
    /// Balances are upscaled to 18-decimal fixed-point and BPT-skipped (for
    /// `ComposableStablePools`). The invariant `D` is pre-computed at resolve
    /// time (one-shot under the core lock) — never recomputed per search
    /// iteration.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct BalancerStableHopState {
        /// Amplification coefficient `amp` (deployed convention, includes
        /// `AMP_PRECISION=1000`).
        pub amp: U256,
        /// BPT-skipped upscaled balances (all non-BPT tokens).
        pub balances: Vec<U256>,
        /// Token index of the input (in the BPT-skipped list).
        pub token_index_in: usize,
        /// Token index of the output (in the BPT-skipped list).
        pub token_index_out: usize,
        /// Pre-computed invariant `D` (via `calculate_invariant` or
        /// `calculate_invariant_deployed`).
        pub invariant: U256,
        /// Swap fee as a fraction of `ONE = 1e18`.
        pub swap_fee: U256,
        /// Scaling factor for the input token (`10^(18 - decimals_in)`).
        pub scaling_factor_in: U256,
        /// Scaling factor for the output token.
        pub scaling_factor_out: U256,
    }

    /// The Balancer-stable family's gate projection: the leaf's out-curve is
    /// concave in the upscaled input wherever the derive arm's closed-form
    /// curvature test holds, so its entry tangent is a sound rise beside the
    /// reserve cap.
    pub(crate) fn stable_to_math(hop: &ResolvedHop) -> Option<HopMath<'_>> {
        match hop {
            ResolvedHop::BalancerStable { state } => Some(HopMath::BalancerStable {
                amp: state.amp,
                balances: state.balances.as_slice(),
                token_index_in: state.token_index_in,
                token_index_out: state.token_index_out,
                invariant: state.invariant,
                swap_fee: state.swap_fee,
                scaling_factor_in: state.scaling_factor_in,
                scaling_factor_out: state.scaling_factor_out,
            }),
            _ => None,
        }
    }

    /// The Balancer-stable family's dispatch-row adapter for its single-argument
    /// path entry (`SolveFn`): wraps [`solve_balancer_stable_path_int`] and reports
    /// no walk telemetry (the golden-section arm does not walk).
    pub(crate) fn solve_balancer_stable_entry(
        resolved: &ResolvedMixedPath,
        _gate: &GateDeps<'_>,
        _walk_env: Option<&PathBoundLines>,
    ) -> crate::solve::SolveArmOutcome {
        crate::solve::SolveArmOutcome::walked(
            solve_balancer_stable_path_int(resolved),
            crate::solve::concentrated_liquidity::WalkStats::default(),
        )
    }

    /// Balancer stable solve — the shared golden-section template
    /// (`solve_golden_section_path_int`) over the stable math leaf
    /// (`calc_out_given_in` with a pre-computed invariant).
    fn solve_balancer_stable_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
        use degenbot_math::v2::IntHopState;

        let hops = &resolved.hops;

        // V2-equivalent IntHopState per hop: stable hops use
        // balances[idx_in]/balances[idx_out] + the retained fee fraction;
        // V2 hops pass through unchanged.
        let v2_equiv: Vec<IntHopState> = hops
            .iter()
            .map(balancer_stable_hop_v2_equiv)
            .collect::<Option<Vec<_>>>()?;

        solve_golden_section_path_int(
            &v2_equiv,
            &|x| simulate_balancer_stable_path(x, hops),
            &|x| simulate_balancer_stable_path_outputs(x, hops),
        )
    }

    /// V2-equivalent [`IntHopState`] for a Balancer stable hop. Used by the
    /// Möbius precheck to narrow the golden-section bracket. `StableSwap` ≠
    /// constant product, but at high amplification it's locally V2-like.
    fn balancer_stable_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_math::v2::IntHopState> {
        use degenbot_math::v2::IntHopState;
        match hop {
            ResolvedHop::V2 { state } => Some(state.clone()),
            ResolvedHop::BalancerStable { state } => {
                let one_e18 = U256::from(10u64).pow(U256::from(18u64));
                let gamma = one_e18.saturating_sub(state.swap_fee);
                let gn: u64 = gamma.try_into().ok()?;
                let fd: u64 = one_e18.try_into().ok()?;
                Some(IntHopState::new(
                    state.balances[state.token_index_in],
                    state.balances[state.token_index_out],
                    gn,
                    fd,
                ))
            }
            _ => None,
        }
    }

    /// Simulate the full path and return the final output.
    fn simulate_balancer_stable_path(x: U256, hops: &[ResolvedHop]) -> U256 {
        let outputs = simulate_balancer_stable_path_outputs(x, hops);
        outputs.last().copied().unwrap_or(U256::ZERO)
    }

    /// Simulate the full path and return each hop's output.
    fn simulate_balancer_stable_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
        let mut amount = x;
        let mut outputs = Vec::with_capacity(hops.len());
        for hop in hops {
            if amount.is_zero() {
                outputs.push(U256::ZERO);
                continue;
            }
            let out = match hop {
                ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
                ResolvedHop::BalancerStable { state } => {
                    simulate_balancer_stable_hop(amount, state)
                }
                _ => U256::ZERO,
            };
            outputs.push(out);
            amount = out;
        }
        outputs
    }

    /// Simulate a single Balancer stable hop (upscaling, fee, math,
    /// downscaling).
    fn simulate_balancer_stable_hop(amount_in: U256, hop: &BalancerStableHopState) -> U256 {
        use degenbot_math::balancer::fixed_point::mul_up;
        use degenbot_math::balancer::stable_math::calc_out_given_in;
        if amount_in.is_zero() {
            return U256::ZERO;
        }
        let amount_in_upscaled = amount_in.saturating_mul(hop.scaling_factor_in);
        let Ok(fee_amount) = mul_up(amount_in_upscaled, hop.swap_fee) else {
            return U256::ZERO;
        };
        let amount_in_after_fee = amount_in_upscaled.saturating_sub(fee_amount);
        let Ok(amount_out_upscaled) = calc_out_given_in(
            hop.amp,
            &hop.balances,
            hop.token_index_in,
            hop.token_index_out,
            amount_in_after_fee,
            hop.invariant,
        ) else {
            return U256::ZERO;
        };
        if hop.scaling_factor_out.is_zero() {
            return U256::ZERO;
        }
        amount_out_upscaled / hop.scaling_factor_out
    }

    /// The Balancer-stable family's line builder.
    pub(crate) fn balancer_stable_hop_lines(
        hop: &HopMath<'_>,
        _cfg: &SolveRuntimeConfig,
        _fan_cache: Option<(&PrefixCache, u64)>,
        _stats: &mut GateStats,
    ) -> Option<(Vec<Line>, U256)> {
        let HopMath::BalancerStable {
            amp,
            balances,
            token_index_in,
            token_index_out,
            invariant,
            swap_fee,
            scaling_factor_in,
            scaling_factor_out,
        } = hop
        else {
            return None;
        };
        balancer_stable_lines(
            *amp,
            balances,
            *token_index_in,
            *token_index_out,
            *invariant,
            *swap_fee,
            *scaling_factor_in,
            *scaling_factor_out,
        )
    }

    /// The Balancer V2 `AMP_PRECISION = 1000` — the leaf's amplification scaling
    /// (`amp` is stored pre-multiplied by it, and the invariant's `amp_times_total`
    /// term is divided by it).
    const BALANCER_AMP_PRECISION: u64 = 1_000;

    /// Exact closed-form curvature test for a `StableSwap` out-curve at the ENTRY
    /// point, in the family's scaled domain.
    ///
    /// Both n-token `StableSwap` invariants the gate covers reduce, for a fixed
    /// invariant `D` and fixed co-balances, to the same implicit out-curve
    /// `y² + k·y = C/x` with `k = x + T` (`T` constant in the input-side balance
    /// `x`, `C` constant in `x`). Writing `m` for the invariant's amplification
    /// coefficient and `W = m·(y + k)` for its product term, the marginal rate is
    /// `s(x) = y·(m·x + W)/(x·(m·y + W))`; differentiating along the curve gives
    /// `ds/dx = −2·y·G/(x²·(2y + k)²)` with
    /// `G = (x + y + k)² + (y − x)·(y + k)`.
    ///
    /// The curve is therefore CONCAVE at the entry exactly when `2y + k > 0` and
    /// `G ≥ 0`. `y + k = C/(x·y) > 0` for positive balances, so only the sign of
    /// `G` is in question — and it is a delicate one (for an extreme imbalance the
    /// two terms nearly cancel), which is why it is evaluated exactly rather than
    /// bounded. `k` arrives as the rational `k_num/k_den` (`k_den > 0`) so no
    /// division happens; `None` means the exact evaluation overflowed `I512`, and
    /// the caller drops the rise (cap-only — never looser than the plain cap).
    fn stableswap_concave_at(x: U512, y: U512, k_num: U512, k_den: U512) -> Option<bool> {
        let kd = I512::try_from(k_den).ok()?;
        if kd <= I512::ZERO {
            return None;
        }
        let k = I512::try_from(k_num).ok()?;
        let x = I512::try_from(x).ok()?;
        let y = I512::try_from(y).ok()?;
        let two = I512::try_from(U512::from(2u64)).ok()?;
        let three = I512::try_from(U512::from(3u64)).ok()?;

        // `2y + k > 0` is required for a positive marginal rate.
        let two_y_plus_k = two.checked_mul(y)?.checked_mul(kd)?.checked_add(k)?;
        if two_y_plus_k <= I512::ZERO {
            return Some(false);
        }
        let x_plus_y_plus_k = x.checked_add(y)?.checked_mul(kd)?.checked_add(k)?;
        let y_times_y_plus_k = y.checked_mul(y.checked_mul(kd)?.checked_add(k)?)?;
        // 2·(3·(y² + y·k) + k²) + x·(2y + 3k), cleared of `kd` denominators.
        let curvature_term = two
            .checked_mul(
                three
                    .checked_mul(y_times_y_plus_k)?
                    .checked_mul(kd)?
                    .checked_add(k.checked_mul(k)?)?,
            )?
            .checked_add(
                x.checked_mul(
                    two.checked_mul(y)?
                        .checked_mul(kd)?
                        .checked_add(three.checked_mul(k)?)?,
                )?
                .checked_mul(kd)?,
            )?;
        // `ds/dx ≤ 0` ⟺ `2·x·(2y + k)²·kd − (x + y + k)·curvature_term ≤ 0` (the
        // common `y > 0` factor divided out; `kd > 0`).
        let lhs = two
            .checked_mul(x)?
            .checked_mul(two_y_plus_k)?
            .checked_mul(two_y_plus_k)?
            .checked_mul(kd)?;
        let rhs = x_plus_y_plus_k.checked_mul(curvature_term)?;
        Some(lhs <= rhs)
    }

    /// Build the line set for a Balancer V2 STABLE hop: `[rise, flat]`.
    ///
    /// The leaf's invariant is `An·Σx + D = An·D + D^(n+1)/(n^n·Πx)` in the
    /// `An = amp·n/AMP_PRECISION` convention (both `calculate_invariant` and the
    /// deployed variant's Newton fixed point reduce to it). An exact-in swap adds
    /// the post-fee input to the input balance and re-solves the output balance
    /// against the SAME `D`, so the out-curve is that invariant's level curve. Its
    /// entry marginal rate is closed-form (no invariant solve): writing
    /// `W = D^(n+1)/(n^n·Πx) = m·(Σx − D) + D` and `u = x_in·x_out`,
    /// `−dx_out/dx_in = (m·u + W·x_out)/(m·u + W·x_in)`.
    ///
    /// The NATIVE-unit rise is that rate times the retained fee fraction and the
    /// scaling ratio, `slope·sf_in·(ONE − swap_fee)/(ONE·sf_out)` — the leaf
    /// upscales the input by `sf_in`, takes `mul_up(·, swap_fee)` off it, and
    /// downscales the output by `sf_out`.
    ///
    /// ADDITIVE ALLOWANCE: the rise carries `a = 0` — and that is derived, not
    /// assumed. Every rounding in the Balancer leaf is pool-favoring: the fee is
    /// taken with `mul_up` (so the retained input is at most the continuous
    /// `x·(1 − f)`), the output Newton divides with `div_up` (so its root is at
    /// least the invariant's), `calc_out_given_in` subtracts a further `1`, and the
    /// final downscale floors. The leaf's integer output is therefore never above
    /// the ideal curve, so the entry tangent dominates it with nothing to absorb
    /// (measured margin `≥ 1` native unit on the proptest matrix).
    ///
    /// The rise is DROPPED (cap-only) whenever the curvature test fails, the `I512`
    /// coefficients overflow, or the leaf's arithmetic could wrap across the whole
    /// evaluation window `[0, reserve_out]`: the gate sweeps the line at any native
    /// input, and the leaf runs its Newton at the POST-trade scale
    /// `x_in + x·sf_in`, where `D²`/`D·P_D` products can wrap `u256` and return a
    /// garbage root that could exceed the tangent. The window test bounds the
    /// upscaled input by `reserve_out·sf_in`, which is exactly the largest amount
    /// the cap lets the gate ask for.
    #[expect(clippy::too_many_arguments)] // mirrors the hop state's field set
    fn balancer_stable_lines(
        amp: U256,
        balances: &[U256],
        token_index_in: usize,
        token_index_out: usize,
        invariant: U256,
        swap_fee: U256,
        scaling_factor_in: U256,
        scaling_factor_out: U256,
    ) -> Option<(Vec<Line>, U256)> {
        let b_out = *balances.get(token_index_out)?;
        if scaling_factor_out.is_zero() {
            return None;
        }
        // Native output reserve: you can never extract more than the pool holds.
        let reserve_out = b_out / scaling_factor_out;
        if reserve_out.is_zero() {
            return None;
        }
        let flat = Line {
            a: I512::try_from(U512::from(reserve_out)).ok()?,
            b: I512::ZERO,
            c: I512::ONE,
        };
        match balancer_stable_rise(
            amp,
            balances,
            token_index_in,
            token_index_out,
            invariant,
            swap_fee,
            scaling_factor_in,
            scaling_factor_out,
        ) {
            Some(rise) => Some((vec![rise, flat], reserve_out)),
            None => Some((vec![flat], reserve_out)),
        }
    }

    /// The Balancer-stable entry-tangent rise line (see [`balancer_stable_lines`]);
    /// `None` whenever the rise must be dropped in favour of the flat cap alone.
    #[expect(clippy::too_many_arguments)] // mirrors the hop state's field set
    fn balancer_stable_rise(
        amp: U256,
        balances: &[U256],
        token_index_in: usize,
        token_index_out: usize,
        invariant: U256,
        swap_fee: U256,
        scaling_factor_in: U256,
        scaling_factor_out: U256,
    ) -> Option<Line> {
        if balances.len() < 2 || scaling_factor_in.is_zero() {
            return None;
        }
        let n_tokens = U256::from(u64::try_from(balances.len()).ok()?);
        let x_in = *balances.get(token_index_in)?;
        let x_out = *balances.get(token_index_out)?;
        if x_in.is_zero() || x_out.is_zero() {
            return None;
        }
        let amp_prec = U256::from(BALANCER_AMP_PRECISION);
        // The leaf's `amp_times_total`.
        let amp_n = amp.checked_mul(n_tokens)?;
        if amp_n.is_zero() {
            return None;
        }
        let sum = balances
            .iter()
            .try_fold(U256::ZERO, |acc, b| acc.checked_add(*b))?;
        // `Σx − D` must be non-negative for the invariant relation to hold.
        let sum_minus_d = sum.checked_sub(invariant)?;

        // `w_scaled = W·AMP_PRECISION = amp·n·(Σx − D) + AMP_PRECISION·D`.
        let w_scaled = U512::from(amp_n)
            .checked_mul(U512::from(sum_minus_d))?
            .checked_add(U512::from(amp_prec).checked_mul(U512::from(invariant))?)?;
        let cross = U512::from(amp_n)
            .checked_mul(U512::from(x_in))?
            .checked_mul(U512::from(x_out))?;
        let num = cross.checked_add(w_scaled.checked_mul(U512::from(x_out))?)?;
        let den = cross.checked_add(w_scaled.checked_mul(U512::from(x_in))?)?;
        if den.is_zero() {
            return None;
        }

        // Curvature guard at the entry: `k = x_in + Σ'' + D/m − D`, carried as the
        // rational `k_num/(amp·n)`.
        let sum_minus_out = sum.checked_sub(x_out)?;
        let k_den = U512::from(amp_n);
        let k_pos = k_den.checked_mul(U512::from(sum_minus_out))?;
        let k_prec = U512::from(amp_prec).checked_mul(U512::from(invariant))?;
        let k_neg = k_den.checked_mul(U512::from(invariant))?;
        let k_num = k_pos.checked_add(k_prec)?.checked_sub(k_neg)?;
        if !stableswap_concave_at(U512::from(x_in), U512::from(x_out), k_num, k_den)? {
            return None;
        }

        // Faithful-arithmetic window guard: the leaf's Newton runs at the maximal
        // post-trade scale, and its `invariant²`, `D·P_D` and `n·D²` products must
        // stay inside `u256` there. The window's largest upscaled input is
        // `reserve_out·scaling_factor_in` (the cap is the largest native input the
        // gate ever evaluates), and the largest output balance stays below
        // `x_out + (Σx − x_out)`.
        let u256_max = U512::from(U256::MAX);
        let out_reserve_native = x_out.checked_div(scaling_factor_out)?;
        let window_in = out_reserve_native.checked_mul(scaling_factor_in)?;
        let x_in_max = U512::from(x_in).checked_add(U512::from(window_in))?;
        let invariant_wide = U512::from(invariant);
        let n_tokens_wide = U512::from(n_tokens);
        // `n·D²` (the deployed numerator's first term) and `n·x·D²` (the monorepo
        // `D_P` accumulation at the maximal scale).
        if invariant_wide
            .checked_mul(invariant_wide)?
            .checked_mul(n_tokens_wide)?
            > u256_max
            || x_in_max
                .checked_mul(invariant_wide)?
                .checked_mul(invariant_wide)?
                .checked_mul(n_tokens_wide)?
                > u256_max
        {
            return None;
        }

        // Native slope: `slope·sf_in·(ONE − swap_fee)/(ONE·sf_out)`, with `ONE`
        // the Balancer leaf's 18-decimal fixed-point unit.
        let one = U512::from(degenbot_math::balancer::constants::ONE);
        if swap_fee >= degenbot_math::balancer::constants::ONE {
            return None;
        }
        let retained = one.checked_sub(U512::from(swap_fee))?;
        let b = num
            .checked_mul(U512::from(scaling_factor_in))?
            .checked_mul(retained)?;
        let c = den
            .checked_mul(one)?
            .checked_mul(U512::from(scaling_factor_out))?;
        if c.is_zero() {
            return None;
        }
        Some(Line {
            a: I512::ZERO,
            b: I512::try_from(b).ok()?,
            c: I512::try_from(c).ok()?,
        })
    }

    #[cfg(test)]
    #[expect(clippy::expect_used)]
    mod tests {
        use super::*;

        use degenbot_math::balancer::stable_math::calculate_invariant;

        /// `10^exp` as a `U256` (saturating).
        fn pow10_u256(exp: u32) -> U256 {
            let mut v = U256::ONE;
            for _ in 0..exp {
                v = v.saturating_mul(U256::from(10u64));
            }
            v
        }

        /// Shape assertion for the Balancer-stable arm: a pool inside the curvature
        /// domain derives the `[rise, flat]` set (the entry-tangent rise is the
        /// active line), while an extreme pool whose post-trade window would wrap
        /// the leaf's `u256` products derives the flat cap ALONE.
        #[test]
        fn balancer_stable_curvature_guard_shape() {
            let amp = U256::from(10_000u64); // A = 10
            let fee = U256::from(3_000_000_000_000_000u64); // 0.3 %

            // A 10x imbalance keeps the entry point inside the curvature domain, so
            // the rise survives beside the cap.
            let inside_balances = vec![pow10_u256(21), pow10_u256(20)];
            let inside_invariant = calculate_invariant(amp, &inside_balances).expect("converges");
            let (inside_lines, inside_cap) = balancer_stable_lines(
                amp,
                &inside_balances,
                0,
                1,
                inside_invariant,
                fee,
                U256::ONE,
                U256::ONE,
            )
            .expect("the inside-domain pool derives");
            assert_eq!(
                inside_lines.len(),
                2,
                "the inside-domain pool must carry [rise, flat]"
            );
            assert!(
                inside_lines[0].b > I512::ZERO,
                "the first line must be the non-constant rise"
            );
            assert_eq!(inside_lines[1].b, I512::ZERO, "the second is the flat cap");
            assert_eq!(inside_cap, inside_balances[1]);

            // The unit-input bound is the point-wise minimum of the derived lines
            // (exactly what `path_output_bound_at` evaluates at `x = 1`): the
            // entry-tangent rise sits STRICTLY below the cap, so the rise is the
            // active line. (Ported from the integration copy this test replaces.)
            let inside_cap_i = I512::try_from(U512::from(inside_cap)).expect("cap fits I512");
            let inside_bound = inside_lines
                .iter()
                .map(|l| l.eval(&U256::ONE))
                .min()
                .expect("non-empty line set");
            assert!(
                inside_bound < inside_cap_i,
                "inside-domain pool must carry the entry-tangent rise \
             (bound {inside_bound} vs cap {inside_cap_i})"
            );

            // The same 10x imbalance at a scale whose post-trade window would wrap
            // the leaf's squared products across `u256` - the faithful-arithmetic
            // window guard drops the rise, leaving the flat cap alone.
            let extreme_balances = vec![pow10_u256(30), pow10_u256(29)];
            let extreme_invariant = calculate_invariant(amp, &extreme_balances).expect("converges");
            let (extreme_lines, extreme_cap) = balancer_stable_lines(
                amp,
                &extreme_balances,
                0,
                1,
                extreme_invariant,
                fee,
                U256::ONE,
                U256::ONE,
            )
            .expect("the extreme pool still derives the cap");
            assert_eq!(
                extreme_lines.len(),
                1,
                "the extreme pool must fall back to cap-ONLY"
            );
            assert_eq!(
                extreme_lines[0].b,
                I512::ZERO,
                "the surviving line is the flat cap"
            );
            assert_eq!(extreme_cap, extreme_balances[1]);

            // The cap-only set's unit-input bound is exactly the reserve.
            let extreme_cap_i = I512::try_from(U512::from(extreme_cap)).expect("cap fits I512");
            let extreme_bound = extreme_lines
                .iter()
                .map(|l| l.eval(&U256::ONE))
                .min()
                .expect("non-empty line set");
            assert_eq!(
                extreme_bound, extreme_cap_i,
                "extreme pool must drop the rise to the cap alone"
            );
        }
    }
}

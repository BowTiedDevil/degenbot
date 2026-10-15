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
//! registry lookup and wraps the solvers crate's `ResolvedHop`).
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

use super::balance_vector::{BalanceVectorEvent, Observe, Observed};

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
/// resolve row wraps this into the solver crate's hop vocabulary.
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

/// The swap-read request: EVERY stable read variant, keyed by direction and
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
    /// N-token surface). Overrides carry the caller's view of the balances
    /// AND the scaling factors (`MetaStable` rates).
    PairExactIn {
        /// Input token index.
        idx_in: usize,
        /// Output token index.
        idx_out: usize,
        /// The input amount (native decimals).
        amount_in: U256,
        /// Replacement balances (caller's view); `None` reads the slice.
        override_balances: Option<&'a [U256]>,
        /// Replacement scaling factors (caller's view); `None` reads the
        /// identity.
        override_scaling_factors: Option<&'a [U256]>,
    },
    /// Exact-output swap across an explicit token pair (the `GIVEN_OUT` arm).
    /// Overrides carry the caller's view of the balances AND the scaling
    /// factors (`MetaStable` rates).
    PairExactOut {
        /// Input token index.
        idx_in: usize,
        /// Output token index.
        idx_out: usize,
        /// The requested output amount (native decimals).
        amount_out: U256,
        /// Replacement balances (caller's view); `None` reads the slice.
        override_balances: Option<&'a [U256]>,
        /// Replacement scaling factors (caller's view); `None` reads the
        /// identity.
        override_scaling_factors: Option<&'a [U256]>,
    },
}

/// The swap-read door — ALL variants behind one entry.
///
/// # Errors
///
/// [`SimulateSwapError::NotComputable`] for an index/shape breach or an
/// arithmetic overflow (mirroring the on-chain `SafeMath` revert contract).
/// The family carries no sparse-map miss — a balance-vector slice has no tick
/// words to miss.
pub fn read_swap(
    state: &BalancerStablePoolState,
    identity: &BalancerStablePoolIdentity,
    request: SwapReadRequest<'_>,
) -> Result<U256, SimulateSwapError> {
    match request {
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
    }
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
pub fn apply_event(state: &mut BalancerStablePoolState, event: BalanceVectorEvent) {
    state.apply_balance_update(event.balances, event.block_number);
}

/// The observe door — family-local GET behavior over one slice.
#[must_use]
pub fn observe(state: &BalancerStablePoolState, probe: Observe) -> Observed {
    match probe {
        Observe::UpdateBlock => Observed::UpdateBlock(state.update_block),
        Observe::StateNonce => Observed::StateNonce(state.state_nonce),
        Observe::TokenCount => Observed::TokenCount(state.balances.len()),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::doc_markdown)]
    use super::skip_bpt;
    use alloy::primitives::U256;

    // Distinct magnitudes per token so a mis-rebase swapping one balance for
    // another is caught.
    fn b(i: u64) -> U256 {
        U256::from(i)
    }

    /// the MetaStable pass-through. `bpt_idx = None` leaves balances +
    /// indices untouched — the ComposableStable rebase machinery is a no-op.
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

    /// ComposableStable `bpt_idx = Some(2)` (BPT at the END). The BPT
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

    /// ComposableStable `bpt_idx = Some(1)` (BPT in the MIDDLE), swap
    /// `token0 → token2` (idx_in=0, idx_out=2). `idx_out` is PAST `bpt_idx`, so
    /// it rebases to `2 - 1 = 1`; `idx_in` (0, below bpt_idx) stays `0`. The BPT
    /// at index 1 is dropped, leaving [token0, token2] with adj_in=0, adj_out=1.
    ///
    /// This is the index-rebase branch the end-to-end token0/1 row
    /// CANNOT reach (the dispatch is `zero_for_one`-based and hardcodes token
    /// positions `0 ↔ 1`, so a `bpt_idx = 1` pool would swap token0 ↔ BPT —
    /// not a valid asset-pair swap). It is the core correctness claim of the
    /// full end-to-end scenario and is pinned directly here so the rebase is
    /// verified independently of the dispatch limitation. The end-to-end wiring
    /// of arbitrary (idx_in, idx_out) through the pair read door is the broader
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
    /// `bpt_idx = Some(1)`, swap `token2 → token0` (idx_in=2, idx_out=0).
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

//! The V4 design-private half of the concentrated-liquidity family read
//! module (ADR-073 D1/D4).
//!
//! GUARDRAIL (load-bearing, do not fuse): this file and the V3 twin
//! ([`super::v3`]) deliberately share NO code — no shared V3/V4 constructor
//! and no projection helper crossing the two files. The load-bearing
//! differences live here and in `build_int_v4_sequence` / `v4_simulate_swap`:
//!
//! - **fee convention** — V4 charges the combined
//!   `swapFee = calculateSwapFee(protocol_fee_dir, lp_fee)` (the identity's
//!   `pool_key.fee` plus the packed per-direction `protocol_fee`), where V3
//!   charges `gamma = 1e6 − lp_fee`;
//! - **per-range net-sign direction** — V4 re-derives the per-range net
//!   liquidity sign from the walk direction (V4-only);
//! - **engine sign convention** — V4's `amountSpecified` is NEGATIVE for
//!   exact input (`Pool.sol:295`), already the canonical user perspective, so
//!   the mapping is identity (ADR-037);
//! - **hooks** — a V4 hook address's low 16 permission bits can alter swap
//!   deltas (ADR-037): the capture arm refuses amount-modifying hooks and the
//!   observe arm reports them.
//!
//! Nothing here reads a state owner (ADR-003) or a solver kernel: every
//! function takes the family's state slice plus registration scalars.

use std::sync::Arc;

use alloy::primitives::{Address, I256};

use super::{CaptureRefusal, DesignScalars, Observe, Observed, OverrideScalars};

use super::state_mutation::ConcentratedLiquidityPoolMut as _;
use crate::families::door::FamilyApply;
use crate::state::int_v3_hop::IntV3TickRangeSequence;
use crate::state::v3_state::V3SwapOutcome;
use crate::state::v3_state::{PoolTickCoverage, SimulateSwapError};
use crate::state::v4_state::{has_amount_modifying_hook, v4_simulate_swap};

/// The V4 state slice + its event/registration DTOs — the design's read
/// surface lives in `crate::v4_state` (I/O-free per ADR-001) and is
/// re-exported here so the family tree presents the whole read story under
/// one path.
pub use crate::state::v4_state::{
    BufferedV4LiquidityUpdate, BufferedV4PoolEvent, BufferedV4SwapEvent, RegisterV4PoolError,
    RegisterV4PoolParams, V4PoolIdentity, V4PoolKey, V4PoolState, V4StateSync, V4SwapUpdate,
};

/// The canonical request mapped onto V4's engine convention (ADR-037): V4 is
/// identity — `amountSpecified < 0` is exact input, which IS the canonical
/// user-perspective sign.
#[must_use]
pub(super) fn engine_amount_specified(canonical: I256) -> I256 {
    canonical
}

/// The V4 capture arm of [`super::capture_hop`] — one fresh observation of
/// one V4 hop, refused rather than defaulted: the directional viability gate
/// (V4 twin of the archived-Python `swap_is_viable` port) runs BEFORE the
/// ADR-037 hook refusal, which runs BEFORE the `O(tick_data)` range walk in
/// `build_int_v4_sequence`.
///
/// Errors
/// ------
/// [`CaptureRefusal::NotViable`] when the direction is dead at the current
/// state (sparse-coverage pools default viable — no data to judge);
/// [`CaptureRefusal::HookedPool`] when the hook address carries an
/// amount-modifying permission bit (ADR-037: admitted at registration but
/// never solvable);
/// [`CaptureRefusal::SequenceUnavailable`] when the integer tick-range
/// sequence could not be built for the direction.
pub(super) fn capture_hop(
    state: &V4PoolState,
    scalars: DesignScalars,
    hooks: Address,
    zero_for_one: bool,
) -> Result<Arc<IntV3TickRangeSequence>, CaptureRefusal> {
    if state.coverage == PoolTickCoverage::Tracked && !state.swap_is_viable(zero_for_one) {
        return Err(CaptureRefusal::NotViable);
    }
    if has_amount_modifying_hook(hooks) {
        return Err(CaptureRefusal::HookedPool);
    }
    let sequence = state
        .build_int_v4_sequence(scalars.tick_spacing, scalars.fee, zero_for_one)
        .ok_or(CaptureRefusal::SequenceUnavailable)?;
    Ok(Arc::new(sequence))
}

/// The V4 compute arm of [`super::read_swap`] — the stateless swap-read
/// against one V4 slice, in V4's own fee + sign conventions. The canonical
/// request is mapped here (never at the callers).
///
/// # Errors
///
/// [`SimulateSwapError::MissingTickWord`] when the walk enters an unfetched
/// tick-bitmap word (sparse coverage — the caller owns fetch + retry), or
/// [`SimulateSwapError::NotComputable`] on arithmetic overflow, invariant
/// violation, or non-positive amount.
pub(super) fn compute(
    state: &V4PoolState,
    scalars: DesignScalars,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: alloy::primitives::U256,
) -> Result<V3SwapOutcome, SimulateSwapError> {
    v4_simulate_swap(
        state,
        scalars.fee,
        scalars.tick_spacing,
        zero_for_one,
        engine_amount_specified(amount_specified),
        sqrt_price_limit,
    )
}

/// The V4 apply arm of the [`FamilyApply`] contract — one buffered pool
/// event (`Liquidity` or `Swap`) onto one V4 slice. V4 twin of the V3 apply
/// arm — the payload already carries the `int128` bound both contracts narrow
/// to, so this arm narrows nothing (ADR-082 D3).
impl FamilyApply for V4PoolState {
    type Event = BufferedV4PoolEvent;

    fn apply_event(&mut self, event: Self::Event) {
        match event {
            BufferedV4PoolEvent::Liquidity(u) => {
                self.apply_liquidity_update(
                    u.tick_lower,
                    u.tick_upper,
                    u.liquidity_delta,
                    u.block_number,
                );
            }
            BufferedV4PoolEvent::Swap(s) => {
                self.apply_swap(s.sqrt_price_x96, s.liquidity, s.tick, s.block_number, &[]);
            }
        }
    }
}

/// The V4 observe arm of [`super::observe`] — family-local GET behavior over
/// one V4 slice. [`Observe::Hooked`] reads the design's hook permission bits.
pub(super) fn observe(state: &V4PoolState, probe: Observe, hooks: Address) -> Observed {
    match probe {
        Observe::Coverage => Observed::Coverage(state.coverage),
        Observe::Hooked => Observed::Hooked(has_amount_modifying_hook(hooks)),
        Observe::TickFingerprint => Observed::TickFingerprint(Some((
            state.update_block,
            state.tick_data.len(),
            state.tick_data.values().map(|t| t.block).sum(),
        ))),
        _ => unreachable!(
            "a concentrated liquidity slice answers only the concentrated liquidity probes"
        ),
    }
}

/// Build the V4 discovery transient: it mirrors the REGISTERED pool's
/// coverage AND its checked-word set (V4 twin of the V3 discovery discipline
/// — a caller-CHECKED empty word must never become a fetch target). The
/// transient zeroes `hook_flags`/`protocol_fee`: registered pools passed the
/// hook/dynamic-fee admission gate, and the discovery walk stages the
/// registered pool's own coverage.
#[must_use]
pub(super) fn discovery_state(
    identity: &V4PoolIdentity,
    state: &V4PoolState,
    journal_depth: usize,
) -> Box<V4PoolState> {
    let mut transient = V4PoolState::from_params(
        RegisterV4PoolParams {
            pool_manager: identity.pool_manager,
            pool_id: identity.pool_id,
            pool_key: identity.pool_key.clone(),
            state_view: identity.state_view,
            hook_flags: 0,
            sqrt_price_x96: state.sqrt_price_x96,
            liquidity: state.liquidity,
            tick: state.tick,
            tick_data: state.tick_data.clone(),
            update_block: state.update_block,
            coverage: state.coverage,
            ..Default::default()
        },
        journal_depth,
    )
    .1;
    transient
        .known_bitmap_words
        .extend(state.known_bitmap_words.iter().copied());
    Box::new(transient)
}

/// Build the V4 override (hypothetical) transient from the registered
/// identity + the caller's override scalars. Coverage is `Sparse` and the
/// checked-word set is NOT mirrored (a hypothetical has no established
/// words). `hook_flags`/`protocol_fee` zero on the transient copy — safe by
/// construction for registered pools (admission gate).
#[must_use]
pub(super) fn override_state(
    identity: &V4PoolIdentity,
    over: &OverrideScalars<'_>,
    update_block: u64,
    journal_depth: usize,
) -> Box<V4PoolState> {
    let transient = V4PoolState::from_params(
        RegisterV4PoolParams {
            pool_manager: identity.pool_manager,
            pool_id: identity.pool_id,
            pool_key: identity.pool_key.clone(),
            state_view: identity.state_view,
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: over.sqrt_price_x96,
            liquidity: over.liquidity,
            tick: over.tick,
            tick_data: over.tick_data.clone(),
            update_block,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            known_bitmap_words: Vec::new(),
        },
        journal_depth,
    )
    .1;
    Box::new(transient)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used)]

    use super::engine_amount_specified;
    use alloy::primitives::I256;

    // -------------------------------------------------------------------
    // Sign-convention table (ADR-037): THE pinning test, V4 row — canonical
    // request sign -> V4 engine amountSpecified. Moved with the family from
    // the substrate swap-simulation gate; the outcome contract is unchanged.
    #[test]
    fn v4_sign_mapping_row() {
        let in_pos = I256::try_from(1_000_u64).expect("positive fits I256");
        let out_neg = -I256::try_from(2_000_u64).expect("negative fits I256");
        // exact-output request (positive): V4 is identity.
        assert_eq!(engine_amount_specified(in_pos), in_pos);
        // exact-input request (negative): V4 is identity.
        assert_eq!(engine_amount_specified(out_neg), out_neg);
    }
}

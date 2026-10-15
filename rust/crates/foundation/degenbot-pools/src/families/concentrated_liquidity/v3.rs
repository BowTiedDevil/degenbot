//! The V3 design-private half of the concentrated-liquidity family read
//! module (ADR-073 D1/D4).
//!
//! GUARDRAIL (load-bearing, do not fuse): this file and the V4 twin
//! ([`super::v4`]) deliberately share NO code — no shared V3/V4 constructor
//! and no projection helper crossing the two files. The load-bearing
//! differences live here and in `build_int_v3_sequence` / `v3_simulate_swap`:
//!
//! - **fee convention** — V3 charges `gamma = 1e6 − lp_fee` (the identity's
//!   `lp_fee` pip fraction), where V4 charges the combined
//!   `swapFee = calculateSwapFee(protocol_fee_dir, lp_fee)`;
//! - **current-tick drain framing** — the V3 sequence builder frames the
//!   leading hop at the current tick itself, where the V4 builder folds
//!   `base_liquidity` first;
//! - **per-range net-sign direction** — V3 has no per-range net-sign
//!   re-derivation (V4-only);
//! - **engine sign convention** — V3's `amountSpecified` is positive for
//!   exact input and negates both directions vs the canonical user
//!   perspective (ADR-037).
//!
//! Nothing here reads a state owner (ADR-003) or a solver kernel: every
//! function takes the family's state slice plus registration scalars.

use std::sync::Arc;

use alloy::primitives::I256;

use super::{CaptureRefusal, DesignScalars, Observe, Observed, OverrideScalars};

use crate::families::door::FamilyApply;
use crate::int_v3_hop::IntV3TickRangeSequence;
use crate::registry::ConcentratedLiquidityPoolMut as _;
use crate::v3_state::{v3_simulate_swap, PoolTickCoverage, SimulateSwapError, V3SwapOutcome};

/// The V3 state slice + its event/registration DTOs — the design's read
/// surface lives in `crate::v3_state` (I/O-free per ADR-001) and is
/// re-exported here so the family tree presents the whole read story under
/// one path.
pub use crate::v3_state::{
    BufferedV3LiquidityUpdate, BufferedV3PoolEvent, BufferedV3SwapEvent, RegisterV3PoolError,
    RegisterV3PoolParams, V3PoolIdentity, V3PoolState, V3SwapUpdate,
};

/// The canonical request mapped onto V3's engine convention (ADR-037): V3
/// negates both directions vs the canonical user perspective — a positive
/// canonical request is exact-OUTPUT, while V3's `amountSpecified` is positive
/// for exact input.
#[must_use]
pub(super) fn engine_amount_specified(canonical: I256) -> I256 {
    -canonical
}

/// The V3 capture arm of [`super::capture_hop`] — one fresh observation of
/// one V3 hop, refused rather than defaulted: the directional viability gate
/// (O(1) extremes check, ported from the archived Python `swap_is_viable`)
/// runs BEFORE the `O(tick_data)` range walk in `build_int_v3_sequence`.
///
/// Errors
/// ------
/// [`CaptureRefusal::NotViable`] when the direction is dead at the current
/// state (sparse-coverage pools default viable — no data to judge, mirroring
/// Python's `sparse_liquidity_map` early-True);
/// [`CaptureRefusal::SequenceUnavailable`] when the integer tick-range
/// sequence could not be built for the direction.
pub(super) fn capture_hop(
    state: &V3PoolState,
    scalars: DesignScalars,
    zero_for_one: bool,
) -> Result<Arc<IntV3TickRangeSequence>, CaptureRefusal> {
    if state.coverage == PoolTickCoverage::Tracked && !state.swap_is_viable(zero_for_one) {
        return Err(CaptureRefusal::NotViable);
    }
    let sequence = state
        .build_int_v3_sequence(scalars.tick_spacing, scalars.fee, zero_for_one)
        .ok_or(CaptureRefusal::SequenceUnavailable)?;
    Ok(Arc::new(sequence))
}

/// The V3 compute arm of [`super::read_swap`] — the stateless swap-read
/// against one V3 slice, in V3's own fee + sign conventions. The canonical
/// request is mapped here (never at the callers).
///
/// # Errors
///
/// [`SimulateSwapError::MissingTickWord`] when the walk enters an unfetched
/// tick-bitmap word (sparse coverage — the caller owns fetch + retry), or
/// [`SimulateSwapError::NotComputable`] on arithmetic overflow, invariant
/// violation, or non-positive amount.
pub(super) fn compute(
    state: &V3PoolState,
    scalars: DesignScalars,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: alloy::primitives::U256,
) -> Result<V3SwapOutcome, SimulateSwapError> {
    v3_simulate_swap(
        state,
        scalars.fee,
        scalars.tick_spacing,
        zero_for_one,
        engine_amount_specified(amount_specified),
        sqrt_price_limit,
    )
}

/// The V3 apply arm of the [`FamilyApply`] contract — one buffered pool
/// event (`Liquidity` or `Swap`) onto one V3 slice. The V3 drain loops
/// dispatch through here so cross-type arrival order within a block is
/// preserved (a `Swap` at logIdx 1433 lands after a `Mint` at logIdx 120 if
/// it arrived after). Mirrors the live-path apply methods: `Liquidity` →
/// `state.apply_liquidity_update`, `Swap` → `state.apply_swap` (with
/// `tick_priors: &[]` — the pump path never carries tick priors).
impl FamilyApply for V3PoolState {
    type Event = BufferedV3PoolEvent;

    fn apply_event(&mut self, event: Self::Event) {
        match event {
            BufferedV3PoolEvent::Liquidity(u) => self.apply_liquidity_update(
                u.tick_lower,
                u.tick_upper,
                u.liquidity_delta,
                u.block_number,
            ),
            BufferedV3PoolEvent::Swap(s) => {
                self.apply_swap(s.sqrt_price_x96, s.liquidity, s.tick, s.block_number, &[]);
            }
        }
    }
}

/// The V3 observe arm of [`super::observe`] — family-local GET behavior over
/// one V3 slice. V3 has no hook concept, so [`Observe::Hooked`] is always
/// `false`.
pub(super) fn observe(state: &V3PoolState, probe: Observe) -> Observed {
    match probe {
        Observe::Coverage => Observed::Coverage(state.coverage),
        Observe::Hooked => Observed::Hooked(false),
        Observe::TickFingerprint => Observed::TickFingerprint(Some((
            state.update_block,
            state.tick_data.len(),
            state.tick_data.values().map(|t| t.block).sum(),
        ))),
        _ => unreachable!("a CL slice answers only the CL probes"),
    }
}

/// Build the V3 discovery transient: it mirrors the REGISTERED pool's
/// coverage (a Tracked pool never raises `MissingTickWord`, so the staging
/// walk must not invent fetch work for one) AND its checked-word set
/// (`from_params` seeds known words from tick ROWS only — a caller-CHECKED
/// empty word has no rows yet must never become a fetch target).
#[must_use]
pub(super) fn discovery_state(
    identity: &V3PoolIdentity,
    state: &V3PoolState,
    journal_depth: usize,
) -> Box<V3PoolState> {
    let mut transient = V3PoolState::from_params(
        RegisterV3PoolParams {
            address: identity.address,
            token0: identity.token0,
            token1: identity.token1,
            fee: identity.fee,
            tick_spacing: identity.tick_spacing,
            factory: identity.factory,
            deployer: identity.deployer,
            init_hash: identity.init_hash,
            sqrt_price_x96: state.sqrt_price_x96,
            liquidity: state.liquidity,
            tick: state.tick,
            tick_data: state.tick_data.clone(),
            update_block: state.update_block,
            tick_data_block: None,
            coverage: state.coverage,
            fetcher: None,
            slot_layout: identity.slot_layout,
        },
        journal_depth,
    )
    .1;
    transient
        .known_bitmap_words
        .extend(state.known_bitmap_words.iter().copied());
    Box::new(transient)
}

/// Build the V3 override (hypothetical) transient from the registered
/// identity + the caller's override scalars. Coverage is `Sparse` (the
/// override walk may enter unfetched words; miss recovery is the caller's
/// fetch mode), and the checked-word set is NOT mirrored (a hypothetical has
/// no established words).
#[must_use]
pub(super) fn override_state(
    identity: &V3PoolIdentity,
    over: &OverrideScalars<'_>,
    update_block: u64,
    journal_depth: usize,
) -> Box<V3PoolState> {
    let transient = V3PoolState::from_params(
        RegisterV3PoolParams {
            address: identity.address,
            token0: identity.token0,
            token1: identity.token1,
            fee: identity.fee,
            tick_spacing: identity.tick_spacing,
            factory: identity.factory,
            deployer: identity.deployer,
            init_hash: identity.init_hash,
            sqrt_price_x96: over.sqrt_price_x96,
            liquidity: over.liquidity,
            tick: over.tick,
            tick_data: over.tick_data.clone(),
            update_block,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            slot_layout: identity.slot_layout,
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
    // Sign-convention table (ADR-037): THE pinning test, V3 row — canonical
    // request sign -> V3 engine amountSpecified. Moved with the family from
    // the substrate swap-simulation gate; the outcome contract is unchanged.
    #[test]
    fn v3_sign_mapping_row() {
        let in_pos = I256::try_from(1_000_u64).expect("positive fits I256");
        let out_neg = -I256::try_from(2_000_u64).expect("negative fits I256");
        // exact-output request (positive, pool delivers 1000): V3 negates.
        assert_eq!(
            engine_amount_specified(in_pos),
            -I256::try_from(1_000_u64).expect("negative fits I256")
        );
        // exact-input request (negative, user sends 2000): V3 negates to the
        // positive engine magnitude.
        assert_eq!(
            engine_amount_specified(out_neg),
            I256::try_from(2_000_u64).expect("positive fits I256")
        );
    }
}

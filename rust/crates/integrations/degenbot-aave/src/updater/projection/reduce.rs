//! The pure reduction step: resolved facts plus the projection view in, one
//! validated canonical delta out (ADR-105 D4).
//!
//! A reducer performs no acquisition and no storage access of any kind — it
//! receives the view and already-resolved facts, so an RPC or database reach is
//! absent by construction rather than by convention. Resolution (substrate
//! reads, entity identity) happens outside, and persistence happens later.

use super::{
    CanonicalDelta, DeltaEffect, GenerationRef, PositionSubject, ProjectionError, ProjectionView,
};
use alloy::primitives::{I256, U256};
use degenbot_core::canonical_head::LogRef;
use degenbot_db::ScaledTokenPosition;

/// The generation gate every reduction passes: the delta may only be reduced
/// under a generation the view is reading under (an unobserved entity is
/// accepted and read as the caller's own observation).
fn checked_generation(
    view: &ProjectionView,
    generation: GenerationRef,
) -> Result<(), ProjectionError> {
    match view.generation(generation.entity()) {
        Some(observed) if observed != generation.revision() => {
            Err(ProjectionError::StaleGeneration {
                entity: generation.entity(),
                reduced_under: generation.revision(),
                observed,
            })
        }
        _ => Ok(()),
    }
}

fn checked_subject(view: &ProjectionView, subject: PositionSubject) -> Result<(), ProjectionError> {
    view.require_position_state(subject).map(|_| ())
}

/// Reduce a signed scaled-balance movement (aToken/vToken Mint or Burn).
///
/// # Errors
///
/// [`ProjectionError::UnresolvedPosition`] when the subject has no preceding
/// state, and [`ProjectionError::StaleGeneration`] when the view reads another
/// generation of the same entity.
pub(crate) fn reduce_scaled_balance(
    view: &ProjectionView,
    subject: PositionSubject,
    balance_delta: I256,
    index: U256,
    source: LogRef,
    generation: GenerationRef,
) -> Result<CanonicalDelta, ProjectionError> {
    checked_generation(view, generation)?;
    checked_subject(view, subject)?;
    Ok(CanonicalDelta::new(
        source,
        Some(generation),
        DeltaEffect::ScaledBalance {
            subject,
            balance_delta,
            index,
        },
    ))
}

/// Reduce an aToken balance movement between two positions at one index.
///
/// # Errors
///
/// [`ProjectionError::UnresolvedPosition`] for either leg with no preceding
/// state, and [`ProjectionError::StaleGeneration`] on a generation mismatch.
pub(crate) fn reduce_scaled_transfer(
    view: &ProjectionView,
    from: PositionSubject,
    to: Option<PositionSubject>,
    scaled_amount: U256,
    index: U256,
    source: LogRef,
    generation: GenerationRef,
) -> Result<CanonicalDelta, ProjectionError> {
    checked_generation(view, generation)?;
    checked_subject(view, from)?;
    if let Some(to) = to {
        checked_subject(view, to)?;
    }
    Ok(CanonicalDelta::new(
        source,
        Some(generation),
        DeltaEffect::ScaledTransfer {
            from,
            to,
            scaled_amount,
            index,
        },
    ))
}

/// Reduce the bad-debt write-off of one debt position.
///
/// # Errors
///
/// [`ProjectionError::UnresolvedPosition`] when the debt position has no
/// preceding state, and [`ProjectionError::StaleGeneration`] on a generation
/// mismatch.
pub(crate) fn reduce_debt_reset(
    view: &ProjectionView,
    position_id: i64,
    index: U256,
    source: LogRef,
    generation: GenerationRef,
) -> Result<CanonicalDelta, ProjectionError> {
    checked_generation(view, generation)?;
    checked_subject(
        view,
        PositionSubject {
            position: ScaledTokenPosition::Debt,
            position_id,
        },
    )?;
    Ok(CanonicalDelta::new(
        source,
        Some(generation),
        DeltaEffect::DebtPositionReset { position_id, index },
    ))
}

/// Reduce the GHO discount-refresh signal for one debt position.
///
/// # Errors
///
/// [`ProjectionError::UnresolvedPosition`] when the debt position has no
/// preceding state, and [`ProjectionError::StaleGeneration`] on a generation
/// mismatch.
pub(crate) fn reduce_gho_refresh(
    view: &ProjectionView,
    position_id: i64,
    source: LogRef,
    generation: GenerationRef,
) -> Result<CanonicalDelta, ProjectionError> {
    checked_generation(view, generation)?;
    checked_subject(
        view,
        PositionSubject {
            position: ScaledTokenPosition::Debt,
            position_id,
        },
    )?;
    Ok(CanonicalDelta::new(
        source,
        Some(generation),
        DeltaEffect::GhoRefreshDiscount { position_id },
    ))
}

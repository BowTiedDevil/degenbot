//! The pure reduction step: resolved facts plus the projection view in, one
//! validated canonical delta out (ADR-105 D4).
//!
//! A reducer performs no acquisition and no storage access of any kind — it
//! receives the view and already-resolved facts, so an RPC or database reach is
//! absent by construction rather than by convention. Resolution (substrate
//! reads, entity identity) happens outside, and persistence happens later.

use super::{
    CanonicalDelta, ConfigurationEffect, DeltaEffect, GenerationRef, PositionSubject,
    ProjectionError, ProjectionView,
};
use alloy::primitives::{I256, U256};
use degenbot_core::canonical_head::LogRef;
use degenbot_db::ScaledTokenPosition;

/// The generation gate every reduction passes: the delta may only be reduced
/// under a generation the view is reading under (an unobserved entity is
/// accepted and read as the caller's own observation, and a delta that is
/// itself the activation installs the generation it names).
fn checked_generation(
    view: &ProjectionView,
    generation: GenerationRef,
    source: LogRef,
) -> Result<(), ProjectionError> {
    view.require_generation(generation, source)
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
    checked_generation(view, generation, source)?;
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
    checked_generation(view, generation, source)?;
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
    checked_generation(view, generation, source)?;
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

/// Reduce one configuration change (ADR-105 D4, D7).
///
/// A configuration effect moves no position state, so its reduction has no
/// preceding-state requirement; what it must prove is the generation it was
/// resolved under — a replacement event is the activation of the generation it
/// names, and every later event reduces under that generation.
///
/// # Errors
///
/// `ProjectionError::StaleGeneration` when the view reads another generation of
/// the same entity and this delta is not its activation.
pub(crate) fn reduce_configuration(
    view: &ProjectionView,
    effect: ConfigurationEffect,
    source: LogRef,
    generation: Option<GenerationRef>,
) -> Result<CanonicalDelta, ProjectionError> {
    if let Some(generation) = generation {
        checked_generation(view, generation, source)?;
    }
    Ok(CanonicalDelta::new(
        source,
        generation,
        DeltaEffect::Configuration(effect),
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
    checked_generation(view, generation, source)?;
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

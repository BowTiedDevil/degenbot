//! The forward-only Projection view and the canonical delta vocabulary
//! (ADR-105 D3, D4, D7).
//!
//! A `ProjectionView` is a protocol read position, not a block snapshot: it
//! holds the preceding state a resolution needs plus the ordered deltas already
//! applied in front of it, and it advances forward one event at a time. It has
//! no inverse and no block argument, so a resolver can read the state at an
//! exact transaction/log position rather than at some post-block height.
//!
//! Reduction is the pure step between resolution and persistence: a delta is
//! built from already-resolved facts plus the view, states the exact
//! [`LogRef`] it was emitted at and the [`GenerationRef`] its arithmetic was
//! defined under, and is then applied to the view so the next same-transaction
//! operation reads the advanced state. The reducers live in [`reduce`] and
//! reach no transport and no storage handle — reduction performs no acquisition
//! and no write (ADR-105 D4).

use std::collections::HashMap;

use alloy::primitives::{I256, U256};
use degenbot_core::canonical_head::LogRef;
use degenbot_db::ScaledTokenPosition;

pub(crate) use super::run::substrate::PositionState;

mod reduce;

#[cfg(test)]
mod tests;

pub(crate) use reduce::{
    reduce_debt_reset, reduce_gho_refresh, reduce_scaled_balance, reduce_scaled_transfer,
};

/// Which logical entity a reduction's generation relationship names.
///
/// A generation is branch-local and event-activated (ADR-105 D6): the pool's
/// revision selects the movement rule, a scaled token's revision selects the
/// rounding rule. The entity is the stable logical key; the revision is the
/// generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GenerationEntity {
    /// The market's Pool contract — the generation that selected the movement
    /// rule of a reduced operation.
    Pool { market_id: i64 },
    /// One reserve leg of a market's scaled token (aToken collateral /
    /// vToken debt) — the generation that selected the rounding rule.
    ScaledToken {
        asset_id: i64,
        position: ScaledTokenPosition,
    },
}

impl GenerationEntity {
    fn encode(self) -> String {
        match self {
            Self::Pool { market_id } => format!("pool{{market={market_id}}}"),
            Self::ScaledToken { asset_id, position } => format!(
                "scaled-token{{asset={asset_id},leg={}}}",
                match position {
                    ScaledTokenPosition::Collateral => "collateral",
                    ScaledTokenPosition::Debt => "debt",
                }
            ),
        }
    }
}

/// The generation-qualified relationship a delta was reduced under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GenerationRef {
    entity: GenerationEntity,
    revision: u32,
}

impl GenerationRef {
    #[must_use]
    pub const fn new(entity: GenerationEntity, revision: u32) -> Self {
        Self { entity, revision }
    }

    #[must_use]
    pub const fn entity(&self) -> GenerationEntity {
        self.entity
    }

    #[must_use]
    pub const fn revision(&self) -> u32 {
        self.revision
    }
}

/// One scaled-token position, named by its stable logical key (the leg) and its
/// settled row identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PositionSubject {
    pub position: ScaledTokenPosition,
    pub position_id: i64,
}

/// What one canonical delta does to the projection.
///
/// The variants are the operation family's outputs: every one of them was a
/// member of `AaveChunkEvent` before it carried a chain position, and now
/// states its effect instead of its payload alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaEffect {
    /// A signed scaled-balance movement (aToken/vToken Mint or Burn, both
    /// signed) reconciled to the event's index.
    ScaledBalance {
        subject: PositionSubject,
        balance_delta: I256,
        index: U256,
    },
    /// An aToken balance moved between two positions at one index. The
    /// recipient leg is absent for the burn-address leg.
    ScaledTransfer {
        from: PositionSubject,
        to: Option<PositionSubject>,
        scaled_amount: U256,
        index: U256,
    },
    /// The bad-debt write-off: the balance becomes zero and the index advances.
    DebtPositionReset { position_id: i64, index: U256 },
    /// The GHO discount-refresh signal: a post-apply read of the debt position,
    /// no state movement of its own.
    GhoRefreshDiscount { position_id: i64 },
}

/// One typed, validated projection write: its exact chain position, the
/// generation its reduction consumed, and its effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalDelta {
    source: LogRef,
    generation: Option<GenerationRef>,
    effect: DeltaEffect,
}

impl CanonicalDelta {
    #[must_use]
    pub const fn new(
        source: LogRef,
        generation: Option<GenerationRef>,
        effect: DeltaEffect,
    ) -> Self {
        Self {
            source,
            generation,
            effect,
        }
    }

    #[must_use]
    pub const fn source(&self) -> LogRef {
        self.source
    }

    #[must_use]
    pub const fn generation(&self) -> Option<GenerationRef> {
        self.generation
    }

    #[must_use]
    pub const fn effect(&self) -> &DeltaEffect {
        &self.effect
    }

    /// The delta's canonical encoding: its exact position, its generation
    /// relationship, and its effect. Two runs that reduce the same chain facts
    /// produce byte-identical encodings regardless of how the block range was
    /// chunked, because nothing chunk-scoped participates.
    #[must_use]
    pub fn encode(&self) -> String {
        let tx = self.source.tx();
        let anchor = tx.anchor();
        let generation = match self.generation {
            Some(generation) => {
                format!("{}@{}", generation.entity().encode(), generation.revision())
            }
            None => "none".to_string(),
        };
        format!(
            "{chain}/{number}/{block_hash}/{tx_hash}/{tx_index}/{log_index} generation={generation} effect={}",
            self.effect.encode(),
            chain = anchor.chain_id(),
            number = anchor.number(),
            block_hash = alloy::hex::encode(anchor.hash()),
            tx_hash = alloy::hex::encode(tx.hash()),
            tx_index = tx.index(),
            log_index = self.source.index(),
        )
    }
}

impl DeltaEffect {
    fn encode(&self) -> String {
        match self {
            Self::ScaledBalance {
                subject,
                balance_delta,
                index,
            } => format!(
                "scaled-balance{{{}:{}{:+},{index}}}",
                subject.position.table(),
                subject.position_id,
                balance_delta
            ),
            Self::ScaledTransfer {
                from,
                to,
                scaled_amount,
                index,
            } => format!(
                "scaled-transfer{{{}:{}->{},{scaled_amount},{index}}}",
                from.position.table(),
                from.position_id,
                to.map_or_else(
                    || "burn-address".to_string(),
                    |to| format!("{}:{}", to.position.table(), to.position_id)
                ),
            ),
            Self::DebtPositionReset { position_id, index } => {
                format!("debt-position-reset{{{position_id},{index}}}")
            }
            Self::GhoRefreshDiscount { position_id } => {
                format!("gho-refresh-discount{{{position_id}}}")
            }
        }
    }

    /// Every position subject the effect reads or moves — the positions a
    /// resolution must supply preceding state for.
    #[must_use]
    pub(crate) fn subjects(&self) -> Vec<PositionSubject> {
        match self {
            Self::ScaledBalance { subject, .. } => vec![*subject],
            Self::ScaledTransfer { from, to, .. } => {
                let mut subjects = vec![*from];
                if let Some(to) = to {
                    subjects.push(*to);
                }
                subjects
            }
            Self::DebtPositionReset { position_id, .. }
            | Self::GhoRefreshDiscount { position_id } => vec![PositionSubject {
                position: ScaledTokenPosition::Debt,
                position_id: *position_id,
            }],
        }
    }

    /// The signed balance movements the effect applies, and the index it
    /// reconciles to. Reset and refresh carry no signed movement of their own.
    ///
    /// A transfer's amount must fit the signed balance domain: the sender's leg
    /// is the negated magnitude, so a magnitude the domain cannot hold — or
    /// cannot negate — is a refusal at the sender rather than a saturated leg.
    ///
    /// # Errors
    ///
    /// [`ProjectionError::BalanceOverflow`] when the transfer's amount leaves the
    /// signed balance domain (reported against the sender).
    fn movements(&self) -> Result<Movements, ProjectionError> {
        match self {
            Self::ScaledBalance {
                subject,
                balance_delta,
                index,
            } => Ok((vec![(*subject, *balance_delta)], Some(*index))),
            Self::ScaledTransfer {
                from,
                to,
                scaled_amount,
                index,
            } => {
                let overflow = ProjectionError::BalanceOverflow {
                    position_id: from.position_id,
                };
                let magnitude = I256::try_from(*scaled_amount).map_err(|_| overflow.clone())?;
                let debit = magnitude.checked_neg().ok_or(overflow)?;
                let mut movements = vec![(*from, debit)];
                if let Some(to) = to {
                    movements.push((*to, magnitude));
                }
                Ok((movements, Some(*index)))
            }
            Self::DebtPositionReset { .. } | Self::GhoRefreshDiscount { .. } => {
                Ok((Vec::new(), None))
            }
        }
    }
}

/// One effect's signed balance movements and the index they reconcile to.
type Movements = (Vec<(PositionSubject, I256)>, Option<U256>);

/// Why a delta cannot be reduced or applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectionError {
    /// The view holds no preceding state for the position: a reduction cannot
    /// invent a zero default (ADR-105 D11).
    #[error("position {position:?} id={position_id} has no resolved state in the projection view")]
    UnresolvedPosition {
        position: ScaledTokenPosition,
        position_id: i64,
    },
    /// The delta's position is behind the view's cursor — a replayed,
    /// out-of-order, or same-height other-branch delta is a refusal, never a
    /// silent re-application. A position the view already advanced to is
    /// forward: one log can state a movement and the signal it raises.
    #[error("delta at {arrived:?} is not forward of the projection cursor {held:?}")]
    NotForward {
        held: Box<LogRef>,
        arrived: Box<LogRef>,
    },
    /// The delta was reduced under a generation the view is not reading under.
    #[error(
        "delta reduced under revision {reduced_under} of {entity:?}, but the view reads revision {observed}"
    )]
    StaleGeneration {
        entity: GenerationEntity,
        reduced_under: u32,
        observed: u32,
    },
    /// The effect's arithmetic left the `U256` balance domain.
    #[error("position id={position_id} balance arithmetic overflowed U256")]
    BalanceOverflow { position_id: i64 },
}

/// The forward-only projection read position: the exact transaction/log event
/// it has advanced to, the preceding position states resolution supplied, and
/// the generations it is reading under.
#[derive(Debug, Default)]
pub struct ProjectionView {
    cursor: Option<LogRef>,
    positions: HashMap<(ScaledTokenPosition, i64), PositionState>,
    generations: HashMap<GenerationEntity, u32>,
}

impl ProjectionView {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The exact position the view has advanced to; `None` before its first
    /// delta.
    #[must_use]
    pub const fn cursor(&self) -> Option<LogRef> {
        self.cursor
    }

    /// Hand the view one resolved preceding state (a read of the relational
    /// substrate, taken outside the write transaction).
    pub fn seed_position(&mut self, subject: PositionSubject, state: PositionState) {
        self.positions
            .insert((subject.position, subject.position_id), state);
    }

    /// The state the view reads for a position, if resolution supplied it.
    #[must_use]
    pub fn position_state(&self, subject: PositionSubject) -> Option<PositionState> {
        self.positions
            .get(&(subject.position, subject.position_id))
            .copied()
    }

    /// The period-correct state a resolution needs; an unresolvable position is
    /// a refusal.
    ///
    /// # Errors
    ///
    /// [`ProjectionError::UnresolvedPosition`] when the view holds no state.
    pub fn require_position_state(
        &self,
        subject: PositionSubject,
    ) -> Result<PositionState, ProjectionError> {
        self.position_state(subject)
            .ok_or(ProjectionError::UnresolvedPosition {
                position: subject.position,
                position_id: subject.position_id,
            })
    }

    /// Record the generation a resolution observed for an entity. The latest
    /// observation is the generation the view reads under.
    pub fn observe_generation(&mut self, generation: GenerationRef) {
        self.generations
            .insert(generation.entity(), generation.revision());
    }

    /// The generation the view reads for an entity, if resolution observed one.
    #[must_use]
    pub fn generation(&self, entity: GenerationEntity) -> Option<u32> {
        self.generations.get(&entity).copied()
    }

    /// Every observed generation.
    pub fn generations(&self) -> impl Iterator<Item = (GenerationEntity, u32)> + '_ {
        self.generations
            .iter()
            .map(|(entity, revision)| (*entity, *revision))
    }

    /// Advance the view by one delta: validate its position and generation,
    /// apply its effect to the preceding state, and take its position as the
    /// new cursor. A refused delta leaves the view untouched.
    ///
    /// # Errors
    ///
    /// [`ProjectionError::NotForward`] when the delta is not ahead of the
    /// cursor, [`ProjectionError::StaleGeneration`] when the view reads another
    /// generation, [`ProjectionError::UnresolvedPosition`] for state the view
    /// never resolved, and [`ProjectionError::BalanceOverflow`] on overflow.
    pub fn apply(&mut self, delta: &CanonicalDelta) -> Result<(), ProjectionError> {
        self.require_forward(delta.source())?;
        if let Some(generation) = delta.generation() {
            if let Some(observed) = self.generation(generation.entity()) {
                if observed != generation.revision() {
                    return Err(ProjectionError::StaleGeneration {
                        entity: generation.entity(),
                        reduced_under: generation.revision(),
                        observed,
                    });
                }
            }
        }
        let mut advanced: Vec<((ScaledTokenPosition, i64), PositionState)> = Vec::new();
        let (movements, index) = delta.effect().movements()?;
        for (subject, balance_delta) in movements {
            let (current_balance, current_index) = self.require_position_state(subject)?;
            let next = advance_scaled_balance(
                subject.position_id,
                current_balance,
                current_index,
                balance_delta,
                index.unwrap_or_default(),
            )?;
            advanced.push(((subject.position, subject.position_id), next));
        }
        if let DeltaEffect::DebtPositionReset { position_id, index } = delta.effect() {
            let subject = PositionSubject {
                position: ScaledTokenPosition::Debt,
                position_id: *position_id,
            };
            let (_, current_index) = self.require_position_state(subject)?;
            advanced.push((
                (ScaledTokenPosition::Debt, *position_id),
                (U256::ZERO, Some(advance_index(current_index, *index))),
            ));
        }
        for (key, state) in advanced {
            self.positions.insert(key, state);
        }
        self.cursor = Some(delta.source());
        Ok(())
    }

    fn require_forward(&self, arrived: LogRef) -> Result<(), ProjectionError> {
        let Some(held) = self.cursor else {
            return Ok(());
        };
        if !is_forward(held, arrived) {
            return Err(ProjectionError::NotForward {
                held: Box::new(held),
                arrived: Box::new(arrived),
            });
        }
        Ok(())
    }
}

/// Whether `arrived` is at or ahead of `held` in canonical event order, on the
/// same chain, and on the same branch when the two share a height.
///
/// One log position can carry more than one delta — a movement and the
/// discount-refresh signal it raises are both stated by the same event — so the
/// cursor is a position the view has advanced to, and equal positions are
/// forward. Anything strictly behind the cursor is a replay, a same-height
/// other-branch delta, or another chain's state under coincidentally identical
/// local coordinates, and is refused.
fn is_forward(held: LogRef, arrived: LogRef) -> bool {
    let held_anchor = held.tx().anchor();
    let arrived_anchor = arrived.tx().anchor();
    if held_anchor.chain_id() != arrived_anchor.chain_id() {
        return false;
    }
    if held_anchor.number() == arrived_anchor.number()
        && held_anchor.hash() != arrived_anchor.hash()
    {
        return false;
    }
    (
        arrived_anchor.number(),
        arrived.tx().index(),
        arrived.index(),
    ) >= (held_anchor.number(), held.tx().index(), held.index())
}

/// The scaled-balance advance: the signed delta, the burn cap to the settled
/// balance, and the max-with-prev index reconciliation — the same state the
/// storage layer writes.
///
/// The sum is checked in the signed domain, so a preceding balance or a result
/// it cannot represent is a refusal rather than a clamped or overflowed state.
/// A genuinely negative result of a negative delta is the contract's
/// burn cap and clamps to zero.
fn advance_scaled_balance(
    position_id: i64,
    current_balance: U256,
    current_index: Option<U256>,
    balance_delta: I256,
    index: U256,
) -> Result<PositionState, ProjectionError> {
    let overflow = ProjectionError::BalanceOverflow { position_id };
    let current = I256::try_from(current_balance).map_err(|_| overflow.clone())?;
    let next = current.checked_add(balance_delta).ok_or(overflow.clone())?;
    let balance = if next.is_negative() && balance_delta.is_negative() {
        U256::ZERO
    } else {
        U256::try_from(next).map_err(|_| overflow)?
    };
    Ok((balance, Some(advance_index(current_index, index))))
}

/// The max-with-prev index reconciliation: advance only to a strictly greater
/// index, so an out-of-log-order event cannot clobber a newer index.
fn advance_index(current: Option<U256>, index: U256) -> U256 {
    match current {
        Some(current) if index > current => index,
        Some(current) => current,
        None => index,
    }
}

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

pub(crate) use super::run::substrate::PositionState;
use alloy::primitives::{I256, U256};
use degenbot_core::canonical_head::LogRef;

mod checkpoint;
mod configuration;
mod journal;
mod reduce;

pub use checkpoint::{
    checkpoint_due, classify_discount_prepass, CheckpointState, DiscountPath,
    DiscountPrepassInputs, CHECKPOINT_ENCODING_VERSION,
};
pub use configuration::ConfigurationEffect;
pub use degenbot_db::ScaledTokenPosition;
pub use journal::{
    journal_entries, journal_entry, market_projection, MarketProjection, DELTA_ENCODING_VERSION,
};

#[cfg(test)]
mod tests;

pub(crate) use reduce::{
    reduce_configuration, reduce_debt_reset, reduce_gho_refresh, reduce_scaled_balance,
    reduce_scaled_transfer,
};

/// Which logical entity a reduction's generation relationship names.
///
/// A generation is branch-local and event-activated (ADR-105 D6): the pool's
/// revision selects the movement rule, a scaled token's revision selects the
/// rounding rule, a contract role's address is the endpoint the fetcher
/// targets. The entity is the stable logical key; the activation event and the
/// fact it carries are the generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GenerationEntity {
    /// One reserve leg of a market's scaled token (aToken collateral /
    /// vToken debt) — the generation that selected the rounding rule.
    ScaledToken {
        asset_id: i64,
        position: ScaledTokenPosition,
    },
    /// One contract role of a market (proxy implementation or fetch endpoint).
    Contract { market_id: i64, role: ContractRole },
    /// The chain's discount-token series — the stkAAVE generation whose ledger
    /// denominates the user discount balances.
    DiscountToken { chain_id: i64 },
    /// The chain's GHO vToken link — the generation that made a vToken the
    /// GHO debt token (the precondition for GHO operation classification).
    GhoDiscountLink { chain_id: i64 },
}

/// The stable set of contract roles a market resolves addresses for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ContractRole {
    /// The market's Pool proxy (and its fetch endpoint).
    Pool,
    /// The market's Pool Configurator proxy (and its fetch endpoint).
    PoolConfigurator,
    /// The Pool Address Provider the bootstrap reads deployments from.
    PoolAddressProvider,
    /// The Pool Data Provider.
    PoolDataProvider,
    /// The market's price oracle.
    PriceOracle,
}

impl ContractRole {
    /// Every role, in the vocabulary's order.
    pub const ALL: &'static [Self] = &[
        Self::Pool,
        Self::PoolConfigurator,
        Self::PoolAddressProvider,
        Self::PoolDataProvider,
        Self::PriceOracle,
    ];

    /// The role a `aave_v3_contracts` row name stands for; `None` for a name
    /// outside the tracked role set.
    #[must_use]
    pub fn from_contract_name(name: &str) -> Option<Self> {
        match name {
            "POOL" => Some(Self::Pool),
            "POOL_CONFIGURATOR" => Some(Self::PoolConfigurator),
            "POOL_ADDRESS_PROVIDER" => Some(Self::PoolAddressProvider),
            "POOL_DATA_PROVIDER" => Some(Self::PoolDataProvider),
            "PRICE_ORACLE" => Some(Self::PriceOracle),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Pool => "pool",
            Self::PoolConfigurator => "pool-configurator",
            Self::PoolAddressProvider => "pool-address-provider",
            Self::PoolDataProvider => "pool-data-provider",
            Self::PriceOracle => "price-oracle",
        }
    }
}

impl GenerationEntity {
    /// The stable logical key a journaled generation relationship states,
    /// decoded back into the entity it names (the inverse of [`Self::encode`]).
    ///
    /// A restore reads a checkpoint's generation family as text, so the inverse
    /// exists once, here, beside the encoding it inverts — and refuses an entity
    /// key it cannot reconstruct rather than guessing.
    ///
    /// # Errors
    ///
    /// [`crate::projection::ProjectionError::Decode`] for an entity key the
    /// vocabulary does not define.
    pub fn decode(key: &str) -> Result<Self, crate::projection::ProjectionError> {
        let bad = || crate::projection::ProjectionError::EntityKey {
            key: key.to_string(),
        };
        if let Some(inner) = key
            .strip_prefix("scaled-token{")
            .and_then(|rest| rest.strip_suffix('}'))
        {
            let (asset, leg) = inner.split_once(",leg=").ok_or_else(bad)?;
            let asset_id: i64 = asset
                .strip_prefix("asset=")
                .ok_or_else(bad)?
                .parse()
                .map_err(|_| bad())?;
            let position = match leg {
                "collateral" => ScaledTokenPosition::Collateral,
                "debt" => ScaledTokenPosition::Debt,
                _ => return Err(bad()),
            };
            return Ok(Self::ScaledToken { asset_id, position });
        }
        if let Some(inner) = key
            .strip_prefix("contract{")
            .and_then(|rest| rest.strip_suffix('}'))
        {
            let (market, role) = inner.split_once(",role=").ok_or_else(bad)?;
            let market_id: i64 = market
                .strip_prefix("market=")
                .ok_or_else(bad)?
                .parse()
                .map_err(|_| bad())?;
            let role = ContractRole::ALL
                .iter()
                .copied()
                .find(|candidate| candidate.name() == role)
                .ok_or_else(bad)?;
            return Ok(Self::Contract { market_id, role });
        }
        if let Some(inner) = key
            .strip_prefix("discount-token{")
            .and_then(|rest| rest.strip_suffix('}'))
        {
            let chain_id = inner
                .strip_prefix("chain=")
                .ok_or_else(bad)?
                .parse()
                .map_err(|_| bad())?;
            return Ok(Self::DiscountToken { chain_id });
        }
        if let Some(inner) = key
            .strip_prefix("gho-discount-link{")
            .and_then(|rest| rest.strip_suffix('}'))
        {
            let chain_id = inner
                .strip_prefix("chain=")
                .ok_or_else(bad)?
                .parse()
                .map_err(|_| bad())?;
            return Ok(Self::GhoDiscountLink { chain_id });
        }
        Err(bad())
    }

    /// The stable logical key a journaled generation relationship states.
    #[must_use]
    pub fn encode(self) -> String {
        match self {
            Self::ScaledToken { asset_id, position } => format!(
                "scaled-token{{asset={asset_id},leg={}}}",
                match position {
                    ScaledTokenPosition::Collateral => "collateral",
                    ScaledTokenPosition::Debt => "debt",
                }
            ),
            Self::Contract { market_id, role } => {
                format!("contract{{market={market_id},role={}}}", role.name())
            }
            Self::DiscountToken { chain_id } => format!("discount-token{{chain={chain_id}}}"),
            Self::GhoDiscountLink { chain_id } => {
                format!("gho-discount-link{{chain={chain_id}}}")
            }
        }
    }
}

/// Where a generation came into being (ADR-105 D6): the canonical event that
/// activated it, or `Seed` for the generation already active when the retained
/// window began (its activation predates the chunk being reduced).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GenerationActivation {
    /// The generation the resolution started from — committed state at the
    /// chunk's first event.
    Seed,
    /// The generation was activated by this exact event.
    Event(LogRef),
}

/// The generation-qualified relationship a delta was reduced under.
///
/// Identity is `(entity, activation)`: a same-block replacement activates a
/// different generation, never a mutated one. `revision` carries the entity's
/// revision fact where the entity has one (a proxy's `REVISION()`, a scaled
/// token's rounding revision); a role or link generation has no revision
/// contract, so it carries none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GenerationRef {
    entity: GenerationEntity,
    activation: GenerationActivation,
    revision: Option<u32>,
}

impl GenerationRef {
    /// The seeded generation of a revisioned entity: the generation resolved
    /// from committed state, carrying the revision its arithmetic reads.
    #[must_use]
    pub const fn new(entity: GenerationEntity, revision: u32) -> Self {
        Self {
            entity,
            activation: GenerationActivation::Seed,
            revision: Some(revision),
        }
    }

    /// The seeded generation of an entity with no revision fact (an endpoint,
    /// a discount token, a link).
    #[must_use]
    pub const fn seed(entity: GenerationEntity) -> Self {
        Self {
            entity,
            activation: GenerationActivation::Seed,
            revision: None,
        }
    }

    /// The generation a checkpoint states, in the activation form it was
    /// journaled under.
    #[must_use]
    pub const fn restored(
        entity: GenerationEntity,
        activation: GenerationActivation,
        revision: Option<u32>,
    ) -> Self {
        Self {
            entity,
            activation,
            revision,
        }
    }

    /// The generation this exact event activated.
    #[must_use]
    pub const fn activated_at(
        entity: GenerationEntity,
        revision: Option<u32>,
        activation: LogRef,
    ) -> Self {
        Self {
            entity,
            activation: GenerationActivation::Event(activation),
            revision,
        }
    }

    #[must_use]
    pub const fn entity(&self) -> GenerationEntity {
        self.entity
    }

    #[must_use]
    pub const fn revision(&self) -> Option<u32> {
        self.revision
    }

    #[must_use]
    pub const fn activation(&self) -> GenerationActivation {
        self.activation
    }

    /// Whether this generation names `source` as its own activation — the one
    /// case where a delta may disagree with the generation the view reads.
    fn activates_at(&self, source: LogRef) -> bool {
        matches!(self.activation, GenerationActivation::Event(activated) if activated == source)
    }

    fn encode(&self) -> String {
        let activation = match self.activation {
            GenerationActivation::Seed => "seed".to_string(),
            GenerationActivation::Event(at) => {
                let tx = at.tx();
                let anchor = tx.anchor();
                format!(
                    "{chain}/{number}/{block_hash}/{tx_hash}/{tx_index}/{log_index}",
                    chain = anchor.chain_id(),
                    number = anchor.number(),
                    block_hash = alloy::hex::encode(anchor.hash()),
                    tx_hash = alloy::hex::encode(tx.hash()),
                    tx_index = tx.index(),
                    log_index = at.index(),
                )
            }
        };
        let revision = self
            .revision
            .map_or_else(|| "none".to_string(), |revision| revision.to_string());
        format!(
            "{} activated={activation} revision={revision}",
            self.entity.encode()
        )
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
/// member of `ConfigurationEffect` before it carried a chain position, and now
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
    /// A resolved stkAAVE balance observation for a user: the seed the GHO
    /// discount computation read (a `balanceOf` at the exact parent block),
    /// persisted so a later chunk reads the projection instead of the chain.
    StkAaveBalanceObserved { user_id: i64, balance: U256 },
    /// The post-apply GHO discount recomputation: the user's discount percent
    /// after the transaction's debt movements. The arithmetic ran in
    /// resolution against the advanced projection view; the delta carries the
    /// settled result so persistence writes it without recomputing.
    GhoDiscountRefreshed { user_id: i64, gho_discount: i64 },
    /// One configuration change — the migrated `ConfigurationEffect` family. Its
    /// payload carries the resolved ids and the fields the effect writes; the
    /// enclosing delta carries the exact position and the generation the event
    /// activated.
    Configuration(ConfigurationEffect),
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
        let generation = self
            .generation
            .map_or_else(|| "none".to_string(), |generation| generation.encode());
        format!(
            "{chain}/{number}/{block_hash}/{tx_hash}/{tx_index}/{log_index} generation {generation} effect={}",
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
            Self::StkAaveBalanceObserved { user_id, balance } => {
                format!("stk-aave-balance-observed{{user={user_id},balance={balance}}}")
            }
            Self::GhoDiscountRefreshed {
                user_id,
                gho_discount,
            } => format!("gho-discount-refreshed{{user={user_id},discount={gho_discount}}}"),
            Self::Configuration(effect) => effect.encode(),
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
            Self::StkAaveBalanceObserved { .. }
            | Self::GhoDiscountRefreshed { .. }
            | Self::Configuration(_) => Vec::new(),
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
            Self::DebtPositionReset { .. }
            | Self::GhoRefreshDiscount { .. }
            | Self::StkAaveBalanceObserved { .. }
            | Self::GhoDiscountRefreshed { .. }
            | Self::Configuration(_) => Ok((Vec::new(), None)),
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
    #[error("delta reduced under generation {reduced_under:?}, but the view reads {observed:?}")]
    StaleGeneration {
        entity: GenerationEntity,
        reduced_under: String,
        observed: String,
    },
    /// The effect's arithmetic left the `U256` balance domain.
    #[error("position id={position_id} balance arithmetic overflowed U256")]
    BalanceOverflow { position_id: i64 },
    /// An entity key the generation vocabulary does not define: a restore cannot
    /// reconstruct it, and guessing would attach a generation to the wrong
    /// logical entity.
    #[error("generation entity key {key:?} is outside the vocabulary")]
    EntityKey { key: String },

    /// A generation revision the vocabulary cannot hold — a negative or
    /// wider-than-`u32` revision is a refusal, never a synthesized zero.
    #[error("generation revision {revision} is outside the u32 domain")]
    InvalidGenerationRevision { revision: i64 },
    /// A later-started operation names an earlier chain position than the
    /// operation reduced before it: the operation stream must advance, even
    /// though one operation's own legs may sit at earlier logs than its
    /// anchor.
    #[error(
        "operation anchored at {arrived:?} is not forward of the previous operation anchor {held:?}"
    )]
    OperationOutOfOrder {
        held: Box<LogRef>,
        arrived: Box<LogRef>,
    },
}

/// The forward-only projection read position: the exact transaction/log event
/// it has advanced to, the preceding position states resolution supplied, and
/// the generations it is reading under.
#[derive(Debug, Default)]
pub struct ProjectionView {
    cursor: Option<LogRef>,
    operation_anchor: Option<LogRef>,
    positions: HashMap<(ScaledTokenPosition, i64), PositionState>,
    generations: HashMap<GenerationEntity, GenerationRef>,
}

impl ProjectionView {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the exact position a restored checkpoint states as the view's
    /// cursor, so a resumed reduction refuses a delta behind it.
    pub fn seed_cursor(&mut self, cursor: LogRef) {
        self.cursor = Some(cursor);
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
        self.generations.insert(generation.entity(), generation);
    }

    /// The generation the view reads for an entity, if resolution observed one.
    #[must_use]
    pub fn generation(&self, entity: GenerationEntity) -> Option<GenerationRef> {
        self.generations.get(&entity).copied()
    }

    /// Every observed generation.
    pub fn generations(&self) -> impl Iterator<Item = (GenerationEntity, GenerationRef)> + '_ {
        self.generations
            .iter()
            .map(|(entity, generation)| (*entity, *generation))
    }

    /// Every position state the view holds, keyed by its subject — the state a
    /// retained delta may move next, which is what a checkpoint captures.
    pub fn positions(&self) -> impl Iterator<Item = (PositionSubject, PositionState)> + '_ {
        self.positions
            .iter()
            .map(|((position, position_id), state)| {
                (
                    PositionSubject {
                        position: *position,
                        position_id: *position_id,
                    },
                    *state,
                )
            })
    }

    /// The generation a delta must be interpreted under, refusing a delta whose
    /// reduction consumed a generation the view is not reading — unless the
    /// delta is itself the activation that installs the generation it names.
    /// This is the ONE generation rule: reduction and advance both apply it, so
    /// a same-block replacement cannot be read under the generation it
    /// replaced (ADR-105 D6).
    fn require_generation(
        &self,
        generation: GenerationRef,
        source: LogRef,
    ) -> Result<(), ProjectionError> {
        match self.generation(generation.entity()) {
            Some(observed) if observed != generation && !generation.activates_at(source) => {
                Err(ProjectionError::StaleGeneration {
                    entity: generation.entity(),
                    reduced_under: generation.encode(),
                    observed: observed.encode(),
                })
            }
            _ => Ok(()),
        }
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
            self.require_generation(generation, delta.source())?;
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
        if let Some(generation) = delta.generation() {
            if generation.activates_at(delta.source()) {
                self.generations.insert(generation.entity(), generation);
            }
        }
        self.cursor = Some(delta.source());
        Ok(())
    }

    /// Open one operation's reduction unit at its anchor (the position the
    /// operation's own event or minimum leg names). The operation stream is
    /// forward-only: a later-started operation may not anchor at an earlier
    /// chain position than the operation reduced before it.
    ///
    /// The delta cursor stays transaction-grained, because one operation's
    /// legs and the configuration effects around it can sit at earlier logs
    /// than a previously reduced operation's. This guard is what keeps the
    /// OPERATIONS ordered without imposing log order on their interior.
    ///
    /// # Errors
    ///
    /// [`ProjectionError::OperationOutOfOrder`] for an anchor behind the
    /// previous operation's anchor (including a same-height other-branch
    /// anchor), and [`ProjectionError::NotForward`] for an operation in an
    /// earlier transaction than the view has advanced to.
    pub fn begin_operation(&mut self, anchor: LogRef) -> Result<(), ProjectionError> {
        self.require_forward(anchor)?;
        if let Some(held) = self.operation_anchor {
            if !same_branch(held, anchor) || position(anchor) < position(held) {
                return Err(ProjectionError::OperationOutOfOrder {
                    held: Box::new(held),
                    arrived: Box::new(anchor),
                });
            }
        }
        self.operation_anchor = Some(anchor);
        Ok(())
    }

    /// The anchor of the operation the view is reducing, if any.
    #[must_use]
    pub const fn operation_anchor(&self) -> Option<LogRef> {
        self.operation_anchor
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
/// The transaction is the view's reduction unit: within one transaction the
/// protocol's own operation order decides the delta sequence (an operation's
/// legs can sit at earlier logs than a previously reduced operation's), so the
/// interior order of one transaction is not a position claim, and one log can
/// carry more than one delta. What must never go backwards is the transaction:
/// a delta from an earlier transaction is a replay, a same-height other-branch
/// delta, or another chain's state under coincidentally identical local
/// coordinates, and is refused.
fn is_forward(held: LogRef, arrived: LogRef) -> bool {
    same_branch(held, arrived)
        && (arrived.tx().anchor().number(), arrived.tx().index())
            >= (held.tx().anchor().number(), held.tx().index())
}

/// Whether two positions name one chain and one branch: identical chains, and
/// identical block hashes when they share a height (a same-height peer block is
/// another branch, whatever its local coordinates).
fn same_branch(held: LogRef, arrived: LogRef) -> bool {
    let held_anchor = held.tx().anchor();
    let arrived_anchor = arrived.tx().anchor();
    held_anchor.chain_id() == arrived_anchor.chain_id()
        && (held_anchor.number() != arrived_anchor.number()
            || held_anchor.hash() == arrived_anchor.hash())
}

/// A position's canonical ordering key: block height, transaction index,
/// block-wide log index.
fn position(at: LogRef) -> (u64, u64, u64) {
    (at.tx().anchor().number(), at.tx().index(), at.index())
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

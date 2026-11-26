//! Total receipt ownership: one terminal classification for every log of a
//! transaction's complete receipt (ADR-105 D5).
//!
//! A receipt is evidence about ONE execution, and every log in it belongs to
//! something: an Aave operation, a recognized non-operation role, or a log this
//! market's projection deliberately does not read. Aave reports one balance
//! movement twice (the index-less ERC20 `Transfer` of underlying units and the
//! index-bearing `BalanceTransfer` of scaled units) and emits a zero-address
//! companion `Transfer` for every semantic `Mint`/`Burn`: those are roles, not
//! operations, and never a second operation source.
//!
//! [`OutOfScope`] is the third arm, and it means deliberately irrelevant, never
//! unknown. A log emitted by this market's Pool or by one of its tracked tokens
//! that the protocol-event catalog cannot place is an ingress error — refused,
//! not swept into the ignored pile — while another protocol's log in the same
//! transaction is genuinely this market's business to ignore.
//!
//! The ledger is built ONCE per transaction, before any operation exists, and
//! the parse claims into it afterwards: an operation owns the logs it consumes.
//! A second operation claiming the same log, an operation claiming a role the
//! receipt already settled, and a log still expecting an owner when the parse
//! ends are all refusals. That is what makes the census total instead of a
//! leftover list nobody reads.

use super::util::{CompanionSide, EventDisposition, EventOwnership};
use super::{log_idx_value, Address, Log, Operation, ScaledTokenEvent, ScaledTokenEventType};
use crate::updater::config_dispatch::tracks_proxy_id;
use crate::updater::protocol_events::{ProtocolContext, ReportAnswer};
use degenbot_decoders::aave_event_decoder::DecodedAaveEvent;
use std::collections::BTreeMap;

/// A recognized Aave role that deliberately emits no operation.
///
/// Each variant names WHY the log carries no operation of its own, so a census
/// reads as protocol vocabulary rather than as a list of exceptions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NonOperationRole {
    /// Aave reports one balance movement twice: this is the index-less ERC20
    /// report (underlying units) of a movement whose index-bearing
    /// `BalanceTransfer` report an operation accounts for.
    MovementMirror {
        /// The movement's scaled report.
        scaled: u64,
    },
    /// The index-bearing `BalanceTransfer` report (scaled units) of a movement
    /// whose index-less ERC20 report carries the operation.
    ScalingMirror {
        /// The movement's index-less report.
        carrier: u64,
    },
    /// A zero-address `Transfer` reporting a delta the named semantic
    /// `Mint`/`Burn` already carries.
    Companion {
        /// The semantic event that carries the delta.
        owner: u64,
        /// Which semantic event it mirrors.
        side: CompanionSide,
    },
    /// A repeated underlying-unit report of a movement an earlier report already
    /// carried (a token that reports the same movement more than once).
    DuplicateCarrier {
        /// The carried movement's first report.
        owner: u64,
    },
    /// A scaled `Mint` whose value equals its balance increase: the position
    /// accrued interest and nothing else, which is what the pool's net
    /// `mintToTreasury` mint reports.
    AccrualOnly,
    /// `ReserveDataUpdated`: the pool's per-reserve accrual state report.
    ReserveReport,
    /// `IsolationModeTotalDebtUpdated`: the pool's per-reserve isolation-debt
    /// state report.
    IsolationDebtReport,
    /// `MintedToTreasury`: the pool's treasury-mint report. The mint's own scaled
    /// log carries the `MintToTreasury` operation's delta.
    TreasuryMintReport,
    /// A user position flag (`ReserveUsedAsCollateralEnabled`/`Disabled`,
    /// `UserEModeSet`).
    PositionFlag,
    /// A market configuration event, owned by the config dispatch stage before
    /// any operation is parsed.
    MarketConfiguration,
    /// A rewards accounting event (`Staked`/`Redeem`/`RewardsClaimed`).
    RewardsAccounting,
    /// The pool's `FlashLoan` report. A flash loan moves no scaled balance, so the
    /// market's projection deliberately emits no operation for one.
    FlashLoan,
    /// A token allowance, credit-delegation, or governance-power delegation report
    /// (`Approval`, `BorrowAllowanceDelegated`, `DelegatedPowerChanged`,
    /// `DelegateChanged`). None moves a balance, so the market's projection
    /// deliberately reads none.
    AllowanceReport,
    /// The Pool's position-manager authorization report
    /// (`PositionManagerApproved`/`PositionManagerRevoked`). It authorizes a
    /// position manager for a user and moves no balance, so the market's
    /// projection deliberately reads none.
    AuthorizationReport,
    /// The pool's deficit-coverage report (`DeficitCovered`). The covered
    /// positions' balance reports carry the movement.
    DeficitCoverageReport,
    /// A scaled token's fee-distribution report (`FeesDistributedToTreasury`). The
    /// fee movement's own balance reports carry the delta.
    FeeDistribution,
}

impl NonOperationRole {
    /// The role's name, for a census or a refusal payload.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::MovementMirror { .. } => "movement mirror",
            Self::ScalingMirror { .. } => "scaling mirror",
            Self::Companion { .. } => "companion",
            Self::DuplicateCarrier { .. } => "duplicate carrier",
            Self::AccrualOnly => "accrual only",
            Self::ReserveReport => "reserve report",
            Self::IsolationDebtReport => "isolation debt report",
            Self::TreasuryMintReport => "treasury mint report",
            Self::PositionFlag => "position flag",
            Self::MarketConfiguration => "market configuration",
            Self::RewardsAccounting => "rewards accounting",
            Self::FlashLoan => "flash loan",
            Self::AllowanceReport => "allowance report",
            Self::AuthorizationReport => "authorization report",
            Self::DeficitCoverageReport => "deficit coverage report",
            Self::FeeDistribution => "fee distribution",
        }
    }

    /// Whether an operation that accounts for the movement may own this log.
    ///
    /// A mirror or companion report is the SAME movement another log carries, so
    /// an operation that consumes it (a standalone transfer operation owns its
    /// carrier AND its scaled report) owns the log; one that does not leaves the
    /// role in place. The terminal roles are settled facts about the receipt, and
    /// no operation may consume them.
    #[must_use]
    pub const fn claimable_by_operation(self) -> bool {
        matches!(
            self,
            Self::MovementMirror { .. }
                | Self::ScalingMirror { .. }
                | Self::Companion { .. }
                | Self::DuplicateCarrier { .. }
        )
    }
}

/// Why a log is deliberately outside this market's projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutOfScope {
    /// A token this market does not track: an underlying `Transfer` of a reserve,
    /// or another series' scaled report. The market's rows hold scaled balances,
    /// so the movement is not its business.
    UntrackedToken,
    /// A contract that is not this market's: another protocol's log in the same
    /// transaction.
    ForeignEmitter,
    /// A `ProxyCreated` whose `id` names no component this market tracks (the
    /// committed `035ce3da2` case: the incentives controller is neither a Pool nor
    /// a Pool Configurator role, so its deployment is deliberately ignored rather
    /// than refused).
    UntrackedProxyComponent,
}

impl OutOfScope {
    /// The reason's name, for a census or a refusal payload.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::UntrackedToken => "untracked token",
            Self::ForeignEmitter => "foreign emitter",
            Self::UntrackedProxyComponent => "untracked proxy component",
        }
    }
}

/// One receipt log's terminal owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogOwner {
    /// An operation consumed the log.
    Operation {
        /// The owning operation's id.
        operation_id: u32,
    },
    /// A recognized Aave role that deliberately emits no operation.
    Role(NonOperationRole),
    /// Deliberately irrelevant to this market's projection.
    OutOfScope(OutOfScope),
}

/// One log's place in the ledger while the transaction's operations are built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    /// An Aave operation is expected to own this log; none has yet.
    ExpectsOperation {
        /// What the receipt says the log is, for the refusal that names it.
        expectation: &'static str,
    },
    /// A recognized role. Whether an operation may claim it is the role's own
    /// answer ([`NonOperationRole::claimable_by_operation`]).
    Role(NonOperationRole),
    /// Deliberately irrelevant: no operation may consume it.
    OutOfScope(OutOfScope),
    /// An operation consumed it.
    Operation {
        /// The owning operation's id.
        operation_id: u32,
    },
}

/// The census of one transaction's receipt: every log counted exactly once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwnershipCensus {
    /// Logs an operation consumed.
    pub operation_owned: usize,
    /// Logs a recognized non-operation role owns.
    pub role_owned: usize,
    /// Logs deliberately outside the market's projection.
    pub out_of_scope: usize,
}

impl OwnershipCensus {
    /// Every log the census counts.
    #[must_use]
    pub const fn total(self) -> usize {
        self.operation_owned + self.role_owned + self.out_of_scope
    }
}

/// Why a transaction's receipt could not be owned.
///
/// Each variant is a different repair. Duplicate and non-operation claims mean
/// two owners describe one log (the parse's builders disagree about what the
/// movement is); an uncovered log means the parse ended with an Aave log nobody
/// accounted for; an unrecognized log means the decoder knows less than the chain
/// stated. None of them is retryable: the receipt is the evidence.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OwnershipRefusal {
    /// Two operations claim one log.
    #[error(
        "log {log_index} is owned by operation {first_operation} and operation {second_operation}"
    )]
    DuplicateOwnership {
        /// The log both operations claim.
        log_index: u64,
        /// The operation that claimed it first.
        first_operation: u32,
        /// The operation that claimed it second.
        second_operation: u32,
    },
    /// An operation claims a log the receipt already settled as a non-operation
    /// role.
    #[error("operation {operation} claims log {log_index}, which the receipt settles as {role}")]
    NonOperationClaim {
        /// The claimed log.
        log_index: u64,
        /// The role the receipt settled it as.
        role: &'static str,
        /// The claiming operation.
        operation: u32,
    },
    /// The parse ended with a log the receipt says an operation must own.
    #[error("log {log_index} expects an operation owner ({expected}) and none claimed it")]
    UncoveredOperationLog {
        /// The unowned log.
        log_index: u64,
        /// What the receipt says the log is.
        expected: &'static str,
    },
    /// A log emitted by this market's Pool or one of its tracked tokens that the
    /// decoder cannot name.
    #[error("log {log_index} from market contract {emitter} carries unrecognized topic {topic}")]
    UnrecognizedLog {
        /// The log.
        log_index: u64,
        /// The market contract that emitted it.
        emitter: Address,
        /// The topic the decoder could not place (empty when the log has no
        /// topic at all).
        topic: String,
    },
    /// A claim names a log this receipt does not carry.
    #[error("log {log_index} is not a log of this transaction's receipt")]
    LogOutsideReceipt {
        /// The claimed log.
        log_index: u64,
    },
}

/// One receipt log paired with the decoder's answer for it.
///
/// The ownership census is a function of the pair, so the log and its decode are
/// born together: no census can read one log's event against another log's index,
/// and no receipt can be classified against a decode list of a different length.
#[derive(Debug, Clone)]
pub(super) struct DecodedReceiptLog<'a> {
    log: &'a Log,
    decoded: Option<DecodedAaveEvent>,
}

impl<'a> DecodedReceiptLog<'a> {
    pub(super) fn new(log: &'a Log, decoded: Option<DecodedAaveEvent>) -> Self {
        Self { log, decoded }
    }

    /// The receipt log itself.
    pub(super) fn log(&self) -> &'a Log {
        self.log
    }

    /// The decoder's answer for THIS log, when the decoder named it.
    pub(super) fn decoded(&self) -> Option<&DecodedAaveEvent> {
        self.decoded.as_ref()
    }
}

/// One transaction's receipt, with every log classified before any operation
/// exists.
#[derive(Debug)]
pub struct ReceiptOwnership {
    slots: BTreeMap<u64, Slot>,
}

impl ReceiptOwnership {
    /// Classify every log of `receipt`.
    ///
    /// Each entry pairs one receipt log with the decoder's answer for that same
    /// log, in the receipt's order; `scaled` holds the market's decoded
    /// scaled-token events and `movements` their pairing; `context` is the
    /// market the receipt is read against (its Pool, its tracked emitter roles,
    /// and the revisions it reported).
    ///
    /// A log Aave's vocabulary names is an operation's, unless its own role says it
    /// carries no operation; a token event from an emitter this market does not
    /// track is `OutOfScope`; another contract's log is `OutOfScope`; and a log one
    /// of this market's contracts emits that the catalog cannot place — an
    /// unknown topic, or a known topic from an emitter role that does not state
    /// it — is a refusal.
    ///
    /// # Errors
    ///
    /// [`OwnershipRefusal::UnrecognizedLog`] when a market contract emits a log the
    /// catalog cannot place.
    pub(super) fn classify(
        receipt: &[DecodedReceiptLog<'_>],
        scaled: &[ScaledTokenEvent<'_>],
        movements: &EventOwnership,
        context: &ProtocolContext,
    ) -> Result<Self, OwnershipRefusal> {
        let mut slots = BTreeMap::new();
        for receipt_log in receipt {
            let log = receipt_log.log();
            let log_index = log_idx_value(log);
            let slot = match receipt_log.decoded() {
                Some(event) => aave_slot(event, log, log_index, scaled, movements, context)?,
                None => match context.classify_report(log) {
                    ReportAnswer::Role(role) => Slot::Role(role),
                    ReportAnswer::ForeignEmitter => Slot::OutOfScope(OutOfScope::ForeignEmitter),
                    ReportAnswer::Unrecognized => {
                        return Err(OwnershipRefusal::UnrecognizedLog {
                            log_index,
                            emitter: log.address(),
                            topic: log
                                .topics()
                                .first()
                                .map(|topic| alloy::hex::encode(topic.as_slice()))
                                .unwrap_or_default(),
                        })
                    }
                },
            };
            slots.insert(log_index, slot);
        }
        Ok(Self { slots })
    }

    /// The build-time slot of `log_index`.
    fn slot(&self, log_index: u64) -> Option<Slot> {
        self.slots.get(&log_index).copied()
    }

    /// `true` when the receipt settles `log_index` as a role no operation may
    /// consume.
    pub(super) fn is_terminal_role(&self, log_index: u64) -> bool {
        matches!(
            self.slot(log_index),
            Some(Slot::Role(role)) if !role.claimable_by_operation()
        )
    }

    /// Record that `operation` consumed the logs it claims.
    ///
    /// # Errors
    ///
    /// [`OwnershipRefusal::DuplicateOwnership`] when another operation already
    /// claimed the log, [`OwnershipRefusal::NonOperationClaim`] when the receipt
    /// settled it as a terminal role or as out of scope, and
    /// [`OwnershipRefusal::LogOutsideReceipt`] when the receipt does not carry it.
    pub(super) fn claim_operation(
        &mut self,
        operation: &Operation<'_>,
    ) -> Result<(), OwnershipRefusal> {
        for log_index in operation.claimed_log_indices() {
            self.claim(operation.operation_id, log_index)?;
        }
        Ok(())
    }

    fn claim(&mut self, operation_id: u32, log_index: u64) -> Result<(), OwnershipRefusal> {
        match self.slot(log_index) {
            None => Err(OwnershipRefusal::LogOutsideReceipt { log_index }),
            Some(Slot::Operation {
                operation_id: first,
            }) if first == operation_id => Ok(()),
            Some(Slot::Operation {
                operation_id: first,
            }) => Err(OwnershipRefusal::DuplicateOwnership {
                log_index,
                first_operation: first,
                second_operation: operation_id,
            }),
            Some(Slot::ExpectsOperation { .. }) => {
                self.slots
                    .insert(log_index, Slot::Operation { operation_id });
                Ok(())
            }
            Some(Slot::Role(role)) if role.claimable_by_operation() => {
                self.slots
                    .insert(log_index, Slot::Operation { operation_id });
                Ok(())
            }
            Some(Slot::Role(role)) => Err(OwnershipRefusal::NonOperationClaim {
                log_index,
                role: role.label(),
                operation: operation_id,
            }),
            Some(Slot::OutOfScope(reason)) => Err(OwnershipRefusal::NonOperationClaim {
                log_index,
                role: reason.label(),
                operation: operation_id,
            }),
        }
    }

    /// Close the ledger: every log must have a terminal owner.
    ///
    /// # Errors
    ///
    /// [`OwnershipRefusal::UncoveredOperationLog`] for a log the receipt says an
    /// operation must own and no operation claimed.
    pub(super) fn conclude(&mut self) -> Result<(), OwnershipRefusal> {
        for (log_index, slot) in &self.slots {
            if let Slot::ExpectsOperation { expectation } = slot {
                return Err(OwnershipRefusal::UncoveredOperationLog {
                    log_index: *log_index,
                    expected: expectation,
                });
            }
        }
        Ok(())
    }

    /// The owner of `log_index`, once the ledger is closed.
    #[must_use]
    pub fn owner(&self, log_index: u64) -> Option<LogOwner> {
        match self.slot(log_index)? {
            Slot::Operation { operation_id } => Some(LogOwner::Operation { operation_id }),
            Slot::Role(role) => Some(LogOwner::Role(role)),
            Slot::OutOfScope(reason) => Some(LogOwner::OutOfScope(reason)),
            Slot::ExpectsOperation { .. } => None,
        }
    }

    /// Every `(log_index, owner)` the ledger holds, in receipt order.
    pub fn owners(&self) -> impl Iterator<Item = (u64, LogOwner)> + '_ {
        self.slots
            .keys()
            .filter_map(|log_index| Some((*log_index, self.owner(*log_index)?)))
    }

    /// The census: every log counted exactly once, by owner class.
    #[must_use]
    pub fn census(&self) -> OwnershipCensus {
        let mut census = OwnershipCensus::default();
        for slot in self.slots.values() {
            match slot {
                Slot::Operation { .. } => census.operation_owned += 1,
                Slot::Role(_) => census.role_owned += 1,
                Slot::OutOfScope(_) => census.out_of_scope += 1,
                Slot::ExpectsOperation { .. } => {}
            }
        }
        census
    }

    /// How many logs the ledger holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the receipt carries no logs at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// The emitter of a decoded token event, for the tokens this market may or may not
/// track. `None` for every event that is not a token transfer report.
fn token_emitter(event: &DecodedAaveEvent) -> Option<Address> {
    match event {
        DecodedAaveEvent::ScaledTokenMint(ev) => Some(ev.token_address),
        DecodedAaveEvent::ScaledTokenBurn(ev) => Some(ev.token_address),
        DecodedAaveEvent::ScaledTokenBalanceTransfer(ev) => Some(ev.token_address),
        DecodedAaveEvent::Erc20Transfer(ev) => Some(ev.token_address),
        _ => None,
    }
}

/// One decoded Aave-vocabulary log's slot: an operation's, or the role its
/// statement carries.
///
/// The emitter is half the identity: a Pool-owned statement from another
/// contract, and a token report from a series this market does not track, are
/// both out of scope before their payload is read.
///
/// # Errors
///
/// [`OwnershipRefusal::UnrecognizedLog`] for a token event the movement pairing did
/// not classify — the ledger and the pairing read one decode, so a gap between them
/// is contradictory evidence.
fn aave_slot(
    event: &DecodedAaveEvent,
    log: &Log,
    log_index: u64,
    scaled: &[ScaledTokenEvent<'_>],
    movements: &EventOwnership,
    context: &ProtocolContext,
) -> Result<Slot, OwnershipRefusal> {
    if pool_owned_event(event) && !context.is_pool(log.address()) {
        return Ok(Slot::OutOfScope(OutOfScope::ForeignEmitter));
    }
    if let Some(token) = token_emitter(event) {
        if context.token_role(token).is_none() {
            return Ok(Slot::OutOfScope(OutOfScope::UntrackedToken));
        }
    }
    Ok(match event {
        DecodedAaveEvent::Supply(_)
        | DecodedAaveEvent::Borrow(_)
        | DecodedAaveEvent::Repay(_)
        | DecodedAaveEvent::Withdraw(_)
        | DecodedAaveEvent::LiquidationCall(_)
        | DecodedAaveEvent::DeficitCreated(_) => Slot::ExpectsOperation {
            expectation: "pool event anchor",
        },
        DecodedAaveEvent::MintedToTreasury(_) => Slot::Role(NonOperationRole::TreasuryMintReport),
        DecodedAaveEvent::ReserveDataUpdated(_) => Slot::Role(NonOperationRole::ReserveReport),
        DecodedAaveEvent::ReserveUsedAsCollateralEnabled(_)
        | DecodedAaveEvent::ReserveUsedAsCollateralDisabled(_)
        | DecodedAaveEvent::UserEModeSet(_) => Slot::Role(NonOperationRole::PositionFlag),
        DecodedAaveEvent::Staked(_)
        | DecodedAaveEvent::Redeem(_)
        | DecodedAaveEvent::RewardsClaimed(_) => Slot::Role(NonOperationRole::RewardsAccounting),
        DecodedAaveEvent::ProxyCreated(ev) => {
            if tracks_proxy_id(&ev.id) {
                Slot::Role(NonOperationRole::MarketConfiguration)
            } else {
                Slot::OutOfScope(OutOfScope::UntrackedProxyComponent)
            }
        }
        DecodedAaveEvent::CollateralConfigurationChanged(_)
        | DecodedAaveEvent::EModeCategoryAdded(_)
        | DecodedAaveEvent::EModeAssetCategoryChanged(_)
        | DecodedAaveEvent::AssetCollateralInEModeChanged(_)
        | DecodedAaveEvent::ReserveInitialized(_)
        | DecodedAaveEvent::PoolUpdated(_)
        | DecodedAaveEvent::PoolConfiguratorUpdated(_)
        | DecodedAaveEvent::PoolDataProviderUpdated(_)
        | DecodedAaveEvent::PriceOracleUpdated(_)
        | DecodedAaveEvent::AddressSet(_)
        | DecodedAaveEvent::Upgraded(_)
        | DecodedAaveEvent::DiscountPercentUpdated(_)
        | DecodedAaveEvent::DiscountRateStrategyUpdated(_)
        | DecodedAaveEvent::DiscountTokenUpdated(_)
        | DecodedAaveEvent::AssetSourceUpdated(_) => {
            Slot::Role(NonOperationRole::MarketConfiguration)
        }
        DecodedAaveEvent::ScaledTokenMint(_)
        | DecodedAaveEvent::ScaledTokenBurn(_)
        | DecodedAaveEvent::ScaledTokenBalanceTransfer(_)
        | DecodedAaveEvent::Erc20Transfer(_) => {
            let disposition =
                movements
                    .disposition(log_index)
                    .ok_or(OwnershipRefusal::UnrecognizedLog {
                        log_index,
                        emitter: token_emitter(event).unwrap_or(Address::ZERO),
                        topic: "unclassified token event".to_string(),
                    })?;
            movement_slot(disposition, log_index, scaled, context.pool())
        }
    })
}

/// The slot a scaled-token report's movement pairing settles.
fn movement_slot(
    disposition: EventDisposition,
    log_index: u64,
    scaled: &[ScaledTokenEvent<'_>],
    pool: Address,
) -> Slot {
    match disposition {
        EventDisposition::SemanticMint | EventDisposition::SemanticBurn => {
            let accrual_only = scaled
                .iter()
                .find(|event| event.log_index == log_index)
                .is_some_and(|event| {
                    event.event_type == ScaledTokenEventType::CollateralMint
                        && event.caller_address == Some(pool)
                        && event.amount == event.balance_increase.unwrap_or_default()
                });
            if accrual_only {
                Slot::Role(NonOperationRole::AccrualOnly)
            } else {
                Slot::ExpectsOperation {
                    expectation: "semantic mint/burn",
                }
            }
        }
        EventDisposition::StandaloneTransfer {
            scaled_partner: Some(scaled_report),
        } => Slot::Role(NonOperationRole::MovementMirror {
            scaled: scaled_report,
        }),
        EventDisposition::StandaloneTransfer {
            scaled_partner: None,
        } => Slot::ExpectsOperation {
            expectation: "movement report",
        },
        EventDisposition::BalanceTransfer {
            underpinned_by: Some(carrier),
        } => Slot::Role(NonOperationRole::ScalingMirror { carrier }),
        EventDisposition::BalanceTransfer {
            underpinned_by: None,
        } => Slot::ExpectsOperation {
            expectation: "scaled balance transfer",
        },
        EventDisposition::Companion { owner, side } => {
            Slot::Role(NonOperationRole::Companion { owner, side })
        }
        EventDisposition::DuplicateCarrier { owner } => {
            Slot::Role(NonOperationRole::DuplicateCarrier { owner })
        }
    }
}

/// Whether the event is the Pool contract's own report: the operation anchors,
/// the pool's state and position reports, and its treasury mint.
///
/// Another Aave deployment (a V2 `LendingPool`, another market's Pool) emits the same
/// signatures, so the emitter is half the identity: those logs are another
/// deployment's, never this market's.
fn pool_owned_event(event: &DecodedAaveEvent) -> bool {
    matches!(
        event,
        DecodedAaveEvent::Supply(_)
            | DecodedAaveEvent::Borrow(_)
            | DecodedAaveEvent::Repay(_)
            | DecodedAaveEvent::Withdraw(_)
            | DecodedAaveEvent::LiquidationCall(_)
            | DecodedAaveEvent::DeficitCreated(_)
            | DecodedAaveEvent::MintedToTreasury(_)
            | DecodedAaveEvent::ReserveDataUpdated(_)
            | DecodedAaveEvent::ReserveUsedAsCollateralEnabled(_)
            | DecodedAaveEvent::ReserveUsedAsCollateralDisabled(_)
            | DecodedAaveEvent::UserEModeSet(_)
    )
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::fixtures::captured_reports::{
        cases as captured_cases, context_of, entry_of, CapturedCase, RevisionGoverns,
    };
    use crate::operations::TokenType;
    use crate::updater::protocol_events::{
        catalog_entry, EmitterRole, ProtocolContext, ProtocolReport, ReportAnswer,
        ReportedRevisions, REPORT_CATALOG,
    };
    use crate::updater::revision::RevisionRange;
    use alloy::primitives::Log as AlloyLog;
    use alloy::primitives::{Bytes, B256};
    use degenbot_decoders::aave_report_decoder::decode_aave_report_log;
    use std::collections::BTreeMap;

    fn ledger(slots: &[(u64, Slot)]) -> ReceiptOwnership {
        ReceiptOwnership {
            slots: slots.iter().copied().collect::<BTreeMap<_, _>>(),
        }
    }

    /// The captured receipt as the census reads it: every log paired with the
    /// decoder's answer for that same log.
    fn undecoded<'a>(logs: &[&'a Log]) -> Vec<DecodedReceiptLog<'a>> {
        logs.iter()
            .map(|log| DecodedReceiptLog::new(log, None))
            .collect()
    }

    /// Two operations claiming one log is contradictory evidence about what that
    /// log is: the ledger refuses instead of letting the second claim win.
    #[test]
    fn a_second_operation_claiming_a_log_is_refused() {
        let mut ownership = ledger(&[(
            7,
            Slot::ExpectsOperation {
                expectation: "movement report",
            },
        )]);

        ownership.claim(0, 7).unwrap();

        assert_eq!(
            ownership.claim(3, 7).unwrap_err(),
            OwnershipRefusal::DuplicateOwnership {
                log_index: 7,
                first_operation: 0,
                second_operation: 3,
            }
        );
        assert_eq!(
            ownership.owner(7),
            Some(LogOwner::Operation { operation_id: 0 }),
            "the first claim keeps the log"
        );
    }

    /// A terminal role is a settled fact about the receipt: no operation may
    /// consume it.
    #[test]
    fn an_operation_cannot_claim_a_terminal_role() {
        let mut ownership = ledger(&[(9, Slot::Role(NonOperationRole::ReserveReport))]);

        assert_eq!(
            ownership.claim(1, 9).unwrap_err(),
            OwnershipRefusal::NonOperationClaim {
                log_index: 9,
                role: "reserve report",
                operation: 1,
            }
        );
    }

    /// A mirror role IS the same movement another log carries, so the operation
    /// that accounts for the movement may own it.
    #[test]
    fn an_operation_may_own_a_mirror_role() {
        let mut ownership = ledger(&[(
            9,
            Slot::Role(NonOperationRole::ScalingMirror { carrier: 8 }),
        )]);

        ownership.claim(2, 9).unwrap();

        assert_eq!(
            ownership.owner(9),
            Some(LogOwner::Operation { operation_id: 2 })
        );
    }

    /// A log outside the market's projection may never be consumed.
    #[test]
    fn an_operation_cannot_claim_an_out_of_scope_log() {
        let mut ownership = ledger(&[(4, Slot::OutOfScope(OutOfScope::ForeignEmitter))]);

        assert!(matches!(
            ownership.claim(0, 4).unwrap_err(),
            OwnershipRefusal::NonOperationClaim { .. }
        ));
    }

    /// Closing the ledger refuses a log the receipt says an operation owns and
    /// nobody claimed, and a claim for a log the receipt does not carry.
    #[test]
    fn closing_refuses_an_unowned_log() {
        let mut ownership = ledger(&[(
            5,
            Slot::ExpectsOperation {
                expectation: "movement report",
            },
        )]);

        assert_eq!(
            ownership.conclude().unwrap_err(),
            OwnershipRefusal::UncoveredOperationLog {
                log_index: 5,
                expected: "movement report",
            }
        );
        assert_eq!(
            ownership.claim(0, 6).unwrap_err(),
            OwnershipRefusal::LogOutsideReceipt { log_index: 6 }
        );
    }

    /// The census counts every slot exactly once, by owner class.
    #[test]
    fn the_census_counts_every_log_once() {
        let ownership = ledger(&[
            (1, Slot::Operation { operation_id: 0 }),
            (2, Slot::Role(NonOperationRole::ReserveReport)),
            (3, Slot::OutOfScope(OutOfScope::UntrackedToken)),
        ]);

        let census = ownership.census();

        assert_eq!(census.operation_owned, 1);
        assert_eq!(census.role_owned, 1);
        assert_eq!(census.out_of_scope, 1);
        assert_eq!(census.total(), 3);
        assert_eq!(ownership.len(), 3);
    }

    // ── the captured report-role cases (data-driven) ──────────────────────

    /// The capture corpus is fixed, not growing: the four recorded cases and
    /// their log distribution are named here, so a fixture edit that drops a
    /// case, duplicates one, or loses a log fails loudly instead of the per-log
    /// tests quietly covering less.
    #[test]
    fn the_capture_fixtures_carry_exactly_the_four_recorded_cases() {
        let cases = captured_cases();

        let ids: Vec<&str> = cases.iter().map(|case| case.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "rev1_gho_token_link_reports",
                "rev5_discount_token_index_and_delegation_reports",
                "rev9_pool_position_manager_approval",
                "rev11_pool_position_manager_revocation",
            ],
            "the fixture states exactly the four recorded captures"
        );
        let unique: std::collections::BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "the case ids are unique: {ids:?}");

        let logs: Vec<(&str, usize)> = cases
            .iter()
            .map(|case| (case.id.as_str(), case.logs.len()))
            .collect();
        assert_eq!(
            logs,
            [
                ("rev1_gho_token_link_reports", 4),
                ("rev5_discount_token_index_and_delegation_reports", 11),
                ("rev9_pool_position_manager_approval", 1),
                ("rev11_pool_position_manager_revocation", 1),
            ],
            "one log per captured report, distributed 4/11/1/1"
        );
        assert_eq!(
            cases.iter().map(|case| case.logs.len()).sum::<usize>(),
            17,
            "the capture corpus is 17 report logs"
        );
    }

    /// Every captured log classifies to the role the fixture records: the exact
    /// rev1/rev5/rev9/rev11 payloads keep their classifications through the
    /// catalog.
    #[test]
    fn every_captured_report_classifies_to_its_recorded_role() {
        for case in captured_cases() {
            let context = context_of(&case);
            let logs: Vec<&Log> = case.logs.iter().map(|entry| &entry.log).collect();
            let ownership = ReceiptOwnership::classify(
                &undecoded(&logs),
                &[],
                &EventOwnership::default(),
                &context,
            )
            .unwrap_or_else(|e| panic!("{}: the captured receipt is owned: {e}", case.id));
            for entry in &case.logs {
                let log_index = entry
                    .log
                    .log_index
                    .expect("every capture log states its index");
                assert_eq!(
                    ownership.owner(log_index),
                    Some(LogOwner::Role(entry.expected)),
                    "{}: log {log_index} from {}",
                    case.id,
                    entry.log.address()
                );
            }
            assert_eq!(
                ownership.census().total(),
                case.logs.len(),
                "{}: every captured log is owned exactly once",
                case.id
            );
        }
    }

    /// The catalog and the neutral decoder agree on every captured payload: the
    /// fixture's recorded report is what the decoder reads, and the catalog
    /// places that report for the emitter role the fixture records.
    #[test]
    fn every_captured_report_decodes_to_the_recorded_report() {
        for case in captured_cases() {
            for entry in &case.logs {
                let catalog = entry_of(&case, entry);
                assert_eq!(
                    ProtocolReport::of(
                        &decode_aave_report_log(&entry.log).expect("the log is a report")
                    )
                    .label(),
                    entry.report,
                    "{}: log {:?}",
                    case.id,
                    entry.log.log_index
                );
                assert_eq!(
                    catalog.role,
                    entry.expected,
                    "{}: {} from {}",
                    case.id,
                    entry.report,
                    entry.log.address()
                );
            }
        }
    }

    /// Every captured report sits inside the open-ended range of revisions that
    /// introduced it: the boundary is the capture's own reported revision, never
    /// a fabricated historical receipt.
    #[test]
    fn every_captured_report_is_inside_its_introduction_range() {
        for case in captured_cases() {
            for entry in &case.logs {
                let catalog = entry_of(&case, entry);
                assert!(
                    catalog.introduced.contains(case.reported_revision),
                    "{}: {} is introduced at revision {} and captured at {}",
                    case.id,
                    entry.report,
                    catalog.introduced.start(),
                    case.reported_revision
                );
            }
        }
    }

    /// Introduction is a boundary, not a set of revisions: a report introduced at
    /// a revision is recognized at that revision and on every later one, so a
    /// later compatible revision needs no second entry. The rev9 approval capture
    /// is the introduction boundary; the market's own reported revision 12 (the
    /// committed block-26458346 capture's) is a later one, and neither receipt is
    /// fabricated.
    #[test]
    fn a_report_is_recognized_at_its_introduction_and_on_every_later_revision() {
        let approval = catalog_entry(ProtocolReport::PositionManagerApproved, EmitterRole::Pool)
            .expect("the Pool states its approval");
        assert_eq!(approval.introduced, RevisionRange::from(9));
        assert!(!approval.introduced.contains(8), "before the introduction");
        assert!(approval.introduced.contains(9), "at the introduction");
        assert!(approval.introduced.contains(12), "at a later revision");
        assert!(approval.introduced.contains(u32::MAX), "open-ended");
        assert_eq!(
            NonOperationRole::AuthorizationReport,
            approval.role,
            "the Pool's position-manager report is an authorization report"
        );
        assert_eq!(
            catalog_entry(ProtocolReport::PositionManagerRevoked, EmitterRole::Pool)
                .expect("the Pool states its revocation")
                .introduced,
            RevisionRange::from(11)
        );

        let cases = captured_cases();
        let pool_governed: Vec<&CapturedCase> = cases
            .iter()
            .filter(|case| case.revision_governs == RevisionGoverns::Pool)
            .collect();
        assert_eq!(pool_governed.len(), 2, "the two Pool revision captures");
        for case in pool_governed {
            let mut tokens: BTreeMap<Address, TokenType> = BTreeMap::new();
            for entry in &case.logs {
                if let Some(role) = entry.role {
                    tokens.insert(entry.log.address(), role);
                }
            }
            let later = ProtocolContext::new(
                case.pool,
                tokens,
                ReportedRevisions {
                    pool: Some(12),
                    ..ReportedRevisions::default()
                },
            );
            for entry in &case.logs {
                assert_eq!(
                    later.classify_report(&entry.log),
                    ReportAnswer::Role(entry.expected),
                    "{}: the same statement at the market's later reported revision",
                    case.id
                );
            }
        }
    }

    /// A Pool report re-emitted by a tracked token is not the Pool's statement:
    /// the catalog refuses the emitter role instead of handing the token the
    /// Pool's role, and the ledger refuses the receipt.
    #[test]
    fn a_pool_report_from_a_tracked_token_is_refused() {
        let case = captured_cases()
            .into_iter()
            .find(|case| case.id == "rev9_pool_position_manager_approval")
            .expect("the approval capture");
        let entry = case.logs.first().expect("the capture carries its log");
        let a_token = Address::repeat_byte(0xA1);
        let context = ProtocolContext::new(
            case.pool,
            BTreeMap::from([(a_token, TokenType::Atoken)]),
            context_of(&case).revisions(),
        );
        let moved = Log {
            inner: AlloyLog::new_unchecked(
                a_token,
                entry.log.topics().to_vec(),
                entry.log.data().data.clone(),
            ),
            ..entry.log.clone()
        };

        assert_eq!(context.classify_report(&moved), ReportAnswer::Unrecognized);
        assert_eq!(
            ReceiptOwnership::classify(
                &undecoded(&[&moved]),
                &[],
                &EventOwnership::default(),
                &context
            )
            .unwrap_err(),
            OwnershipRefusal::UnrecognizedLog {
                log_index: entry.log.log_index.expect("the capture states its index"),
                emitter: a_token,
                topic: alloy::hex::encode(entry.log.topics()[0].as_slice()),
            },
            "a market contract's unplaceable log is an ingress error, not out of scope"
        );
    }

    /// A GHO discount-token report from a tracked vToken is the wrong token
    /// role: the discount series states it, and the catalog refuses it
    /// elsewhere.
    #[test]
    fn a_discount_token_report_from_a_tracked_vtoken_is_refused() {
        let case = captured_cases()
            .into_iter()
            .find(|case| case.id == "rev1_gho_token_link_reports")
            .expect("the GHO link capture");
        let entry = case
            .logs
            .iter()
            .find(|entry| entry.report == "GhoDebtTokenChanged")
            .expect("the capture carries the discount-token report");
        let v_token = Address::repeat_byte(0xB2);
        let context = ProtocolContext::new(
            case.pool,
            BTreeMap::from([(v_token, TokenType::Vtoken)]),
            ReportedRevisions::default(),
        );
        let moved = Log {
            inner: AlloyLog::new_unchecked(
                v_token,
                entry.log.topics().to_vec(),
                entry.log.data().data.clone(),
            ),
            ..entry.log.clone()
        };

        assert_eq!(context.classify_report(&moved), ReportAnswer::Unrecognized);
        assert_eq!(
            ReceiptOwnership::classify(
                &undecoded(&[&moved]),
                &[],
                &EventOwnership::default(),
                &context
            )
            .unwrap_err(),
            OwnershipRefusal::UnrecognizedLog {
                log_index: entry.log.log_index.expect("the capture states its index"),
                emitter: v_token,
                topic: alloy::hex::encode(entry.log.topics()[0].as_slice()),
            }
        );
    }

    /// An unknown topic from a tracked token is an ingress error, exactly as it
    /// is from the Pool: the market's model expects coverage, never a leftover
    /// pile.
    #[test]
    fn an_unknown_topic_from_a_tracked_token_refuses_the_transaction() {
        let a_token = Address::repeat_byte(0xA1);
        let context = ProtocolContext::new(
            Address::repeat_byte(0x99),
            BTreeMap::from([(a_token, TokenType::Atoken)]),
            ReportedRevisions::default(),
        );
        let log = Log {
            inner: AlloyLog::new_unchecked(a_token, vec![B256::repeat_byte(0x5A)], Bytes::new()),
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: Some(3),
            removed: false,
        };

        assert_eq!(context.classify_report(&log), ReportAnswer::Unrecognized);
        assert_eq!(
            ReceiptOwnership::classify(
                &undecoded(&[&log]),
                &[],
                &EventOwnership::default(),
                &context
            )
            .unwrap_err(),
            OwnershipRefusal::UnrecognizedLog {
                log_index: 3,
                emitter: a_token,
                topic: alloy::hex::encode([0x5A_u8; 32]),
            }
        );
    }

    /// The catalog states each report once, every entry carries a role no
    /// operation may consume, and its introduction range is open-ended.
    #[test]
    fn the_catalog_states_every_report_once() {
        for (index, entry) in REPORT_CATALOG.iter().enumerate() {
            assert!(
                !REPORT_CATALOG[..index]
                    .iter()
                    .any(|earlier| earlier.report == entry.report),
                "{} is stated once",
                entry.report.label()
            );
            assert!(
                !entry.role.claimable_by_operation(),
                "{} carries a terminal role",
                entry.report.label()
            );
            assert!(entry.introduced.start() >= 1);
        }
        assert_eq!(
            REPORT_CATALOG.len(),
            25,
            "the neutral decoder's 25 reports, none dropped and none invented"
        );
    }
}

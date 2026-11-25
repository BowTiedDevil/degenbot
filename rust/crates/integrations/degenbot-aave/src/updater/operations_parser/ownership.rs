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
//! that the decoder cannot name is an ingress error — refused, not swept into
//! the ignored pile — while another protocol's log in the same transaction is
//! genuinely this market's business to ignore.
//!
//! The ledger is built ONCE per transaction, before any operation exists, and
//! the parse claims into it afterwards: an operation owns the logs it consumes.
//! A second operation claiming the same log, an operation claiming a role the
//! receipt already settled, and a log still expecting an owner when the parse
//! ends are all refusals. That is what makes the census total instead of a
//! leftover list nobody reads.

use super::util::{CompanionSide, EventDisposition, EventOwnership};
use super::{
    log_idx_value, Address, Log, Operation, ScaledTokenEvent, ScaledTokenEventType, TokenType,
};
use crate::updater::config_dispatch::tracks_proxy_id;
use alloy::primitives::B256;
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
    /// A token allowance, credit-delegation, or governance-power delegation report.
    /// None moves a balance, so the market's projection deliberately reads none.
    AllowanceReport,
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
    /// scaled-token events and `movements` their pairing; `tokens` names the
    /// emitters this market tracks and `pool` is the market's Pool contract.
    ///
    /// A log Aave's vocabulary names is an operation's, unless its own role says it
    /// carries no operation; a token event from an emitter this market does not
    /// track is `OutOfScope`; another contract's log is `OutOfScope`; and a log one
    /// of this market's contracts emits that the decoder cannot name is a refusal.
    ///
    /// # Errors
    ///
    /// [`OwnershipRefusal::UnrecognizedLog`] when a market contract emits a log the
    /// decoder cannot name.
    pub(super) fn classify(
        receipt: &[DecodedReceiptLog<'_>],
        scaled: &[ScaledTokenEvent<'_>],
        movements: &EventOwnership,
        tokens: &BTreeMap<Address, TokenType>,
        pool: Address,
    ) -> Result<Self, OwnershipRefusal> {
        let mut slots = BTreeMap::new();
        for receipt_log in receipt {
            let log = receipt_log.log();
            let log_index = log_idx_value(log);
            let emitter = log.address();
            let slot = match receipt_log.decoded() {
                Some(event) => {
                    if let Some(token) = token_emitter(event) {
                        if !tokens.contains_key(&token) {
                            slots.insert(log_index, Slot::OutOfScope(OutOfScope::UntrackedToken));
                            continue;
                        }
                    } else if pool_owned_event(event) && emitter != pool {
                        slots.insert(log_index, Slot::OutOfScope(OutOfScope::ForeignEmitter));
                        continue;
                    }
                    aave_slot(event, log_index, scaled, movements, pool)?
                }
                None => {
                    if emitter == pool || tokens.contains_key(&emitter) {
                        unmodeled_role_emitted_by(log, emitter, pool, tokens).ok_or_else(|| {
                            OwnershipRefusal::UnrecognizedLog {
                                log_index,
                                emitter,
                                topic: log
                                    .topics()
                                    .first()
                                    .map(|topic| alloy::hex::encode(topic.as_slice()))
                                    .unwrap_or_default(),
                            }
                        })?
                    } else {
                        Slot::OutOfScope(OutOfScope::ForeignEmitter)
                    }
                }
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

/// One Aave-vocabulary log's slot: an operation's, or the one role that
/// deliberately carries none.
///
/// # Errors
///
/// [`OwnershipRefusal::UnrecognizedLog`] for a token event the movement pairing did
/// not classify — the ledger and the pairing read one decode, so a gap between them
/// is contradictory evidence.
fn aave_slot(
    event: &DecodedAaveEvent,
    log_index: u64,
    scaled: &[ScaledTokenEvent<'_>],
    movements: &EventOwnership,
    pool: Address,
) -> Result<Slot, OwnershipRefusal> {
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
            movement_slot(disposition, log_index, scaled, pool)
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

/// `keccak256("FlashLoan(address,address,address,uint256,uint8,uint256,uint16)")`.
const AAVE_FLASH_LOAN_TOPIC: B256 =
    alloy::primitives::b256!("0xefefaba5e921573100900a3ad9cf29f222d995fb3b6045797eaea7521bd8d6f0");

/// `keccak256("IsolationModeTotalDebtUpdated(address,uint256)")` — the pool's
/// isolation-debt state report.
const AAVE_ISOLATION_MODE_TOTAL_DEBT_TOPIC: B256 =
    alloy::primitives::b256!("0xaef84d3b40895fd58c561f3998000f0583abb992a52fbdc99ace8e8de4d676a5");

/// `keccak256("DeficitCovered(address,address,uint256)")` — the pool's report that a
/// deficit was covered. The covered positions' own balance reports carry the
/// movement, so the pool's report is a state role.
const AAVE_DEFICIT_COVERED_TOPIC: B256 =
    alloy::primitives::b256!("0x84b203e49f1a4b553088061534231969a68ad1c81be192205e96d23a206cb26a");

/// `keccak256("PositionManagerApproved(address,address)")` — the Pool revision 9
/// position-manager authorization report.
const AAVE_POSITION_MANAGER_APPROVED_TOPIC: B256 =
    alloy::primitives::b256!("0x540e692f36c2fa13e7583c4deeffd91ce6bc04f91e7d84f295d9d858372875fc");

/// `keccak256("PositionManagerRevoked(address,address)")` — the Pool revision 11
/// position-manager authorization report.
const AAVE_POSITION_MANAGER_REVOKED_TOPIC: B256 =
    alloy::primitives::b256!("0x08c92c3870d10c79e9673fecea8f4ff261f8e6b661067d9ca63fd777882bff15");

/// `keccak256("FeesDistributedToTreasury(address,address,uint256)")` — a scaled token's
/// fee-distribution report.
const SCALED_TOKEN_FEE_DISTRIBUTION_TOPIC: B256 =
    alloy::primitives::b256!("0xb29fcda740927812f5a71075b62e132bead3769a455319c29b9a1cc461a65475");

/// `keccak256("BorrowAllowanceDelegated(address,address,address,uint256)")` — a debt
/// token's credit-delegation report.
const DEBT_BORROW_ALLOWANCE_TOPIC: B256 =
    alloy::primitives::b256!("0xda919360433220e13b51e8c211e490d148e61a3bd53de8c097194e458b97f3e1");

/// `keccak256("Approval(address,address,uint256)")`.
const ERC20_APPROVAL_TOPIC: B256 =
    alloy::primitives::b256!("0x8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925");

/// `keccak256("Initialized(address,address,address,address,uint8,string,string,bytes)")` — a
/// scaled token's own initialization report (implementation upgrade), emitted with
/// the reserve's underlying and the Pool as indexed fields.
const SCALED_TOKEN_INITIALIZED_TOPIC: B256 =
    alloy::primitives::b256!("0xb19e051f8af41150ccccb3fc2c2d8d15f4a4cf434f32a559ba75fe73d6eea20b");

/// `keccak256("Initialized(address,address,address,uint8,string,string,bytes)")` — the
/// debt token's variant of the same initialization report.
const SCALED_TOKEN_INITIALIZED_DEBT_TOPIC: B256 =
    alloy::primitives::b256!("0x40251fbfb6656cfa65a00d7879029fec1fad21d28fdcff2f4f68f52795b74f2c");

/// `keccak256("VariableDebtTokenSet(address)")` — the GHO aToken's link report.
const GHO_VARIABLE_DEBT_TOKEN_SET_TOPIC: B256 =
    alloy::primitives::b256!("0x9a2092516d2c53586ed59ca6d2878442977a16f16e1c85ac0a8a8cd9cb024666");

/// `keccak256("ATokenSet(address)")` — the GHO vToken's reciprocal link report.
const GHO_ATOKEN_SET_TOPIC: B256 =
    alloy::primitives::b256!("0x07cba78d3a9bb0cf2cb0e30140558e278b50a2ba6da2e13369a0d9113034548b");

/// `keccak256("GhoTreasuryUpdated(address,address)")` — the GHO aToken's treasury report.
const GHO_TREASURY_UPDATED_TOPIC: B256 =
    alloy::primitives::b256!("0x13f4413d8d93a259bd6c10f35095371f30ed50f81a73407e52e9f02000d5d16b");

/// `keccak256("GHODebtTokenChanged(address)")` — the GHO discount token's debt-token report.
const GHO_DEBT_TOKEN_CHANGED_TOPIC: B256 =
    alloy::primitives::b256!("0x62bbced765bccc9ed93d0bae1b2a8c86e13b4663f8bf27d5936d9d619194c8a9");

/// `keccak256("AssetIndexUpdated(address,uint256)")` — the GHO discount token's rewards index.
const GHO_ASSET_INDEX_UPDATED_TOPIC: B256 =
    alloy::primitives::b256!("0x5777ca300dfe5bead41006fbce4389794dbc0ed8d6cccebfaf94630aa04184bc");

/// `keccak256("UserIndexUpdated(address,address,uint256)")` — per-user rewards accounting.
const GHO_USER_INDEX_UPDATED_TOPIC: B256 =
    alloy::primitives::b256!("0xbb123b5c06d5408bbea3c4fef481578175cfb432e3b482c6186f02ed9086585b");

/// `keccak256("DelegatedPowerChanged(address,uint256,uint8)")` — governance-power delegation.
const GHO_DELEGATED_POWER_CHANGED_TOPIC: B256 =
    alloy::primitives::b256!("0xa0a19463ee116110c9b282012d9b65cc5522dc38a9520340cbaf3142e550127f");

/// `keccak256("RewardsAccrued(address,uint256)")` — accrued rewards accounting.
const GHO_REWARDS_ACCRUED_TOPIC: B256 =
    alloy::primitives::b256!("0x2468f9268c60ad90e2d49edb0032c8a001e733ae888b3ab8e982edf535be1a76");

/// `keccak256("RewardsClaimed(address,address,uint256)")` — direct stkAAVE rewards claims.
const GHO_REWARDS_CLAIMED_TOPIC: B256 =
    alloy::primitives::b256!("0x9310ccfcb8de723f578a9e4282ea9f521f05ae40dc08f3068dfad528a65ee3c7");

/// `keccak256("DelegateChanged(address,address,uint8)")` — delegation-target accounting.
const GHO_DELEGATE_CHANGED_TOPIC: B256 =
    alloy::primitives::b256!("0xe8d51c8e11bd570db1734c8ec775785330e77007feed45c43b608ef33ff914bd");

/// `keccak256("PendingAdminChanged(address,uint256)")` — pending-admin configuration.
const GHO_PENDING_ADMIN_CHANGED_TOPIC: B256 =
    alloy::primitives::b256!("0xc3d1bf52795a20771ee6de3e59948fd83e10db956122d4709a3b86c512963855");

/// `keccak256("RoleClaimed(address,uint256)")` — admin-role claim configuration.
const GHO_ROLE_CLAIMED_TOPIC: B256 =
    alloy::primitives::b256!("0x83a9ddad961dcb7c6894c9585a16ff7792c2ec8281256a3cc7303ae830152dcf");

/// `keccak256("Cooldown(address,uint256)")` — staking cooldown accounting.
const GHO_COOLDOWN_TOPIC: B256 =
    alloy::primitives::b256!("0x8a05f911d8ab7fc50fec37ef4ba7f9bfcb1a3c191c81dcd824ad0946c4e20d65");

/// `keccak256("AssetConfigUpdated(address,uint256)")` — asset configuration.
const GHO_ASSET_CONFIG_UPDATED_TOPIC: B256 =
    alloy::primitives::b256!("0x87fa03892a0556cb6b8f97e6d533a150d4d55fcbf275fff5fa003fa636bcc7fa");

/// `keccak256("MaxSlashablePercentageChanged(uint256)")` — slashing configuration.
const GHO_MAX_SLASHABLE_PERCENTAGE_CHANGED_TOPIC: B256 =
    alloy::primitives::b256!("0xb79c2d6c7f5c95eec9dc05affc0ed002620a4ec6d72b7b0744cc8638168c600a");

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

/// The role of a log one of this market's contracts emits that the decoder
/// deliberately does not name, or `None` when the topic is genuinely unknown.
///
/// Aave's Pool reports flash loans and its scaled tokens report allowances; the
/// market's projection reads scaled balances, and neither moves one. Those two
/// are recognized vocabulary, so they are roles — every other topic from a market
/// contract is an ingress error.
fn unmodeled_role_emitted_by(
    log: &Log,
    emitter: Address,
    pool: Address,
    tokens: &BTreeMap<Address, TokenType>,
) -> Option<Slot> {
    let topic = log.topics().first()?;
    if emitter == pool && *topic == AAVE_FLASH_LOAN_TOPIC {
        return Some(Slot::Role(NonOperationRole::FlashLoan));
    }
    if emitter == pool && *topic == AAVE_DEFICIT_COVERED_TOPIC {
        return Some(Slot::Role(NonOperationRole::DeficitCoverageReport));
    }
    if emitter == pool
        && (*topic == AAVE_POSITION_MANAGER_APPROVED_TOPIC
            || *topic == AAVE_POSITION_MANAGER_REVOKED_TOPIC)
    {
        return Some(Slot::Role(NonOperationRole::AllowanceReport));
    }
    if emitter == pool && *topic == AAVE_ISOLATION_MODE_TOTAL_DEBT_TOPIC {
        return Some(Slot::Role(NonOperationRole::IsolationDebtReport));
    }
    if tokens.contains_key(&emitter)
        && (*topic == SCALED_TOKEN_INITIALIZED_TOPIC
            || *topic == SCALED_TOKEN_INITIALIZED_DEBT_TOPIC
            || *topic == GHO_VARIABLE_DEBT_TOKEN_SET_TOPIC
            || *topic == GHO_ATOKEN_SET_TOPIC
            || *topic == GHO_TREASURY_UPDATED_TOPIC
            || *topic == GHO_DEBT_TOKEN_CHANGED_TOPIC
            || *topic == GHO_PENDING_ADMIN_CHANGED_TOPIC
            || *topic == GHO_ROLE_CLAIMED_TOPIC
            || *topic == GHO_ASSET_CONFIG_UPDATED_TOPIC
            || *topic == GHO_MAX_SLASHABLE_PERCENTAGE_CHANGED_TOPIC)
    {
        return Some(Slot::Role(NonOperationRole::MarketConfiguration));
    }
    if tokens.contains_key(&emitter)
        && (*topic == GHO_ASSET_INDEX_UPDATED_TOPIC
            || *topic == GHO_USER_INDEX_UPDATED_TOPIC
            || *topic == GHO_REWARDS_ACCRUED_TOPIC
            || *topic == GHO_REWARDS_CLAIMED_TOPIC
            || *topic == GHO_COOLDOWN_TOPIC)
    {
        return Some(Slot::Role(NonOperationRole::RewardsAccounting));
    }
    if tokens.contains_key(&emitter)
        && (*topic == ERC20_APPROVAL_TOPIC
            || *topic == DEBT_BORROW_ALLOWANCE_TOPIC
            || *topic == GHO_DELEGATED_POWER_CHANGED_TOPIC
            || *topic == GHO_DELEGATE_CHANGED_TOPIC)
    {
        return Some(Slot::Role(NonOperationRole::AllowanceReport));
    }
    if tokens.contains_key(&emitter) && *topic == SCALED_TOKEN_FEE_DISTRIBUTION_TOPIC {
        return Some(Slot::Role(NonOperationRole::FeeDistribution));
    }
    None
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
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

    #[test]
    #[expect(clippy::too_many_lines)]
    fn captured_rev1_gho_token_link_reports_are_market_configuration() {
        use alloy::primitives::{Bytes, Log as AlloyLog};

        let collateral_token: Address = "0x00907f9921424583e7ffBfEdf84F92B7B2Be4977"
            .parse()
            .unwrap();
        let debt_token: Address = "0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B"
            .parse()
            .unwrap();
        let discount_strategy: Address = "0x4da27a545c0c5B758a6BA100e3a049001de870f5"
            .parse()
            .unwrap();
        let transaction: B256 =
            "0xae8e542d4fdb5a6a33eeb129bb80f9bf23a1ceb3ef5f6caed1fd634ae3730c0b"
                .parse()
                .unwrap();
        let make_log = |emitter: Address, topics: &[B256], index: u64| Log {
            inner: AlloyLog::new_unchecked(emitter, topics.to_vec(), Bytes::new()),
            block_hash: Some(
                "0xd6af9d366c145bbdebd34ce7fc1fa7bc52dc81c2d4f5de3881c11210d343ab58"
                    .parse()
                    .unwrap(),
            ),
            block_number: Some(17_699_249),
            block_timestamp: None,
            transaction_hash: Some(transaction),
            transaction_index: Some(13),
            log_index: Some(index),
            removed: false,
        };
        let logs = [
            make_log(
                collateral_token,
                &[
                    alloy::primitives::b256!(
                        "0x9a2092516d2c53586ed59ca6d2878442977a16f16e1c85ac0a8a8cd9cb024666"
                    ),
                    debt_token.into_word(),
                ],
                87,
            ),
            make_log(
                collateral_token,
                &[
                    alloy::primitives::b256!(
                        "0x13f4413d8d93a259bd6c10f35095371f30ed50f81a73407e52e9f02000d5d16b"
                    ),
                    B256::ZERO,
                    alloy::primitives::b256!(
                        "0x000000000000000000000000464c71f6c2f760dda6093dcb91c24c39e5d6e18c"
                    ),
                ],
                88,
            ),
            make_log(
                debt_token,
                &[
                    alloy::primitives::b256!(
                        "0x07cba78d3a9bb0cf2cb0e30140558e278b50a2ba6da2e13369a0d9113034548b"
                    ),
                    collateral_token.into_word(),
                ],
                89,
            ),
            make_log(
                discount_strategy,
                &[
                    alloy::primitives::b256!(
                        "0x62bbced765bccc9ed93d0bae1b2a8c86e13b4663f8bf27d5936d9d619194c8a9"
                    ),
                    debt_token.into_word(),
                ],
                92,
            ),
        ];
        let tokens = BTreeMap::from([
            (collateral_token, TokenType::Atoken),
            (debt_token, TokenType::Vtoken),
            (discount_strategy, TokenType::GhoDiscount),
        ]);
        let receipt = undecoded(&logs.iter().collect::<Vec<_>>());
        let ownership = ReceiptOwnership::classify(
            &receipt,
            &[],
            &EventOwnership::default(),
            &tokens,
            Address::ZERO,
        )
        .unwrap();

        assert_eq!(
            ownership.owner(87),
            Some(LogOwner::Role(NonOperationRole::MarketConfiguration))
        );
        assert_eq!(
            ownership.owner(88),
            Some(LogOwner::Role(NonOperationRole::MarketConfiguration))
        );
        assert_eq!(
            ownership.owner(89),
            Some(LogOwner::Role(NonOperationRole::MarketConfiguration))
        );
        assert_eq!(
            ownership.owner(92),
            Some(LogOwner::Role(NonOperationRole::MarketConfiguration))
        );
    }

    #[test]
    #[expect(clippy::too_many_lines)]
    fn captured_rev5_discount_token_index_and_delegation_reports_are_classified() {
        use alloy::primitives::{Bytes, Log as AlloyLog};

        let discount_token: Address = "0x4da27a545c0c5B758a6BA100e3a049001de870f5"
            .parse()
            .unwrap();
        let log = Log {
            inner: AlloyLog::new_unchecked(
                discount_token,
                vec![
                    alloy::primitives::b256!(
                        "0x5777ca300dfe5bead41006fbce4389794dbc0ed8d6cccebfaf94630aa04184bc"
                    ),
                    discount_token.into_word(),
                ],
                Bytes::from(
                    alloy::hex::decode(
                        "000000000000000000000000000000000000000000000001696a636d44852dcc",
                    )
                    .unwrap(),
                ),
            ),
            block_hash: Some(
                "0x02baf4aeaeba75e020a93706c6c8add928d3040e2bd1685355772d2dc9c6b634"
                    .parse()
                    .unwrap(),
            ),
            block_number: Some(17_699_277),
            block_timestamp: None,
            transaction_hash: Some(
                "0x7ef07bae613c423a7c11ab1be6e4899db837312a6fef47522fc0af260f2a6fe3"
                    .parse()
                    .unwrap(),
            ),
            transaction_index: Some(76),
            log_index: Some(321),
            removed: false,
        };
        let user_log = Log {
            inner: AlloyLog::new_unchecked(
                discount_token,
                vec![
                    alloy::primitives::b256!(
                        "0xbb123b5c06d5408bbea3c4fef481578175cfb432e3b482c6186f02ed9086585b"
                    ),
                    alloy::primitives::b256!(
                        "0x000000000000000000000000ca1f6d7d8e902617f8bdd87866e00f9844c40a77"
                    ),
                    discount_token.into_word(),
                ],
                Bytes::from(
                    alloy::hex::decode(
                        "000000000000000000000000000000000000000000000001696a636d44852dcc",
                    )
                    .unwrap(),
                ),
            ),
            log_index: Some(322),
            ..log.clone()
        };
        let delegation_log = Log {
            inner: AlloyLog::new_unchecked(
                discount_token,
                vec![
                    alloy::primitives::b256!(
                        "0xa0a19463ee116110c9b282012d9b65cc5522dc38a9520340cbaf3142e550127f"
                    ),
                    alloy::primitives::b256!(
                        "0x000000000000000000000000ca1f6d7d8e902617f8bdd87866e00f9844c40a77"
                    ),
                ],
                Bytes::from(
                    alloy::hex::decode(
                        "0000000000000000000000000000000000000000000000056bc75e2d631000000000000000000000000000000000000000000000000000000000000000000000",
                    )
                    .unwrap(),
                ),
            ),
            log_index: Some(329),
            ..log.clone()
        };
        let rewards_log = Log {
            inner: AlloyLog::new_unchecked(
                discount_token,
                vec![alloy::primitives::b256!(
                    "0x2468f9268c60ad90e2d49edb0032c8a001e733ae888b3ab8e982edf535be1a76"
                )],
                Bytes::from(
                    alloy::hex::decode(
                        "0000000000000000000000006b62909ad3f315833a5e5062b670034a3016910b0000000000000000000000000000000000000000000000006bc387c95b2fc39d",
                    )
                    .unwrap(),
                ),
            ),
            block_hash: Some(
                "0xe99f4e137b9dc3caf06ecb2c571ce489130c0799373e73f37a0fffe0ba829ce8"
                    .parse()
                    .unwrap(),
            ),
            block_number: Some(17_699_305),
            transaction_hash: Some(
                "0x411a900563fd4bf5091a56651b22a0d621f0bd5d0319d05948a1510a6df12b1b"
                    .parse()
                    .unwrap(),
            ),
            transaction_index: Some(127),
            log_index: Some(325),
            ..log.clone()
        };
        let direct_claim_log = Log {
            inner: AlloyLog::new_unchecked(
                discount_token,
                vec![
                    alloy::primitives::b256!(
                        "0x9310ccfcb8de723f578a9e4282ea9f521f05ae40dc08f3068dfad528a65ee3c7"
                    ),
                    alloy::primitives::b256!(
                        "0x0000000000000000000000009f07a0233abc178428aa9a18baef3188f093a7d0"
                    ),
                    discount_token.into_word(),
                ],
                Bytes::from(
                    alloy::hex::decode(
                        "0000000000000000000000000000000000000000000000004a868b7352b2fa66",
                    )
                    .unwrap(),
                ),
            ),
            block_hash: Some(
                "0x6fa39364eb137b759563c2e2d6430066751f8dabe7a647b23f09ef4d29ad6b0a"
                    .parse()
                    .unwrap(),
            ),
            block_number: Some(17_699_689),
            transaction_hash: Some(
                "0x50cdffe9a2ae653c46b8e057bd665d26d365c50558c96db401dd21514dc4d598"
                    .parse()
                    .unwrap(),
            ),
            transaction_index: Some(132),
            log_index: Some(357),
            ..log.clone()
        };
        let delegate_change_log = Log {
            inner: AlloyLog::new_unchecked(
                discount_token,
                vec![
                    alloy::primitives::b256!(
                        "0xe8d51c8e11bd570db1734c8ec775785330e77007feed45c43b608ef33ff914bd"
                    ),
                    alloy::primitives::b256!(
                        "0x000000000000000000000000167c606be99dbf5a8af61e1983e5b309e8fa2ae7"
                    ),
                    alloy::primitives::b256!(
                        "0x0000000000000000000000001ae6dcbc88d6f81a7bcfccc7198397d776f3592e"
                    ),
                ],
                Bytes::from([0_u8; 32].as_slice()),
            ),
            block_hash: Some(
                "0xb1f119ad9186d6b0447132789f283a76ec4be9bc78cd15303d7f5075c292c8e1"
                    .parse()
                    .unwrap(),
            ),
            block_number: Some(17_824_467),
            transaction_hash: Some(
                "0x92acbe5d2f09da6032f481ac4610825e9af7a423bf26f77988b65f8c83f3b0e3"
                    .parse()
                    .unwrap(),
            ),
            transaction_index: Some(82),
            log_index: Some(292),
            ..log.clone()
        };
        let pending_admin_log = Log {
            inner: AlloyLog::new_unchecked(
                discount_token,
                vec![
                    alloy::primitives::b256!(
                        "0xc3d1bf52795a20771ee6de3e59948fd83e10db956122d4709a3b86c512963855"
                    ),
                    alloy::primitives::b256!(
                        "0x0000000000000000000000005300a1a15135ea4dc7ad5a167152c01efc9b192a"
                    ),
                ],
                Bytes::from([0_u8; 32].as_slice()),
            ),
            block_hash: Some(
                "0x95a70030118aa47f6bbe78300fe5aa2d6a14c03c39e4773cd511b6d36aa5846b"
                    .parse()
                    .unwrap(),
            ),
            block_number: Some(18_454_069),
            transaction_hash: Some(
                "0x7e7f7adda9ab6af8fc05a248ed2bb0eaf9b53886cfb461d2fb768ce4d0d02a8c"
                    .parse()
                    .unwrap(),
            ),
            transaction_index: Some(15),
            log_index: Some(118),
            ..log.clone()
        };
        let role_claimed_log = Log {
            inner: AlloyLog::new_unchecked(
                discount_token,
                vec![
                    alloy::primitives::b256!(
                        "0x83a9ddad961dcb7c6894c9585a16ff7792c2ec8281256a3cc7303ae830152dcf"
                    ),
                    alloy::primitives::b256!(
                        "0x0000000000000000000000005300a1a15135ea4dc7ad5a167152c01efc9b192a"
                    ),
                ],
                Bytes::from([0_u8; 32].as_slice()),
            ),
            log_index: Some(124),
            ..pending_admin_log.clone()
        };
        let cooldown_log = Log {
            inner: AlloyLog::new_unchecked(
                discount_token,
                vec![
                    alloy::primitives::b256!(
                        "0x8a05f911d8ab7fc50fec37ef4ba7f9bfcb1a3c191c81dcd824ad0946c4e20d65"
                    ),
                    alloy::primitives::b256!(
                        "0x0000000000000000000000001f847fd5e08fb559a69280a14e7e904e6dbff81f"
                    ),
                ],
                Bytes::from(
                    alloy::hex::decode(
                        "000000000000000000000000000000000000000000000000b447a45ad02f832e",
                    )
                    .unwrap(),
                ),
            ),
            block_hash: Some(
                "0x85e1e99c2aa4344f2d8f6b6600fdf64cb1da5f1c039aec1a6726e48eb4e4e6c2"
                    .parse()
                    .unwrap(),
            ),
            block_number: Some(18_763_122),
            transaction_hash: Some(
                "0x3991c229452ce82fb530fe6200094619b1d53ea6bf1ef4d4b0427363de9d0375"
                    .parse()
                    .unwrap(),
            ),
            transaction_index: Some(255),
            log_index: Some(422),
            ..log.clone()
        };
        let asset_config_log = Log {
            inner: AlloyLog::new_unchecked(
                discount_token,
                vec![
                    alloy::primitives::b256!(
                        "0x87fa03892a0556cb6b8f97e6d533a150d4d55fcbf275fff5fa003fa636bcc7fa"
                    ),
                    discount_token.into_word(),
                ],
                Bytes::from(
                    alloy::hex::decode(
                        "000000000000000000000000000000000000000000000000000cf3ddb8be5d55",
                    )
                    .unwrap(),
                ),
            ),
            block_hash: Some(
                "0x0da0fdd7093f650472f8e1e825ebd652b670a418ea38050081dcf9954985f71d"
                    .parse()
                    .unwrap(),
            ),
            block_number: Some(22_638_170),
            transaction_hash: Some(
                "0xaa900e1ac9ece8a1a0db38c111ccfe5b5fb735a838278995a7e6534a8fc32a63"
                    .parse()
                    .unwrap(),
            ),
            transaction_index: Some(56),
            log_index: Some(338),
            ..log.clone()
        };
        let max_slashable_log = Log {
            inner: AlloyLog::new_unchecked(
                discount_token,
                vec![alloy::primitives::b256!(
                    "0xb79c2d6c7f5c95eec9dc05affc0ed002620a4ec6d72b7b0744cc8638168c600a"
                )],
                Bytes::from(
                    alloy::hex::decode(
                        "00000000000000000000000000000000000000000000000000000000000007d0",
                    )
                    .unwrap(),
                ),
            ),
            log_index: Some(347),
            ..asset_config_log.clone()
        };
        let logs = [
            rewards_log,
            log,
            user_log,
            delegation_log,
            direct_claim_log,
            delegate_change_log,
            pending_admin_log,
            role_claimed_log,
            cooldown_log,
            asset_config_log,
            max_slashable_log,
        ];
        let tokens = BTreeMap::from([(discount_token, TokenType::GhoDiscount)]);
        let receipt = undecoded(&logs.iter().collect::<Vec<_>>());
        let ownership = ReceiptOwnership::classify(
            &receipt,
            &[],
            &EventOwnership::default(),
            &tokens,
            Address::ZERO,
        )
        .unwrap();

        assert_eq!(
            ownership.owner(325),
            Some(LogOwner::Role(NonOperationRole::RewardsAccounting))
        );
        assert_eq!(
            ownership.owner(321),
            Some(LogOwner::Role(NonOperationRole::RewardsAccounting))
        );
        assert_eq!(
            ownership.owner(322),
            Some(LogOwner::Role(NonOperationRole::RewardsAccounting))
        );
        assert_eq!(
            ownership.owner(329),
            Some(LogOwner::Role(NonOperationRole::AllowanceReport))
        );
        assert_eq!(
            ownership.owner(357),
            Some(LogOwner::Role(NonOperationRole::RewardsAccounting))
        );
        assert_eq!(
            ownership.owner(292),
            Some(LogOwner::Role(NonOperationRole::AllowanceReport))
        );
        assert_eq!(
            ownership.owner(118),
            Some(LogOwner::Role(NonOperationRole::MarketConfiguration))
        );
        assert_eq!(
            ownership.owner(124),
            Some(LogOwner::Role(NonOperationRole::MarketConfiguration))
        );
        assert_eq!(
            ownership.owner(422),
            Some(LogOwner::Role(NonOperationRole::RewardsAccounting))
        );
        assert_eq!(
            ownership.owner(338),
            Some(LogOwner::Role(NonOperationRole::MarketConfiguration))
        );
        assert_eq!(
            ownership.owner(347),
            Some(LogOwner::Role(NonOperationRole::MarketConfiguration))
        );
    }

    #[test]
    fn captured_rev9_pool_position_manager_approval_is_an_allowance_report() {
        use alloy::primitives::{Bytes, Log as AlloyLog};

        let pool: Address = "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2"
            .parse()
            .unwrap();
        let position_manager: Address = "0xd611d10cf51552227cab601a3788a6be52d53ae0"
            .parse()
            .unwrap();
        let log = Log {
            inner: AlloyLog::new_unchecked(
                pool,
                vec![
                    alloy::primitives::b256!(
                        "0x540e692f36c2fa13e7583c4deeffd91ce6bc04f91e7d84f295d9d858372875fc"
                    ),
                    position_manager.into_word(),
                    position_manager.into_word(),
                ],
                Bytes::new(),
            ),
            block_hash: Some(
                "0x1c967ead522aac509e69dd23f2c0b0c74b42c43c1340cd8ca0ea81c3a2024401"
                    .parse()
                    .unwrap(),
            ),
            block_number: Some(23_105_903),
            block_timestamp: None,
            transaction_hash: Some(
                "0x59cb182caf4d5f99f75050578f93e873ca84674012256cc8ee75cfdd7d4737f8"
                    .parse()
                    .unwrap(),
            ),
            transaction_index: Some(242),
            log_index: Some(568),
            removed: false,
        };
        let ownership = ReceiptOwnership::classify(
            &undecoded(&[&log]),
            &[],
            &EventOwnership::default(),
            &BTreeMap::new(),
            pool,
        )
        .unwrap();

        assert_eq!(
            ownership.owner(568),
            Some(LogOwner::Role(NonOperationRole::AllowanceReport))
        );
    }

    #[test]
    fn captured_rev11_pool_position_manager_revocation_is_an_allowance_report() {
        use alloy::primitives::{Bytes, Log as AlloyLog};

        let pool: Address = "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2"
            .parse()
            .unwrap();
        let user: Address = "0x2c143f9c8a3da16dbabbb7bf1679cb8e070a3245"
            .parse()
            .unwrap();
        let position_manager: Address = "0x366d12b5c7840d88212e6f78c8cd578275be8f8d"
            .parse()
            .unwrap();
        let log = Log {
            inner: AlloyLog::new_unchecked(
                pool,
                vec![
                    alloy::primitives::b256!(
                        "0x08c92c3870d10c79e9673fecea8f4ff261f8e6b661067d9ca63fd777882bff15"
                    ),
                    user.into_word(),
                    position_manager.into_word(),
                ],
                Bytes::new(),
            ),
            block_hash: Some(
                "0x81de53198111fc56b7ce96d41bd92ae683be19357b2f8d8a266c3e854a863bf1"
                    .parse()
                    .unwrap(),
            ),
            block_number: Some(26_036_607),
            block_timestamp: None,
            transaction_hash: Some(
                "0x18a9f88fca2c29fd83d41a3d19cc7e0c84042fae4040cfea3a5a613a9acaedb9"
                    .parse()
                    .unwrap(),
            ),
            transaction_index: Some(46),
            log_index: Some(165),
            removed: false,
        };
        let ownership = ReceiptOwnership::classify(
            &undecoded(&[&log]),
            &[],
            &EventOwnership::default(),
            &BTreeMap::new(),
            pool,
        )
        .unwrap();

        assert_eq!(
            ownership.owner(165),
            Some(LogOwner::Role(NonOperationRole::AllowanceReport))
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
}

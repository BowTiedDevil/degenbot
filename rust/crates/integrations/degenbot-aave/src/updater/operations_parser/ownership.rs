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
    /// A token allowance or credit-delegation report (`Approval`,
    /// `BorrowAllowanceDelegated`). An allowance moves no balance, so the market's
    /// projection deliberately reads neither.
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

/// One transaction's receipt, with every log classified before any operation
/// exists.
#[derive(Debug)]
pub struct ReceiptOwnership {
    slots: BTreeMap<u64, Slot>,
}

impl ReceiptOwnership {
    /// Classify every log of `receipt`.
    ///
    /// `decoded` is aligned with `receipt` (one entry per log, in the receipt's
    /// order); `scaled` holds the market's decoded scaled-token events and
    /// `movements` their pairing; `tokens` names the emitters this market tracks
    /// and `pool` is the market's Pool contract.
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
        receipt: &[&Log],
        decoded: &[Option<DecodedAaveEvent>],
        scaled: &[ScaledTokenEvent<'_>],
        movements: &EventOwnership,
        tokens: &BTreeMap<Address, TokenType>,
        pool: Address,
    ) -> Result<Self, OwnershipRefusal> {
        let mut slots = BTreeMap::new();
        for (log, decoded) in receipt.iter().zip(decoded) {
            let log_index = log_idx_value(log);
            let emitter = log.address();
            let slot = match decoded {
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
    if emitter == pool && *topic == AAVE_ISOLATION_MODE_TOTAL_DEBT_TOPIC {
        return Some(Slot::Role(NonOperationRole::IsolationDebtReport));
    }
    if tokens.contains_key(&emitter)
        && (*topic == SCALED_TOKEN_INITIALIZED_TOPIC
            || *topic == SCALED_TOKEN_INITIALIZED_DEBT_TOPIC)
    {
        return Some(Slot::Role(NonOperationRole::MarketConfiguration));
    }
    if tokens.contains_key(&emitter)
        && (*topic == ERC20_APPROVAL_TOPIC || *topic == DEBT_BORROW_ALLOWANCE_TOPIC)
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
}

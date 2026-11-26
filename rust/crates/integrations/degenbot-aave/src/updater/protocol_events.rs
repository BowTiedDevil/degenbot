//! The Aave protocol-event catalog and the market context a receipt is read
//! against.
//!
//! A decoded non-operation report means nothing on its own: the same signature
//! is not the same statement depending on who emitted it. `FlashLoan` is the
//! Pool's statement; a tracked token emitting it is contradictory evidence. A
//! GHO discount-token report belongs to the discount series; a vToken emitting
//! it is a different emitter's log wearing its topic. So the catalog keys every
//! report by `(report, emitter role)` and answers ONE thing: the ownership role
//! that statement carries in this market, or nothing.
//!
//! The catalog is revision- and revision-range aware without ever refusing a
//! known report for its revision. Each entry states the open-ended range of
//! contract revisions that introduce it ([`RevisionRange`]), so the tree
//! records what the chain said rather than an assumption: a later compatible
//! revision carrying a known report is the same report, and the
//! introduction ranges are evidence the boundary tests read. Recognition is
//! therefore open-ended by construction, matching the neutral decoder leaf that
//! never keys on a revision. An unknown topic — or a known topic from an
//! emitter role that does not state it — fails closed.
//!
//! This module owns the LAST step the decoder leaf deliberately does not
//! (ADR-105 D12): which emitter role a report belongs to, and which ownership
//! role the market's projection gives it.

use crate::operations::TokenType;
use crate::updater::operations_parser::ownership::NonOperationRole;
use crate::updater::revision::RevisionRange;
use alloy::primitives::Address;
use alloy::rpc::types::Log;
use degenbot_decoders::aave_report_decoder::{decode_aave_report_log, DecodedAaveReport};
use std::collections::BTreeMap;

/// A role one of this market's contracts plays in its receipts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmitterRole {
    /// The market's Pool: the operation anchors, the Pool's state and position
    /// reports, and its treasury mint.
    Pool,
    /// A token series this market tracks: a reserve's aToken or vToken, or the
    /// GHO discount token.
    Token(TokenType),
}

impl EmitterRole {
    /// The role's name, for a census or a refusal payload.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pool => "pool",
            Self::Token(TokenType::Atoken) => "aToken",
            Self::Token(TokenType::Vtoken) => "vToken",
            Self::Token(TokenType::GhoDiscount) => "GHO discount token",
        }
    }
}

/// The revisions the Aave model reads, as the market reported them.
///
/// These are market facts, not chain facts: the reported revision is what the
/// market's own rows state. A capture's boundary test and a policy gate read the
/// same fact, and nothing here refuses a report because that bookkeeping lags a
/// known on-chain implementation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReportedRevisions {
    /// The Pool's reported `POOL_REVISION()`, when the market states one.
    pub pool: Option<u32>,
    /// The GHO vToken's reported `DEBT_TOKEN_REVISION()`, when the market has
    /// a GHO reserve.
    pub gho_vtoken: Option<u32>,
    /// The GHO discount token's reported revision, when the market tracks one.
    pub discount_token: Option<u32>,
}

/// The market a receipt's logs are classified against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolContext {
    pool: Address,
    emitters: BTreeMap<Address, EmitterRole>,
    revisions: ReportedRevisions,
}

impl ProtocolContext {
    /// Build the context from the market's Pool, the token census its rows
    /// state, and the revisions it reported.
    #[must_use]
    pub fn new(
        pool: Address,
        tokens: BTreeMap<Address, TokenType>,
        revisions: ReportedRevisions,
    ) -> Self {
        let mut emitters: BTreeMap<Address, EmitterRole> = tokens
            .into_iter()
            .map(|(address, kind)| (address, EmitterRole::Token(kind)))
            .collect();
        emitters.insert(pool, EmitterRole::Pool);
        Self {
            pool,
            emitters,
            revisions,
        }
    }

    /// The market's Pool address.
    #[must_use]
    pub const fn pool(&self) -> Address {
        self.pool
    }

    /// Whether `emitter` is the market's Pool.
    #[must_use]
    pub fn is_pool(&self, emitter: Address) -> bool {
        emitter == self.pool
    }

    /// The token series `emitter` is, when this market tracks it.
    #[must_use]
    pub fn token_role(&self, emitter: Address) -> Option<TokenType> {
        match self.emitters.get(&emitter) {
            Some(EmitterRole::Token(kind)) => Some(*kind),
            Some(EmitterRole::Pool) | None => None,
        }
    }

    /// The role `emitter` plays in this market's receipts, or `None` when the
    /// market does not track it at all.
    #[must_use]
    pub fn emitter_role(&self, emitter: Address) -> Option<EmitterRole> {
        self.emitters.get(&emitter).copied()
    }

    /// The revisions the market reported.
    #[must_use]
    pub const fn revisions(&self) -> ReportedRevisions {
        self.revisions
    }

    /// Classify one receipt log against the catalog.
    #[must_use]
    pub fn classify_report(&self, log: &Log) -> ReportAnswer {
        let Some(emitter) = self.emitter_role(log.address()) else {
            return ReportAnswer::ForeignEmitter;
        };
        let Some(report) = decode_aave_report_log(log) else {
            return ReportAnswer::Unrecognized;
        };
        match catalog_entry(ProtocolReport::of(&report), emitter) {
            Some(entry) => ReportAnswer::Role(entry.role),
            None => ReportAnswer::Unrecognized,
        }
    }
}

/// One receipt log's place in the Aave report vocabulary, read against this
/// market.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportAnswer {
    /// A report this market's model gives a role.
    Role(NonOperationRole),
    /// The emitter is not one of this market's contracts: another protocol's
    /// (or another market's) log, deliberately out of scope.
    ForeignEmitter,
    /// One of this market's contracts emitted a report the catalog does not
    /// place — an unknown topic, or a known topic from the wrong emitter role.
    Unrecognized,
}

/// The neutral report the catalog keys an Aave statement by. One variant per
/// recognized `DecodedAaveReport` variant, so a report the decoder gains must
/// state its catalog entry here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolReport {
    FlashLoan,
    DeficitCovered,
    IsolationModeTotalDebtUpdated,
    PositionManagerApproved,
    PositionManagerRevoked,
    ATokenInitialized,
    DebtTokenInitialized,
    FeesDistributedToTreasury,
    VariableDebtTokenSet,
    ATokenSet,
    GhoTreasuryUpdated,
    GhoDebtTokenChanged,
    AssetIndexUpdated,
    UserIndexUpdated,
    AssetConfigUpdated,
    MaxSlashablePercentageChanged,
    DelegatedPowerChanged,
    DelegateChanged,
    RewardsAccrued,
    RewardsClaimed,
    Cooldown,
    PendingAdminChanged,
    RoleClaimed,
    Approval,
    BorrowAllowanceDelegated,
}

impl ProtocolReport {
    /// The catalog's name for a decoded report.
    #[must_use]
    pub const fn of(report: &DecodedAaveReport) -> Self {
        match report {
            DecodedAaveReport::FlashLoan(_) => Self::FlashLoan,
            DecodedAaveReport::DeficitCovered(_) => Self::DeficitCovered,
            DecodedAaveReport::IsolationModeTotalDebtUpdated(_) => {
                Self::IsolationModeTotalDebtUpdated
            }
            DecodedAaveReport::PositionManagerApproved(_) => Self::PositionManagerApproved,
            DecodedAaveReport::PositionManagerRevoked(_) => Self::PositionManagerRevoked,
            DecodedAaveReport::ATokenInitialized(_) => Self::ATokenInitialized,
            DecodedAaveReport::DebtTokenInitialized(_) => Self::DebtTokenInitialized,
            DecodedAaveReport::FeesDistributedToTreasury(_) => Self::FeesDistributedToTreasury,
            DecodedAaveReport::VariableDebtTokenSet(_) => Self::VariableDebtTokenSet,
            DecodedAaveReport::ATokenSet(_) => Self::ATokenSet,
            DecodedAaveReport::GhoTreasuryUpdated(_) => Self::GhoTreasuryUpdated,
            DecodedAaveReport::GhoDebtTokenChanged(_) => Self::GhoDebtTokenChanged,
            DecodedAaveReport::AssetIndexUpdated(_) => Self::AssetIndexUpdated,
            DecodedAaveReport::UserIndexUpdated(_) => Self::UserIndexUpdated,
            DecodedAaveReport::AssetConfigUpdated(_) => Self::AssetConfigUpdated,
            DecodedAaveReport::MaxSlashablePercentageChanged(_) => {
                Self::MaxSlashablePercentageChanged
            }
            DecodedAaveReport::DelegatedPowerChanged(_) => Self::DelegatedPowerChanged,
            DecodedAaveReport::DelegateChanged(_) => Self::DelegateChanged,
            DecodedAaveReport::RewardsAccrued(_) => Self::RewardsAccrued,
            DecodedAaveReport::RewardsClaimed(_) => Self::RewardsClaimed,
            DecodedAaveReport::Cooldown(_) => Self::Cooldown,
            DecodedAaveReport::PendingAdminChanged(_) => Self::PendingAdminChanged,
            DecodedAaveReport::RoleClaimed(_) => Self::RoleClaimed,
            DecodedAaveReport::Approval(_) => Self::Approval,
            DecodedAaveReport::BorrowAllowanceDelegated(_) => Self::BorrowAllowanceDelegated,
        }
    }

    /// The report's name, for a catalog census or a refusal payload.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::FlashLoan => "FlashLoan",
            Self::DeficitCovered => "DeficitCovered",
            Self::IsolationModeTotalDebtUpdated => "IsolationModeTotalDebtUpdated",
            Self::PositionManagerApproved => "PositionManagerApproved",
            Self::PositionManagerRevoked => "PositionManagerRevoked",
            Self::ATokenInitialized => "ATokenInitialized",
            Self::DebtTokenInitialized => "DebtTokenInitialized",
            Self::FeesDistributedToTreasury => "FeesDistributedToTreasury",
            Self::VariableDebtTokenSet => "VariableDebtTokenSet",
            Self::ATokenSet => "ATokenSet",
            Self::GhoTreasuryUpdated => "GhoTreasuryUpdated",
            Self::GhoDebtTokenChanged => "GhoDebtTokenChanged",
            Self::AssetIndexUpdated => "AssetIndexUpdated",
            Self::UserIndexUpdated => "UserIndexUpdated",
            Self::AssetConfigUpdated => "AssetConfigUpdated",
            Self::MaxSlashablePercentageChanged => "MaxSlashablePercentageChanged",
            Self::DelegatedPowerChanged => "DelegatedPowerChanged",
            Self::DelegateChanged => "DelegateChanged",
            Self::RewardsAccrued => "RewardsAccrued",
            Self::RewardsClaimed => "RewardsClaimed",
            Self::Cooldown => "Cooldown",
            Self::PendingAdminChanged => "PendingAdminChanged",
            Self::RoleClaimed => "RoleClaimed",
            Self::Approval => "Approval",
            Self::BorrowAllowanceDelegated => "BorrowAllowanceDelegated",
        }
    }
}

/// The emitter roles a catalog entry accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportEmitter {
    /// The market's Pool, and only it.
    Pool,
    /// A tracked token of exactly this series.
    Token(TokenType),
    /// Any tracked token series — a statement every ERC-20-style series makes
    /// about the same movement.
    AnyToken,
}

impl ReportEmitter {
    /// Whether `role` emits the report.
    #[must_use]
    pub fn accepts(self, role: EmitterRole) -> bool {
        match (self, role) {
            (Self::Token(expected), EmitterRole::Token(actual)) => expected == actual,
            (Self::Pool, EmitterRole::Pool) | (Self::AnyToken, EmitterRole::Token(_)) => true,
            _ => false,
        }
    }
}

/// One catalog entry: a report, the emitter role that states it, the ownership
/// role it carries, and the open-ended revisions that introduce it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportCatalogEntry {
    /// The neutral report.
    pub report: ProtocolReport,
    /// The emitter role that states it.
    pub emitter: ReportEmitter,
    /// The ownership role the market's projection gives it.
    pub role: NonOperationRole,
    /// The revisions that carry the report.
    pub introduced: RevisionRange,
}

/// The one catalog: every report the neutral decoder recognizes, with the
/// emitter role that states it and the role the market gives it.
///
/// The Pool's position-manager authorization reports state the revisions the
/// tree's captured audits recorded (approved at Pool revision 9, revoked at 11);
/// every other report states revision 1, the range the captured blame-era
/// receipts prove and the earliest revision any of them is read at — no earlier
/// revision is claimed and no upper bound is stated.
pub const REPORT_CATALOG: [ReportCatalogEntry; 25] = [
    ReportCatalogEntry {
        report: ProtocolReport::FlashLoan,
        emitter: ReportEmitter::Pool,
        role: NonOperationRole::FlashLoan,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::DeficitCovered,
        emitter: ReportEmitter::Pool,
        role: NonOperationRole::DeficitCoverageReport,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::IsolationModeTotalDebtUpdated,
        emitter: ReportEmitter::Pool,
        role: NonOperationRole::IsolationDebtReport,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::PositionManagerApproved,
        emitter: ReportEmitter::Pool,
        role: NonOperationRole::AuthorizationReport,
        introduced: RevisionRange::from(9),
    },
    ReportCatalogEntry {
        report: ProtocolReport::PositionManagerRevoked,
        emitter: ReportEmitter::Pool,
        role: NonOperationRole::AuthorizationReport,
        introduced: RevisionRange::from(11),
    },
    ReportCatalogEntry {
        report: ProtocolReport::ATokenInitialized,
        emitter: ReportEmitter::Token(TokenType::Atoken),
        role: NonOperationRole::MarketConfiguration,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::DebtTokenInitialized,
        emitter: ReportEmitter::Token(TokenType::Vtoken),
        role: NonOperationRole::MarketConfiguration,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::FeesDistributedToTreasury,
        emitter: ReportEmitter::Token(TokenType::Atoken),
        role: NonOperationRole::FeeDistribution,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::VariableDebtTokenSet,
        emitter: ReportEmitter::Token(TokenType::Atoken),
        role: NonOperationRole::MarketConfiguration,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::ATokenSet,
        emitter: ReportEmitter::Token(TokenType::Vtoken),
        role: NonOperationRole::MarketConfiguration,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::GhoTreasuryUpdated,
        emitter: ReportEmitter::Token(TokenType::Atoken),
        role: NonOperationRole::MarketConfiguration,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::GhoDebtTokenChanged,
        emitter: ReportEmitter::Token(TokenType::GhoDiscount),
        role: NonOperationRole::MarketConfiguration,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::AssetIndexUpdated,
        emitter: ReportEmitter::Token(TokenType::GhoDiscount),
        role: NonOperationRole::RewardsAccounting,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::UserIndexUpdated,
        emitter: ReportEmitter::Token(TokenType::GhoDiscount),
        role: NonOperationRole::RewardsAccounting,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::AssetConfigUpdated,
        emitter: ReportEmitter::Token(TokenType::GhoDiscount),
        role: NonOperationRole::MarketConfiguration,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::MaxSlashablePercentageChanged,
        emitter: ReportEmitter::Token(TokenType::GhoDiscount),
        role: NonOperationRole::MarketConfiguration,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::DelegatedPowerChanged,
        emitter: ReportEmitter::Token(TokenType::GhoDiscount),
        role: NonOperationRole::AllowanceReport,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::DelegateChanged,
        emitter: ReportEmitter::Token(TokenType::GhoDiscount),
        role: NonOperationRole::AllowanceReport,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::RewardsAccrued,
        emitter: ReportEmitter::Token(TokenType::GhoDiscount),
        role: NonOperationRole::RewardsAccounting,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::RewardsClaimed,
        emitter: ReportEmitter::Token(TokenType::GhoDiscount),
        role: NonOperationRole::RewardsAccounting,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::Cooldown,
        emitter: ReportEmitter::Token(TokenType::GhoDiscount),
        role: NonOperationRole::RewardsAccounting,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::PendingAdminChanged,
        emitter: ReportEmitter::Token(TokenType::GhoDiscount),
        role: NonOperationRole::MarketConfiguration,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::RoleClaimed,
        emitter: ReportEmitter::Token(TokenType::GhoDiscount),
        role: NonOperationRole::MarketConfiguration,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::Approval,
        emitter: ReportEmitter::AnyToken,
        role: NonOperationRole::AllowanceReport,
        introduced: RevisionRange::from(1),
    },
    ReportCatalogEntry {
        report: ProtocolReport::BorrowAllowanceDelegated,
        emitter: ReportEmitter::Token(TokenType::Vtoken),
        role: NonOperationRole::AllowanceReport,
        introduced: RevisionRange::from(1),
    },
];

/// The catalog entry that states `report` from `emitter`, or `None` when that
/// emitter role does not state it.
#[must_use]
pub fn catalog_entry(
    report: ProtocolReport,
    emitter: EmitterRole,
) -> Option<&'static ReportCatalogEntry> {
    REPORT_CATALOG
        .iter()
        .find(|entry| entry.report == report && entry.emitter.accepts(emitter))
}

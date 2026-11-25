//! Neutral Aave V3 non-operation report decoders (25 signatures).
//!
//! Aave reports state without emitting an operation of their own: the Pool's
//! flash-loan, deficit-coverage and isolation-debt statements; a scaled
//! token's fee distribution, credit delegation and `Initialized` upgrade
//! report; the GHO aToken's dependency links; and stkAAVE's staking, emission,
//! delegation and rewards accounting.
//!
//! This module owns topic and payload recognition for those reports — the
//! signature, the indexed fields, and every data field — and nothing more.
//! Which emitter role a report belongs to, whether a market tracks that
//! emitter, and which revision introduced it are `degenbot-aave` decisions;
//! this leaf never acquires that dependency (ADR-105 D12).
//!
//! Recognition is open-ended by construction: a decoder keys on the
//! signature's `topic0` and never on a block number, market revision, or
//! emitter allow-list, so a later compatible revision carrying a known report
//! decodes exactly as an earlier one did. Unknown topics fail closed —
//! [`decode_aave_report_log`] returns `None` instead of a catch-all variant.
//!
//! # Report scope (25 signatures)
//!
//! ## Pool reports
//!
//! ```text
//! event FlashLoan(address indexed target, address initiator, address indexed asset, uint256 amount, uint8 interestRateMode, uint256 premium, uint16 indexed referralCode);
//! event DeficitCovered(address indexed reserve, address caller, uint256 amountCovered);
//! event IsolationModeTotalDebtUpdated(address indexed asset, uint256 totalDebt);
//! event PositionManagerApproved(address indexed user, address indexed positionManager);
//! event PositionManagerRevoked(address indexed user, address indexed positionManager);
//! ```
//!
//! ## Scaled-token and GHO reports
//!
//! ```text
//! event Initialized(address indexed underlyingAsset, address indexed pool, address treasury, address incentivesController, uint8 aTokenDecimals, string aTokenName, string aTokenSymbol, bytes params);
//! event Initialized(address indexed underlyingAsset, address indexed pool, address incentivesController, uint8 debtTokenDecimals, string debtTokenName, string debtTokenSymbol, bytes params);
//! event FeesDistributedToTreasury(address indexed ghoTreasury, address indexed asset, uint256 amount);
//! event VariableDebtTokenSet(address indexed variableDebtToken);
//! event ATokenSet(address indexed aToken);
//! event GhoTreasuryUpdated(address indexed oldGhoTreasury, address indexed newGhoTreasury);
//! ```
//!
//! ## stkAAVE and GHO-discount reports
//!
//! ```text
//! event AssetIndexUpdated(address indexed asset, uint256 index);
//! event UserIndexUpdated(address indexed user, address indexed asset, uint256 index);
//! event AssetConfigUpdated(address indexed asset, uint256 emission);
//! event MaxSlashablePercentageChanged(uint256 newPercentage);
//! event DelegatedPowerChanged(address indexed user, uint256 amount, uint8 delegationType);
//! event DelegateChanged(address indexed delegator, address indexed delegatee, uint8 delegationType);
//! event RewardsAccrued(address user, uint256 amount);
//! event RewardsClaimed(address indexed from, address indexed to, uint256 amount);
//! event Cooldown(address indexed user, uint256 amount);
//! event PendingAdminChanged(address indexed newPendingAdmin, uint256 role);
//! event RoleClaimed(address indexed newAdmin, uint256 role);
//! event GHODebtTokenChanged(address indexed newDebtToken);
//! ```
//!
//! ## Allowance and delegation reports
//!
//! ```text
//! event Approval(address indexed owner, address indexed spender, uint256 value);
//! event BorrowAllowanceDelegated(address indexed fromUser, address indexed toUser, address indexed asset, uint256 amount);
//! ```
//!
//! The two `Initialized` signatures are distinct topics because their
//! parameter lists differ: the aToken report carries the reserve treasury, the
//! debt-token report does not.

use alloy::primitives::{b256, Address, B256, U256};
use alloy::rpc::types::Log;

use crate::aave_event_decoder::{
    data_address, data_u256, data_u8, log_topics, topic_address, topic_u16,
};

// ── topic constants (25 — keccak256 of the canonical report signatures) ───

/// `keccak256("FlashLoan(address,address,address,uint256,uint8,uint256,uint16)")`.
pub const AAVE_FLASH_LOAN_TOPIC: B256 =
    b256!("0xefefaba5e921573100900a3ad9cf29f222d995fb3b6045797eaea7521bd8d6f0");
/// `keccak256("DeficitCovered(address,address,uint256)")`.
pub const AAVE_DEFICIT_COVERED_TOPIC: B256 =
    b256!("0x84b203e49f1a4b553088061534231969a68ad1c81be192205e96d23a206cb26a");
/// `keccak256("IsolationModeTotalDebtUpdated(address,uint256)")`.
pub const AAVE_ISOLATION_MODE_TOTAL_DEBT_UPDATED_TOPIC: B256 =
    b256!("0xaef84d3b40895fd58c561f3998000f0583abb992a52fbdc99ace8e8de4d676a5");
/// `keccak256("PositionManagerApproved(address,address)")`.
pub const AAVE_POSITION_MANAGER_APPROVED_TOPIC: B256 =
    b256!("0x540e692f36c2fa13e7583c4deeffd91ce6bc04f91e7d84f295d9d858372875fc");
/// `keccak256("PositionManagerRevoked(address,address)")`.
pub const AAVE_POSITION_MANAGER_REVOKED_TOPIC: B256 =
    b256!("0x08c92c3870d10c79e9673fecea8f4ff261f8e6b661067d9ca63fd777882bff15");
/// `keccak256("Initialized(address,address,address,address,uint8,string,string,bytes)")`.
pub const AAVE_ATOKEN_INITIALIZED_TOPIC: B256 =
    b256!("0xb19e051f8af41150ccccb3fc2c2d8d15f4a4cf434f32a559ba75fe73d6eea20b");
/// `keccak256("Initialized(address,address,address,uint8,string,string,bytes)")`.
pub const AAVE_DEBT_TOKEN_INITIALIZED_TOPIC: B256 =
    b256!("0x40251fbfb6656cfa65a00d7879029fec1fad21d28fdcff2f4f68f52795b74f2c");
/// `keccak256("FeesDistributedToTreasury(address,address,uint256)")`.
pub const AAVE_FEES_DISTRIBUTED_TO_TREASURY_TOPIC: B256 =
    b256!("0xb29fcda740927812f5a71075b62e132bead3769a455319c29b9a1cc461a65475");
/// `keccak256("VariableDebtTokenSet(address)")`.
pub const AAVE_VARIABLE_DEBT_TOKEN_SET_TOPIC: B256 =
    b256!("0x9a2092516d2c53586ed59ca6d2878442977a16f16e1c85ac0a8a8cd9cb024666");
/// `keccak256("ATokenSet(address)")`.
pub const AAVE_ATOKEN_SET_TOPIC: B256 =
    b256!("0x07cba78d3a9bb0cf2cb0e30140558e278b50a2ba6da2e13369a0d9113034548b");
/// `keccak256("GhoTreasuryUpdated(address,address)")`.
pub const AAVE_GHO_TREASURY_UPDATED_TOPIC: B256 =
    b256!("0x13f4413d8d93a259bd6c10f35095371f30ed50f81a73407e52e9f02000d5d16b");
/// `keccak256("GHODebtTokenChanged(address)")`.
pub const AAVE_GHO_DEBT_TOKEN_CHANGED_TOPIC: B256 =
    b256!("0x62bbced765bccc9ed93d0bae1b2a8c86e13b4663f8bf27d5936d9d619194c8a9");
/// `keccak256("AssetIndexUpdated(address,uint256)")`.
pub const AAVE_ASSET_INDEX_UPDATED_TOPIC: B256 =
    b256!("0x5777ca300dfe5bead41006fbce4389794dbc0ed8d6cccebfaf94630aa04184bc");
/// `keccak256("UserIndexUpdated(address,address,uint256)")`.
pub const AAVE_USER_INDEX_UPDATED_TOPIC: B256 =
    b256!("0xbb123b5c06d5408bbea3c4fef481578175cfb432e3b482c6186f02ed9086585b");
/// `keccak256("AssetConfigUpdated(address,uint256)")`.
pub const AAVE_ASSET_CONFIG_UPDATED_TOPIC: B256 =
    b256!("0x87fa03892a0556cb6b8f97e6d533a150d4d55fcbf275fff5fa003fa636bcc7fa");
/// `keccak256("MaxSlashablePercentageChanged(uint256)")`.
pub const AAVE_MAX_SLASHABLE_PERCENTAGE_CHANGED_TOPIC: B256 =
    b256!("0xb79c2d6c7f5c95eec9dc05affc0ed002620a4ec6d72b7b0744cc8638168c600a");
/// `keccak256("DelegatedPowerChanged(address,uint256,uint8)")`.
pub const AAVE_DELEGATED_POWER_CHANGED_TOPIC: B256 =
    b256!("0xa0a19463ee116110c9b282012d9b65cc5522dc38a9520340cbaf3142e550127f");
/// `keccak256("DelegateChanged(address,address,uint8)")`.
pub const AAVE_DELEGATE_CHANGED_TOPIC: B256 =
    b256!("0xe8d51c8e11bd570db1734c8ec775785330e77007feed45c43b608ef33ff914bd");
/// `keccak256("RewardsAccrued(address,uint256)")`.
pub const AAVE_REWARDS_ACCRUED_TOPIC: B256 =
    b256!("0x2468f9268c60ad90e2d49edb0032c8a001e733ae888b3ab8e982edf535be1a76");
/// `keccak256("RewardsClaimed(address,address,uint256)")` — the staking report, distinct from the five-argument `RewardsController` event.
pub const AAVE_STK_AAVE_REWARDS_CLAIMED_TOPIC: B256 =
    b256!("0x9310ccfcb8de723f578a9e4282ea9f521f05ae40dc08f3068dfad528a65ee3c7");
/// `keccak256("Cooldown(address,uint256)")`.
pub const AAVE_COOLDOWN_TOPIC: B256 =
    b256!("0x8a05f911d8ab7fc50fec37ef4ba7f9bfcb1a3c191c81dcd824ad0946c4e20d65");
/// `keccak256("PendingAdminChanged(address,uint256)")`.
pub const AAVE_PENDING_ADMIN_CHANGED_TOPIC: B256 =
    b256!("0xc3d1bf52795a20771ee6de3e59948fd83e10db956122d4709a3b86c512963855");
/// `keccak256("RoleClaimed(address,uint256)")`.
pub const AAVE_ROLE_CLAIMED_TOPIC: B256 =
    b256!("0x83a9ddad961dcb7c6894c9585a16ff7792c2ec8281256a3cc7303ae830152dcf");
/// `keccak256("Approval(address,address,uint256)")`.
pub const AAVE_APPROVAL_TOPIC: B256 =
    b256!("0x8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925");
/// `keccak256("BorrowAllowanceDelegated(address,address,address,uint256)")`.
pub const AAVE_BORROW_ALLOWANCE_DELEGATED_TOPIC: B256 =
    b256!("0xda919360433220e13b51e8c211e490d148e61a3bd53de8c097194e458b97f3e1");

// ── dynamic ABI helpers ───────────────────────────────────────────────────

/// Read the ABI-encoded dynamic `bytes`/`string` whose head word is
/// `data[word_idx]`: an offset to a length word, then that many payload bytes.
fn data_tail(data: &[u8], word_idx: usize) -> Option<&[u8]> {
    let offset: usize = data_u256(data, word_idx)?.try_into().ok()?;
    let length_start = offset.checked_add(32)?;
    let length_word: [u8; 32] = data.get(offset..length_start)?.try_into().ok()?;
    let length: usize = U256::from_be_bytes::<32>(length_word).try_into().ok()?;
    let end = length_start.checked_add(length)?;
    data.get(length_start..end)
}

fn data_string(data: &[u8], word_idx: usize) -> Option<String> {
    String::from_utf8(data_tail(data, word_idx)?.to_vec()).ok()
}

// ── decoded reports ───────────────────────────────────────────────────────
// Every report carries the emitter verbatim; the emitter's role (Pool, scaled
// token, stkAAVE, GHO discount token) is a `degenbot-aave` classification.

/// Decoded Aave V3 `FlashLoan` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3FlashLoanReport {
    pub emitter: Address,
    /// `target` (topic[1]).
    pub target: Address,
    /// `asset` (topic[2]).
    pub asset: Address,
    /// `referralCode` (topic[3] — uint16).
    pub referral_code: u16,
    /// `initiator` (data word 0).
    pub initiator: Address,
    /// `amount` (data word 1).
    pub amount: U256,
    /// `interestRateMode` (data word 2 — uint8).
    pub interest_rate_mode: u8,
    /// `premium` (data word 3).
    pub premium: U256,
}

/// Decoded Aave V3 `DeficitCovered` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3DeficitCoveredReport {
    pub emitter: Address,
    /// `reserve` (topic[1]).
    pub reserve: Address,
    /// `caller` (data word 0).
    pub caller: Address,
    /// `amountCovered` (data word 1).
    pub amount_covered: U256,
}

/// Decoded Aave V3 `IsolationModeTotalDebtUpdated` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3IsolationModeTotalDebtUpdatedReport {
    pub emitter: Address,
    /// `asset` (topic[1]).
    pub asset: Address,
    /// `totalDebt` (data word 0).
    pub total_debt: U256,
}

/// Decoded Aave V3 `PositionManagerApproved` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3PositionManagerApprovedReport {
    pub emitter: Address,
    /// `user` (topic[1]).
    pub user: Address,
    /// `positionManager` (topic[2]).
    pub position_manager: Address,
}

/// Decoded Aave V3 `PositionManagerRevoked` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3PositionManagerRevokedReport {
    pub emitter: Address,
    /// `user` (topic[1]).
    pub user: Address,
    /// `positionManager` (topic[2]).
    pub position_manager: Address,
}

/// Decoded aToken `Initialized` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3ATokenInitializedReport {
    pub emitter: Address,
    /// `underlyingAsset` (topic[1]).
    pub underlying_asset: Address,
    /// `pool` (topic[2]).
    pub pool: Address,
    /// `treasury` (data word 0).
    pub treasury: Address,
    /// `incentivesController` (data word 1).
    pub incentives_controller: Address,
    /// `aTokenDecimals` (data word 2).
    pub decimals: u8,
    /// `aTokenName` (dynamic tail, data word 3).
    pub name: String,
    /// `aTokenSymbol` (dynamic tail, data word 4).
    pub symbol: String,
    /// `params` (dynamic tail, data word 5).
    pub params: Vec<u8>,
}

/// Decoded debt-token `Initialized` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3DebtTokenInitializedReport {
    pub emitter: Address,
    /// `underlyingAsset` (topic[1]).
    pub underlying_asset: Address,
    /// `pool` (topic[2]).
    pub pool: Address,
    /// `incentivesController` (data word 0).
    pub incentives_controller: Address,
    /// `debtTokenDecimals` (data word 1).
    pub decimals: u8,
    /// `debtTokenName` (dynamic tail, data word 2).
    pub name: String,
    /// `debtTokenSymbol` (dynamic tail, data word 3).
    pub symbol: String,
    /// `params` (dynamic tail, data word 4).
    pub params: Vec<u8>,
}

/// Decoded scaled-token `FeesDistributedToTreasury` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3FeesDistributedToTreasuryReport {
    pub emitter: Address,
    /// `ghoTreasury` (topic[1]).
    pub gho_treasury: Address,
    /// `asset` (topic[2]).
    pub asset: Address,
    /// `amount` (data word 0).
    pub amount: U256,
}

/// Decoded GHO aToken `VariableDebtTokenSet` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3VariableDebtTokenSetReport {
    pub emitter: Address,
    /// `variableDebtToken` (topic[1]).
    pub variable_debt_token: Address,
}

/// Decoded GHO debt-token `ATokenSet` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3ATokenSetReport {
    pub emitter: Address,
    /// `aToken` (topic[1]).
    pub a_token: Address,
}

/// Decoded GHO aToken `GhoTreasuryUpdated` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3GhoTreasuryUpdatedReport {
    pub emitter: Address,
    /// `oldGhoTreasury` (topic[1]).
    pub old_gho_treasury: Address,
    /// `newGhoTreasury` (topic[2]).
    pub new_gho_treasury: Address,
}

/// Decoded `GHODebtTokenChanged` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3GhoDebtTokenChangedReport {
    pub emitter: Address,
    /// `newDebtToken` (topic[1]).
    pub new_debt_token: Address,
}

/// Decoded `AssetIndexUpdated` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3AssetIndexUpdatedReport {
    pub emitter: Address,
    /// `asset` (topic[1]).
    pub asset: Address,
    /// `index` (data word 0).
    pub index: U256,
}

/// Decoded `UserIndexUpdated` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3UserIndexUpdatedReport {
    pub emitter: Address,
    /// `user` (topic[1]).
    pub user: Address,
    /// `asset` (topic[2]).
    pub asset: Address,
    /// `index` (data word 0).
    pub index: U256,
}

/// Decoded `AssetConfigUpdated` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3AssetConfigUpdatedReport {
    pub emitter: Address,
    /// `asset` (topic[1]).
    pub asset: Address,
    /// `emission` (data word 0).
    pub emission: U256,
}

/// Decoded `MaxSlashablePercentageChanged` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3MaxSlashablePercentageChangedReport {
    pub emitter: Address,
    /// `newPercentage` (data word 0).
    pub new_percentage: U256,
}

/// Decoded `DelegatedPowerChanged` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3DelegatedPowerChangedReport {
    pub emitter: Address,
    /// `user` (topic[1]).
    pub user: Address,
    /// `amount` (data word 0).
    pub amount: U256,
    /// `delegationType` (data word 1 — uint8).
    pub delegation_type: u8,
}

/// Decoded `DelegateChanged` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3DelegateChangedReport {
    pub emitter: Address,
    /// `delegator` (topic[1]).
    pub delegator: Address,
    /// `delegatee` (topic[2]).
    pub delegatee: Address,
    /// `delegationType` (data word 0 — uint8).
    pub delegation_type: u8,
}

/// Decoded `RewardsAccrued` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3RewardsAccruedReport {
    pub emitter: Address,
    /// `user` (data word 0 — non-indexed).
    pub user: Address,
    /// `amount` (data word 1).
    pub amount: U256,
}

/// Decoded staking `RewardsClaimed` report — the three-argument signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3RewardsClaimedReport {
    pub emitter: Address,
    /// `from` (topic[1]).
    pub from: Address,
    /// `to` (topic[2]).
    pub to: Address,
    /// `amount` (data word 0).
    pub amount: U256,
}

/// Decoded `Cooldown` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3CooldownReport {
    pub emitter: Address,
    /// `user` (topic[1]).
    pub user: Address,
    /// `amount` (data word 0).
    pub amount: U256,
}

/// Decoded `PendingAdminChanged` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3PendingAdminChangedReport {
    pub emitter: Address,
    /// `newPendingAdmin` (topic[1]).
    pub new_pending_admin: Address,
    /// `role` (data word 0).
    pub role: U256,
}

/// Decoded `RoleClaimed` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3RoleClaimedReport {
    pub emitter: Address,
    /// `newAdmin` (topic[1]).
    pub new_admin: Address,
    /// `role` (data word 0).
    pub role: U256,
}

/// Decoded ERC-20 `Approval` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3ApprovalReport {
    pub emitter: Address,
    /// `owner` (topic[1]).
    pub owner: Address,
    /// `spender` (topic[2]).
    pub spender: Address,
    /// `value` (data word 0).
    pub value: U256,
}

/// Decoded `BorrowAllowanceDelegated` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3BorrowAllowanceDelegatedReport {
    pub emitter: Address,
    /// `fromUser` (topic[1]).
    pub from_user: Address,
    /// `toUser` (topic[2]).
    pub to_user: Address,
    /// `asset` (topic[3]).
    pub asset: Address,
    /// `amount` (data word 0).
    pub amount: U256,
}

/// The discriminated union of Aave V3 non-operation reports. One variant per
/// recognized signature; [`decode_aave_report_log`] returns `None` for every
/// other topic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodedAaveReport {
    FlashLoan(AaveV3FlashLoanReport),
    DeficitCovered(AaveV3DeficitCoveredReport),
    IsolationModeTotalDebtUpdated(AaveV3IsolationModeTotalDebtUpdatedReport),
    PositionManagerApproved(AaveV3PositionManagerApprovedReport),
    PositionManagerRevoked(AaveV3PositionManagerRevokedReport),
    ATokenInitialized(AaveV3ATokenInitializedReport),
    DebtTokenInitialized(AaveV3DebtTokenInitializedReport),
    FeesDistributedToTreasury(AaveV3FeesDistributedToTreasuryReport),
    VariableDebtTokenSet(AaveV3VariableDebtTokenSetReport),
    ATokenSet(AaveV3ATokenSetReport),
    GhoTreasuryUpdated(AaveV3GhoTreasuryUpdatedReport),
    GhoDebtTokenChanged(AaveV3GhoDebtTokenChangedReport),
    AssetIndexUpdated(AaveV3AssetIndexUpdatedReport),
    UserIndexUpdated(AaveV3UserIndexUpdatedReport),
    AssetConfigUpdated(AaveV3AssetConfigUpdatedReport),
    MaxSlashablePercentageChanged(AaveV3MaxSlashablePercentageChangedReport),
    DelegatedPowerChanged(AaveV3DelegatedPowerChangedReport),
    DelegateChanged(AaveV3DelegateChangedReport),
    RewardsAccrued(AaveV3RewardsAccruedReport),
    RewardsClaimed(AaveV3RewardsClaimedReport),
    Cooldown(AaveV3CooldownReport),
    PendingAdminChanged(AaveV3PendingAdminChangedReport),
    RoleClaimed(AaveV3RoleClaimedReport),
    Approval(AaveV3ApprovalReport),
    BorrowAllowanceDelegated(AaveV3BorrowAllowanceDelegatedReport),
}

// ── per-report decode fns ─────────────────────────────────────────────────

/// Decode a `FlashLoan` report. Returns `None` on topic mismatch, < 4 topics,
/// or data < 128 bytes.
#[must_use]
pub fn decode_aave_flash_loan_log(log: &Log) -> Option<AaveV3FlashLoanReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_FLASH_LOAN_TOPIC {
        return None;
    }
    let target = topic_address(topics, 1)?;
    let asset = topic_address(topics, 2)?;
    let referral_code = topic_u16(topics, 3)?;
    let data = log.data().data.as_ref();
    if data.len() < 128 {
        return None;
    }
    Some(AaveV3FlashLoanReport {
        emitter: log.address(),
        target,
        asset,
        referral_code,
        initiator: data_address(data, 0)?,
        amount: data_u256(data, 1)?,
        interest_rate_mode: data_u8(data, 2)?,
        premium: data_u256(data, 3)?,
    })
}

/// Decode a `DeficitCovered` report. Returns `None` on topic mismatch, < 2
/// topics, or data < 64 bytes.
#[must_use]
pub fn decode_aave_deficit_covered_log(log: &Log) -> Option<AaveV3DeficitCoveredReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_DEFICIT_COVERED_TOPIC {
        return None;
    }
    let reserve = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    if data.len() < 64 {
        return None;
    }
    Some(AaveV3DeficitCoveredReport {
        emitter: log.address(),
        reserve,
        caller: data_address(data, 0)?,
        amount_covered: data_u256(data, 1)?,
    })
}

/// Decode an `IsolationModeTotalDebtUpdated` report. Returns `None` on topic
/// mismatch, < 2 topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_isolation_mode_total_debt_updated_log(
    log: &Log,
) -> Option<AaveV3IsolationModeTotalDebtUpdatedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_ISOLATION_MODE_TOTAL_DEBT_UPDATED_TOPIC {
        return None;
    }
    let asset = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    Some(AaveV3IsolationModeTotalDebtUpdatedReport {
        emitter: log.address(),
        asset,
        total_debt: data_u256(data, 0)?,
    })
}

/// Decode a `PositionManagerApproved` report. Returns `None` on topic mismatch
/// or < 3 topics (both parameters are indexed).
#[must_use]
pub fn decode_aave_position_manager_approved_log(
    log: &Log,
) -> Option<AaveV3PositionManagerApprovedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_POSITION_MANAGER_APPROVED_TOPIC {
        return None;
    }
    Some(AaveV3PositionManagerApprovedReport {
        emitter: log.address(),
        user: topic_address(topics, 1)?,
        position_manager: topic_address(topics, 2)?,
    })
}

/// Decode a `PositionManagerRevoked` report. Returns `None` on topic mismatch
/// or < 3 topics (both parameters are indexed).
#[must_use]
pub fn decode_aave_position_manager_revoked_log(
    log: &Log,
) -> Option<AaveV3PositionManagerRevokedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_POSITION_MANAGER_REVOKED_TOPIC {
        return None;
    }
    Some(AaveV3PositionManagerRevokedReport {
        emitter: log.address(),
        user: topic_address(topics, 1)?,
        position_manager: topic_address(topics, 2)?,
    })
}

/// Decode an aToken `Initialized` report. Returns `None` on topic mismatch,
/// < 3 topics, a truncated head, or a dynamic field that does not fit.
#[must_use]
pub fn decode_aave_a_token_initialized_log(log: &Log) -> Option<AaveV3ATokenInitializedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_ATOKEN_INITIALIZED_TOPIC {
        return None;
    }
    let underlying_asset = topic_address(topics, 1)?;
    let pool = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    if data.len() < 192 {
        return None;
    }
    Some(AaveV3ATokenInitializedReport {
        emitter: log.address(),
        underlying_asset,
        pool,
        treasury: data_address(data, 0)?,
        incentives_controller: data_address(data, 1)?,
        decimals: data_u8(data, 2)?,
        name: data_string(data, 3)?,
        symbol: data_string(data, 4)?,
        params: data_tail(data, 5)?.to_vec(),
    })
}

/// Decode a debt-token `Initialized` report. Returns `None` on topic mismatch,
/// < 3 topics, a truncated head, or a dynamic field that does not fit.
#[must_use]
pub fn decode_aave_debt_token_initialized_log(
    log: &Log,
) -> Option<AaveV3DebtTokenInitializedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_DEBT_TOKEN_INITIALIZED_TOPIC {
        return None;
    }
    let underlying_asset = topic_address(topics, 1)?;
    let pool = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    if data.len() < 160 {
        return None;
    }
    Some(AaveV3DebtTokenInitializedReport {
        emitter: log.address(),
        underlying_asset,
        pool,
        incentives_controller: data_address(data, 0)?,
        decimals: data_u8(data, 1)?,
        name: data_string(data, 2)?,
        symbol: data_string(data, 3)?,
        params: data_tail(data, 4)?.to_vec(),
    })
}

/// Decode a `FeesDistributedToTreasury` report. Returns `None` on topic
/// mismatch, < 3 topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_fees_distributed_to_treasury_log(
    log: &Log,
) -> Option<AaveV3FeesDistributedToTreasuryReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_FEES_DISTRIBUTED_TO_TREASURY_TOPIC {
        return None;
    }
    let gho_treasury = topic_address(topics, 1)?;
    let asset = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    Some(AaveV3FeesDistributedToTreasuryReport {
        emitter: log.address(),
        gho_treasury,
        asset,
        amount: data_u256(data, 0)?,
    })
}

/// Decode a `VariableDebtTokenSet` report. Returns `None` on topic mismatch or
/// < 2 topics (the parameter is indexed).
#[must_use]
pub fn decode_aave_variable_debt_token_set_log(
    log: &Log,
) -> Option<AaveV3VariableDebtTokenSetReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_VARIABLE_DEBT_TOKEN_SET_TOPIC {
        return None;
    }
    Some(AaveV3VariableDebtTokenSetReport {
        emitter: log.address(),
        variable_debt_token: topic_address(topics, 1)?,
    })
}

/// Decode an `ATokenSet` report. Returns `None` on topic mismatch or < 2 topics
/// (the parameter is indexed).
#[must_use]
pub fn decode_aave_a_token_set_log(log: &Log) -> Option<AaveV3ATokenSetReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_ATOKEN_SET_TOPIC {
        return None;
    }
    Some(AaveV3ATokenSetReport {
        emitter: log.address(),
        a_token: topic_address(topics, 1)?,
    })
}

/// Decode a `GhoTreasuryUpdated` report. Returns `None` on topic mismatch or
/// < 3 topics (both parameters are indexed).
#[must_use]
pub fn decode_aave_gho_treasury_updated_log(log: &Log) -> Option<AaveV3GhoTreasuryUpdatedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_GHO_TREASURY_UPDATED_TOPIC {
        return None;
    }
    Some(AaveV3GhoTreasuryUpdatedReport {
        emitter: log.address(),
        old_gho_treasury: topic_address(topics, 1)?,
        new_gho_treasury: topic_address(topics, 2)?,
    })
}

/// Decode a `GHODebtTokenChanged` report. Returns `None` on topic mismatch or
/// < 2 topics (the parameter is indexed).
#[must_use]
pub fn decode_aave_gho_debt_token_changed_log(
    log: &Log,
) -> Option<AaveV3GhoDebtTokenChangedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_GHO_DEBT_TOKEN_CHANGED_TOPIC {
        return None;
    }
    Some(AaveV3GhoDebtTokenChangedReport {
        emitter: log.address(),
        new_debt_token: topic_address(topics, 1)?,
    })
}

/// Decode an `AssetIndexUpdated` report. Returns `None` on topic mismatch, < 2
/// topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_asset_index_updated_log(log: &Log) -> Option<AaveV3AssetIndexUpdatedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_ASSET_INDEX_UPDATED_TOPIC {
        return None;
    }
    let asset = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    Some(AaveV3AssetIndexUpdatedReport {
        emitter: log.address(),
        asset,
        index: data_u256(data, 0)?,
    })
}

/// Decode a `UserIndexUpdated` report. Returns `None` on topic mismatch, < 3
/// topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_user_index_updated_log(log: &Log) -> Option<AaveV3UserIndexUpdatedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_USER_INDEX_UPDATED_TOPIC {
        return None;
    }
    let user = topic_address(topics, 1)?;
    let asset = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    Some(AaveV3UserIndexUpdatedReport {
        emitter: log.address(),
        user,
        asset,
        index: data_u256(data, 0)?,
    })
}

/// Decode an `AssetConfigUpdated` report. Returns `None` on topic mismatch, < 2
/// topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_asset_config_updated_log(log: &Log) -> Option<AaveV3AssetConfigUpdatedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_ASSET_CONFIG_UPDATED_TOPIC {
        return None;
    }
    let asset = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    Some(AaveV3AssetConfigUpdatedReport {
        emitter: log.address(),
        asset,
        emission: data_u256(data, 0)?,
    })
}

/// Decode a `MaxSlashablePercentageChanged` report. Returns `None` on topic
/// mismatch or data < 32 bytes.
#[must_use]
pub fn decode_aave_max_slashable_percentage_changed_log(
    log: &Log,
) -> Option<AaveV3MaxSlashablePercentageChangedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_MAX_SLASHABLE_PERCENTAGE_CHANGED_TOPIC {
        return None;
    }
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    Some(AaveV3MaxSlashablePercentageChangedReport {
        emitter: log.address(),
        new_percentage: data_u256(data, 0)?,
    })
}

/// Decode a `DelegatedPowerChanged` report. Returns `None` on topic mismatch,
/// < 2 topics, or data < 64 bytes.
#[must_use]
pub fn decode_aave_delegated_power_changed_log(
    log: &Log,
) -> Option<AaveV3DelegatedPowerChangedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_DELEGATED_POWER_CHANGED_TOPIC {
        return None;
    }
    let user = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    if data.len() < 64 {
        return None;
    }
    Some(AaveV3DelegatedPowerChangedReport {
        emitter: log.address(),
        user,
        amount: data_u256(data, 0)?,
        delegation_type: data_u8(data, 1)?,
    })
}

/// Decode a `DelegateChanged` report. Returns `None` on topic mismatch, < 3
/// topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_delegate_changed_log(log: &Log) -> Option<AaveV3DelegateChangedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_DELEGATE_CHANGED_TOPIC {
        return None;
    }
    let delegator = topic_address(topics, 1)?;
    let delegatee = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    Some(AaveV3DelegateChangedReport {
        emitter: log.address(),
        delegator,
        delegatee,
        delegation_type: data_u8(data, 0)?,
    })
}

/// Decode a `RewardsAccrued` report. Returns `None` on topic mismatch or data
/// < 64 bytes.
#[must_use]
pub fn decode_aave_rewards_accrued_log(log: &Log) -> Option<AaveV3RewardsAccruedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_REWARDS_ACCRUED_TOPIC {
        return None;
    }
    let data = log.data().data.as_ref();
    if data.len() < 64 {
        return None;
    }
    Some(AaveV3RewardsAccruedReport {
        emitter: log.address(),
        user: data_address(data, 0)?,
        amount: data_u256(data, 1)?,
    })
}

/// Decode a staking `RewardsClaimed` report. Returns `None` on topic mismatch,
/// < 3 topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_rewards_claimed_log(log: &Log) -> Option<AaveV3RewardsClaimedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_STK_AAVE_REWARDS_CLAIMED_TOPIC {
        return None;
    }
    let from = topic_address(topics, 1)?;
    let to = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    Some(AaveV3RewardsClaimedReport {
        emitter: log.address(),
        from,
        to,
        amount: data_u256(data, 0)?,
    })
}

/// Decode a `Cooldown` report. Returns `None` on topic mismatch, < 2 topics, or
/// data < 32 bytes.
#[must_use]
pub fn decode_aave_cooldown_log(log: &Log) -> Option<AaveV3CooldownReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_COOLDOWN_TOPIC {
        return None;
    }
    let user = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    Some(AaveV3CooldownReport {
        emitter: log.address(),
        user,
        amount: data_u256(data, 0)?,
    })
}

/// Decode a `PendingAdminChanged` report. Returns `None` on topic mismatch, < 2
/// topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_pending_admin_changed_log(log: &Log) -> Option<AaveV3PendingAdminChangedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_PENDING_ADMIN_CHANGED_TOPIC {
        return None;
    }
    let new_pending_admin = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    Some(AaveV3PendingAdminChangedReport {
        emitter: log.address(),
        new_pending_admin,
        role: data_u256(data, 0)?,
    })
}

/// Decode a `RoleClaimed` report. Returns `None` on topic mismatch, < 2 topics,
/// or data < 32 bytes.
#[must_use]
pub fn decode_aave_role_claimed_log(log: &Log) -> Option<AaveV3RoleClaimedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_ROLE_CLAIMED_TOPIC {
        return None;
    }
    let new_admin = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    Some(AaveV3RoleClaimedReport {
        emitter: log.address(),
        new_admin,
        role: data_u256(data, 0)?,
    })
}

/// Decode an ERC-20 `Approval` report. Returns `None` on topic mismatch, < 3
/// topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_approval_log(log: &Log) -> Option<AaveV3ApprovalReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_APPROVAL_TOPIC {
        return None;
    }
    let owner = topic_address(topics, 1)?;
    let spender = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    Some(AaveV3ApprovalReport {
        emitter: log.address(),
        owner,
        spender,
        value: data_u256(data, 0)?,
    })
}

/// Decode a `BorrowAllowanceDelegated` report. Returns `None` on topic
/// mismatch, < 4 topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_borrow_allowance_delegated_log(
    log: &Log,
) -> Option<AaveV3BorrowAllowanceDelegatedReport> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_BORROW_ALLOWANCE_DELEGATED_TOPIC {
        return None;
    }
    let from_user = topic_address(topics, 1)?;
    let to_user = topic_address(topics, 2)?;
    let asset = topic_address(topics, 3)?;
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    Some(AaveV3BorrowAllowanceDelegatedReport {
        emitter: log.address(),
        from_user,
        to_user,
        asset,
        amount: data_u256(data, 0)?,
    })
}

// ── the dispatch fn ───────────────────────────────────────────────────────

/// Decode an Aave V3 non-operation report by switching on `log.topics[0]`.
/// Returns `None` for every topic this module does not recognize, including an
/// Aave operation event's topic — the report boundary is closed, never a
/// catch-all. A recognized topic whose payload does not decode is also `None`.
#[must_use]
pub fn decode_aave_report_log(log: &Log) -> Option<DecodedAaveReport> {
    let topic = log.topics().first()?;
    Some(match *topic {
        AAVE_FLASH_LOAN_TOPIC => DecodedAaveReport::FlashLoan(decode_aave_flash_loan_log(log)?),
        AAVE_DEFICIT_COVERED_TOPIC => {
            DecodedAaveReport::DeficitCovered(decode_aave_deficit_covered_log(log)?)
        }
        AAVE_ISOLATION_MODE_TOTAL_DEBT_UPDATED_TOPIC => {
            DecodedAaveReport::IsolationModeTotalDebtUpdated(
                decode_aave_isolation_mode_total_debt_updated_log(log)?,
            )
        }
        AAVE_POSITION_MANAGER_APPROVED_TOPIC => DecodedAaveReport::PositionManagerApproved(
            decode_aave_position_manager_approved_log(log)?,
        ),
        AAVE_POSITION_MANAGER_REVOKED_TOPIC => DecodedAaveReport::PositionManagerRevoked(
            decode_aave_position_manager_revoked_log(log)?,
        ),
        AAVE_ATOKEN_INITIALIZED_TOPIC => {
            DecodedAaveReport::ATokenInitialized(decode_aave_a_token_initialized_log(log)?)
        }
        AAVE_DEBT_TOKEN_INITIALIZED_TOPIC => {
            DecodedAaveReport::DebtTokenInitialized(decode_aave_debt_token_initialized_log(log)?)
        }
        AAVE_FEES_DISTRIBUTED_TO_TREASURY_TOPIC => DecodedAaveReport::FeesDistributedToTreasury(
            decode_aave_fees_distributed_to_treasury_log(log)?,
        ),
        AAVE_VARIABLE_DEBT_TOKEN_SET_TOPIC => {
            DecodedAaveReport::VariableDebtTokenSet(decode_aave_variable_debt_token_set_log(log)?)
        }
        AAVE_ATOKEN_SET_TOPIC => DecodedAaveReport::ATokenSet(decode_aave_a_token_set_log(log)?),
        AAVE_GHO_TREASURY_UPDATED_TOPIC => {
            DecodedAaveReport::GhoTreasuryUpdated(decode_aave_gho_treasury_updated_log(log)?)
        }
        AAVE_GHO_DEBT_TOKEN_CHANGED_TOPIC => {
            DecodedAaveReport::GhoDebtTokenChanged(decode_aave_gho_debt_token_changed_log(log)?)
        }
        AAVE_ASSET_INDEX_UPDATED_TOPIC => {
            DecodedAaveReport::AssetIndexUpdated(decode_aave_asset_index_updated_log(log)?)
        }
        AAVE_USER_INDEX_UPDATED_TOPIC => {
            DecodedAaveReport::UserIndexUpdated(decode_aave_user_index_updated_log(log)?)
        }
        AAVE_ASSET_CONFIG_UPDATED_TOPIC => {
            DecodedAaveReport::AssetConfigUpdated(decode_aave_asset_config_updated_log(log)?)
        }
        AAVE_MAX_SLASHABLE_PERCENTAGE_CHANGED_TOPIC => {
            DecodedAaveReport::MaxSlashablePercentageChanged(
                decode_aave_max_slashable_percentage_changed_log(log)?,
            )
        }
        AAVE_DELEGATED_POWER_CHANGED_TOPIC => {
            DecodedAaveReport::DelegatedPowerChanged(decode_aave_delegated_power_changed_log(log)?)
        }
        AAVE_DELEGATE_CHANGED_TOPIC => {
            DecodedAaveReport::DelegateChanged(decode_aave_delegate_changed_log(log)?)
        }
        AAVE_REWARDS_ACCRUED_TOPIC => {
            DecodedAaveReport::RewardsAccrued(decode_aave_rewards_accrued_log(log)?)
        }
        AAVE_STK_AAVE_REWARDS_CLAIMED_TOPIC => {
            DecodedAaveReport::RewardsClaimed(decode_aave_rewards_claimed_log(log)?)
        }
        AAVE_COOLDOWN_TOPIC => DecodedAaveReport::Cooldown(decode_aave_cooldown_log(log)?),
        AAVE_PENDING_ADMIN_CHANGED_TOPIC => {
            DecodedAaveReport::PendingAdminChanged(decode_aave_pending_admin_changed_log(log)?)
        }
        AAVE_ROLE_CLAIMED_TOPIC => {
            DecodedAaveReport::RoleClaimed(decode_aave_role_claimed_log(log)?)
        }
        AAVE_APPROVAL_TOPIC => DecodedAaveReport::Approval(decode_aave_approval_log(log)?),
        AAVE_BORROW_ALLOWANCE_DELEGATED_TOPIC => DecodedAaveReport::BorrowAllowanceDelegated(
            decode_aave_borrow_allowance_delegated_log(log)?,
        ),
        _ => return None,
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, Log as AlloyLog};

    fn make_log(address: Address, topics: Vec<B256>, data: Vec<u8>) -> Log {
        let inner = AlloyLog::new_unchecked(address, topics, Bytes::from(data));
        Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        }
    }

    fn addr_word(addr: Address) -> B256 {
        let mut word = [0u8; 32];
        word[12..32].copy_from_slice(addr.as_slice());
        B256::new(word)
    }

    fn u256_word(value: U256) -> [u8; 32] {
        value.to_be_bytes::<32>()
    }

    fn u8_word(value: u8) -> [u8; 32] {
        let mut word = [0u8; 32];
        word[31] = value;
        word
    }

    fn u16_word(value: u16) -> [u8; 32] {
        let mut word = [0u8; 32];
        word[30..32].copy_from_slice(&value.to_be_bytes());
        word
    }

    fn u256_hex(hex: &str) -> U256 {
        U256::from_be_bytes::<32>(alloy::hex::decode(hex).unwrap().try_into().unwrap())
    }

    fn data_hex(hex: &str) -> Vec<u8> {
        alloy::hex::decode(hex).unwrap()
    }

    fn encode_tail(data: &mut Vec<u8>, bytes: &[u8]) {
        data.extend_from_slice(&u256_word(U256::from(bytes.len())));
        data.extend_from_slice(bytes);
        let padding = (32 - bytes.len() % 32) % 32;
        data.resize(data.len() + padding, 0);
    }

    #[test]
    fn report_topic_constants_match_their_declared_signatures() {
        let cases: [(&B256, &str); 25] = [
            (
                &AAVE_FLASH_LOAN_TOPIC,
                "FlashLoan(address,address,address,uint256,uint8,uint256,uint16)",
            ),
            (
                &AAVE_DEFICIT_COVERED_TOPIC,
                "DeficitCovered(address,address,uint256)",
            ),
            (
                &AAVE_ISOLATION_MODE_TOTAL_DEBT_UPDATED_TOPIC,
                "IsolationModeTotalDebtUpdated(address,uint256)",
            ),
            (
                &AAVE_POSITION_MANAGER_APPROVED_TOPIC,
                "PositionManagerApproved(address,address)",
            ),
            (
                &AAVE_POSITION_MANAGER_REVOKED_TOPIC,
                "PositionManagerRevoked(address,address)",
            ),
            (
                &AAVE_ATOKEN_INITIALIZED_TOPIC,
                "Initialized(address,address,address,address,uint8,string,string,bytes)",
            ),
            (
                &AAVE_DEBT_TOKEN_INITIALIZED_TOPIC,
                "Initialized(address,address,address,uint8,string,string,bytes)",
            ),
            (
                &AAVE_FEES_DISTRIBUTED_TO_TREASURY_TOPIC,
                "FeesDistributedToTreasury(address,address,uint256)",
            ),
            (
                &AAVE_VARIABLE_DEBT_TOKEN_SET_TOPIC,
                "VariableDebtTokenSet(address)",
            ),
            (&AAVE_ATOKEN_SET_TOPIC, "ATokenSet(address)"),
            (
                &AAVE_GHO_TREASURY_UPDATED_TOPIC,
                "GhoTreasuryUpdated(address,address)",
            ),
            (
                &AAVE_GHO_DEBT_TOKEN_CHANGED_TOPIC,
                "GHODebtTokenChanged(address)",
            ),
            (
                &AAVE_ASSET_INDEX_UPDATED_TOPIC,
                "AssetIndexUpdated(address,uint256)",
            ),
            (
                &AAVE_USER_INDEX_UPDATED_TOPIC,
                "UserIndexUpdated(address,address,uint256)",
            ),
            (
                &AAVE_ASSET_CONFIG_UPDATED_TOPIC,
                "AssetConfigUpdated(address,uint256)",
            ),
            (
                &AAVE_MAX_SLASHABLE_PERCENTAGE_CHANGED_TOPIC,
                "MaxSlashablePercentageChanged(uint256)",
            ),
            (
                &AAVE_DELEGATED_POWER_CHANGED_TOPIC,
                "DelegatedPowerChanged(address,uint256,uint8)",
            ),
            (
                &AAVE_DELEGATE_CHANGED_TOPIC,
                "DelegateChanged(address,address,uint8)",
            ),
            (
                &AAVE_REWARDS_ACCRUED_TOPIC,
                "RewardsAccrued(address,uint256)",
            ),
            (
                &AAVE_STK_AAVE_REWARDS_CLAIMED_TOPIC,
                "RewardsClaimed(address,address,uint256)",
            ),
            (&AAVE_COOLDOWN_TOPIC, "Cooldown(address,uint256)"),
            (
                &AAVE_PENDING_ADMIN_CHANGED_TOPIC,
                "PendingAdminChanged(address,uint256)",
            ),
            (&AAVE_ROLE_CLAIMED_TOPIC, "RoleClaimed(address,uint256)"),
            (&AAVE_APPROVAL_TOPIC, "Approval(address,address,uint256)"),
            (
                &AAVE_BORROW_ALLOWANCE_DELEGATED_TOPIC,
                "BorrowAllowanceDelegated(address,address,address,uint256)",
            ),
        ];
        for (constant, signature) in cases {
            assert_eq!(
                constant,
                &alloy::primitives::keccak256(signature.as_bytes()),
                "topic constant diverged from its declared signature ({signature})"
            );
        }
    }

    #[test]
    fn captured_rev1_gho_token_links_decode_completely() {
        let a_token: Address = "0x00907f9921424583e7ffBfEdf84F92B7B2Be4977"
            .parse()
            .unwrap();
        let debt_token: Address = "0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B"
            .parse()
            .unwrap();
        let stk_aave: Address = "0x4da27a545c0c5B758a6BA100e3a049001de870f5"
            .parse()
            .unwrap();
        let gho_treasury: Address = "0x464c71f6c2f760dda6093dcb91c24c39e5d6e18c"
            .parse()
            .unwrap();

        let variable_debt_token_set = make_log(
            a_token,
            vec![AAVE_VARIABLE_DEBT_TOKEN_SET_TOPIC, addr_word(debt_token)],
            Vec::new(),
        );
        assert_eq!(
            decode_aave_report_log(&variable_debt_token_set),
            Some(DecodedAaveReport::VariableDebtTokenSet(
                AaveV3VariableDebtTokenSetReport {
                    emitter: a_token,
                    variable_debt_token: debt_token,
                }
            ))
        );

        let gho_treasury_updated = make_log(
            a_token,
            vec![
                AAVE_GHO_TREASURY_UPDATED_TOPIC,
                B256::ZERO,
                addr_word(gho_treasury),
            ],
            Vec::new(),
        );
        assert_eq!(
            decode_aave_report_log(&gho_treasury_updated),
            Some(DecodedAaveReport::GhoTreasuryUpdated(
                AaveV3GhoTreasuryUpdatedReport {
                    emitter: a_token,
                    old_gho_treasury: Address::ZERO,
                    new_gho_treasury: gho_treasury,
                }
            ))
        );

        let a_token_set = make_log(
            debt_token,
            vec![AAVE_ATOKEN_SET_TOPIC, addr_word(a_token)],
            Vec::new(),
        );
        assert_eq!(
            decode_aave_report_log(&a_token_set),
            Some(DecodedAaveReport::ATokenSet(AaveV3ATokenSetReport {
                emitter: debt_token,
                a_token,
            }))
        );

        let gho_debt_token_changed = make_log(
            stk_aave,
            vec![AAVE_GHO_DEBT_TOKEN_CHANGED_TOPIC, addr_word(debt_token)],
            Vec::new(),
        );
        assert_eq!(
            decode_aave_report_log(&gho_debt_token_changed),
            Some(DecodedAaveReport::GhoDebtTokenChanged(
                AaveV3GhoDebtTokenChangedReport {
                    emitter: stk_aave,
                    new_debt_token: debt_token,
                }
            ))
        );
    }

    #[test]
    #[expect(clippy::too_many_lines)]
    fn captured_rev5_staking_reports_decode_completely() {
        let stk_aave: Address = "0x4da27a545c0c5B758a6BA100e3a049001de870f5"
            .parse()
            .unwrap();
        let user: Address = "0xca1f6d7d8e902617f8bdd87866e00f9844c40a77"
            .parse()
            .unwrap();
        let index = u256_hex("000000000000000000000000000000000000000000000001696a636d44852dcc");

        let asset_index_updated = make_log(
            stk_aave,
            vec![AAVE_ASSET_INDEX_UPDATED_TOPIC, addr_word(stk_aave)],
            data_hex("000000000000000000000000000000000000000000000001696a636d44852dcc"),
        );
        assert_eq!(
            decode_aave_report_log(&asset_index_updated),
            Some(DecodedAaveReport::AssetIndexUpdated(
                AaveV3AssetIndexUpdatedReport {
                    emitter: stk_aave,
                    asset: stk_aave,
                    index,
                }
            ))
        );

        let user_index_updated = make_log(
            stk_aave,
            vec![
                AAVE_USER_INDEX_UPDATED_TOPIC,
                addr_word(user),
                addr_word(stk_aave),
            ],
            data_hex("000000000000000000000000000000000000000000000001696a636d44852dcc"),
        );
        assert_eq!(
            decode_aave_report_log(&user_index_updated),
            Some(DecodedAaveReport::UserIndexUpdated(
                AaveV3UserIndexUpdatedReport {
                    emitter: stk_aave,
                    user,
                    asset: stk_aave,
                    index,
                }
            ))
        );

        let delegated_power_changed = make_log(
            stk_aave,
            vec![AAVE_DELEGATED_POWER_CHANGED_TOPIC, addr_word(user)],
            data_hex(
                "0000000000000000000000000000000000000000000000056bc75e2d63100000\
                 0000000000000000000000000000000000000000000000000000000000000000",
            ),
        );
        assert_eq!(
            decode_aave_report_log(&delegated_power_changed),
            Some(DecodedAaveReport::DelegatedPowerChanged(
                AaveV3DelegatedPowerChangedReport {
                    emitter: stk_aave,
                    user,
                    amount: U256::from(100_000_000_000_000_000_000u128),
                    delegation_type: 0,
                }
            ))
        );

        let rewards_accrued = make_log(
            stk_aave,
            vec![AAVE_REWARDS_ACCRUED_TOPIC],
            data_hex(
                "0000000000000000000000006b62909ad3f315833a5e5062b670034a3016910b\
                 0000000000000000000000000000000000000000000000006bc387c95b2fc39d",
            ),
        );
        assert_eq!(
            decode_aave_report_log(&rewards_accrued),
            Some(DecodedAaveReport::RewardsAccrued(
                AaveV3RewardsAccruedReport {
                    emitter: stk_aave,
                    user: "0x6b62909ad3f315833a5e5062b670034a3016910b"
                        .parse()
                        .unwrap(),
                    amount: u256_hex(
                        "0000000000000000000000000000000000000000000000006bc387c95b2fc39d"
                    ),
                }
            ))
        );

        let rewards_claimed = make_log(
            stk_aave,
            vec![
                AAVE_STK_AAVE_REWARDS_CLAIMED_TOPIC,
                addr_word(
                    "0x9f07a0233abc178428aa9a18baef3188f093a7d0"
                        .parse()
                        .unwrap(),
                ),
                addr_word(stk_aave),
            ],
            data_hex("0000000000000000000000000000000000000000000000004a868b7352b2fa66"),
        );
        assert_eq!(
            decode_aave_report_log(&rewards_claimed),
            Some(DecodedAaveReport::RewardsClaimed(
                AaveV3RewardsClaimedReport {
                    emitter: stk_aave,
                    from: "0x9f07a0233abc178428aa9a18baef3188f093a7d0"
                        .parse()
                        .unwrap(),
                    to: stk_aave,
                    amount: u256_hex(
                        "0000000000000000000000000000000000000000000000004a868b7352b2fa66"
                    ),
                }
            ))
        );

        let delegate_changed = make_log(
            stk_aave,
            vec![
                AAVE_DELEGATE_CHANGED_TOPIC,
                addr_word(
                    "0x167c606be99dbf5a8af61e1983e5b309e8fa2ae7"
                        .parse()
                        .unwrap(),
                ),
                addr_word(
                    "0x1ae6dcbc88d6f81a7bcfccc7198397d776f3592e"
                        .parse()
                        .unwrap(),
                ),
            ],
            vec![0u8; 32],
        );
        assert_eq!(
            decode_aave_report_log(&delegate_changed),
            Some(DecodedAaveReport::DelegateChanged(
                AaveV3DelegateChangedReport {
                    emitter: stk_aave,
                    delegator: "0x167c606be99dbf5a8af61e1983e5b309e8fa2ae7"
                        .parse()
                        .unwrap(),
                    delegatee: "0x1ae6dcbc88d6f81a7bcfccc7198397d776f3592e"
                        .parse()
                        .unwrap(),
                    delegation_type: 0,
                }
            ))
        );

        let pending_admin: Address = "0x5300a1a15135ea4dc7ad5a167152c01efc9b192a"
            .parse()
            .unwrap();
        let pending_admin_changed = make_log(
            stk_aave,
            vec![AAVE_PENDING_ADMIN_CHANGED_TOPIC, addr_word(pending_admin)],
            vec![0u8; 32],
        );
        assert_eq!(
            decode_aave_report_log(&pending_admin_changed),
            Some(DecodedAaveReport::PendingAdminChanged(
                AaveV3PendingAdminChangedReport {
                    emitter: stk_aave,
                    new_pending_admin: pending_admin,
                    role: U256::ZERO,
                }
            ))
        );

        let role_claimed = make_log(
            stk_aave,
            vec![AAVE_ROLE_CLAIMED_TOPIC, addr_word(pending_admin)],
            vec![0u8; 32],
        );
        assert_eq!(
            decode_aave_report_log(&role_claimed),
            Some(DecodedAaveReport::RoleClaimed(AaveV3RoleClaimedReport {
                emitter: stk_aave,
                new_admin: pending_admin,
                role: U256::ZERO,
            }))
        );

        let cooldown = make_log(
            stk_aave,
            vec![
                AAVE_COOLDOWN_TOPIC,
                addr_word(
                    "0x1f847fd5e08fb559a69280a14e7e904e6dbff81f"
                        .parse()
                        .unwrap(),
                ),
            ],
            data_hex("000000000000000000000000000000000000000000000000b447a45ad02f832e"),
        );
        assert_eq!(
            decode_aave_report_log(&cooldown),
            Some(DecodedAaveReport::Cooldown(AaveV3CooldownReport {
                emitter: stk_aave,
                user: "0x1f847fd5e08fb559a69280a14e7e904e6dbff81f"
                    .parse()
                    .unwrap(),
                amount: u256_hex(
                    "000000000000000000000000000000000000000000000000b447a45ad02f832e"
                ),
            }))
        );

        let asset_config_updated = make_log(
            stk_aave,
            vec![AAVE_ASSET_CONFIG_UPDATED_TOPIC, addr_word(stk_aave)],
            data_hex("000000000000000000000000000000000000000000000000000cf3ddb8be5d55"),
        );
        assert_eq!(
            decode_aave_report_log(&asset_config_updated),
            Some(DecodedAaveReport::AssetConfigUpdated(
                AaveV3AssetConfigUpdatedReport {
                    emitter: stk_aave,
                    asset: stk_aave,
                    emission: u256_hex(
                        "000000000000000000000000000000000000000000000000000cf3ddb8be5d55"
                    ),
                }
            ))
        );

        let max_slashable = make_log(
            stk_aave,
            vec![AAVE_MAX_SLASHABLE_PERCENTAGE_CHANGED_TOPIC],
            data_hex("00000000000000000000000000000000000000000000000000000000000007d0"),
        );
        assert_eq!(
            decode_aave_report_log(&max_slashable),
            Some(DecodedAaveReport::MaxSlashablePercentageChanged(
                AaveV3MaxSlashablePercentageChangedReport {
                    emitter: stk_aave,
                    new_percentage: U256::from(2000u64),
                }
            ))
        );
    }

    #[test]
    fn captured_rev9_pool_position_manager_approval_decodes_completely() {
        let pool: Address = "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2"
            .parse()
            .unwrap();
        let position_manager: Address = "0xd611d10cf51552227cab601a3788a6be52d53ae0"
            .parse()
            .unwrap();
        let log = make_log(
            pool,
            vec![
                AAVE_POSITION_MANAGER_APPROVED_TOPIC,
                addr_word(position_manager),
                addr_word(position_manager),
            ],
            Vec::new(),
        );
        assert_eq!(
            decode_aave_report_log(&log),
            Some(DecodedAaveReport::PositionManagerApproved(
                AaveV3PositionManagerApprovedReport {
                    emitter: pool,
                    user: position_manager,
                    position_manager,
                }
            ))
        );
    }

    #[test]
    fn captured_rev11_pool_position_manager_revocation_decodes_completely() {
        let pool: Address = "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2"
            .parse()
            .unwrap();
        let user: Address = "0x2c143f9c8a3da16dbabbb7bf1679cb8e070a3245"
            .parse()
            .unwrap();
        let position_manager: Address = "0x366d12b5c7840d88212e6f78c8cd578275be8f8d"
            .parse()
            .unwrap();
        let log = make_log(
            pool,
            vec![
                AAVE_POSITION_MANAGER_REVOKED_TOPIC,
                addr_word(user),
                addr_word(position_manager),
            ],
            Vec::new(),
        );
        assert_eq!(
            decode_aave_report_log(&log),
            Some(DecodedAaveReport::PositionManagerRevoked(
                AaveV3PositionManagerRevokedReport {
                    emitter: pool,
                    user,
                    position_manager,
                }
            ))
        );
    }

    #[test]
    fn declared_pool_reports_decode_completely() {
        let pool = Address::from([0x11; 20]);
        let target = Address::from([0x12; 20]);
        let initiator = Address::from([0x13; 20]);
        let asset = Address::from([0x14; 20]);
        let amount = U256::from(1_000_000u64);
        let premium = U256::from(500u64);
        let referral_code = 42u16;
        let mut flash_loan_data = vec![0u8; 128];
        flash_loan_data[12..32].copy_from_slice(initiator.as_slice());
        flash_loan_data[32..64].copy_from_slice(&u256_word(amount));
        flash_loan_data[64..96].copy_from_slice(&u8_word(2));
        flash_loan_data[96..128].copy_from_slice(&u256_word(premium));
        let flash_loan = make_log(
            pool,
            vec![
                AAVE_FLASH_LOAN_TOPIC,
                addr_word(target),
                addr_word(asset),
                B256::new(u16_word(referral_code)),
            ],
            flash_loan_data,
        );
        assert_eq!(
            decode_aave_report_log(&flash_loan),
            Some(DecodedAaveReport::FlashLoan(AaveV3FlashLoanReport {
                emitter: pool,
                target,
                asset,
                referral_code,
                initiator,
                amount,
                interest_rate_mode: 2,
                premium,
            }))
        );

        let reserve = Address::from([0x15; 20]);
        let caller = Address::from([0x16; 20]);
        let mut deficit_data = vec![0u8; 64];
        deficit_data[12..32].copy_from_slice(caller.as_slice());
        deficit_data[32..64].copy_from_slice(&u256_word(amount));
        let deficit_covered = make_log(
            pool,
            vec![AAVE_DEFICIT_COVERED_TOPIC, addr_word(reserve)],
            deficit_data,
        );
        assert_eq!(
            decode_aave_report_log(&deficit_covered),
            Some(DecodedAaveReport::DeficitCovered(
                AaveV3DeficitCoveredReport {
                    emitter: pool,
                    reserve,
                    caller,
                    amount_covered: amount,
                }
            ))
        );

        let isolation = make_log(
            pool,
            vec![
                AAVE_ISOLATION_MODE_TOTAL_DEBT_UPDATED_TOPIC,
                addr_word(asset),
            ],
            u256_word(premium).to_vec(),
        );
        assert_eq!(
            decode_aave_report_log(&isolation),
            Some(DecodedAaveReport::IsolationModeTotalDebtUpdated(
                AaveV3IsolationModeTotalDebtUpdatedReport {
                    emitter: pool,
                    asset,
                    total_debt: premium,
                }
            ))
        );
    }

    #[test]
    fn declared_token_reports_decode_completely() {
        let a_token = Address::from([0x21; 20]);
        let gho_treasury = Address::from([0x22; 20]);
        let underlying = Address::from([0x23; 20]);
        let amount = U256::from(7_000u64);

        let fees_distributed = make_log(
            a_token,
            vec![
                AAVE_FEES_DISTRIBUTED_TO_TREASURY_TOPIC,
                addr_word(gho_treasury),
                addr_word(underlying),
            ],
            u256_word(amount).to_vec(),
        );
        assert_eq!(
            decode_aave_report_log(&fees_distributed),
            Some(DecodedAaveReport::FeesDistributedToTreasury(
                AaveV3FeesDistributedToTreasuryReport {
                    emitter: a_token,
                    gho_treasury,
                    asset: underlying,
                    amount,
                }
            ))
        );

        let from_user = Address::from([0x24; 20]);
        let to_user = Address::from([0x25; 20]);
        let borrow_allowance = make_log(
            a_token,
            vec![
                AAVE_BORROW_ALLOWANCE_DELEGATED_TOPIC,
                addr_word(from_user),
                addr_word(to_user),
                addr_word(underlying),
            ],
            u256_word(amount).to_vec(),
        );
        assert_eq!(
            decode_aave_report_log(&borrow_allowance),
            Some(DecodedAaveReport::BorrowAllowanceDelegated(
                AaveV3BorrowAllowanceDelegatedReport {
                    emitter: a_token,
                    from_user,
                    to_user,
                    asset: underlying,
                    amount,
                }
            ))
        );

        let owner = Address::from([0x26; 20]);
        let spender = Address::from([0x27; 20]);
        let approval = make_log(
            a_token,
            vec![AAVE_APPROVAL_TOPIC, addr_word(owner), addr_word(spender)],
            u256_word(amount).to_vec(),
        );
        assert_eq!(
            decode_aave_report_log(&approval),
            Some(DecodedAaveReport::Approval(AaveV3ApprovalReport {
                emitter: a_token,
                owner,
                spender,
                value: amount,
            }))
        );
    }

    #[test]
    fn initialized_reports_decode_dynamic_payloads() {
        let a_token = Address::from([0x31; 20]);
        let underlying = Address::from([0x32; 20]);
        let pool = Address::from([0x33; 20]);
        let treasury = Address::from([0x34; 20]);
        let incentives = Address::from([0x35; 20]);
        let name = "Aave interest bearing WETH";
        let symbol = "aEthWETH";
        let params = [0xde_u8, 0xad, 0xbe, 0xef];

        let head_len = 6 * 32;
        let name_offset = head_len;
        let symbol_offset = name_offset + 32 + 32;
        let params_offset = symbol_offset + 32 + 32;
        let mut data = vec![0u8; head_len];
        data[12..32].copy_from_slice(treasury.as_slice());
        data[44..64].copy_from_slice(incentives.as_slice());
        data[95] = 18;
        data[96..128].copy_from_slice(&u256_word(U256::from(name_offset)));
        data[128..160].copy_from_slice(&u256_word(U256::from(symbol_offset)));
        data[160..192].copy_from_slice(&u256_word(U256::from(params_offset)));
        encode_tail(&mut data, name.as_bytes());
        encode_tail(&mut data, symbol.as_bytes());
        encode_tail(&mut data, &params);

        let a_token_initialized = make_log(
            a_token,
            vec![
                AAVE_ATOKEN_INITIALIZED_TOPIC,
                addr_word(underlying),
                addr_word(pool),
            ],
            data,
        );
        assert_eq!(
            decode_aave_report_log(&a_token_initialized),
            Some(DecodedAaveReport::ATokenInitialized(
                AaveV3ATokenInitializedReport {
                    emitter: a_token,
                    underlying_asset: underlying,
                    pool,
                    treasury,
                    incentives_controller: incentives,
                    decimals: 18,
                    name: name.to_string(),
                    symbol: symbol.to_string(),
                    params: params.to_vec(),
                }
            ))
        );

        let debt_token = Address::from([0x36; 20]);
        let debt_name = "Aave variable debt bearing WETH";
        let debt_symbol = "variableDebtEthWETH";
        let debt_head_len = 5 * 32;
        let debt_name_offset = debt_head_len;
        let debt_symbol_offset = debt_name_offset + 32 + 32;
        let debt_params_offset = debt_symbol_offset + 32 + 32;
        let mut debt_data = vec![0u8; debt_head_len];
        debt_data[12..32].copy_from_slice(incentives.as_slice());
        debt_data[63] = 18;
        debt_data[64..96].copy_from_slice(&u256_word(U256::from(debt_name_offset)));
        debt_data[96..128].copy_from_slice(&u256_word(U256::from(debt_symbol_offset)));
        debt_data[128..160].copy_from_slice(&u256_word(U256::from(debt_params_offset)));
        encode_tail(&mut debt_data, debt_name.as_bytes());
        encode_tail(&mut debt_data, debt_symbol.as_bytes());
        encode_tail(&mut debt_data, &params);

        let debt_token_initialized = make_log(
            debt_token,
            vec![
                AAVE_DEBT_TOKEN_INITIALIZED_TOPIC,
                addr_word(underlying),
                addr_word(pool),
            ],
            debt_data,
        );
        assert_eq!(
            decode_aave_report_log(&debt_token_initialized),
            Some(DecodedAaveReport::DebtTokenInitialized(
                AaveV3DebtTokenInitializedReport {
                    emitter: debt_token,
                    underlying_asset: underlying,
                    pool,
                    incentives_controller: incentives,
                    decimals: 18,
                    name: debt_name.to_string(),
                    symbol: debt_symbol.to_string(),
                    params: params.to_vec(),
                }
            ))
        );
    }

    #[test]
    fn initialized_reports_reject_unfittable_dynamic_fields() {
        let token = Address::from([0x41; 20]);
        let topics = vec![
            AAVE_ATOKEN_INITIALIZED_TOPIC,
            addr_word(Address::from([0x42; 20])),
            addr_word(Address::from([0x43; 20])),
        ];

        let truncated_head = make_log(token, topics.clone(), vec![0u8; 160]);
        assert!(
            decode_aave_report_log(&truncated_head).is_none(),
            "a truncated Initialized head is not a decode"
        );

        let name = b"name";
        let mut data = vec![0u8; 6 * 32];
        data[96..128].copy_from_slice(&u256_word(U256::from(6 * 32)));
        data[128..160].copy_from_slice(&u256_word(U256::from(6 * 32 + 64)));
        data[160..192].copy_from_slice(&u256_word(U256::from(6 * 32 + 128)));
        encode_tail(&mut data, name);
        let truncated_tail = make_log(token, topics.clone(), data[..data.len() - 8].to_vec());
        assert!(
            decode_aave_report_log(&truncated_tail).is_none(),
            "a dynamic field that overruns the data is not a decode"
        );

        let mut invalid_utf8 = vec![0u8; 6 * 32];
        invalid_utf8[96..128].copy_from_slice(&u256_word(U256::from(6 * 32)));
        invalid_utf8[128..160].copy_from_slice(&u256_word(U256::from(6 * 32 + 64)));
        invalid_utf8[160..192].copy_from_slice(&u256_word(U256::from(6 * 32 + 128)));
        encode_tail(&mut invalid_utf8, &[0xff, 0xfe]);
        encode_tail(&mut invalid_utf8, b"sym");
        encode_tail(&mut invalid_utf8, &[]);
        let bad_string = make_log(token, topics, invalid_utf8);
        assert!(
            decode_aave_report_log(&bad_string).is_none(),
            "invalid UTF-8 in a string field is not a decode"
        );
    }

    #[test]
    fn known_report_decodes_identically_on_a_later_revision_carrier() {
        let pool = Address::from([0x51; 20]);
        let reserve = Address::from([0x52; 20]);
        let caller = Address::from([0x53; 20]);
        let mut data = vec![0u8; 64];
        data[12..32].copy_from_slice(caller.as_slice());
        data[32..64].copy_from_slice(&u256_word(U256::from(9u64)));
        let mut earlier = make_log(
            pool,
            vec![AAVE_DEFICIT_COVERED_TOPIC, addr_word(reserve)],
            data.clone(),
        );
        earlier.block_number = Some(17_699_249);
        earlier.block_hash = Some(B256::repeat_byte(0x01));
        let mut later = make_log(
            pool,
            vec![AAVE_DEFICIT_COVERED_TOPIC, addr_word(reserve)],
            data,
        );
        later.block_number = Some(26_495_525);
        later.block_hash = Some(B256::repeat_byte(0x02));

        let decoded = decode_aave_report_log(&earlier);
        assert!(decoded.is_some());
        assert_eq!(decoded, decode_aave_report_log(&later));
    }

    macro_rules! assert_foreign_topic_rejected {
        ($log:expr; $($decoder:path),* $(,)?) => {
            $(
                assert!(
                    $decoder(&$log).is_none(),
                    concat!(stringify!($decoder), " decoded a foreign report topic")
                );
            )*
        };
    }

    #[test]
    fn report_decoders_reject_a_foreign_topic() {
        let foreign = make_log(
            Address::from([0x61; 20]),
            vec![AAVE_FLASH_LOAN_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            vec![0u8; 128],
        );
        assert!(decode_aave_flash_loan_log(&foreign).is_some());
        assert_foreign_topic_rejected!(
            foreign;
            decode_aave_deficit_covered_log,
            decode_aave_isolation_mode_total_debt_updated_log,
            decode_aave_position_manager_approved_log,
            decode_aave_position_manager_revoked_log,
            decode_aave_a_token_initialized_log,
            decode_aave_debt_token_initialized_log,
            decode_aave_fees_distributed_to_treasury_log,
            decode_aave_variable_debt_token_set_log,
            decode_aave_a_token_set_log,
            decode_aave_gho_treasury_updated_log,
            decode_aave_gho_debt_token_changed_log,
            decode_aave_asset_index_updated_log,
            decode_aave_user_index_updated_log,
            decode_aave_asset_config_updated_log,
            decode_aave_max_slashable_percentage_changed_log,
            decode_aave_delegated_power_changed_log,
            decode_aave_delegate_changed_log,
            decode_aave_rewards_accrued_log,
            decode_aave_rewards_claimed_log,
            decode_aave_cooldown_log,
            decode_aave_pending_admin_changed_log,
            decode_aave_role_claimed_log,
            decode_aave_approval_log,
            decode_aave_borrow_allowance_delegated_log,
        );
    }

    #[test]
    fn unknown_and_operation_topics_fail_closed() {
        let unknown = make_log(
            Address::from([0x71; 20]),
            vec![B256::repeat_byte(0x42)],
            vec![0u8; 256],
        );
        assert!(
            decode_aave_report_log(&unknown).is_none(),
            "an unknown topic is not a report"
        );

        let operation_event = make_log(
            Address::from([0x72; 20]),
            vec![
                crate::aave_event_decoder::AAVE_SUPPLY_TOPIC,
                B256::ZERO,
                B256::ZERO,
                B256::ZERO,
            ],
            vec![0u8; 64],
        );
        assert!(
            decode_aave_report_log(&operation_event).is_none(),
            "an operation event's topic is not a report"
        );
    }
}

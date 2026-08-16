//! Aave V3 event-log decoders (34 events across 8 enums).
//!
//! The chunk-loop fetcher (epic `AZGJUN`, task `ECFB5C`) decodes raw Aave V3
//! event logs into typed `DecodedAaveEvent` variants (raw `Address`/`U256`
//! fields — no id resolution; the orchestrator `6SWY4R` resolves
//! address→id via `get_or_create_*_on_conn` within the chunk transaction).
//! This module is the pure-Rust decode leaf — alloy-only, no `pyo3`/`tokio`/
//! `degenbot-core`/`degenbot-abi` (the same constraint as
//! [`crate::pool_created_decoder`] + [`crate::v3_mint_burn_decoder`]).
//!
//! The returned `DecodedAaveEvent` variants align by-name with the
//! `AaveChunkEvent` variants from CXRGX4/UR7QNL/5Z3QQ2/RYKCC4 — the decoder
//! emits raw fields (addresses + U256 amounts + the emitter address); the
//! orchestrator/parser maps them to resolved ids before constructing
//! `AaveChunkEvent` variants the apply layer consumes.
//!
//! Event signatures + topic constants are reproduced from the Python
//! `src/degenbot/aave/events.py` (the canonical degenbot source), which
//! itself computes them as `keccak256` of the canonical Solidity event
//! signature. The 34 topic-hash constants below are byte-for-byte copies of
//! the Python `HexBytes(...)` values; a `parity_topic_constants_match_python`
//! test pins every constant against the `events.py` source of truth.
//!
//! # Decode-error handling (mirrors precedent)
//!
//! Like [`crate::pool_created_decoder`], the per-event decode fns return
//! `Option<T>` — `None` if the topic doesn't match, the topics are
//! insufficient, or the data is too short (mirrors the precedent's
//! None-on-mismatch contract — there is NO `DecodeError` type; the
//! `revert` module is for revert-data classification, unrelated). The
//! [`decode_aave_log`] dispatch fn returns `None` for unknown topics so the
//! orchestrator can filter unrelated logs without crashing (the orchestrator
//! fetches logs RPC-filtered by topic list; an unknown topic landing here is
//! a caller filter bug or an unrelated emitter — surfacing it as `None`
//! preserves the precedent ethos of "let the caller decide what to do").
//!
//! # Event scope (ALL 34 events)
//!
//! ## `AaveV3PoolEvent` (11) — the position + state events
//!
//! ```text
//! event Supply(address indexed reserve, address user, address indexed onBehalfOf, uint256 amount, uint16 indexed referralCode);
//! event Borrow(address indexed reserve, address user, address indexed onBehalfOf, uint256 amount, uint8 interestRateMode, uint256 borrowRate, uint16 indexed referralCode);
//! event Repay(address indexed reserve, address indexed user, address indexed repayer, uint256 amount, bool useATokens);
//! event Withdraw(address indexed reserve, address indexed user, address indexed to, uint256 amount);
//! event LiquidationCall(address indexed collateralAsset, address indexed debtAsset, address indexed user, uint256 debtToCover, uint256 liquidatedCollateralAmount, address liquidator, bool receiveAToken);
//! event MintedToTreasury(address indexed reserve, uint256 amountMinted);
//! event DeficitCreated(address indexed user, address indexed debtAsset, uint256 amountCreated);
//! event ReserveDataUpdated(address indexed reserve, uint256 liquidityRate, uint256 stableBorrowRate, uint256 variableBorrowRate, uint256 liquidityIndex, uint256 variableBorrowIndex);
//! event ReserveUsedAsCollateralEnabled(address indexed reserve, address indexed user);
//! event ReserveUsedAsCollateralDisabled(address indexed reserve, address indexed user);
//! event UserEModeSet(address indexed user, uint8 categoryId);
//! ```
//!
//! ## `AaveV3ScaledTokenEvent` (3) — aToken/vToken Mint/Burn/BalanceTransfer
//!
//! ```text
//! event Mint(address indexed caller, address indexed onBehalfOf, uint256 value, uint256 balanceIncrease, uint256 index);
//! event Burn(address indexed from, address indexed target, uint256 value, uint256 balanceIncrease, uint256 index);
//! event BalanceTransfer(address indexed from, address indexed to, uint256 value, uint256 index);
//! ```
//!
//! ## `AaveV3PoolConfigEvent` (12) — the config events
//!
//! ```text
//! event CollateralConfigurationChanged(address indexed asset, uint256 ltv, uint256 liquidationThreshold, uint256 liquidationBonus);
//! event EModeCategoryAdded(uint8 indexed categoryId, uint256 ltv, uint256 liquidationThreshold, uint256 liquidationBonus, address oracle, string label);
//! event EModeAssetCategoryChanged(address indexed asset, uint8 oldCategoryId, uint8 newCategoryId);
//! event AssetCollateralInEModeChanged(address indexed asset, uint8 categoryId, bool collateral);
//! event ReserveInitialized(address indexed asset, address indexed aToken, address stableDebtToken, address variableDebtToken, address interestRateStrategyAddress);
//! event PoolUpdated(address indexed oldAddress, address indexed newAddress);
//! event PoolConfiguratorUpdated(address indexed oldAddress, address indexed newAddress);
//! event PoolDataProviderUpdated(address indexed oldAddress, address indexed newAddress);
//! event PriceOracleUpdated(address indexed oldAddress, address indexed newAddress);
//! event ProxyCreated(bytes32 indexed id, address indexed proxyAddress, address indexed implementationAddress);
//! event AddressSet(bytes32 indexed id, address indexed oldAddress, address indexed newAddress);
//! event Upgraded(address indexed implementation);
//! ```
//!
//! ## `AaveV3GhoDebtTokenEvent` (3)
//!
//! ```text
//! event DiscountPercentUpdated(address indexed user, uint256 oldDiscountPercent, uint256 indexed newDiscountPercent);
//! event DiscountRateStrategyUpdated(address indexed oldDiscountRateStrategy, address indexed newDiscountRateStrategy);
//! event DiscountTokenUpdated(address indexed oldDiscountToken, address indexed newDiscountToken);
//! ```
//!
//! ## `AaveV3StkAaveEvent` (2)
//!
//! ```text
//! event Staked(address indexed from, address indexed onBehalfOf, uint256 amount);
//! event Redeem(address indexed from, address indexed to, uint256 amount);
//! ```
//!
//! ## `AaveV3RewardsControllerEvent` (1)
//!
//! ```text
//! event RewardsClaimed(address indexed user, address indexed reward, address indexed to, address claimer, uint256 amount);
//! ```
//!
//! ## `AaveV3OracleEvent` (1)
//!
//! ```text
//! event AssetSourceUpdated(address indexed asset, address indexed source);
//! ```
//!
//! ## `ERC20Event` (1)
//!
//! ```text
//! event Transfer(address indexed from, address indexed to, uint256 value);
//! ```

use alloy::primitives::{b256, Address, B256, U256};
use alloy::rpc::types::Log;

// ── topic constants (35 — keccak256 of the canonical event signatures) ─────
//
// Each constant is the keccak256 of its canonical Solidity event signature
// (byte-for-byte copies of the Python `events.py` values; pinned by the
// `parity_topic_constants_match_python` test). The one-line doc per constant
// states that signature so a plain-text search for any event name lands on
// both its topic constant and its decoder in one step.

/// `keccak256("Borrow(address,address,address,uint256,uint8,uint256,uint16)")`.
pub const AAVE_BORROW_TOPIC: B256 =
    b256!("0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0");
/// `keccak256("DeficitCreated(address,address,address,uint256)")`.
pub const AAVE_DEFICIT_CREATED_TOPIC: B256 =
    b256!("0x2bccfb3fad376d59d7accf970515eb77b2f27b082c90ed0fb15583dd5a942699");
/// `keccak256("LiquidationCall(address,address,address,uint256,uint256,address,bool)")`.
pub const AAVE_LIQUIDATION_CALL_TOPIC: B256 =
    b256!("0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286");
/// `keccak256("MintedToTreasury(address,uint256)")`.
pub const AAVE_MINTED_TO_TREASURY_TOPIC: B256 =
    b256!("0xbfa21aa5d5f9a1f0120a95e7c0749f389863cbdbfff531aa7339077a5bc919de");
/// `keccak256("Repay(address,address,address,uint256,bool)")`.
pub const AAVE_REPAY_TOPIC: B256 =
    b256!("0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051");
/// `keccak256("ReserveDataUpdated(address,uint256,uint256,uint256,uint256,uint256)")`.
pub const AAVE_RESERVE_DATA_UPDATED_TOPIC: B256 =
    b256!("0x804c9b842b2748a22bb64b345453a3de7ca54a6ca45ce00d415894979e22897a");
/// `keccak256("ReserveUsedAsCollateralDisabled(address,address)")`.
pub const AAVE_RESERVE_USED_AS_COLLATERAL_DISABLED_TOPIC: B256 =
    b256!("0x44c58d81365b66dd4b1a7f36c25aa97b8c71c361ee4937adc1a00000227db5dd");
/// `keccak256("ReserveUsedAsCollateralEnabled(address,address)")`.
pub const AAVE_RESERVE_USED_AS_COLLATERAL_ENABLED_TOPIC: B256 =
    b256!("0x00058a56ea94653cdf4f152d227ace22d4c00ad99e2a43f58cb7d9e3feb295f2");
/// `keccak256("Supply(address,address,address,uint256,uint16)")`.
pub const AAVE_SUPPLY_TOPIC: B256 =
    b256!("0x2b627736bca15cd5381dcf80b0bf11fd197d01a037c52b927a881a10fb73ba61");
/// `keccak256("UserEModeSet(address,uint8)")`.
pub const AAVE_USER_E_MODE_SET_TOPIC: B256 =
    b256!("0xd728da875fc88944cbf17638bcbe4af0eedaef63becd1d1c57cc097eb4608d84");
/// `keccak256("Withdraw(address,address,address,uint256)")`.
pub const AAVE_WITHDRAW_TOPIC: B256 =
    b256!("0x3115d1449a7b732c986cba18244e897a450f61e1bb8d589cd2e69e6c8924f9f7");
/// `keccak256("BalanceTransfer(address,address,uint256,uint256)")`.
pub const AAVE_BALANCE_TRANSFER_TOPIC: B256 =
    b256!("0x4beccb90f994c31aced7a23b5611020728a23d8ec5cddd1a3e9d97b96fda8666");
/// `keccak256("Burn(address,address,uint256,uint256,uint256)")`.
pub const AAVE_BURN_TOPIC: B256 =
    b256!("0x4cf25bc1d991c17529c25213d3cc0cda295eeaad5f13f361969b12ea48015f90");
/// `keccak256("Mint(address,address,uint256,uint256,uint256)")`.
pub const AAVE_MINT_TOPIC: B256 =
    b256!("0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196");
/// `keccak256("AddressSet(bytes32,address,address)")`.
pub const AAVE_ADDRESS_SET_TOPIC: B256 =
    b256!("0x9ef0e8c8e52743bb38b83b17d9429141d494b8041ca6d616a6c77cebae9cd8b7");
/// `keccak256("AssetCollateralInEModeChanged(address,uint8,bool)")`.
pub const AAVE_ASSET_COLLATERAL_IN_EMODE_CHANGED_TOPIC: B256 =
    b256!("0x79409190108b26fcb0e4570f8e240f627bf18fd01a55f751010224d5bd486098");
/// `keccak256("CollateralConfigurationChanged(address,uint256,uint256,uint256)")`.
pub const AAVE_COLLATERAL_CONFIGURATION_CHANGED_TOPIC: B256 =
    b256!("0x637febbda9275aea2e85c0ff690444c8d87eb2e8339bbede9715abcc89cb0995");
/// `keccak256("EModeAssetCategoryChanged(address,uint8,uint8)")`.
pub const AAVE_EMODE_ASSET_CATEGORY_CHANGED_TOPIC: B256 =
    b256!("0x5bb69795b6a2ea222d73a5f8939c23471a1f85a99c7ca43c207f1b71f10c6264");
/// `keccak256("EModeCategoryAdded(uint8,uint256,uint256,uint256,address,string)")`.
pub const AAVE_EMODE_CATEGORY_ADDED_TOPIC: B256 =
    b256!("0x0acf8b4a3cace10779798a89a206a0ae73a71b63acdd3be2801d39c2ef7ab3cb");
/// `keccak256("PoolConfiguratorUpdated(address,address)")`.
pub const AAVE_POOL_CONFIGURATOR_UPDATED_TOPIC: B256 =
    b256!("0x8932892569eba59c8382a089d9b732d1f49272878775235761a2a6b0309cd465");
/// `keccak256("PoolDataProviderUpdated(address,address)")`.
pub const AAVE_POOL_DATA_PROVIDER_UPDATED_TOPIC: B256 =
    b256!("0xc853974cfbf81487a14a23565917bee63f527853bcb5fa54f2ae1cdf8a38356d");
/// `keccak256("PoolUpdated(address,address)")`.
pub const AAVE_POOL_UPDATED_TOPIC: B256 =
    b256!("0x90affc163f1a2dfedcd36aa02ed992eeeba8100a4014f0b4cdc20ea265a66627");
/// `keccak256("PriceOracleUpdated(address,address)")`.
pub const AAVE_PRICE_ORACLE_UPDATED_TOPIC: B256 =
    b256!("0x56b5f80d8cac1479698aa7d01605fd6111e90b15fc4d2b377417f46034876cbd");
/// `keccak256("ProxyCreated(bytes32,address,address)")`.
pub const AAVE_PROXY_CREATED_TOPIC: B256 =
    b256!("0x4a465a9bd819d9662563c1e11ae958f8109e437e7f4bf1c6ef0b9a7b3f35d478");
/// `keccak256("ReserveInitialized(address,address,address,address,address)")`.
pub const AAVE_RESERVE_INITIALIZED_TOPIC: B256 =
    b256!("0x3a0ca721fc364424566385a1aa271ed508cc2c0949c2272575fb3013a163a45f");
/// `keccak256("Upgraded(address)")`.
pub const AAVE_UPGRADED_TOPIC: B256 =
    b256!("0xbc7cd75a20ee27fd9adebab32041f755214dbc6bffa90cc0225b39da2e5c2d3b");
/// `keccak256("DiscountPercentUpdated(address,uint256,uint256)")`.
pub const AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC: B256 =
    b256!("0x74ab9665e7c36c29ddb78ef88a3e2eac73d35b8b16de7bc573e313e320104956");
/// `keccak256("DiscountRateStrategyUpdated(address,address)")`.
pub const AAVE_DISCOUNT_RATE_STRATEGY_UPDATED_TOPIC: B256 =
    b256!("0x194bd59f47b230edccccc2be58b92dde3a5dadd835751a621af59006928bccef");
/// `keccak256("DiscountTokenUpdated(address,address)")`.
pub const AAVE_DISCOUNT_TOKEN_UPDATED_TOPIC: B256 =
    b256!("0x6b489e1dbfbe36f55c511c098bcc9d92fec7f04f74ceb75018697ab68f7d3529");
/// `keccak256("Redeem(address,address,uint256)")`.
pub const AAVE_REDEEM_TOPIC: B256 =
    b256!("0x3f693fff038bb8a046aa76d9516190ac7444f7d69cf952c4cbdc086fdef2d6fc");
/// `keccak256("Staked(address,address,uint256)")`.
pub const AAVE_STAKED_TOPIC: B256 =
    b256!("0x6c86f3fd5118b3aa8bb4f389a617046de0a3d3d477de1a1673d227f802f616dc");
/// `keccak256("RewardsClaimed(address,address,address,address,uint256)")`.
pub const AAVE_REWARDS_CLAIMED_TOPIC: B256 =
    b256!("0xc052130bc4ef84580db505783484b067ea8b71b3bca78a7e12db7aea8658f004");
/// `keccak256("AssetSourceUpdated(address,address)")`.
pub const AAVE_ASSET_SOURCE_UPDATED_TOPIC: B256 =
    b256!("0x22c5b7b2d8561d39f7f210b6b326a1aa69f15311163082308ac4877db6339dc1");
/// `keccak256("Transfer(address,address,uint256)")` — the canonical ERC-20 transfer topic.
pub const ERC20_TRANSFER_TOPIC: B256 =
    b256!("0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

// ── helper fns ────────────────────────────────────────────────────────────

/// Extract an `Address` from `topics[idx]` (an indexed `address` arg occupies
/// a full 32-byte topic word; `Address::from_word` right-aligns the 20-byte
/// payload). Returns `None` if `topics` has no `idx`.
fn topic_address(topics: &[B256], idx: usize) -> Option<Address> {
    topics.get(idx).copied().map(Address::from_word)
}

/// Extract a `U256` from `topics[idx]` (an indexed `uint256`/`bytes32` arg
/// occupies the full word). Returns `None` if `topics` has no `idx`.
fn topic_u256(topics: &[B256], idx: usize) -> Option<U256> {
    topics.get(idx).map(|w| U256::from_be_bytes::<32>(w.0))
}

/// Extract a `B256` from `topics[idx]` (an indexed `bytes32` arg verbatim).
fn topic_b256(topics: &[B256], idx: usize) -> Option<B256> {
    topics.get(idx).copied()
}

/// Extract a `uint8` (or `uint8`-typed enum like `InterestRateMode`) from the
/// low byte of `topics[idx]` (uint8 is sign-extended to uint256 in the topic
/// word). Returns `None` if `topics` has no `idx`.
fn topic_u8(topics: &[B256], idx: usize) -> Option<u8> {
    let w = topics.get(idx)?;
    Some(w.0[31])
}

/// Extract a `uint16` from the low 2 bytes of `topics[idx]` (uint16 is
/// sign-extended to uint256 in the topic word; takes bytes 30..32).
fn topic_u16(topics: &[B256], idx: usize) -> Option<u16> {
    let w = topics.get(idx)?;
    Some(u16::from_be_bytes([w.0[30], w.0[31]]))
}

/// Extract an `Address` from the data word at byte offset `word_idx * 32`
/// (an `address` is right-aligned in its 32-byte word; bytes 12..32 of the
/// word). Returns `None` if the data is too short.
fn data_address(data: &[u8], word_idx: usize) -> Option<Address> {
    let start = word_idx.checked_mul(32)?;
    let end = start.checked_add(32)?;
    let word = data.get(start..end)?;
    Some(Address::from_slice(&word[12..32]))
}

/// Extract a `U256` from the data word at byte offset `word_idx * 32`.
/// Returns `None` if the data is too short.
fn data_u256(data: &[u8], word_idx: usize) -> Option<U256> {
    let start = word_idx.checked_mul(32)?;
    let end = start.checked_add(32)?;
    let word: [u8; 32] = data.get(start..end)?.try_into().ok()?;
    Some(U256::from_be_bytes::<32>(word))
}

/// Extract a `uint8` from byte 31 of data word `word_idx`.
fn data_u8(data: &[u8], word_idx: usize) -> Option<u8> {
    let start = word_idx.checked_mul(32)?;
    Some(*data.get(start.checked_add(31)?)?)
}

/// Extract a `bool` from byte 31 of data word `word_idx` (non-zero = true).
fn data_bool(data: &[u8], word_idx: usize) -> Option<bool> {
    Some(data_u8(data, word_idx)? != 0)
}

/// Extract the `topics` slice from a `Log` (alloy exposes `log.topics()`
/// returning `&[B256]`). Returns `None` if the log has no topics (a log
/// without even the topic0 hash is malformed for non-anonymous events).
fn log_topics(log: &Log) -> Option<&[B256]> {
    let topics = log.topics();
    if topics.is_empty() {
        None
    } else {
        Some(topics)
    }
}

// ── decoded structs (one per event family — mirror `AaveChunkEvent`) ───────
// NB: every variant carries the emitter `Address` so the orchestrator can
// disambiguate which token contract emitted a ScaledToken event (aToken vs
// vToken — the `ScaledTokenEventType` disambiguation the Python does via the
// token contract address). The Rust decode emits the emitter verbatim and the
// orchestrator classifies downstream.

/// Decoded Aave V3 `Supply` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3SupplyEvent {
    /// The Pool contract (the log emitter).
    pub pool_address: Address,
    /// `reserve` (topic[1]) — the underlying asset.
    pub reserve: Address,
    /// `onBehalfOf` (topic[3]) — the user the supply is credited to.
    pub on_behalf_of: Address,
    /// `amount` (data word 1 — the Python `extract_supply` reads
    /// `address user, uint256 amount`; the user-address word 0 is skipped as
    /// non-indexed-but-unneeded).
    pub amount: U256,
    /// `referralCode` (topic[4] — uint16 indexed).
    pub referral_code: u16,
}

/// Decoded Aave V3 `Borrow` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3BorrowEvent {
    pub pool_address: Address,
    /// `reserve` (topic[1]).
    pub reserve: Address,
    /// `onBehalfOf` (topic[3]).
    pub on_behalf_of: Address,
    /// `amount` (data word 1).
    pub amount: U256,
    /// `interestRateMode` (data word 2 — uint8; 1 = stable, 2 = variable).
    pub interest_rate_mode: u8,
    /// `borrowRate` (data word 3).
    pub borrow_rate: U256,
    /// `referralCode` (topic[4] — uint16 indexed).
    pub referral_code: u16,
}

/// Decoded Aave V3 `Repay` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3RepayEvent {
    pub pool_address: Address,
    /// `reserve` (topic[1]).
    pub reserve: Address,
    /// `user` (topic[2]).
    pub user: Address,
    /// `repayer` (topic[3]).
    pub repayer: Address,
    /// `amount` (data word 0).
    pub amount: U256,
    /// `useATokens` (data word 1 — bool).
    pub use_a_tokens: bool,
}

/// Decoded Aave V3 `Withdraw` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3WithdrawEvent {
    pub pool_address: Address,
    /// `reserve` (topic[1]).
    pub reserve: Address,
    /// `user` (topic[2]).
    pub user: Address,
    /// `to` (topic[3]).
    pub to: Address,
    /// `amount` (data word 0).
    pub amount: U256,
}

/// Decoded Aave V3 `LiquidationCall` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3LiquidationCallEvent {
    pub pool_address: Address,
    /// `collateralAsset` (topic[1]).
    pub collateral_asset: Address,
    /// `debtAsset` (topic[2]).
    pub debt_asset: Address,
    /// `user` (topic[3] — the liquidatee).
    pub user: Address,
    /// `debtToCover` (data word 0).
    pub debt_to_cover: U256,
    /// `liquidatedCollateralAmount` (data word 1).
    pub liquidated_collateral_amount: U256,
    /// `liquidator` (data word 2 — address).
    pub liquidator: Address,
    /// `receiveAToken` (data word 3 — bool).
    pub receive_a_token: bool,
}

/// Decoded Aave V3 `MintedToTreasury` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3MintedToTreasuryEvent {
    /// The aToken contract (the log emitter).
    pub a_token_address: Address,
    /// `reserve` (topic[1]).
    pub reserve: Address,
    /// `amountMinted` (data word 0).
    pub amount_minted: U256,
}

/// Decoded Aave V3 `DeficitCreated` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3DeficitCreatedEvent {
    pub pool_address: Address,
    /// `user` (topic[1]).
    pub user: Address,
    /// `debtAsset` (topic[2]).
    pub debt_asset: Address,
    /// `amountCreated` (data word 0).
    pub amount_created: U256,
}

/// Decoded Aave V3 `ReserveDataUpdated` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3ReserveDataUpdatedEvent {
    pub pool_address: Address,
    /// `reserve` (topic[1]).
    pub reserve: Address,
    /// `liquidityRate` (data word 0 — a 27-decimal ray).
    pub liquidity_rate: U256,
    /// `stableBorrowRate` (data word 1 — ray; deprecated but decoded for
    /// parity — the Python passes it through verbatim).
    pub stable_borrow_rate: U256,
    /// `variableBorrowRate` (data word 2 — ray).
    pub variable_borrow_rate: U256,
    /// `liquidityIndex` (data word 3 — ray).
    pub liquidity_index: U256,
    /// `variableBorrowIndex` (data word 4 — ray).
    pub variable_borrow_index: U256,
}

/// Decoded Aave V3 `ReserveUsedAsCollateralEnabled` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3ReserveUsedAsCollateralEnabledEvent {
    pub pool_address: Address,
    /// `reserve` (topic[1]).
    pub reserve: Address,
    /// `user` (topic[2]).
    pub user: Address,
}

/// Decoded Aave V3 `ReserveUsedAsCollateralDisabled` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3ReserveUsedAsCollateralDisabledEvent {
    pub pool_address: Address,
    pub reserve: Address,
    pub user: Address,
}

/// Decoded Aave V3 `UserEModeSet` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3UserEModeSetEvent {
    pub pool_address: Address,
    /// `user` (topic[1]).
    pub user: Address,
    /// `categoryId` (data word 0 — uint8).
    pub category_id: u8,
}

/// Decoded Aave V3 `ScaledToken` `Mint` event (aToken or vToken — disambiguated
/// by the orchestrator via the emitter contract address).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3ScaledTokenMintEvent {
    /// The aToken/vToken contract (the log emitter).
    pub token_address: Address,
    /// `caller` (topic[1]).
    pub caller: Address,
    /// `onBehalfOf` (topic[2]).
    pub on_behalf_of: Address,
    /// `value` (data word 0).
    pub value: U256,
    /// `balanceIncrease` (data word 1).
    pub balance_increase: U256,
    /// `index` (data word 2 — ray).
    pub index: U256,
}

/// Decoded Aave V3 `ScaledToken` `Burn` event (aToken or vToken).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3ScaledTokenBurnEvent {
    pub token_address: Address,
    /// `from` (topic[1]).
    pub from: Address,
    /// `target` (topic[2]).
    pub target: Address,
    /// `value` (data word 0).
    pub value: U256,
    /// `balanceIncrease` (data word 1).
    pub balance_increase: U256,
    /// `index` (data word 2 — ray).
    pub index: U256,
}

/// Decoded Aave V3 `ScaledToken` `BalanceTransfer` event (aToken-only — there's
/// no vToken `BalanceTransfer` in Aave V3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3ScaledTokenBalanceTransferEvent {
    /// The aToken contract (the log emitter).
    pub token_address: Address,
    /// `from` (topic[1]).
    pub from: Address,
    /// `to` (topic[2]).
    pub to: Address,
    /// `value` (data word 0 — the scaled amount).
    pub value: U256,
    /// `index` (data word 1 — ray).
    pub index: U256,
}

/// Decoded Aave V3 `CollateralConfigurationChanged` event. NB: the Python
/// path (`_process_collateral_configuration_changed_event`) does NOT decode
/// the ltv/threshold/bonus from the event data — it RPC-calls `getReserveConfig`
/// for the canonical values (degenbot doesn't trust the on-chain event
/// emission for the config bits, since a pool upgrade can emit stale values).
/// The decoder emits the event fields verbatim; the orchestrator's
/// `_process_collateral_configuration_changed_event` port (CXRGX4's purview)
/// decides whether to use them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3CollateralConfigurationChangedEvent {
    pub pool_configurator_address: Address,
    /// `asset` (topic[1]).
    pub asset: Address,
    /// `ltv` (data word 0).
    pub ltv: U256,
    /// `liquidationThreshold` (data word 1).
    pub liquidation_threshold: U256,
    /// `liquidationBonus` (data word 2).
    pub liquidation_bonus: U256,
}

/// Decoded Aave V3 `EModeCategoryAdded` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3EModeCategoryAddedEvent {
    pub pool_configurator_address: Address,
    /// `categoryId` (topic[1] — uint8, sign-extended to uint256).
    pub category_id: u8,
    /// `ltv` (data word 0).
    pub ltv: U256,
    /// `liquidationThreshold` (data word 1).
    pub liquidation_threshold: U256,
    /// `liquidationBonus` (data word 2).
    pub liquidation_bonus: U256,
    /// `oracle` (data word 3 — address; non-canonical — the actual oracle
    /// source is set via `AssetSourceUpdated` on the `AaveOracle`).
    pub oracle: Address,
    /// `label` (data word 4 onward — a dynamic-length string; the decoder
    /// extracts the raw bytes but does NOT UTF-8 validate; the orchestrator
    /// may defang/trim nul bytes like the Python does).
    pub label: String,
}

/// Decoded Aave V3 `EModeAssetCategoryChanged` event (older Aave versions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3EModeAssetCategoryChangedEvent {
    pub pool_configurator_address: Address,
    /// `asset` (topic[1]).
    pub asset: Address,
    /// `oldCategoryId` (data word 0 — uint8).
    pub old_category_id: u8,
    /// `newCategoryId` (data word 1 — uint8).
    pub new_category_id: u8,
}

/// Decoded Aave V3 `AssetCollateralInEModeChanged` event (newer Aave 3.4+).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3AssetCollateralInEModeChangedEvent {
    pub pool_configurator_address: Address,
    /// `asset` (topic[1]).
    pub asset: Address,
    /// `categoryId` (data word 0 — uint8).
    pub category_id: u8,
    /// `collateral` (data word 1 — bool).
    pub collateral: bool,
}

/// Decoded Aave V3 `ReserveInitialized` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3ReserveInitializedEvent {
    pub pool_configurator_address: Address,
    /// `asset` (topic[1]) — the underlying.
    pub asset: Address,
    /// `aToken` (topic[2]).
    pub a_token: Address,
    /// `stableDebtToken` (data word 0 — deprecated in Aave V3; included for
    /// parity — the Python ignores it but the field is in the event signature).
    pub stable_debt_token: Address,
    /// `variableDebtToken` (data word 1).
    pub variable_debt_token: Address,
    /// `interestRateStrategyAddress` (data word 2).
    pub interest_rate_strategy_address: Address,
}

/// Decoded Aave V3 configurator `PoolUpdated` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3PoolUpdatedEvent {
    pub address_provider: Address,
    /// `oldAddress` (topic[1]).
    pub old_address: Address,
    /// `newAddress` (topic[2]).
    pub new_address: Address,
}

/// Aave V3 configurator `PoolConfiguratorUpdated`/`PoolDataProviderUpdated`/
/// `PriceOracleUpdated` events share an identical shape (the `old/new` pair);
/// one struct covers them. Disambiguated by topic via [`DecodedAaveEvent`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3ConfigAddressPairEvent {
    pub address_provider: Address,
    pub old_address: Address,
    pub new_address: Address,
}

/// Decoded Aave V3 `ProxyCreated` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3ProxyCreatedEvent {
    pub address_provider: Address,
    /// `id` (topic[1] — bytes32; the contract identifier e.g.
    /// `keccak256("POOL")`, NOT necessarily a packed-ascii string).
    pub id: B256,
    /// `proxyAddress` (topic[2]).
    pub proxy_address: Address,
    /// `implementationAddress` (topic[3]).
    pub implementation_address: Address,
}

/// Decoded Aave V3 `AddressSet` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3AddressSetEvent {
    pub address_provider: Address,
    /// `id` (topic[1] — bytes32; the contract identifier; the Python decodes
    /// as ascii-stripped nul bytes — the decoder emits the raw bytes32, the
    /// orchestrator does the ascii-clean).
    pub id: B256,
    /// `oldAddress` (topic[2]).
    pub old_address: Address,
    /// `newAddress` (topic[3]).
    pub new_address: Address,
}

/// Decoded Aave V3 `Upgraded` event (emitted on aToken/vToken upgrade).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3UpgradedEvent {
    /// The proxy contract (aToken/vToken/etc.) — the log emitter.
    pub proxy_address: Address,
    /// `implementation` (topic[1]).
    pub implementation: Address,
}

/// Decoded Aave V3 GHO `DiscountPercentUpdated` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3DiscountPercentUpdatedEvent {
    /// The GHO vToken contract (the log emitter).
    pub v_token_address: Address,
    /// `user` (topic[1]).
    pub user: Address,
    /// `oldDiscountPercent` (data word 0 — uint256).
    pub old_discount_percent: U256,
    /// `newDiscountPercent` (topic[2] — uint256 indexed).
    pub new_discount_percent: U256,
}

/// Decoded Aave V3 GHO `DiscountRateStrategyUpdated` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3DiscountRateStrategyUpdatedEvent {
    pub v_token_address: Address,
    /// `oldDiscountRateStrategy` (topic[1]).
    pub old_strategy: Address,
    /// `newDiscountRateStrategy` (topic[2]).
    pub new_strategy: Address,
}

/// Decoded Aave V3 GHO `DiscountTokenUpdated` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3DiscountTokenUpdatedEvent {
    pub v_token_address: Address,
    /// `oldDiscountToken` (topic[1]).
    pub old_discount_token: Address,
    /// `newDiscountToken` (topic[2]).
    pub new_discount_token: Address,
}

/// Decoded Aave V3 `Staked` event (stkAAVE token).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3StakedEvent {
    /// The stkAAVE token contract (the log emitter).
    pub stk_aave_address: Address,
    /// `from` (topic[1] — the staker; usually zero addr on a true stake).
    pub from: Address,
    /// `onBehalfOf` (topic[2] — the recipient of the stake).
    pub on_behalf_of: Address,
    /// `amount` (data word 0).
    pub amount: U256,
}

/// Decoded Aave V3 `Redeem` event (stkAAVE token).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3RedeemEvent {
    pub stk_aave_address: Address,
    /// `from` (topic[1] — the redeemer).
    pub from: Address,
    /// `to` (topic[2] — the recipient of the redeemed assets).
    pub to: Address,
    /// `amount` (data word 0).
    pub amount: U256,
}

/// Decoded Aave V3 `RewardsController` `RewardsClaimed` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3RewardsClaimedEvent {
    /// The `RewardsController` contract (the log emitter).
    pub rewards_controller_address: Address,
    /// `user` (topic[1]).
    pub user: Address,
    /// `reward` (topic[2] — the reward token address).
    pub reward: Address,
    /// `to` (topic[3] — the claim recipient).
    pub to: Address,
    /// `claimer` (data word 0 — address).
    pub claimer: Address,
    /// `amount` (data word 1 — uint256).
    pub amount: U256,
}

/// Decoded Aave V3 Oracle `AssetSourceUpdated` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3AssetSourceUpdatedEvent {
    /// The `AaveOracle` contract (the log emitter).
    pub oracle_address: Address,
    /// `asset` (topic[1]).
    pub asset: Address,
    /// `source` (topic[2]).
    pub source: Address,
}

/// Decoded standard ERC20 `Transfer` event (used for aToken/vToken/stkAAVE
/// balance tracking where the `ScaledToken` `BalanceTransfer` doesn't fire).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveV3Erc20TransferEvent {
    /// The token contract (the log emitter).
    pub token_address: Address,
    /// `from` (topic[1] — zero address on a mint).
    pub from: Address,
    /// `to` (topic[2] — zero address on a burn).
    pub to: Address,
    /// `value` (data word 0).
    pub value: U256,
}

/// The discriminated union of all 34 decoded Aave V3 events (mirror of the
/// `AaveChunkEvent` variant set, but with raw `Address`/`U256` fields — no
/// id resolution). One variant per event; the
/// [`decode_aave_log`] dispatch fn returns `Option<DecodedAaveEvent>` (None
/// on unknown topic — see the module doc on decode-error handling).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodedAaveEvent {
    Supply(AaveV3SupplyEvent),
    Borrow(AaveV3BorrowEvent),
    Repay(AaveV3RepayEvent),
    Withdraw(AaveV3WithdrawEvent),
    LiquidationCall(AaveV3LiquidationCallEvent),
    MintedToTreasury(AaveV3MintedToTreasuryEvent),
    DeficitCreated(AaveV3DeficitCreatedEvent),
    ReserveDataUpdated(AaveV3ReserveDataUpdatedEvent),
    ReserveUsedAsCollateralEnabled(AaveV3ReserveUsedAsCollateralEnabledEvent),
    ReserveUsedAsCollateralDisabled(AaveV3ReserveUsedAsCollateralDisabledEvent),
    UserEModeSet(AaveV3UserEModeSetEvent),
    ScaledTokenMint(AaveV3ScaledTokenMintEvent),
    ScaledTokenBurn(AaveV3ScaledTokenBurnEvent),
    ScaledTokenBalanceTransfer(AaveV3ScaledTokenBalanceTransferEvent),
    CollateralConfigurationChanged(AaveV3CollateralConfigurationChangedEvent),
    EModeCategoryAdded(AaveV3EModeCategoryAddedEvent),
    EModeAssetCategoryChanged(AaveV3EModeAssetCategoryChangedEvent),
    AssetCollateralInEModeChanged(AaveV3AssetCollateralInEModeChangedEvent),
    ReserveInitialized(AaveV3ReserveInitializedEvent),
    PoolUpdated(AaveV3PoolUpdatedEvent),
    PoolConfiguratorUpdated(AaveV3ConfigAddressPairEvent),
    PoolDataProviderUpdated(AaveV3ConfigAddressPairEvent),
    PriceOracleUpdated(AaveV3ConfigAddressPairEvent),
    ProxyCreated(AaveV3ProxyCreatedEvent),
    AddressSet(AaveV3AddressSetEvent),
    Upgraded(AaveV3UpgradedEvent),
    DiscountPercentUpdated(AaveV3DiscountPercentUpdatedEvent),
    DiscountRateStrategyUpdated(AaveV3DiscountRateStrategyUpdatedEvent),
    DiscountTokenUpdated(AaveV3DiscountTokenUpdatedEvent),
    Staked(AaveV3StakedEvent),
    Redeem(AaveV3RedeemEvent),
    RewardsClaimed(AaveV3RewardsClaimedEvent),
    AssetSourceUpdated(AaveV3AssetSourceUpdatedEvent),
    Erc20Transfer(AaveV3Erc20TransferEvent),
}

// ── per-event decode fns ───────────────────────────────────────────────────

/// Decode a `Supply` event. Returns `None` on topic mismatch, < 5 topics, or
/// data < 64 bytes.
#[must_use]
pub fn decode_aave_supply_log(log: &Log) -> Option<AaveV3SupplyEvent> {
    // Real Aave V3 Supply signature (verified on mainnet, block 16496817):
    //   Supply(address indexed reserve, address user, address indexed onBehalfOf,
    //          uint256 amount, uint16 indexed referralCode)
    // 3 indexed params → 4 topics: [sig, reserve(topic1), onBehalfOf(topic2),
    // referralCode(topic3)]. Non-indexed data: [user, amount] = 2 words = 64 bytes.
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_SUPPLY_TOPIC {
        return None;
    }
    let reserve = topic_address(topics, 1)?;
    let on_behalf_of = topic_address(topics, 2)?;
    let referral_code = topic_u16(topics, 3).unwrap_or(0);
    let data = log.data().data.as_ref();
    if data.len() < 64 {
        return None;
    }
    let amount = data_u256(data, 1)?;
    Some(AaveV3SupplyEvent {
        pool_address: log.address(),
        reserve,
        on_behalf_of,
        amount,
        referral_code,
    })
}

/// Decode a `Borrow` event. Returns `None` on topic mismatch, < 5 topics, or
/// data < 128 bytes.
#[must_use]
pub fn decode_aave_borrow_log(log: &Log) -> Option<AaveV3BorrowEvent> {
    // Real Aave V3 Borrow signature (verified on mainnet: block 16497464
    // referralCode=64, block 16498172 referralCode=6671):
    //   Borrow(address indexed reserve, address user, address indexed onBehalfOf,
    //          uint256 amount, DataTypes.InterestRateMode interestRateMode,
    //          uint256 borrowRate, uint16 indexed referralCode)
    // 3 indexed params → 4 topics: [sig, reserve(topic1), onBehalfOf(topic2),
    // referralCode(topic3)]. Non-indexed data: [user, amount, mode, borrowRate]
    // = 4 words = 128 bytes.
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_BORROW_TOPIC {
        return None;
    }
    let reserve = topic_address(topics, 1)?;
    let on_behalf_of = topic_address(topics, 2)?;
    let referral_code = topic_u16(topics, 3).unwrap_or(0);
    let data = log.data().data.as_ref();
    if data.len() < 128 {
        return None;
    }
    let amount = data_u256(data, 1)?;
    let interest_rate_mode = data_u8(data, 2)?;
    let borrow_rate = data_u256(data, 3)?;
    Some(AaveV3BorrowEvent {
        pool_address: log.address(),
        reserve,
        on_behalf_of,
        amount,
        interest_rate_mode,
        borrow_rate,
        referral_code,
    })
}

/// Decode a `Repay` event. Returns `None` on topic mismatch, < 4 topics, or
/// data < 64 bytes.
#[must_use]
pub fn decode_aave_repay_log(log: &Log) -> Option<AaveV3RepayEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_REPAY_TOPIC {
        return None;
    }
    let reserve = topic_address(topics, 1)?;
    let user = topic_address(topics, 2)?;
    let repayer = topic_address(topics, 3)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 amount, bool useATokens) = 2 words = 64 bytes.
    if data.len() < 64 {
        return None;
    }
    let amount = data_u256(data, 0)?;
    let use_a_tokens = data_bool(data, 1)?;
    Some(AaveV3RepayEvent {
        pool_address: log.address(),
        reserve,
        user,
        repayer,
        amount,
        use_a_tokens,
    })
}

/// Decode a `Withdraw` event. Returns `None` on topic mismatch, < 4 topics,
/// or data < 32 bytes.
#[must_use]
pub fn decode_aave_withdraw_log(log: &Log) -> Option<AaveV3WithdrawEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_WITHDRAW_TOPIC {
        return None;
    }
    let reserve = topic_address(topics, 1)?;
    let user = topic_address(topics, 2)?;
    let to = topic_address(topics, 3)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 amount) = 1 word = 32 bytes.
    if data.len() < 32 {
        return None;
    }
    let amount = data_u256(data, 0)?;
    Some(AaveV3WithdrawEvent {
        pool_address: log.address(),
        reserve,
        user,
        to,
        amount,
    })
}

/// Decode a `LiquidationCall` event. Returns `None` on topic mismatch, < 4
/// topics, or data < 128 bytes.
#[must_use]
pub fn decode_aave_liquidation_call_log(log: &Log) -> Option<AaveV3LiquidationCallEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_LIQUIDATION_CALL_TOPIC {
        return None;
    }
    let collateral_asset = topic_address(topics, 1)?;
    let debt_asset = topic_address(topics, 2)?;
    let user = topic_address(topics, 3)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 debtToCover, uint256 liquidatedCollateralAmount,
    // address liquidator, bool receiveAToken) = 4 words = 128 bytes.
    if data.len() < 128 {
        return None;
    }
    let debt_to_cover = data_u256(data, 0)?;
    let liquidated_collateral_amount = data_u256(data, 1)?;
    let liquidator = data_address(data, 2)?;
    let receive_a_token = data_bool(data, 3)?;
    Some(AaveV3LiquidationCallEvent {
        pool_address: log.address(),
        collateral_asset,
        debt_asset,
        user,
        debt_to_cover,
        liquidated_collateral_amount,
        liquidator,
        receive_a_token,
    })
}

/// Decode a `MintedToTreasury` event. Returns `None` on topic mismatch, < 2
/// topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_minted_to_treasury_log(log: &Log) -> Option<AaveV3MintedToTreasuryEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_MINTED_TO_TREASURY_TOPIC {
        return None;
    }
    let reserve = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 amountMinted) = 1 word = 32 bytes.
    if data.len() < 32 {
        return None;
    }
    let amount_minted = data_u256(data, 0)?;
    Some(AaveV3MintedToTreasuryEvent {
        a_token_address: log.address(),
        reserve,
        amount_minted,
    })
}

/// Decode a `DeficitCreated` event. Returns `None` on topic mismatch, < 3
/// topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_deficit_created_log(log: &Log) -> Option<AaveV3DeficitCreatedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_DEFICIT_CREATED_TOPIC {
        return None;
    }
    let user = topic_address(topics, 1)?;
    let debt_asset = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 amountCreated) = 1 word = 32 bytes.
    if data.len() < 32 {
        return None;
    }
    let amount_created = data_u256(data, 0)?;
    Some(AaveV3DeficitCreatedEvent {
        pool_address: log.address(),
        user,
        debt_asset,
        amount_created,
    })
}

/// Decode a `ReserveDataUpdated` event. Returns `None` on topic mismatch, < 2
/// topics, or data < 160 bytes.
#[must_use]
pub fn decode_aave_reserve_data_updated_log(log: &Log) -> Option<AaveV3ReserveDataUpdatedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_RESERVE_DATA_UPDATED_TOPIC {
        return None;
    }
    let reserve = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 liquidityRate, uint256 stableBorrowRate,
    // uint256 variableBorrowRate, uint256 liquidityIndex,
    // uint256 variableBorrowIndex) = 5 words = 160 bytes.
    if data.len() < 160 {
        return None;
    }
    let liquidity_rate = data_u256(data, 0)?;
    let stable_borrow_rate = data_u256(data, 1)?;
    let variable_borrow_rate = data_u256(data, 2)?;
    let liquidity_index = data_u256(data, 3)?;
    let variable_borrow_index = data_u256(data, 4)?;
    Some(AaveV3ReserveDataUpdatedEvent {
        pool_address: log.address(),
        reserve,
        liquidity_rate,
        stable_borrow_rate,
        variable_borrow_rate,
        liquidity_index,
        variable_borrow_index,
    })
}

/// Decode a `ReserveUsedAsCollateralEnabled` event. Returns `None` on topic
/// mismatch or < 3 topics (no data field — both args are indexed).
#[must_use]
pub fn decode_aave_reserve_used_as_collateral_enabled_log(
    log: &Log,
) -> Option<AaveV3ReserveUsedAsCollateralEnabledEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_RESERVE_USED_AS_COLLATERAL_ENABLED_TOPIC {
        return None;
    }
    let reserve = topic_address(topics, 1)?;
    let user = topic_address(topics, 2)?;
    Some(AaveV3ReserveUsedAsCollateralEnabledEvent {
        pool_address: log.address(),
        reserve,
        user,
    })
}

/// Decode a `ReserveUsedAsCollateralDisabled` event. Returns `None` on topic
/// mismatch or < 3 topics (no data field).
#[must_use]
pub fn decode_aave_reserve_used_as_collateral_disabled_log(
    log: &Log,
) -> Option<AaveV3ReserveUsedAsCollateralDisabledEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_RESERVE_USED_AS_COLLATERAL_DISABLED_TOPIC {
        return None;
    }
    let reserve = topic_address(topics, 1)?;
    let user = topic_address(topics, 2)?;
    Some(AaveV3ReserveUsedAsCollateralDisabledEvent {
        pool_address: log.address(),
        reserve,
        user,
    })
}

/// Decode a `UserEModeSet` event. Returns `None` on topic mismatch, < 2
/// topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_user_e_mode_set_log(log: &Log) -> Option<AaveV3UserEModeSetEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_USER_E_MODE_SET_TOPIC {
        return None;
    }
    let user = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint8 categoryId) = 1 word = 32 bytes.
    if data.len() < 32 {
        return None;
    }
    let category_id = data_u8(data, 0)?;
    Some(AaveV3UserEModeSetEvent {
        pool_address: log.address(),
        user,
        category_id,
    })
}

/// Decode a `ScaledToken` `Mint` event (aToken or vToken — disambiguated by the
/// orchestrator). Returns `None` on topic mismatch, < 3 topics, or data < 96
/// bytes.
#[must_use]
pub fn decode_aave_scaled_token_mint_log(log: &Log) -> Option<AaveV3ScaledTokenMintEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_MINT_TOPIC {
        return None;
    }
    let caller = topic_address(topics, 1)?;
    let on_behalf_of = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 value, uint256 balanceIncrease, uint256 index)
    // = 3 words = 96 bytes.
    if data.len() < 96 {
        return None;
    }
    let value = data_u256(data, 0)?;
    let balance_increase = data_u256(data, 1)?;
    let index = data_u256(data, 2)?;
    Some(AaveV3ScaledTokenMintEvent {
        token_address: log.address(),
        caller,
        on_behalf_of,
        value,
        balance_increase,
        index,
    })
}

/// Decode a `ScaledToken` `Burn` event. Returns `None` on topic mismatch, < 3
/// topics, or data < 96 bytes.
#[must_use]
pub fn decode_aave_scaled_token_burn_log(log: &Log) -> Option<AaveV3ScaledTokenBurnEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_BURN_TOPIC {
        return None;
    }
    let from = topic_address(topics, 1)?;
    let target = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 value, uint256 balanceIncrease, uint256 index)
    // = 3 words = 96 bytes.
    if data.len() < 96 {
        return None;
    }
    let value = data_u256(data, 0)?;
    let balance_increase = data_u256(data, 1)?;
    let index = data_u256(data, 2)?;
    Some(AaveV3ScaledTokenBurnEvent {
        token_address: log.address(),
        from,
        target,
        value,
        balance_increase,
        index,
    })
}

/// Decode a `ScaledToken` `BalanceTransfer` event. Returns `None` on topic
/// mismatch, < 3 topics, or data < 64 bytes.
#[must_use]
pub fn decode_aave_scaled_token_balance_transfer_log(
    log: &Log,
) -> Option<AaveV3ScaledTokenBalanceTransferEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_BALANCE_TRANSFER_TOPIC {
        return None;
    }
    let from = topic_address(topics, 1)?;
    let to = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 value, uint256 index) = 2 words = 64 bytes.
    if data.len() < 64 {
        return None;
    }
    let value = data_u256(data, 0)?;
    let index = data_u256(data, 1)?;
    Some(AaveV3ScaledTokenBalanceTransferEvent {
        token_address: log.address(),
        from,
        to,
        value,
        index,
    })
}

/// Decode a `CollateralConfigurationChanged` event. Returns `None` on topic
/// mismatch, < 2 topics, or data < 96 bytes.
#[must_use]
pub fn decode_aave_collateral_configuration_changed_log(
    log: &Log,
) -> Option<AaveV3CollateralConfigurationChangedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_COLLATERAL_CONFIGURATION_CHANGED_TOPIC {
        return None;
    }
    let asset = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 ltv, uint256 liquidationThreshold,
    // uint256 liquidationBonus) = 3 words = 96 bytes.
    if data.len() < 96 {
        return None;
    }
    let ltv = data_u256(data, 0)?;
    let liquidation_threshold = data_u256(data, 1)?;
    let liquidation_bonus = data_u256(data, 2)?;
    Some(AaveV3CollateralConfigurationChangedEvent {
        pool_configurator_address: log.address(),
        asset,
        ltv,
        liquidation_threshold,
        liquidation_bonus,
    })
}

/// Decode a `EModeCategoryAdded` event. The `label` field is a dynamic-length
/// string at the tail; the decoder reads the length prefix + the UTF-8 bytes
/// (lossy — invalid UTF-8 is replaced with the replacement char; the Python
/// strips nul bytes but doesn't UTF-8-validate either). Returns `None` on
/// topic mismatch, < 2 topics, or data too short to hold the fixed fields +
/// the string offset.
#[must_use]
pub fn decode_aave_e_mode_category_added_log(log: &Log) -> Option<AaveV3EModeCategoryAddedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_EMODE_CATEGORY_ADDED_TOPIC {
        return None;
    }
    let category_id = topic_u8(topics, 1)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 ltv, uint256 liquidationThreshold,
    // uint256 liquidationBonus, address oracle, string label).
    // Words 0..3 are the fixed uint256/uint256/uint256 fields.
    // Word 3 is `address oracle`. Word 4 is the string's offset (uint256).
    // The string payload starts at that offset (length word + bytes).
    if data.len() < 5 * 32 {
        return None;
    }
    let ltv = data_u256(data, 0)?;
    let liquidation_threshold = data_u256(data, 1)?;
    let liquidation_bonus = data_u256(data, 2)?;
    let oracle = data_address(data, 3)?;
    let string_offset = data_u256(data, 4)?;
    let string_offset_usize: usize = string_offset.try_into().ok()?;
    // At `string_offset`: a uint256 length, then `length` bytes (right-padded
    // to a 32-byte boundary).
    let len_start = string_offset_usize.checked_add(32)?;
    if len_start > data.len() {
        return None;
    }
    let len_word: [u8; 32] = data.get(string_offset_usize..len_start)?.try_into().ok()?;
    let str_len: usize = U256::from_be_bytes::<32>(len_word).try_into().ok()?;
    let str_start = len_start;
    let str_end = str_start.checked_add(str_len)?;
    if str_end > data.len() {
        return None;
    }
    let label_bytes = data.get(str_start..str_end)?;
    // Lossy UTF-8 (the Python does `contract_id_bytes.decode("ascii").strip`
    // — ASCII subset; this is more permissive but lossless for valid UTF-8).
    let label = String::from_utf8_lossy(label_bytes).into_owned();
    Some(AaveV3EModeCategoryAddedEvent {
        pool_configurator_address: log.address(),
        category_id,
        ltv,
        liquidation_threshold,
        liquidation_bonus,
        oracle,
        label,
    })
}

/// Decode a `EModeAssetCategoryChanged` event. Returns `None` on topic
/// mismatch, < 2 topics, or data < 64 bytes.
#[must_use]
pub fn decode_aave_e_mode_asset_category_changed_log(
    log: &Log,
) -> Option<AaveV3EModeAssetCategoryChangedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_EMODE_ASSET_CATEGORY_CHANGED_TOPIC {
        return None;
    }
    let asset = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint8 oldCategoryId, uint8 newCategoryId) = 2 words =
    // 64 bytes (each uint8 sign-extended to uint256).
    if data.len() < 64 {
        return None;
    }
    let old_category_id = data_u8(data, 0)?;
    let new_category_id = data_u8(data, 1)?;
    Some(AaveV3EModeAssetCategoryChangedEvent {
        pool_configurator_address: log.address(),
        asset,
        old_category_id,
        new_category_id,
    })
}

/// Decode a `AssetCollateralInEModeChanged` event. Returns `None` on topic
/// mismatch, < 2 topics, or data < 64 bytes.
#[must_use]
pub fn decode_aave_asset_collateral_in_emode_changed_log(
    log: &Log,
) -> Option<AaveV3AssetCollateralInEModeChangedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_ASSET_COLLATERAL_IN_EMODE_CHANGED_TOPIC {
        return None;
    }
    let asset = topic_address(topics, 1)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint8 categoryId, bool collateral) = 2 words = 64 bytes.
    if data.len() < 64 {
        return None;
    }
    let category_id = data_u8(data, 0)?;
    let collateral = data_bool(data, 1)?;
    Some(AaveV3AssetCollateralInEModeChangedEvent {
        pool_configurator_address: log.address(),
        asset,
        category_id,
        collateral,
    })
}

/// Decode a `ReserveInitialized` event. Returns `None` on topic mismatch, < 3
/// topics, or data < 96 bytes.
#[must_use]
pub fn decode_aave_reserve_initialized_log(log: &Log) -> Option<AaveV3ReserveInitializedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_RESERVE_INITIALIZED_TOPIC {
        return None;
    }
    let asset = topic_address(topics, 1)?;
    let a_token = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(address stableDebtToken, address variableDebtToken,
    // address interestRateStrategyAddress) = 3 words = 96 bytes.
    if data.len() < 96 {
        return None;
    }
    let stable_debt_token = data_address(data, 0)?;
    let variable_debt_token = data_address(data, 1)?;
    let interest_rate_strategy_address = data_address(data, 2)?;
    Some(AaveV3ReserveInitializedEvent {
        pool_configurator_address: log.address(),
        asset,
        a_token,
        stable_debt_token,
        variable_debt_token,
        interest_rate_strategy_address,
    })
}

/// Decode a `PoolUpdated` event. Returns `None` on topic mismatch or < 3
/// topics (no data field).
#[must_use]
pub fn decode_aave_pool_updated_log(log: &Log) -> Option<AaveV3PoolUpdatedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_POOL_UPDATED_TOPIC {
        return None;
    }
    let old_address = topic_address(topics, 1)?;
    let new_address = topic_address(topics, 2)?;
    Some(AaveV3PoolUpdatedEvent {
        address_provider: log.address(),
        old_address,
        new_address,
    })
}

/// Decode a `PoolConfiguratorUpdated` event. Returns `None` on topic mismatch
/// or < 3 topics.
#[must_use]
pub fn decode_aave_pool_configurator_updated_log(
    log: &Log,
) -> Option<AaveV3ConfigAddressPairEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_POOL_CONFIGURATOR_UPDATED_TOPIC {
        return None;
    }
    let old_address = topic_address(topics, 1)?;
    let new_address = topic_address(topics, 2)?;
    Some(AaveV3ConfigAddressPairEvent {
        address_provider: log.address(),
        old_address,
        new_address,
    })
}

/// Decode a `PoolDataProviderUpdated` event. Returns `None` on topic mismatch
/// or < 3 topics.
#[must_use]
pub fn decode_aave_pool_data_provider_updated_log(
    log: &Log,
) -> Option<AaveV3ConfigAddressPairEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_POOL_DATA_PROVIDER_UPDATED_TOPIC {
        return None;
    }
    let old_address = topic_address(topics, 1)?;
    let new_address = topic_address(topics, 2)?;
    Some(AaveV3ConfigAddressPairEvent {
        address_provider: log.address(),
        old_address,
        new_address,
    })
}

/// Decode a `PriceOracleUpdated` event. Returns `None` on topic mismatch or < 3
/// topics.
#[must_use]
pub fn decode_aave_price_oracle_updated_log(log: &Log) -> Option<AaveV3ConfigAddressPairEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_PRICE_ORACLE_UPDATED_TOPIC {
        return None;
    }
    let old_address = topic_address(topics, 1)?;
    let new_address = topic_address(topics, 2)?;
    Some(AaveV3ConfigAddressPairEvent {
        address_provider: log.address(),
        old_address,
        new_address,
    })
}

/// Decode a `ProxyCreated` event. Returns `None` on topic mismatch or < 4
/// topics (no data field — all 3 args are indexed).
#[must_use]
pub fn decode_aave_proxy_created_log(log: &Log) -> Option<AaveV3ProxyCreatedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_PROXY_CREATED_TOPIC {
        return None;
    }
    let id = topic_b256(topics, 1)?;
    let proxy_address = topic_address(topics, 2)?;
    let implementation_address = topic_address(topics, 3)?;
    Some(AaveV3ProxyCreatedEvent {
        address_provider: log.address(),
        id,
        proxy_address,
        implementation_address,
    })
}

/// Decode an `AddressSet` event. Returns `None` on topic mismatch or < 4
/// topics (no data field — all 3 args are indexed).
#[must_use]
pub fn decode_aave_address_set_log(log: &Log) -> Option<AaveV3AddressSetEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_ADDRESS_SET_TOPIC {
        return None;
    }
    let id = topic_b256(topics, 1)?;
    let old_address = topic_address(topics, 2)?;
    let new_address = topic_address(topics, 3)?;
    Some(AaveV3AddressSetEvent {
        address_provider: log.address(),
        id,
        old_address,
        new_address,
    })
}

/// Decode an `Upgraded` event. Returns `None` on topic mismatch or < 2 topics
/// (no data field — the single arg is indexed).
#[must_use]
pub fn decode_aave_upgraded_log(log: &Log) -> Option<AaveV3UpgradedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_UPGRADED_TOPIC {
        return None;
    }
    let implementation = topic_address(topics, 1)?;
    Some(AaveV3UpgradedEvent {
        proxy_address: log.address(),
        implementation,
    })
}

/// Decode a GHO `DiscountPercentUpdated` event. Returns `None` on topic
/// mismatch, < 3 topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_discount_percent_updated_log(
    log: &Log,
) -> Option<AaveV3DiscountPercentUpdatedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC {
        return None;
    }
    let user = topic_address(topics, 1)?;
    let new_discount_percent = topic_u256(topics, 2)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 oldDiscountPercent) = 1 word = 32 bytes.
    if data.len() < 32 {
        return None;
    }
    let old_discount_percent = data_u256(data, 0)?;
    Some(AaveV3DiscountPercentUpdatedEvent {
        v_token_address: log.address(),
        user,
        old_discount_percent,
        new_discount_percent,
    })
}

/// Decode a GHO `DiscountRateStrategyUpdated` event. Returns `None` on topic
/// mismatch or < 3 topics (no data field — both args are indexed).
#[must_use]
pub fn decode_aave_discount_rate_strategy_updated_log(
    log: &Log,
) -> Option<AaveV3DiscountRateStrategyUpdatedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_DISCOUNT_RATE_STRATEGY_UPDATED_TOPIC {
        return None;
    }
    let old_strategy = topic_address(topics, 1)?;
    let new_strategy = topic_address(topics, 2)?;
    Some(AaveV3DiscountRateStrategyUpdatedEvent {
        v_token_address: log.address(),
        old_strategy,
        new_strategy,
    })
}

/// Decode a GHO `DiscountTokenUpdated` event. Returns `None` on topic mismatch
/// or < 3 topics (no data field).
#[must_use]
pub fn decode_aave_discount_token_updated_log(
    log: &Log,
) -> Option<AaveV3DiscountTokenUpdatedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_DISCOUNT_TOKEN_UPDATED_TOPIC {
        return None;
    }
    let old_discount_token = topic_address(topics, 1)?;
    let new_discount_token = topic_address(topics, 2)?;
    Some(AaveV3DiscountTokenUpdatedEvent {
        v_token_address: log.address(),
        old_discount_token,
        new_discount_token,
    })
}

/// Decode a `Staked` event (stkAAVE token). Returns `None` on topic mismatch,
/// < 3 topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_staked_log(log: &Log) -> Option<AaveV3StakedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_STAKED_TOPIC {
        return None;
    }
    let from = topic_address(topics, 1)?;
    let on_behalf_of = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 amount) = 1 word = 32 bytes.
    if data.len() < 32 {
        return None;
    }
    let amount = data_u256(data, 0)?;
    Some(AaveV3StakedEvent {
        stk_aave_address: log.address(),
        from,
        on_behalf_of,
        amount,
    })
}

/// Decode a `Redeem` event (stkAAVE token). Returns `None` on topic mismatch,
/// < 3 topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_redeem_log(log: &Log) -> Option<AaveV3RedeemEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_REDEEM_TOPIC {
        return None;
    }
    let from = topic_address(topics, 1)?;
    let to = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 amount) = 1 word = 32 bytes.
    if data.len() < 32 {
        return None;
    }
    let amount = data_u256(data, 0)?;
    Some(AaveV3RedeemEvent {
        stk_aave_address: log.address(),
        from,
        to,
        amount,
    })
}

/// Decode a `RewardsController` `RewardsClaimed` event. Returns `None` on topic
/// mismatch, < 4 topics, or data < 64 bytes.
#[must_use]
pub fn decode_aave_rewards_claimed_log(log: &Log) -> Option<AaveV3RewardsClaimedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_REWARDS_CLAIMED_TOPIC {
        return None;
    }
    let user = topic_address(topics, 1)?;
    let reward = topic_address(topics, 2)?;
    let to = topic_address(topics, 3)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(address claimer, uint256 amount) = 2 words = 64 bytes.
    if data.len() < 64 {
        return None;
    }
    let claimer = data_address(data, 0)?;
    let amount = data_u256(data, 1)?;
    Some(AaveV3RewardsClaimedEvent {
        rewards_controller_address: log.address(),
        user,
        reward,
        to,
        claimer,
        amount,
    })
}

/// Decode an `AaveOracle` `AssetSourceUpdated` event. Returns `None` on topic
/// mismatch or < 3 topics (no data field — both args are indexed).
#[must_use]
pub fn decode_aave_asset_source_updated_log(log: &Log) -> Option<AaveV3AssetSourceUpdatedEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &AAVE_ASSET_SOURCE_UPDATED_TOPIC {
        return None;
    }
    let asset = topic_address(topics, 1)?;
    let source = topic_address(topics, 2)?;
    Some(AaveV3AssetSourceUpdatedEvent {
        oracle_address: log.address(),
        asset,
        source,
    })
}

/// Decode a standard ERC20 `Transfer` event. Returns `None` on topic mismatch,
/// < 3 topics, or data < 32 bytes.
#[must_use]
pub fn decode_aave_erc20_transfer_log(log: &Log) -> Option<AaveV3Erc20TransferEvent> {
    let topics = log_topics(log)?;
    if topics.first()? != &ERC20_TRANSFER_TOPIC {
        return None;
    }
    let from = topic_address(topics, 1)?;
    let to = topic_address(topics, 2)?;
    let data = log.data().data.as_ref();
    // data = abi.encode(uint256 value) = 1 word = 32 bytes.
    if data.len() < 32 {
        return None;
    }
    let value = data_u256(data, 0)?;
    Some(AaveV3Erc20TransferEvent {
        token_address: log.address(),
        from,
        to,
        value,
    })
}

// ── the dispatch fn ───────────────────────────────────────────────────────

/// Decode an Aave V3 event log by switching on `log.topics[0]`. Returns the
/// typed `DecodedAaveEvent`; `None` if the topic doesn't match ANY of the 34
/// known Aave V3 event topics (the orchestrator can filter unknowns out
/// without crashing — mirrors the precedent
/// [`crate::pool_created_decoder::decode_v2_pair_created_log`] None-on-mismatch
/// contract). The per-event decoder returning `None` (malformed data) is
/// also surfaced as `None` here.
///
/// # Performance
///
/// Switches via a `match` on the topic `B256` (the alloy `B256` `PartialEq`
/// is a constant-time 32-byte compare). 34-arm linear-match is fine for the
/// chunk-loop's per-log decode (the orchestrator typically filters logs
/// RPC-side by topic list, so only Aave V3 topics reach this dispatch).
#[must_use]
pub fn decode_aave_log(log: &Log) -> Option<DecodedAaveEvent> {
    let topic = log.topics().first()?;
    Some(match *topic {
        AAVE_SUPPLY_TOPIC => DecodedAaveEvent::Supply(decode_aave_supply_log(log)?),
        AAVE_BORROW_TOPIC => DecodedAaveEvent::Borrow(decode_aave_borrow_log(log)?),
        AAVE_REPAY_TOPIC => DecodedAaveEvent::Repay(decode_aave_repay_log(log)?),
        AAVE_WITHDRAW_TOPIC => DecodedAaveEvent::Withdraw(decode_aave_withdraw_log(log)?),
        AAVE_LIQUIDATION_CALL_TOPIC => {
            DecodedAaveEvent::LiquidationCall(decode_aave_liquidation_call_log(log)?)
        }
        AAVE_MINTED_TO_TREASURY_TOPIC => {
            DecodedAaveEvent::MintedToTreasury(decode_aave_minted_to_treasury_log(log)?)
        }
        AAVE_DEFICIT_CREATED_TOPIC => {
            DecodedAaveEvent::DeficitCreated(decode_aave_deficit_created_log(log)?)
        }
        AAVE_RESERVE_DATA_UPDATED_TOPIC => {
            DecodedAaveEvent::ReserveDataUpdated(decode_aave_reserve_data_updated_log(log)?)
        }
        AAVE_RESERVE_USED_AS_COLLATERAL_ENABLED_TOPIC => {
            DecodedAaveEvent::ReserveUsedAsCollateralEnabled(
                decode_aave_reserve_used_as_collateral_enabled_log(log)?,
            )
        }
        AAVE_RESERVE_USED_AS_COLLATERAL_DISABLED_TOPIC => {
            DecodedAaveEvent::ReserveUsedAsCollateralDisabled(
                decode_aave_reserve_used_as_collateral_disabled_log(log)?,
            )
        }
        AAVE_USER_E_MODE_SET_TOPIC => {
            DecodedAaveEvent::UserEModeSet(decode_aave_user_e_mode_set_log(log)?)
        }
        AAVE_MINT_TOPIC => {
            DecodedAaveEvent::ScaledTokenMint(decode_aave_scaled_token_mint_log(log)?)
        }
        AAVE_BURN_TOPIC => {
            DecodedAaveEvent::ScaledTokenBurn(decode_aave_scaled_token_burn_log(log)?)
        }
        AAVE_BALANCE_TRANSFER_TOPIC => DecodedAaveEvent::ScaledTokenBalanceTransfer(
            decode_aave_scaled_token_balance_transfer_log(log)?,
        ),
        AAVE_COLLATERAL_CONFIGURATION_CHANGED_TOPIC => {
            DecodedAaveEvent::CollateralConfigurationChanged(
                decode_aave_collateral_configuration_changed_log(log)?,
            )
        }
        AAVE_EMODE_CATEGORY_ADDED_TOPIC => {
            DecodedAaveEvent::EModeCategoryAdded(decode_aave_e_mode_category_added_log(log)?)
        }
        AAVE_EMODE_ASSET_CATEGORY_CHANGED_TOPIC => DecodedAaveEvent::EModeAssetCategoryChanged(
            decode_aave_e_mode_asset_category_changed_log(log)?,
        ),
        AAVE_ASSET_COLLATERAL_IN_EMODE_CHANGED_TOPIC => {
            DecodedAaveEvent::AssetCollateralInEModeChanged(
                decode_aave_asset_collateral_in_emode_changed_log(log)?,
            )
        }
        AAVE_RESERVE_INITIALIZED_TOPIC => {
            DecodedAaveEvent::ReserveInitialized(decode_aave_reserve_initialized_log(log)?)
        }
        AAVE_POOL_UPDATED_TOPIC => {
            DecodedAaveEvent::PoolUpdated(decode_aave_pool_updated_log(log)?)
        }
        AAVE_POOL_CONFIGURATOR_UPDATED_TOPIC => DecodedAaveEvent::PoolConfiguratorUpdated(
            decode_aave_pool_configurator_updated_log(log)?,
        ),
        AAVE_POOL_DATA_PROVIDER_UPDATED_TOPIC => DecodedAaveEvent::PoolDataProviderUpdated(
            decode_aave_pool_data_provider_updated_log(log)?,
        ),
        AAVE_PRICE_ORACLE_UPDATED_TOPIC => {
            DecodedAaveEvent::PriceOracleUpdated(decode_aave_price_oracle_updated_log(log)?)
        }
        AAVE_PROXY_CREATED_TOPIC => {
            DecodedAaveEvent::ProxyCreated(decode_aave_proxy_created_log(log)?)
        }
        AAVE_ADDRESS_SET_TOPIC => DecodedAaveEvent::AddressSet(decode_aave_address_set_log(log)?),
        AAVE_UPGRADED_TOPIC => DecodedAaveEvent::Upgraded(decode_aave_upgraded_log(log)?),
        AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC => {
            DecodedAaveEvent::DiscountPercentUpdated(decode_aave_discount_percent_updated_log(log)?)
        }
        AAVE_DISCOUNT_RATE_STRATEGY_UPDATED_TOPIC => DecodedAaveEvent::DiscountRateStrategyUpdated(
            decode_aave_discount_rate_strategy_updated_log(log)?,
        ),
        AAVE_DISCOUNT_TOKEN_UPDATED_TOPIC => {
            DecodedAaveEvent::DiscountTokenUpdated(decode_aave_discount_token_updated_log(log)?)
        }
        AAVE_STAKED_TOPIC => DecodedAaveEvent::Staked(decode_aave_staked_log(log)?),
        AAVE_REDEEM_TOPIC => DecodedAaveEvent::Redeem(decode_aave_redeem_log(log)?),
        AAVE_REWARDS_CLAIMED_TOPIC => {
            DecodedAaveEvent::RewardsClaimed(decode_aave_rewards_claimed_log(log)?)
        }
        AAVE_ASSET_SOURCE_UPDATED_TOPIC => {
            DecodedAaveEvent::AssetSourceUpdated(decode_aave_asset_source_updated_log(log)?)
        }
        ERC20_TRANSFER_TOPIC => {
            DecodedAaveEvent::Erc20Transfer(decode_aave_erc20_transfer_log(log)?)
        }
        _ => return None,
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, Log as AlloyLog};

    /// Build a minimal `alloy::rpc::types::Log` with the given emitter,
    /// topics, + data (mirrors the construction helper in
    /// `pool_created_decoder`'s tests).
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

    /// Left-pad an address to a 32-byte word.
    fn addr_word(addr: Address) -> B256 {
        let mut word = [0u8; 32];
        word[12..32].copy_from_slice(addr.as_slice());
        B256::new(word)
    }

    /// Encode a `U256` as a 32-byte big-endian word.
    fn u256_word(value: U256) -> [u8; 32] {
        value.to_be_bytes::<32>()
    }

    /// Encode a `u8` as a sign-extended 32-byte word (uint8 → uint256).
    fn u8_word(value: u8) -> [u8; 32] {
        let mut word = [0u8; 32];
        word[31] = value;
        word
    }

    /// Encode a `bool` as a sign-extended 32-byte word.
    fn bool_word(value: bool) -> [u8; 32] {
        u8_word(u8::from(value))
    }

    /// Encode a `u16` as a sign-extended 32-byte word.
    fn u16_word(value: u16) -> [u8; 32] {
        let mut word = [0u8; 32];
        word[30..32].copy_from_slice(&value.to_be_bytes());
        word
    }

    // ── parity: every topic constant matches the Python `events.py` ─────
    // This test pins every Rust topic constant against the Python source of
    // truth (catches transcription errors in the byte literals — a sentinel
    // value that's off-by-one would silently decode every log as Unknown).

    #[test]
    #[expect(clippy::too_many_lines)] // fixture table — one entry per event is intrinsic
    fn parity_topic_constants_match_python() {
        // (Rust constant, Python `events.py` topic0 hex string).
        let cases: [(&B256, &str); 34] = [
            (
                &AAVE_BORROW_TOPIC,
                "b3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0",
            ),
            (
                &AAVE_DEFICIT_CREATED_TOPIC,
                "2bccfb3fad376d59d7accf970515eb77b2f27b082c90ed0fb15583dd5a942699",
            ),
            (
                &AAVE_LIQUIDATION_CALL_TOPIC,
                "e413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286",
            ),
            (
                &AAVE_MINTED_TO_TREASURY_TOPIC,
                "bfa21aa5d5f9a1f0120a95e7c0749f389863cbdbfff531aa7339077a5bc919de",
            ),
            (
                &AAVE_REPAY_TOPIC,
                "a534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051",
            ),
            (
                &AAVE_RESERVE_DATA_UPDATED_TOPIC,
                "804c9b842b2748a22bb64b345453a3de7ca54a6ca45ce00d415894979e22897a",
            ),
            (
                &AAVE_RESERVE_USED_AS_COLLATERAL_DISABLED_TOPIC,
                "44c58d81365b66dd4b1a7f36c25aa97b8c71c361ee4937adc1a00000227db5dd",
            ),
            (
                &AAVE_RESERVE_USED_AS_COLLATERAL_ENABLED_TOPIC,
                "00058a56ea94653cdf4f152d227ace22d4c00ad99e2a43f58cb7d9e3feb295f2",
            ),
            (
                &AAVE_SUPPLY_TOPIC,
                "2b627736bca15cd5381dcf80b0bf11fd197d01a037c52b927a881a10fb73ba61",
            ),
            (
                &AAVE_USER_E_MODE_SET_TOPIC,
                "d728da875fc88944cbf17638bcbe4af0eedaef63becd1d1c57cc097eb4608d84",
            ),
            (
                &AAVE_WITHDRAW_TOPIC,
                "3115d1449a7b732c986cba18244e897a450f61e1bb8d589cd2e69e6c8924f9f7",
            ),
            (
                &AAVE_BALANCE_TRANSFER_TOPIC,
                "4beccb90f994c31aced7a23b5611020728a23d8ec5cddd1a3e9d97b96fda8666",
            ),
            (
                &AAVE_BURN_TOPIC,
                "4cf25bc1d991c17529c25213d3cc0cda295eeaad5f13f361969b12ea48015f90",
            ),
            (
                &AAVE_MINT_TOPIC,
                "458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196",
            ),
            (
                &AAVE_ADDRESS_SET_TOPIC,
                "9ef0e8c8e52743bb38b83b17d9429141d494b8041ca6d616a6c77cebae9cd8b7",
            ),
            (
                &AAVE_ASSET_COLLATERAL_IN_EMODE_CHANGED_TOPIC,
                "79409190108b26fcb0e4570f8e240f627bf18fd01a55f751010224d5bd486098",
            ),
            (
                &AAVE_COLLATERAL_CONFIGURATION_CHANGED_TOPIC,
                "637febbda9275aea2e85c0ff690444c8d87eb2e8339bbede9715abcc89cb0995",
            ),
            (
                &AAVE_EMODE_ASSET_CATEGORY_CHANGED_TOPIC,
                "5bb69795b6a2ea222d73a5f8939c23471a1f85a99c7ca43c207f1b71f10c6264",
            ),
            (
                &AAVE_EMODE_CATEGORY_ADDED_TOPIC,
                "0acf8b4a3cace10779798a89a206a0ae73a71b63acdd3be2801d39c2ef7ab3cb",
            ),
            (
                &AAVE_POOL_CONFIGURATOR_UPDATED_TOPIC,
                "8932892569eba59c8382a089d9b732d1f49272878775235761a2a6b0309cd465",
            ),
            (
                &AAVE_POOL_DATA_PROVIDER_UPDATED_TOPIC,
                "c853974cfbf81487a14a23565917bee63f527853bcb5fa54f2ae1cdf8a38356d",
            ),
            (
                &AAVE_POOL_UPDATED_TOPIC,
                "90affc163f1a2dfedcd36aa02ed992eeeba8100a4014f0b4cdc20ea265a66627",
            ),
            (
                &AAVE_PRICE_ORACLE_UPDATED_TOPIC,
                "56b5f80d8cac1479698aa7d01605fd6111e90b15fc4d2b377417f46034876cbd",
            ),
            (
                &AAVE_PROXY_CREATED_TOPIC,
                "4a465a9bd819d9662563c1e11ae958f8109e437e7f4bf1c6ef0b9a7b3f35d478",
            ),
            (
                &AAVE_RESERVE_INITIALIZED_TOPIC,
                "3a0ca721fc364424566385a1aa271ed508cc2c0949c2272575fb3013a163a45f",
            ),
            (
                &AAVE_UPGRADED_TOPIC,
                "bc7cd75a20ee27fd9adebab32041f755214dbc6bffa90cc0225b39da2e5c2d3b",
            ),
            (
                &AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC,
                "74ab9665e7c36c29ddb78ef88a3e2eac73d35b8b16de7bc573e313e320104956",
            ),
            (
                &AAVE_DISCOUNT_RATE_STRATEGY_UPDATED_TOPIC,
                "194bd59f47b230edccccc2be58b92dde3a5dadd835751a621af59006928bccef",
            ),
            (
                &AAVE_DISCOUNT_TOKEN_UPDATED_TOPIC,
                "6b489e1dbfbe36f55c511c098bcc9d92fec7f04f74ceb75018697ab68f7d3529",
            ),
            (
                &AAVE_REDEEM_TOPIC,
                "3f693fff038bb8a046aa76d9516190ac7444f7d69cf952c4cbdc086fdef2d6fc",
            ),
            (
                &AAVE_STAKED_TOPIC,
                "6c86f3fd5118b3aa8bb4f389a617046de0a3d3d477de1a1673d227f802f616dc",
            ),
            (
                &AAVE_REWARDS_CLAIMED_TOPIC,
                "c052130bc4ef84580db505783484b067ea8b71b3bca78a7e12db7aea8658f004",
            ),
            (
                &AAVE_ASSET_SOURCE_UPDATED_TOPIC,
                "22c5b7b2d8561d39f7f210b6b326a1aa69f15311163082308ac4877db6339dc1",
            ),
            (
                &ERC20_TRANSFER_TOPIC,
                "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
            ),
        ];
        for (rust_const, python_hex) in cases {
            let python = B256::from_slice(
                &alloy::hex::decode(python_hex).expect("python hex constant is well-formed"),
            );
            assert_eq!(
                rust_const, &python,
                "Rust topic constant diverged from Python events.py ({python_hex})"
            );
        }
    }

    // ── per-event decode fixtures ──────────────────────────────────────

    #[test]
    fn decode_valid_supply() {
        // Real Aave V3 Supply signature (verified on mainnet, block 16496817):
        //   Supply(address indexed reserve, address user,
        //          address indexed onBehalfOf, uint256 amount,
        //          uint16 indexed referralCode)
        // 3 indexed params → 4 topics: [sig, reserve, onBehalfOf, referralCode].
        // Non-indexed data: abi.encode(address user, uint256 amount) = 2 words.
        // DISTINCT user ≠ onBehalfOf + non-zero referralCode — this is the
        // shape that catches a topic-indexing bug (the prior fictional layout
        // with `user` at topic[2] + `onBehalfOf` at topic[3] masked it).
        let pool = Address::from([0xa0; 20]);
        let reserve = Address::from([0xa1; 20]);
        let on_behalf_of = Address::from([0xa2; 20]);
        let user = Address::from([0xab; 20]); // distinct from on_behalf_of
        let amount = U256::from(1_000_000u64);
        let referral = 0x1234u16;

        let mut data = vec![0u8; 64];
        data[12..32].copy_from_slice(user.as_slice()); // data word 0 = user
        data[32..64].copy_from_slice(&u256_word(amount)); // data word 1 = amount

        let log = make_log(
            pool,
            vec![
                AAVE_SUPPLY_TOPIC,
                addr_word(reserve),            // topic[1] = reserve
                addr_word(on_behalf_of),       // topic[2] = onBehalfOf
                B256::new(u16_word(referral)), // topic[3] = referralCode
            ],
            data,
        );
        let event = decode_aave_supply_log(&log).unwrap();
        assert_eq!(event.pool_address, pool);
        assert_eq!(event.reserve, reserve);
        assert_eq!(event.on_behalf_of, on_behalf_of);
        assert_eq!(event.amount, amount);
        assert_eq!(event.referral_code, referral);

        // Dispatch fn wraps it.
        let DecodedAaveEvent::Supply(dispatched) = decode_aave_log(&log).unwrap() else {
            panic!("expected Supply");
        };
        assert_eq!(dispatched, event);
    }

    #[test]
    fn decode_valid_borrow() {
        let pool = Address::from([0xb0; 20]);
        let reserve = Address::from([0xb1; 20]);
        let on_behalf_of = Address::from([0xb2; 20]);
        let amount = U256::from(5_000u64);
        let rate_mode: u8 = 2; // variable
        let borrow_rate = U256::from(123_456_789u64);
        let referral = 0x5678u16; // non-zero — catches the topic-indexing bug

        // Real Aave V3 Borrow signature (verified on mainnet, block 16497464
        // referralCode=64, block 16498172 referralCode=6671):
        //   Borrow(address indexed reserve, address user,
        //          address indexed onBehalfOf, uint256 amount,
        //          DataTypes.InterestRateMode interestRateMode, uint256 borrowRate,
        //          uint16 indexed referralCode)
        // 3 indexed params → 4 topics: [sig, reserve, onBehalfOf, referralCode].
        // Non-indexed data: abi.encode(user, amount, mode, borrowRate) = 4 words.
        // DISTINCT user ≠ onBehalfOf + non-zero referralCode — shape that
        // catches a topic-indexing bug (the prior fictional layout masked it).
        let mut data = vec![0u8; 128];
        data[12..32].copy_from_slice(Address::from([0xb3; 20]).as_slice()); // data word 0 = user (distinct)
        data[32..64].copy_from_slice(&u256_word(amount));
        data[64..96].copy_from_slice(&u8_word(rate_mode));
        data[96..128].copy_from_slice(&u256_word(borrow_rate));

        let log = make_log(
            pool,
            vec![
                AAVE_BORROW_TOPIC,
                addr_word(reserve),            // topic[1] = reserve
                addr_word(on_behalf_of),       // topic[2] = onBehalfOf
                B256::new(u16_word(referral)), // topic[3] = referralCode
            ],
            data,
        );
        let event = decode_aave_borrow_log(&log).unwrap();
        assert_eq!(event.reserve, reserve);
        assert_eq!(event.on_behalf_of, on_behalf_of);
        assert_eq!(event.amount, amount);
        assert_eq!(event.interest_rate_mode, rate_mode);
        assert_eq!(event.borrow_rate, borrow_rate);
        assert_eq!(event.referral_code, referral);
    }

    #[test]
    fn decode_valid_repay() {
        let pool = Address::from([0xc0; 20]);
        let reserve = Address::from([0xc1; 20]);
        let user = Address::from([0xc2; 20]);
        let repayer = Address::from([0xc3; 20]);
        let amount = U256::from(2_000u64);

        // data = abi.encode(uint256 amount, bool useATokens)
        let mut data = vec![0u8; 64];
        data[0..32].copy_from_slice(&u256_word(amount));
        data[32..64].copy_from_slice(&bool_word(true));

        let log = make_log(
            pool,
            vec![
                AAVE_REPAY_TOPIC,
                addr_word(reserve),
                addr_word(user),
                addr_word(repayer),
            ],
            data,
        );
        let event = decode_aave_repay_log(&log).unwrap();
        assert_eq!(event.reserve, reserve);
        assert_eq!(event.user, user);
        assert_eq!(event.repayer, repayer);
        assert_eq!(event.amount, amount);
        assert!(event.use_a_tokens);
    }

    #[test]
    fn decode_valid_withdraw() {
        let pool = Address::from([0xd0; 20]);
        let reserve = Address::from([0xd1; 20]);
        let user = Address::from([0xd2; 20]);
        let to = Address::from([0xd3; 20]);
        let amount = U256::from(7_500u64);

        let log = make_log(
            pool,
            vec![
                AAVE_WITHDRAW_TOPIC,
                addr_word(reserve),
                addr_word(user),
                addr_word(to),
            ],
            u256_word(amount).to_vec(),
        );
        let event = decode_aave_withdraw_log(&log).unwrap();
        assert_eq!(event.reserve, reserve);
        assert_eq!(event.user, user);
        assert_eq!(event.to, to);
        assert_eq!(event.amount, amount);
    }

    #[test]
    fn decode_valid_liquidation_call() {
        let pool = Address::from([0xe0; 20]);
        let collateral_asset = Address::from([0xe1; 20]);
        let debt_asset = Address::from([0xe2; 20]);
        let user = Address::from([0xe3; 20]);
        let debt_to_cover = U256::from(1_000u64);
        let liquidated_collateral = U256::from(900u64);
        let liquidator = Address::from([0xe4; 20]);

        // data = abi.encode(uint256 debtToCover, uint256 liquidatedCollateralAmount,
        //   address liquidator, bool receiveAToken).
        let mut data = vec![0u8; 128];
        data[0..32].copy_from_slice(&u256_word(debt_to_cover));
        data[32..64].copy_from_slice(&u256_word(liquidated_collateral));
        data[64 + 12..96].copy_from_slice(liquidator.as_slice());
        data[96..128].copy_from_slice(&bool_word(false));

        let log = make_log(
            pool,
            vec![
                AAVE_LIQUIDATION_CALL_TOPIC,
                addr_word(collateral_asset),
                addr_word(debt_asset),
                addr_word(user),
            ],
            data,
        );
        let event = decode_aave_liquidation_call_log(&log).unwrap();
        assert_eq!(event.collateral_asset, collateral_asset);
        assert_eq!(event.debt_asset, debt_asset);
        assert_eq!(event.user, user);
        assert_eq!(event.debt_to_cover, debt_to_cover);
        assert_eq!(event.liquidated_collateral_amount, liquidated_collateral);
        assert_eq!(event.liquidator, liquidator);
        assert!(!event.receive_a_token);
    }

    #[test]
    fn decode_valid_reserve_data_updated() {
        let pool = Address::from([0xf0; 20]);
        let reserve = Address::from([0xf1; 20]);
        let liquidity_rate = U256::from(100u64);
        let stable_borrow_rate = U256::ZERO; // deprecated
        let variable_borrow_rate = U256::from(200u64);
        let liquidity_index = U256::from(1_000_000_000_000_000_000u64); // 1e27 (RAY)
        let variable_borrow_index = U256::from(1_000_000_000_000_000_000u64);

        let mut data = vec![0u8; 160];
        data[0..32].copy_from_slice(&u256_word(liquidity_rate));
        data[32..64].copy_from_slice(&u256_word(stable_borrow_rate));
        data[64..96].copy_from_slice(&u256_word(variable_borrow_rate));
        data[96..128].copy_from_slice(&u256_word(liquidity_index));
        data[128..160].copy_from_slice(&u256_word(variable_borrow_index));

        let log = make_log(
            pool,
            vec![AAVE_RESERVE_DATA_UPDATED_TOPIC, addr_word(reserve)],
            data,
        );
        let event = decode_aave_reserve_data_updated_log(&log).unwrap();
        assert_eq!(event.reserve, reserve);
        assert_eq!(event.liquidity_rate, liquidity_rate);
        assert_eq!(event.stable_borrow_rate, stable_borrow_rate);
        assert_eq!(event.variable_borrow_rate, variable_borrow_rate);
        assert_eq!(event.liquidity_index, liquidity_index);
        assert_eq!(event.variable_borrow_index, variable_borrow_index);
    }

    #[test]
    fn decode_valid_scaled_token_mint() {
        let token = Address::from([0x10; 20]);
        let caller = Address::from([0x11; 20]);
        let on_behalf_of = Address::from([0x12; 20]);
        let value = U256::from(500u64);
        let balance_increase = U256::from(50u64);
        let index = U256::from(1_000_000_000_000_000_000u64);

        let mut data = vec![0u8; 96];
        data[0..32].copy_from_slice(&u256_word(value));
        data[32..64].copy_from_slice(&u256_word(balance_increase));
        data[64..96].copy_from_slice(&u256_word(index));

        let log = make_log(
            token,
            vec![AAVE_MINT_TOPIC, addr_word(caller), addr_word(on_behalf_of)],
            data,
        );
        let event = decode_aave_scaled_token_mint_log(&log).unwrap();
        assert_eq!(event.token_address, token);
        assert_eq!(event.caller, caller);
        assert_eq!(event.on_behalf_of, on_behalf_of);
        assert_eq!(event.value, value);
        assert_eq!(event.balance_increase, balance_increase);
        assert_eq!(event.index, index);
    }

    #[test]
    fn decode_valid_scaled_token_burn() {
        let token = Address::from([0x20; 20]);
        let from = Address::from([0x21; 20]);
        let target = Address::from([0x22; 20]);
        let value = U256::from(300u64);
        let balance_increase = U256::from(30u64);
        let index = U256::from(1_000_000_000_000_000_000u64);

        let mut data = vec![0u8; 96];
        data[0..32].copy_from_slice(&u256_word(value));
        data[32..64].copy_from_slice(&u256_word(balance_increase));
        data[64..96].copy_from_slice(&u256_word(index));

        let log = make_log(
            token,
            vec![AAVE_BURN_TOPIC, addr_word(from), addr_word(target)],
            data,
        );
        let event = decode_aave_scaled_token_burn_log(&log).unwrap();
        assert_eq!(event.from, from);
        assert_eq!(event.target, target);
        assert_eq!(event.value, value);
        assert_eq!(event.balance_increase, balance_increase);
        assert_eq!(event.index, index);
    }

    #[test]
    fn decode_valid_balance_transfer() {
        let token = Address::from([0x30; 20]);
        let from = Address::from([0x31; 20]);
        let to = Address::from([0x32; 20]);
        let value = U256::from(250u64);
        let index = U256::from(1_000_000_000_000_000_000u64);

        let mut data = vec![0u8; 64];
        data[0..32].copy_from_slice(&u256_word(value));
        data[32..64].copy_from_slice(&u256_word(index));

        let log = make_log(
            token,
            vec![AAVE_BALANCE_TRANSFER_TOPIC, addr_word(from), addr_word(to)],
            data,
        );
        let event = decode_aave_scaled_token_balance_transfer_log(&log).unwrap();
        assert_eq!(event.token_address, token);
        assert_eq!(event.from, from);
        assert_eq!(event.to, to);
        assert_eq!(event.value, value);
        assert_eq!(event.index, index);
    }

    #[test]
    fn decode_valid_reserve_initialized() {
        let configurator = Address::from([0x40; 20]);
        let asset = Address::from([0x41; 20]);
        let a_token = Address::from([0x42; 20]);
        let stable_debt = Address::from([0x43; 20]); // deprecated
        let v_token = Address::from([0x44; 20]);
        let strategy = Address::from([0x45; 20]);

        let mut data = vec![0u8; 96];
        data[12..32].copy_from_slice(stable_debt.as_slice());
        data[32 + 12..64].copy_from_slice(v_token.as_slice());
        data[64 + 12..96].copy_from_slice(strategy.as_slice());

        let log = make_log(
            configurator,
            vec![
                AAVE_RESERVE_INITIALIZED_TOPIC,
                addr_word(asset),
                addr_word(a_token),
            ],
            data,
        );
        let event = decode_aave_reserve_initialized_log(&log).unwrap();
        assert_eq!(event.asset, asset);
        assert_eq!(event.a_token, a_token);
        assert_eq!(event.stable_debt_token, stable_debt);
        assert_eq!(event.variable_debt_token, v_token);
        assert_eq!(event.interest_rate_strategy_address, strategy);
    }

    #[test]
    fn decode_valid_user_e_mode_set() {
        let pool = Address::from([0x50; 20]);
        let user = Address::from([0x51; 20]);
        let category_id: u8 = 2;

        let log = make_log(
            pool,
            vec![AAVE_USER_E_MODE_SET_TOPIC, addr_word(user)],
            u8_word(category_id).to_vec(),
        );
        let event = decode_aave_user_e_mode_set_log(&log).unwrap();
        assert_eq!(event.user, user);
        assert_eq!(event.category_id, category_id);
    }

    #[test]
    fn decode_valid_reserve_used_as_collateral_enabled() {
        let pool = Address::from([0x60; 20]);
        let reserve = Address::from([0x61; 20]);
        let user = Address::from([0x62; 20]);

        let log = make_log(
            pool,
            vec![
                AAVE_RESERVE_USED_AS_COLLATERAL_ENABLED_TOPIC,
                addr_word(reserve),
                addr_word(user),
            ],
            vec![],
        );
        let event = decode_aave_reserve_used_as_collateral_enabled_log(&log).unwrap();
        assert_eq!(event.reserve, reserve);
        assert_eq!(event.user, user);
    }

    #[test]
    fn decode_valid_staked() {
        let stk = Address::from([0x70; 20]);
        let from = Address::ZERO; // mint-from-zero on a true stake
        let on_behalf_of = Address::from([0x71; 20]);
        let amount = U256::from(1_000u64);

        let log = make_log(
            stk,
            vec![AAVE_STAKED_TOPIC, addr_word(from), addr_word(on_behalf_of)],
            u256_word(amount).to_vec(),
        );
        let event = decode_aave_staked_log(&log).unwrap();
        assert_eq!(event.stk_aave_address, stk);
        assert_eq!(event.from, from);
        assert_eq!(event.on_behalf_of, on_behalf_of);
        assert_eq!(event.amount, amount);
    }

    #[test]
    fn decode_valid_redeem() {
        let stk = Address::from([0x80; 20]);
        let from = Address::from([0x81; 20]);
        let to = Address::from([0x82; 20]);
        let amount = U256::from(500u64);

        let log = make_log(
            stk,
            vec![AAVE_REDEEM_TOPIC, addr_word(from), addr_word(to)],
            u256_word(amount).to_vec(),
        );
        let event = decode_aave_redeem_log(&log).unwrap();
        assert_eq!(event.from, from);
        assert_eq!(event.to, to);
        assert_eq!(event.amount, amount);
    }

    #[test]
    fn decode_valid_discount_percent_updated() {
        let v_token = Address::from([0x90; 20]);
        let user = Address::from([0x91; 20]);
        let old_percent = U256::from(10u64);
        let new_percent = U256::from(20u64);

        let log = make_log(
            v_token,
            vec![
                AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC,
                addr_word(user),
                B256::new(u256_word(new_percent)),
            ],
            u256_word(old_percent).to_vec(),
        );
        let event = decode_aave_discount_percent_updated_log(&log).unwrap();
        assert_eq!(event.user, user);
        assert_eq!(event.old_discount_percent, old_percent);
        assert_eq!(event.new_discount_percent, new_percent);
    }

    #[test]
    fn decode_valid_erc20_transfer() {
        let token = Address::from([0xa0; 20]);
        let from = Address::from([0xa1; 20]);
        let to = Address::from([0xa2; 20]);
        let value = U256::from(999u64);

        let log = make_log(
            token,
            vec![ERC20_TRANSFER_TOPIC, addr_word(from), addr_word(to)],
            u256_word(value).to_vec(),
        );
        let event = decode_aave_erc20_transfer_log(&log).unwrap();
        assert_eq!(event.token_address, token);
        assert_eq!(event.from, from);
        assert_eq!(event.to, to);
        assert_eq!(event.value, value);
    }

    #[test]
    fn decode_valid_emode_category_added_with_label() {
        let configurator = Address::from([0xb0; 20]);
        let oracle = Address::from([0xb1; 20]);
        let label = "ETH";

        // data = abi.encode(uint256 ltv, uint256 lt, uint256 lb, address oracle, string label)
        // = 5 fixed words (160 bytes) + dynamic string. The string offset is in word 4.
        // The string itself = 1 length word + 1 padded-bytes word.
        let ltv = U256::from(8250u64);
        let lt = U256::from(9000u64);
        let lb = U256::from(9300u64);
        let category_id: u8 = 1;

        // Encode by hand: fixed 5 words (ltv, lt, lb, oracle, string-offset) + string.
        let string_bytes = label.as_bytes();
        let string_offset = 5 * 32; // 160 — the string starts right after the fixed words.
        let string_padded_len = string_bytes.len().div_ceil(32) * 32;
        let mut data = vec![0u8; string_offset + 32 + string_padded_len];
        data[0..32].copy_from_slice(&u256_word(ltv));
        data[32..64].copy_from_slice(&u256_word(lt));
        data[64..96].copy_from_slice(&u256_word(lb));
        data[96 + 12..128].copy_from_slice(oracle.as_slice());
        data[128..160].copy_from_slice(&u256_word(U256::from(string_offset)));
        // At string_offset: length word + bytes.
        data[string_offset..string_offset + 32]
            .copy_from_slice(&u256_word(U256::from(string_bytes.len())));
        data[string_offset + 32..string_offset + 32 + string_bytes.len()]
            .copy_from_slice(string_bytes);

        let log = make_log(
            configurator,
            vec![
                AAVE_EMODE_CATEGORY_ADDED_TOPIC,
                B256::new(u8_word(category_id)),
            ],
            data,
        );
        let event = decode_aave_e_mode_category_added_log(&log).unwrap();
        assert_eq!(event.category_id, category_id);
        assert_eq!(event.ltv, ltv);
        assert_eq!(event.liquidation_threshold, lt);
        assert_eq!(event.liquidation_bonus, lb);
        assert_eq!(event.oracle, oracle);
        assert_eq!(event.label, label);
    }

    // ── dispatch: unknown topic returns None ────────────────────────────

    #[test]
    fn decode_unknown_topic_returns_none() {
        let log = make_log(
            Address::ZERO,
            vec![B256::repeat_byte(0x42)], // unknown topic
            vec![0u8; 96],
        );
        assert!(decode_aave_log(&log).is_none(), "unknown topic → None");
    }

    #[test]
    fn decode_log_with_no_topics_returns_none() {
        let log = make_log(Address::ZERO, vec![], vec![0u8; 32]);
        assert!(decode_aave_log(&log).is_none(), "no topics → None");
    }

    #[test]
    fn decode_truncated_data_returns_none() {
        // Supply expects ≥ 64 bytes; give it 32.
        let log = make_log(
            Address::ZERO,
            vec![
                AAVE_SUPPLY_TOPIC,
                B256::ZERO,
                B256::ZERO,
                B256::ZERO,
                B256::ZERO,
            ],
            vec![0u8; 32],
        );
        assert!(
            decode_aave_supply_log(&log).is_none(),
            "truncated data → None"
        );
    }

    #[test]
    fn decode_wrong_topic_returns_none_for_per_event_decoder() {
        // Pool decoder shouldn't match a Supply topic.
        let log = make_log(
            Address::ZERO,
            vec![
                AAVE_SUPPLY_TOPIC,
                B256::ZERO,
                B256::ZERO,
                B256::ZERO,
                B256::ZERO,
            ],
            vec![0u8; 64],
        );
        assert!(decode_aave_borrow_log(&log).is_none(), "wrong topic → None");
    }
}

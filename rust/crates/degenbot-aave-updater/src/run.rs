//! The transactional Aave V3 chunk-write apply core + its atomicity tests.
//!
//! See the crate-level docs for the §3.4 atomicity invariant this file enforces.

use alloy::primitives::U256;
use degenbot_db::DegenbotDb;
#[cfg(test)]
use rusqlite::params;
use rusqlite::Connection;

/// One pre-decoded Aave V3 event for the chunk apply loop.
///
/// Each variant carries the resolved ids/fields the matching
/// `DegenbotDb::apply_*_on_conn` fn consumes — the RPC fetch + decode + the
/// `get_or_create_user` / `get_asset_by_token_type` resolution happen in the
/// `run_aave_update` orchestrator (sibling `6SWY4R`, NOT this task), which
/// constructs this enum. This core does NO RPC, NO ABI decode, NO
/// address→id resolution.
///
/// # The event→fn dispatch map
///
/// | Variant                              | `_on_conn` fn                                    |
/// |--------------------------------------|---------------------------------------------------|
/// | [`CollateralConfigurationChanged`]  | [`apply_collateral_configuration_changed_on_conn`] |
/// | [`EModeCategoryAdded`]               | [`apply_e_mode_category_added_on_conn`]            |
/// | [`EModeAssetCategoryChanged`]        | [`apply_emode_asset_category_changed_on_conn`]     |
/// | [`AssetCollateralInEModeChanged`]    | [`apply_asset_collateral_in_emode_changed_on_conn`] |
/// | [`ReserveUsedAsCollateral`]          | [`apply_reserve_used_as_collateral_on_conn`]       |
/// | [`UserEModeSet`]                     | [`apply_user_e_mode_set_on_conn`]                  |
/// | [`PriceOracleUpdated`]               | [`apply_price_oracle_updated_on_conn`]             |
/// | [`AssetSourceUpdated`]               | [`apply_asset_source_updated_on_conn`]             |
/// | [`ReserveDataUpdated`]               | [`apply_reserve_data_updated_on_conn`]             |
/// | [`ReserveInitialized`]              | [`apply_reserve_initialized_on_conn`]              |
///
/// [`apply_collateral_configuration_changed_on_conn`]: DegenbotDb::apply_collateral_configuration_changed_on_conn
/// [`apply_e_mode_category_added_on_conn`]: DegenbotDb::apply_e_mode_category_added_on_conn
/// [`apply_emode_asset_category_changed_on_conn`]: DegenbotDb::apply_emode_asset_category_changed_on_conn
/// [`apply_asset_collateral_in_emode_changed_on_conn`]: DegenbotDb::apply_asset_collateral_in_emode_changed_on_conn
/// [`apply_reserve_used_as_collateral_on_conn`]: DegenbotDb::apply_reserve_used_as_collateral_on_conn
/// [`apply_user_e_mode_set_on_conn`]: DegenbotDb::apply_user_e_mode_set_on_conn
/// [`apply_price_oracle_updated_on_conn`]: DegenbotDb::apply_price_oracle_updated_on_conn
/// [`apply_asset_source_updated_on_conn`]: DegenbotDb::apply_asset_source_updated_on_conn
/// [`apply_reserve_data_updated_on_conn`]: DegenbotDb::apply_reserve_data_updated_on_conn
/// [`apply_reserve_initialized_on_conn`]: DegenbotDb::apply_reserve_initialized_on_conn
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AaveChunkEvent {
    /// `CollateralConfigurationChanged(asset, config_bitmap)` — decode the
    /// bitmap + upsert the `aave_v3_asset_configs` row.
    CollateralConfigurationChanged { asset_id: i64, config_bitmap: U256 },
    /// `EModeCategoryAdded(id, label, ltv, lt, bonus, price_source)` — upsert
    /// the `aave_v3_emode_categories` row.
    EModeCategoryAdded {
        market_id: i64,
        category_id: i64,
        ltv: u64,
        liquidation_threshold: u64,
        liquidation_bonus: u64,
        /// The checksummed oracle address (`None` when the event's address is zero).
        price_source: Option<String>,
        label: String,
    },
    /// `EModeAssetCategoryChanged(asset, category)` — the older variant:
    /// unconditionally set `e_mode_category_id` (`None` when `category_id` is 0).
    EModeAssetCategoryChanged { asset_id: i64, new_category_id: i64 },
    /// `AssetCollateralInEModeChanged(asset, category, is_collateral)` — the
    /// newer Aave v3.4+ variant.
    AssetCollateralInEModeChanged {
        asset_id: i64,
        category_id: i64,
        is_collateral: bool,
    },
    /// `ReserveUsedAsCollateral{Enabled,Disabled}(user, asset)` — collapses to
    /// setting the `enabled` flag on the `aave_v3_user_collateral_configs` row.
    ReserveUsedAsCollateral {
        user_id: i64,
        asset_id: i64,
        enabled: bool,
    },
    /// `UserEModeSet(user, e_mode)` — set the user's `aave_v3_users.e_mode`.
    UserEModeSet { user_id: i64, e_mode: i64 },
    /// `PriceOracleUpdated(market, new_oracle)` — register/replace the
    /// `PRICE_ORACLE` `aave_v3_contracts` row.
    PriceOracleUpdated {
        market_id: i64,
        new_oracle_address: String,
    },
    /// `AssetSourceUpdated(asset, source)` — set the
    /// `aave_v3_assets.price_source` column.
    AssetSourceUpdated {
        asset_id: i64,
        source_address: String,
    },
    /// `ReserveDataUpdated(reserve, liquidityRate, stableBorrowRate,
    /// variableBorrowRate, liquidityIndex, variableBorrowIndex)` — update the
    /// `aave_v3_assets` row's indices/rates (UR7QNL — one of the only two Pool
    /// events that write DB rows directly). `stableBorrowRate` is deprecated on
    /// Aave V3 + dropped (mirrors the Python handler). Stored raw (27-decimal
    /// ray as decimal `VARCHAR(78)`); no ray-math in the apply path.
    ReserveDataUpdated {
        asset_id: i64,
        liquidity_rate: U256,
        variable_borrow_rate: U256,
        liquidity_index: U256,
        variable_borrow_index: U256,
        block_number: u64,
    },
    /// `ReserveInitialized(asset, aToken, stableDebtToken, variableDebtToken,
    /// interestRateStrategyAddress)` — seed the `aave_v3_assets` row (UR7QNL —
    /// the other direct Pool-event DB writer). The orchestrator (6SWY4R)
    /// pre-resolves the erc20 token ids + the `ATOKEN_REVISION()` /
    /// `DEBT_TOKEN_REVISION()` (via the EIP-1967 implementation slot) + the
    /// `getSourceOfAsset` `price_source`; this enum carries the resolved
    /// fields (mirrors CXRGX4 design decision #1 — the apply core is pure
    /// substrate, no RPC, no address→id resolution).
    ReserveInitialized {
        market_id: i64,
        underlying_asset_id: i64,
        a_token_id: i64,
        a_token_revision: i64,
        v_token_id: i64,
        v_token_revision: i64,
        /// The checksummed oracle address (`None` when the event's address is zero).
        price_source: Option<String>,
    },
    /// `ScaledTokenMint(from, to, value)` — aToken/vToken Mint event (5Z3QQ2 —
    /// SCALEAPPLY). Carries the PRE-COMPUTED signed `balance_delta` (the
    /// orchestrator/parser ran [`ScaledTokenProcessor::process_collateral_mint`]
    /// / [`ScaledTokenProcessor::process_debt_mint`] BEFORE constructing this
    /// variant — mirrors CXRGX4 design decision #1: the apply core is pure
    /// substrate, no processor calls). The apply dispatch just forwards the
    /// delta to [`DegenbotDb::apply_scaled_token_mint_on_conn`].
    ScaledTokenMint {
        /// Which position table (collateral for aToken / debt for vToken).
        position: degenbot_db::ScaledTokenPosition,
        /// The pre-resolved position row id (the orchestrator called
        /// `get_or_create_collateral_position_on_conn` /
        /// `get_or_create_debt_position_on_conn`).
        position_id: i64,
        /// The pre-computed signed delta (positive for true mint, negative
        /// for the interest-exceeds-value edge case).
        balance_delta: alloy::primitives::I256,
        /// The event's index (the apply fn reconciles `last_index` to it via
        /// max-with-prev).
        new_index: alloy::primitives::U256,
    },
    /// `ScaledTokenBurn(from, to, value)` — aToken/vToken Burn event (5Z3QQ2).
    /// Carries the PRE-COMPUTED signed `balance_delta` (always negative — the
    /// processor's `process_collateral_burn` / `process_debt_burn` returned
    /// it).
    ScaledTokenBurn {
        position: degenbot_db::ScaledTokenPosition,
        position_id: i64,
        balance_delta: alloy::primitives::I256,
        new_index: alloy::primitives::U256,
    },
    /// `BalanceTransfer(from, to, value)` — aToken transfer between users
    /// (5Z3QQ2). Carries the resolved `from_position_id` + `to_position_id`
    /// (both collateral — `BalanceTransfer` is aToken-only) + the scaled
    /// amount + the transfer's index. The apply fn debits `from`, credits
    /// `to`, + reconciles both positions' `last_index`.
    ScaledTokenTransfer {
        from_position_id: i64,
        to_position_id: i64,
        scaled_amount: alloy::primitives::U256,
        transfer_index: alloy::primitives::U256,
    },
    // ── RYKCC4 (SPECIALAPPLY): GHO + stkAAVE + Rewards events ─────────────
    /// GHO `DiscountPercentUpdated(user, oldPercent, newPercent)` — sets the
    /// user's `gho_discount` (an `i64` percentage; the Aave protocol caps at
    /// 100%). Port of `event_handlers._process_discount_percent_updated_event`.
    GhoDiscountPercentUpdated {
        /// Pre-resolved `aave_v3_users.id` (the orchestrator called
        /// `get_or_create_user_on_conn`).
        user_id: i64,
        /// The new discount percent.
        new_discount_percent: i64,
    },
    /// GHO `DiscountRateStrategyUpdated(oldStrategy, newStrategy)` — sets the
    /// chain-unique GHO token row's `v_gho_discount_rate_strategy`. Port of
    /// `event_handlers._process_discount_rate_strategy_updated_event`.
    GhoDiscountRateStrategyUpdated {
        /// Pre-resolved `aave_gho_tokens.id` (chain-unique).
        gho_token_id: i64,
        /// The checksummed address of the new discount rate strategy contract,
        /// or `None` to clear it.
        new_strategy: Option<String>,
    },
    /// GHO `DiscountTokenUpdated(oldToken, newToken)` — sets the GHO token
    /// row's `v_gho_discount_token`. Port of
    /// `event_handlers._process_discount_token_updated_event`.
    GhoDiscountTokenUpdated {
        gho_token_id: i64,
        new_discount_token: Option<String>,
    },
    /// stkAAVE `Staked(staker, amount, totalRewards)` — increments the user's
    /// `stk_aave_balance` by `amount`. Port of
    /// `stkaave.process_stk_aave_transfer_event` (the `Transfer(from=0,
    /// to=X)` arm — the Staked event coincides with a mint Transfer).
    StkAaveStaked {
        /// Pre-resolved `aave_v3_users.id` (the staker).
        user_id: i64,
        /// The staked amount (the `amount` field; `totalRewards` is a
        /// diagnostics-only field + is NOT applied to any balance).
        amount: alloy::primitives::U256,
    },
    /// stkAAVE `Redeem(redeemer, staker, amount)` — decrements the redeemer's
    /// `stk_aave_balance` by `amount`. Port of
    /// `stkaave.process_stk_aave_transfer_event` (the `Transfer(from=X,
    /// to=0)` arm — the Redeem event coincides with a burn Transfer).
    StkAaveRedeem {
        /// Pre-resolved `aave_v3_users.id` (the redeemer).
        user_id: i64,
        /// The redeemed amount.
        amount: alloy::primitives::U256,
    },
    /// `RewardsController` `RewardsClaimed(user, reward, to, claimer,
    /// claimedAmount)` — **no-op apply**. Investigation (RYKCC4) confirmed the
    /// Python declares the event in `events.py` but has NO handler: rewards
    /// claims surface only via the stkAAVE token's `Transfer` events
    /// (`transaction_processor.py:249`). The DB has no rewards table. The
    /// variant exists for parser routing/event-accounting only.
    RewardsClaimed {
        user_id: i64,
        reward_token_id: i64,
        claimer_id: i64,
        claimed_amount: alloy::primitives::U256,
    },
    /// The bad-debt liquidation reset (C3 — `DEFICIT_CREATED` path). The
    /// contract burns the ENTIRE remaining debt (not just `debtToCover`) when
    /// a `DeficitCreated` event accompanies a `LiquidationCall` — the
    /// protocol writes off the bad debt. Sets the debt position's `balance` to
    /// 0 + advances `last_index` (max-with-prev). Mirrors the Python's
    /// `debt_position.balance = 0` + the `last_index` guard in
    /// `_process_debt_burn_with_match`'s bad-debt arm.
    DebtPositionReset {
        /// The debt position row id (`aave_v3_debt_positions.id`).
        position_id: i64,
        /// The event's index (the apply fn reconciles `last_index` to it via
        /// max-with-prev).
        new_index: alloy::primitives::U256,
    },
    // ── 6SWY4R-2b: the 6 missing-variant config events ───────────────────
    /// `Upgraded(implementation)` — emitted by an aToken or vToken proxy. The
    /// dispatch resolves which asset (by `a_token`/`v_token` address match) +
    /// RPCs `ATOKEN_REVISION()`/`DEBT_TOKEN_REVISION()` on the new
    /// implementation. Port of `_process_scaled_token_upgrade_event`
    /// (event_handlers.py:848-940). When the upgraded token is the GHO vToken
    /// and the new revision ≥ `GHO_DISCOUNT_DEPRECATION_REVISION` (4), the
    /// `deprecated_gho_token_id` is `Some(gho_token.id)`: the apply fn then
    /// clears `v_gho_discount_token` and `v_gho_discount_rate_strategy`, and
    /// bulk-resets all users' `gho_discount` to 0 (the GHO-discount-deprecation
    /// side effect — the riskiest piece).
    Upgraded {
        /// `aave_v3_assets.id` (the asset whose aToken/vToken was upgraded).
        asset_id: i64,
        /// The market the asset belongs to (for the bulk user reset scope).
        market_id: i64,
        /// `true` → update `a_token_revision`; `false` → `v_token_revision`.
        is_a_token: bool,
        /// The RPC-resolved new revision.
        new_revision: i64,
        /// `Some(gho_token_id)` when the GHO discount deprecation fires
        /// (vToken upgraded to rev ≥ 4 + it's the GHO vToken); `None` otherwise.
        deprecated_gho_token_id: Option<i64>,
    },
    /// `PoolUpdated`/`PoolConfiguratorUpdated(old, new)` — RPC
    /// `POOL_REVISION()`/`CONFIGURATOR_REVISION()` on the new address → update
    /// the `aave_v3_contracts` row's `revision` (LOOKED UP BY NAME: "POOL"/
    /// `POOL_CONFIGURATOR`). Port of `_update_contract_revision`
    /// (event_handlers.py:944-974). **§4.2 parity:** the Python updates ONLY
    /// `revision` — NOT `address` (the proxy address is stable; the `new_address`
    /// is used only for the RPC call). The apply mirrors this exactly.
    ContractRevisionUpdated {
        /// The market owning the contract row.
        market_id: i64,
        /// `"POOL"` or `"POOL_CONFIGURATOR"` (the contract-row name).
        contract_name: String,
        /// The RPC-resolved new revision.
        new_revision: i64,
    },
    /// `PoolDataProviderUpdated(old, new)` — INSERT the `POOL_DATA_PROVIDER`
    /// contract row when `old == ZERO_ADDRESS`, else UPDATE the existing row's
    /// `address` (looked up by the old address). Port of
    /// `_process_pool_data_provider_updated_event` (event_handlers.py:1017-1046).
    /// Pure decode — no RPC. `old_address` is `None` when the event's old is
    /// the zero address (the INSERT path).
    PoolDataProviderUpdated {
        /// The market owning the contract row.
        market_id: i64,
        /// `None` when `old` is the zero address (INSERT path); else the
        /// checksummed old address (UPDATE-by-old-address path).
        old_address: Option<String>,
        /// The checksummed new address.
        new_address: String,
    },
    /// `AddressSet(id, old, new)` + `ProxyCreated(id, proxy, impl)` (the
    /// id-filtered ones) — INSERT a contract row.
    /// `AddressSet` (event_handlers.py:1048-1078): the `id` is ASCII-decoded
    /// from the bytes32 topic + null-stripped → the `name`; `old` is asserted
    /// == zero (the dispatch returns Err otherwise). `revision` is `None`.
    /// `ProxyCreated` (event_handlers.py:977-1008): the `id` is matched
    /// against the right-padded ASCII bytes32 `b"POOL"`/`b"POOL_CONFIGURATOR"`
    /// (NOT `keccak256` — §4.2 finding); the match resolves the `name` + the
    /// revision-function (`POOL_REVISION`/`CONFIGURATOR_REVISION`). The
    /// dispatch RPCs the revision on the implementation address; `revision`
    /// is `Some`.
    ContractInserted {
        market_id: i64,
        name: String,
        address: String,
        /// `Some(rev)` for `ProxyCreated` (RPC-fetched); `None` for `AddressSet`
        /// (no revision RPC — the Python doesn't fetch one).
        revision: Option<i64>,
    },
}

/// Per-event-type apply counts for a chunk (mirrors `ChunkWriteReport`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AaveChunkWriteReport {
    pub collateral_configuration_changed: usize,
    pub e_mode_category_added: usize,
    pub e_mode_asset_category_changed: usize,
    pub asset_collateral_in_emode_changed: usize,
    pub reserve_used_as_collateral: usize,
    pub user_e_mode_set: usize,
    pub price_oracle_updated: usize,
    pub asset_source_updated: usize,
    /// UR7QNL — the two direct-write Pool events.
    pub reserve_data_updated: usize,
    pub reserve_initialized: usize,
    /// 5Z3QQ2 — the three `ScaledToken` (aToken/vToken) events.
    pub scaled_token_mint: usize,
    pub scaled_token_burn: usize,
    pub scaled_token_transfer: usize,
    /// RYKCC4 (SPECIALAPPLY) — the GHO + stkAAVE + Rewards events.
    pub gho_discount_percent_updated: usize,
    pub gho_discount_rate_strategy_updated: usize,
    pub gho_discount_token_updated: usize,
    pub stk_aave_staked: usize,
    pub stk_aave_redeem: usize,
    /// RYKCC4 no-op variant — the count is tracked for accounting even though
    /// the apply writes nothing.
    pub rewards_claimed: usize,
    /// The bad-debt liquidation reset count (C3 — `DebtPositionReset`).
    pub debt_position_reset: usize,
    /// 6SWY4R-2b — the 6 missing-variant config events.
    pub upgraded: usize,
    pub contract_revision_updated: usize,
    pub pool_data_provider_updated: usize,
    pub contract_inserted: usize,
    /// The `chunk_end_block` stamped onto `aave_v3_markets.last_update_block`
    /// as the LAST write in the transaction. `None` if `events` was empty (no
    /// stamp written — mirrors the precedent's "no events ⇒ no stamp" guard
    /// is NOT taken here; `chunk_end_block` is always stamped when this core is
    /// invoked with a chunk range). Set on commit.
    pub stamped_block: Option<u64>,
}

/// Apply a chunk's worth of pre-decoded Aave V3 events under the caller's
/// `Transaction` (borrowed as a `&Connection`), then stamp
/// `aave_v3_markets.last_update_block = chunk_end_block` as the LAST write.
///
/// Pure, synchronous, transactional. NONE of: RPC, ABI decode, `pyo3`,
/// `database_path`, `open_for_writes`. The caller owns the `Connection` +
/// its `Transaction`'s commit/rollback — every write goes through here on the
/// single connection, + the commit is the single point of durability.
///
/// # The §3.4 atomicity invariant
///
/// All `apply_*` calls + the `last_update_block` stamp go through `_on_conn`
/// fns on this one connection. Any `?` early-return (a `UNIQUE` violation, a
/// constraint failure, ...) leaves the caller's `Transaction` uncommitted →
/// it drops → the whole chunk reverts → the stamp does NOT advance → a
/// restart re-processes the chunk clean (restart-invariant).
///
/// # Errors
///
/// Returns [`degenbot_db::DbError`] on any apply/lookup failure — the caller
/// drops the `Transaction` (rollback) on `Err`.
#[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
pub fn apply_aave_chunk_writes_on_conn(
    conn: &Connection,
    market_id: i64,
    events: &[AaveChunkEvent],
    chunk_end_block: u64,
) -> Result<AaveChunkWriteReport, degenbot_db::DbError> {
    let mut report = AaveChunkWriteReport::default();

    for event in events {
        match event {
            AaveChunkEvent::CollateralConfigurationChanged {
                asset_id,
                config_bitmap,
            } => {
                DegenbotDb::apply_collateral_configuration_changed_on_conn(
                    conn,
                    *asset_id,
                    *config_bitmap,
                )?;
                report.collateral_configuration_changed += 1;
            }
            AaveChunkEvent::EModeCategoryAdded {
                market_id: ev_market_id,
                category_id,
                ltv,
                liquidation_threshold,
                liquidation_bonus,
                price_source,
                label,
            } => {
                DegenbotDb::apply_e_mode_category_added_on_conn(
                    conn,
                    *ev_market_id,
                    *category_id,
                    *ltv,
                    *liquidation_threshold,
                    *liquidation_bonus,
                    price_source.as_deref(),
                    label,
                )?;
                report.e_mode_category_added += 1;
            }
            AaveChunkEvent::EModeAssetCategoryChanged {
                asset_id,
                new_category_id,
            } => {
                DegenbotDb::apply_emode_asset_category_changed_on_conn(
                    conn,
                    *asset_id,
                    *new_category_id,
                )?;
                report.e_mode_asset_category_changed += 1;
            }
            AaveChunkEvent::AssetCollateralInEModeChanged {
                asset_id,
                category_id,
                is_collateral,
            } => {
                DegenbotDb::apply_asset_collateral_in_emode_changed_on_conn(
                    conn,
                    *asset_id,
                    *category_id,
                    *is_collateral,
                )?;
                report.asset_collateral_in_emode_changed += 1;
            }
            AaveChunkEvent::ReserveUsedAsCollateral {
                user_id,
                asset_id,
                enabled,
            } => {
                DegenbotDb::apply_reserve_used_as_collateral_on_conn(
                    conn, *user_id, *asset_id, *enabled,
                )?;
                report.reserve_used_as_collateral += 1;
            }
            AaveChunkEvent::UserEModeSet { user_id, e_mode } => {
                DegenbotDb::apply_user_e_mode_set_on_conn(conn, *user_id, *e_mode)?;
                report.user_e_mode_set += 1;
            }
            AaveChunkEvent::PriceOracleUpdated {
                market_id: ev_market_id,
                new_oracle_address,
            } => {
                DegenbotDb::apply_price_oracle_updated_on_conn(
                    conn,
                    *ev_market_id,
                    new_oracle_address,
                )?;
                report.price_oracle_updated += 1;
            }
            AaveChunkEvent::AssetSourceUpdated {
                asset_id,
                source_address,
            } => {
                DegenbotDb::apply_asset_source_updated_on_conn(conn, *asset_id, source_address)?;
                report.asset_source_updated += 1;
            }
            AaveChunkEvent::ReserveDataUpdated {
                asset_id,
                liquidity_rate,
                variable_borrow_rate,
                liquidity_index,
                variable_borrow_index,
                block_number,
            } => {
                DegenbotDb::apply_reserve_data_updated_on_conn(
                    conn,
                    *asset_id,
                    *liquidity_rate,
                    *variable_borrow_rate,
                    *liquidity_index,
                    *variable_borrow_index,
                    *block_number,
                )?;
                report.reserve_data_updated += 1;
            }
            AaveChunkEvent::ReserveInitialized {
                market_id: ev_market_id,
                underlying_asset_id,
                a_token_id,
                a_token_revision,
                v_token_id,
                v_token_revision,
                price_source,
            } => {
                DegenbotDb::apply_reserve_initialized_on_conn(
                    conn,
                    *ev_market_id,
                    *underlying_asset_id,
                    *a_token_id,
                    *a_token_revision,
                    *v_token_id,
                    *v_token_revision,
                    price_source.as_deref(),
                )?;
                report.reserve_initialized += 1;
            }
            AaveChunkEvent::ScaledTokenMint {
                position,
                position_id,
                balance_delta,
                new_index,
            } => {
                DegenbotDb::apply_scaled_token_mint_on_conn(
                    conn,
                    *position,
                    *position_id,
                    *balance_delta,
                    *new_index,
                )?;
                report.scaled_token_mint += 1;
            }
            AaveChunkEvent::ScaledTokenBurn {
                position,
                position_id,
                balance_delta,
                new_index,
            } => {
                DegenbotDb::apply_scaled_token_burn_on_conn(
                    conn,
                    *position,
                    *position_id,
                    *balance_delta,
                    *new_index,
                )?;
                report.scaled_token_burn += 1;
            }
            AaveChunkEvent::ScaledTokenTransfer {
                from_position_id,
                to_position_id,
                scaled_amount,
                transfer_index,
            } => {
                DegenbotDb::apply_scaled_token_transfer_on_conn(
                    conn,
                    *from_position_id,
                    *to_position_id,
                    *scaled_amount,
                    *transfer_index,
                )?;
                report.scaled_token_transfer += 1;
            }
            AaveChunkEvent::GhoDiscountPercentUpdated {
                user_id,
                new_discount_percent,
            } => {
                DegenbotDb::apply_gho_discount_percent_updated_on_conn(
                    conn,
                    *user_id,
                    *new_discount_percent,
                )?;
                report.gho_discount_percent_updated += 1;
            }
            AaveChunkEvent::GhoDiscountRateStrategyUpdated {
                gho_token_id,
                new_strategy,
            } => {
                DegenbotDb::apply_gho_discount_rate_strategy_updated_on_conn(
                    conn,
                    *gho_token_id,
                    new_strategy.as_deref(),
                )?;
                report.gho_discount_rate_strategy_updated += 1;
            }
            AaveChunkEvent::GhoDiscountTokenUpdated {
                gho_token_id,
                new_discount_token,
            } => {
                DegenbotDb::apply_gho_discount_token_updated_on_conn(
                    conn,
                    *gho_token_id,
                    new_discount_token.as_deref(),
                )?;
                report.gho_discount_token_updated += 1;
            }
            AaveChunkEvent::StkAaveStaked { user_id, amount } => {
                DegenbotDb::apply_stk_aave_staked_on_conn(conn, *user_id, *amount)?;
                report.stk_aave_staked += 1;
            }
            AaveChunkEvent::StkAaveRedeem { user_id, amount } => {
                DegenbotDb::apply_stk_aave_redeem_on_conn(conn, *user_id, *amount)?;
                report.stk_aave_redeem += 1;
            }
            AaveChunkEvent::RewardsClaimed {
                user_id,
                reward_token_id,
                claimer_id,
                claimed_amount,
            } => {
                DegenbotDb::apply_rewards_claimed_on_conn(
                    conn,
                    *user_id,
                    *reward_token_id,
                    *claimer_id,
                    *claimed_amount,
                )?;
                report.rewards_claimed += 1;
            }
            AaveChunkEvent::DebtPositionReset {
                position_id,
                new_index,
            } => {
                DegenbotDb::reset_debt_position_to_zero_on_conn(conn, *position_id, *new_index)?;
                report.debt_position_reset += 1;
            }
            // ── 6SWY4R-2b: the 6 missing-variant config events ─────────
            AaveChunkEvent::Upgraded {
                asset_id,
                market_id: _,
                is_a_token,
                new_revision,
                deprecated_gho_token_id,
            } => {
                DegenbotDb::apply_upgraded_on_conn(
                    conn,
                    *asset_id,
                    *is_a_token,
                    *new_revision,
                    *deprecated_gho_token_id,
                )?;
                report.upgraded += 1;
            }
            AaveChunkEvent::ContractRevisionUpdated {
                market_id: ev_market_id,
                contract_name,
                new_revision,
            } => {
                DegenbotDb::apply_contract_revision_updated_on_conn(
                    conn,
                    *ev_market_id,
                    contract_name,
                    *new_revision,
                )?;
                report.contract_revision_updated += 1;
            }
            AaveChunkEvent::PoolDataProviderUpdated {
                market_id: ev_market_id,
                old_address,
                new_address,
            } => {
                DegenbotDb::apply_pool_data_provider_updated_on_conn(
                    conn,
                    *ev_market_id,
                    old_address.as_deref(),
                    new_address,
                )?;
                report.pool_data_provider_updated += 1;
            }
            AaveChunkEvent::ContractInserted {
                market_id: ev_market_id,
                name,
                address,
                revision,
            } => {
                DegenbotDb::apply_contract_inserted_on_conn(
                    conn,
                    *ev_market_id,
                    name,
                    address,
                    *revision,
                )?;
                report.contract_inserted += 1;
            }
        }
    }

    // Stamp `last_update_block` as the LAST write (§3.4 restart-invariant:
    // on rollback the stamp does NOT advance, so a restart re-processes the
    // chunk clean).
    let chunk_end_i64 = i64::try_from(chunk_end_block).unwrap_or(i64::MAX);
    DegenbotDb::set_market_last_update_block_on_conn(conn, market_id, chunk_end_i64)?;
    report.stamped_block = Some(chunk_end_block);

    Ok(report)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::too_many_lines)]
mod tests {
    use super::*;

    /// A fresh in-memory **write-capable** DB seeded with a single market
    /// (id 1, `last_update_block = NULL`) — the FK parent every Aave row
    /// references. Mirrors `write.rs::write_db_with_market`.
    fn fresh_db() -> DegenbotDb {
        // `:memory:` DB — the `DegenbotDb` handle owns it; `db.lock()` for the
        // tx + `db.lock()` for assertions hit the same DB. Mirrors the
        // pool-updater precedent's `write_db()`.
        let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (1, 1, 'mainnet', 1, NULL)",
                [],
            )
            .unwrap();
        }
        db
    }

    /// Read the market's `last_update_block` back (independent read path).
    fn market_stamp(db: &DegenbotDb) -> Option<i64> {
        let conn = db.lock();
        conn.query_row(
            "SELECT last_update_block FROM aave_v3_markets WHERE id = 1",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .unwrap()
    }

    // ── §3.4 atomicity: commit path writes the rows + stamps together ──────

    #[test]
    fn apply_aave_chunk_writes_on_conn_commits_events_and_stamp_together() {
        let db = fresh_db();
        // Seed an `aave_v3_assets` parent (FK target for `asset_configs`) +
        // its erc20 parents so `apply_collateral_configuration_changed` can
        // upsert the asset_config row (FK `asset_id → aave_v3_assets.id`).
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, 1, '0xu1')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_assets \
                    (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                     v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                     borrow_index, borrow_rate) \
                 VALUES (1, 1, 1, 1, 1, 1, 1, '0', '0', '1', '0')",
                [],
            )
            .unwrap();
        }

        let events = vec![AaveChunkEvent::CollateralConfigurationChanged {
            asset_id: 1,
            config_bitmap: U256::ZERO, // all-zero bitmap → all-default decode + create the asset_config row
        }];

        // ONE transaction wraps the apply + the stamp.
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 1_000).unwrap();
            assert_eq!(report.collateral_configuration_changed, 1);
            assert_eq!(report.stamped_block, Some(1_000));
            tx.commit().unwrap();
        }

        // Both the asset_config row + the stamp landed (atomicity).
        {
            let conn = db.lock();
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM aave_v3_asset_configs WHERE asset_id = 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "the apply must persist its asset_config row");
        }
        assert_eq!(
            market_stamp(&db),
            Some(1_000),
            "the stamp must land on commit"
        );
    }

    // ── §3.4 atomicity: rollback path writes NOTHING + stamp unchanged ─────

    #[test]
    fn apply_aave_chunk_writes_on_conn_rolls_back_on_injected_failure() {
        // The Aave apply fns are idempotent get-or-create (check-existing then
        // UPDATE-or-INSERT), so they never naturally hit a UNIQUE violation in
        // single-connection flow. To inject a deterministic `Err` we enable
        // SQLite FK enforcement on the test connection (SQLite defaults it
        // OFF; `open_for_writes` doesn't toggle it) + send a
        // `PriceOracleUpdated` event pointing at `market_id=9999` — the INSERT
        // path of `apply_price_oracle_updated_on_conn` violates the
        // `aave_v3_contracts.market_id → aave_v3_markets` FK + returns Err,
        // which propagates via `?` → the caller drops the tx → rollback. FK
        // enforcement is the production portable failure mode: the constraint
        // fires wherever `aave_v3_markets` is FK-owned, regardless of the
        // apply fns' idempotence.
        let db = fresh_db();
        // Seed the asset parent (so the first chunk's collateral-config apply
        // succeeds) + seed the market's stamp at 100 (the prior chunk's
        // committed stamp).
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, 1, '0xu1')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_assets \
                    (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                     v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                     borrow_index, borrow_rate) \
                 VALUES (1, 1, 1, 1, 1, 1, 1, '0', '0', '1', '0')",
                [],
            )
            .unwrap();
            conn.execute(
                "UPDATE aave_v3_markets SET last_update_block = 100 WHERE id = 1",
                [],
            )
            .unwrap();
        }

        // Build a chunk whose FIRST event succeeds + whose SECOND event fails.
        // The failure: `PriceOracleUpdated { market_id: 9999 }` — the INSERT
        // path of `apply_price_oracle_updated_on_conn` violates the
        // `aave_v3_contracts.market_id → aave_v3_markets` FK + returns Err →
        // the caller drops the tx → rollback.
        let events = vec![
            AaveChunkEvent::CollateralConfigurationChanged {
                asset_id: 1,
                config_bitmap: U256::ZERO, // succeeds: creates asset_config(1)
            },
            AaveChunkEvent::PriceOracleUpdated {
                market_id: 9999, // FAILS: FK violation on aave_v3_markets
                new_oracle_address: "0xoracle".to_string(),
            },
        ];

        let err = {
            let mut guard = db.lock();
            // FK enforcement must be set OUTSIDE a transaction (SQLite quirk).
            guard.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
            let tx = guard.transaction().unwrap();
            let result = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 200);
            assert!(result.is_err(), "the FK violation must surface as Err");
            drop(tx); // ← rollback (the `?` already returned).
            result.err()
        };
        let _ = err;

        // (a) The stamp stayed at 100 (the 200 advance rolled back).
        assert_eq!(
            market_stamp(&db),
            Some(100),
            "rolled-back chunk's stamp advance must not be durable (restart-safe)",
        );
        // (b) The asset_config row that was written before the failure did NOT
        // land (whole-chunk revert).
        {
            let conn = db.lock();
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM aave_v3_asset_configs WHERE asset_id = 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 0, "rolled-back chunk's writes must not be durable");
        }
    }

    // ── §3.4 atomicity: empty events still stamps (chunk-end semantics) ─────

    #[test]
    fn apply_aave_chunk_writes_on_conn_empty_events_stamps_block() {
        // Degenerate but correct: a chunk with no decoded events still advances
        // the cursor (the driver's "no changes this chunk" path stamps + commits
        // so the next chunk starts at working_end_block + 1). Mirrors how the
        // Python `aave_update` driver stamps `market.last_update_block` even
        // when `update_aave_market` produced no writes.
        let db = fresh_db();
        let events: Vec<AaveChunkEvent> = vec![];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 500).unwrap();
            assert_eq!(report.stamped_block, Some(500));
            tx.commit().unwrap();
        }
        assert_eq!(market_stamp(&db), Some(500));
    }

    // ── the apply fns reach the substrate (a representative dispatch) ──────

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_user_e_mode_set() {
        let db = fresh_db();
        // Seed a user (FK parent) at e_mode=0.
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_users \
                    (market_id, address, e_mode, gho_discount, stk_aave_balance, \
                     isolation_mode_collateral_asset_id, isolation_mode_debt) \
                 VALUES (1, '0xuser1', 0, 0, NULL, NULL, '0')",
                [],
            )
            .unwrap();
        }

        let events = vec![AaveChunkEvent::UserEModeSet {
            user_id: 1,
            e_mode: 2,
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 1_000).unwrap();
            assert_eq!(report.user_e_mode_set, 1);
            tx.commit().unwrap();
        }

        // e_mode landed.
        let e_mode: i64 = {
            let conn = db.lock();
            conn.query_row("SELECT e_mode FROM aave_v3_users WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap()
        };
        assert_eq!(e_mode, 2);
        assert_eq!(market_stamp(&db), Some(1_000));
    }

    // ── UR7QNL: the two direct-write Pool events ────────────────────────────

    /// Seed the erc20 parents (underlying + aToken + vToken) + return the
    /// seeded asset's id. Mirrors the CXRGX4 `fresh_db` seeding shape.
    fn seed_asset_row(db: &DegenbotDb, asset_pk: i64) {
        let conn = db.lock();
        for id in 1..=3 {
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
                params![id, format!("0xtok{id}")],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO aave_v3_assets \
                (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                 v_token_id, v_token_revision, price_source, \
                 liquidity_index, liquidity_rate, borrow_index, borrow_rate) \
             VALUES (?1, 1, 1, 2, 1, 3, 1, '0xoracle', '0', '0', '1', '0')",
            params![asset_pk],
        )
        .unwrap();
    }

    /// Read the asset row's index/rate columns back (independent read path).
    fn asset_indices_rates(
        db: &DegenbotDb,
        asset_id: i64,
    ) -> (String, String, String, String, Option<i64>) {
        let conn = db.lock();
        conn.query_row(
            "SELECT liquidity_rate, borrow_rate, liquidity_index, borrow_index, \
             last_update_block FROM aave_v3_assets WHERE id = ?1",
            [asset_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            },
        )
        .unwrap()
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_reserve_data_updated() {
        let db = fresh_db();
        seed_asset_row(&db, 1);

        // A ReserveDataUpdated event with non-zero indices/rates (ray-scale
        // values, stored raw as decimal VARCHAR — no ray-math in the apply).
        let events = vec![AaveChunkEvent::ReserveDataUpdated {
            asset_id: 1,
            liquidity_rate: U256::from(1_000_000_000u64), // 0.001 in ray
            variable_borrow_rate: U256::from(2_000_000_000u64), // 0.002 in ray
            liquidity_index: U256::from(1_000_000_007u64), // ~1.0 (ray)
            variable_borrow_index: U256::from(1_000_000_009u64), // ~1.0 (ray)
            block_number: 1_234,
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 2_000).unwrap();
            assert_eq!(report.reserve_data_updated, 1);
            tx.commit().unwrap();
        }

        let (lr, br, li, bi, lub) = asset_indices_rates(&db, 1);
        assert_eq!(lr, "1000000000", "liquidity_rate stored raw");
        assert_eq!(
            br, "2000000000",
            "borrow_rate = variable borrow rate (stable dropped)"
        );
        assert_eq!(li, "1000000007", "liquidity_index stored raw");
        assert_eq!(bi, "1000000009", "borrow_index = variable borrow index");
        assert_eq!(lub, Some(1_234), "last_update_block stamped from the event");
        assert_eq!(market_stamp(&db), Some(2_000), "chunk-end stamp advanced");
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_reserve_initialized_create() {
        let db = fresh_db();
        // Pre-seed ONLY the erc20 token parents (the orchestrator resolves
        // these before constructing the event). The asset row itself should
        // NOT exist — ReserveInitialized creates it.
        {
            let conn = db.lock();
            for id in 1..=3 {
                conn.execute(
                    "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
                    params![id, format!("0xtok{id}")],
                )
                .unwrap();
            }
        }

        let events = vec![AaveChunkEvent::ReserveInitialized {
            market_id: 1,
            underlying_asset_id: 1,
            a_token_id: 2,
            a_token_revision: 8,
            v_token_id: 3,
            v_token_revision: 4,
            price_source: Some("0xoracle".to_string()),
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 5_000).unwrap();
            assert_eq!(report.reserve_initialized, 1);
            tx.commit().unwrap();
        }

        // The asset row was created with the seeded fields + the zero
        // index/rate defaults (mirrors the Python AaveV3Asset constructor).
        let (lr, br, li, bi, lub) = asset_indices_rates(&db, 1);
        assert_eq!(lr, "0", "create-path zero default for liquidity_rate");
        assert_eq!(br, "0", "create-path zero default for borrow_rate");
        assert_eq!(li, "0", "create-path zero default for liquidity_index");
        assert_eq!(bi, "0", "create-path zero default for borrow_index");
        assert_eq!(
            lub, None,
            "create-path leaves last_update_block NULL (RDU owns it)"
        );
        assert_eq!(market_stamp(&db), Some(5_000));
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_reserve_initialized_update_existing() {
        // A reserve can be re-initialized across a Pool revision upgrade: the
        // get-or-create path must UPDATE the a/v token ids + revisions +
        // price_source on the existing row WITHOUT touching the indices/rates
        // (those are owned by ReserveDataUpdated).
        let db = fresh_db();
        seed_asset_row(&db, 1); // existing asset with non-zero indices

        let events = vec![AaveChunkEvent::ReserveInitialized {
            market_id: 1,
            underlying_asset_id: 1, // matches the seeded row's natural key
            a_token_id: 22,         // new aToken id (revision upgrade)
            a_token_revision: 9,
            v_token_id: 33,
            v_token_revision: 5,
            price_source: Some("0xneworacle".to_string()),
        }];
        // Seed the new aToken + vToken erc20 rows the UPDATE references
        // (FK `a_token_id`/`v_token_id` → `erc20_tokens.id`).
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (22, 1, '0xa22')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (33, 1, '0xv33')",
                [],
            )
            .unwrap();
        }
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 6_000).unwrap();
            assert_eq!(report.reserve_initialized, 1);
            tx.commit().unwrap();
        }

        // The a/v token ids + revisions + price_source were updated; the
        // indices/rates were left alone (the seeded asset had '0'/'0'/'1'/'0').
        let conn = db.lock();
        let (a_token_id, a_token_rev, v_token_id, v_token_rev, price_source): (
            i64,
            i64,
            i64,
            i64,
            String,
        ) = conn
            .query_row(
                "SELECT a_token_id, a_token_revision, v_token_id, v_token_revision, \
                 COALESCE(price_source, '') FROM aave_v3_assets WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(a_token_id, 22);
        assert_eq!(a_token_rev, 9);
        assert_eq!(v_token_id, 33);
        assert_eq!(v_token_rev, 5);
        assert_eq!(price_source, "0xneworacle");
        // Indices untouched by the ReserveInitialized UPDATE path (owned by RDU).
        let li: String = conn
            .query_row(
                "SELECT liquidity_index FROM aave_v3_assets WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(li, "0", "index left alone on re-init (RDU owns it)");
    }

    // ── §3.4 atomicity: a Pool-event variant rolls back with the stamp ───────

    #[test]
    fn apply_aave_chunk_writes_on_conn_rolls_back_reserve_data_updated_on_missing_asset() {
        // ReserveDataUpdated targets an asset_id that has NO row → the UPDATE
        // affects zero rows → Err(MissingRow) → the caller drops the tx →
        // rollback. Asserts the stamp does NOT advance + no stray write landed.
        // This is the Pool-event counterpart to the CXRGX4 FK-violation
        // rollback test (the apply fns are idempotent get-or-create, so the
        // deterministic injected failure is the MissingRow path here).
        let db = fresh_db();
        // Pre-seed the market stamp at 100 (the prior chunk's committed stamp).
        // Do NOT seed any asset row — asset_id=999 is missing.
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE aave_v3_markets SET last_update_block = 100 WHERE id = 1",
                [],
            )
            .unwrap();
        }

        let events = vec![
            // First event: a no-op-state-change but row-creating event that
            // would land if the chunk committed (proves the whole chunk rolled
            // back, not just the failing event).
            AaveChunkEvent::ReserveInitialized {
                market_id: 1,
                underlying_asset_id: 1,
                a_token_id: 2,
                a_token_revision: 1,
                v_token_id: 3,
                v_token_revision: 1,
                price_source: None,
            },
            // Second event: FAILS — ReserveDataUpdated on the missing asset_id 999.
            AaveChunkEvent::ReserveDataUpdated {
                asset_id: 999,
                liquidity_rate: U256::ZERO,
                variable_borrow_rate: U256::ZERO,
                liquidity_index: U256::ZERO,
                variable_borrow_index: U256::ZERO,
                block_number: 200,
            },
        ];
        // The erc20 parents for the ReserveInitialized event must exist for
        // the first event to succeed (proving the failure is the second event's).
        {
            let conn = db.lock();
            for id in 1..=3 {
                conn.execute(
                    "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
                    params![id, format!("0xtok{id}")],
                )
                .unwrap();
            }
        }

        let err = {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let result = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 200);
            assert!(result.is_err(), "the MissingRow must surface as Err");
            drop(tx); // ← rollback.
            result.err()
        };
        let _ = err;

        // (a) The stamp stayed at 100 (the 200 advance rolled back).
        assert_eq!(
            market_stamp(&db),
            Some(100),
            "rolled-back chunk's stamp advance must not be durable (restart-safe)",
        );
        // (b) The ReserveInitialized-written asset row did NOT land (whole-chunk revert).
        {
            let conn = db.lock();
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM aave_v3_assets WHERE underlying_asset_id = 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 0, "rolled-back chunk's writes must not be durable");
        }
    }

    // ── 5Z3QQ2: ScaledToken (aToken/vToken) apply fns ────────────────────

    use alloy::primitives::{I256, U256};
    use degenbot_db::ScaledTokenPosition;
    use degenbot_evm_math::RAY;

    /// Seed a collateral position row (balance='0', `last_index=NULL`) + the
    /// FK parents it needs (market, `erc20_tokens`, asset, user). Returns the
    /// position id (always 1 in the test fixture seeding).
    fn seed_collateral_position_with_balance(
        db: &DegenbotDb,
        position_id: i64,
        balance_str: &str,
        last_index: Option<&str>,
    ) {
        let conn = db.lock();
        // erc20 parents (underlying + aToken + vToken).
        for id in 1..=3 {
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
                params![id, format!("0xtok{id}")],
            )
            .unwrap();
        }
        // Asset (at pk=1) with liquidity_index=borrow_index=RAY.
        conn.execute(
            "INSERT INTO aave_v3_assets \
                (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                 v_token_id, v_token_revision, price_source, \
                 liquidity_index, liquidity_rate, borrow_index, borrow_rate) \
             VALUES (1, 1, 1, 2, 1, 3, 1, NULL, ?1, '0', ?1, '0')",
            params![RAY.to_string()],
        )
        .unwrap();
        // User (at pk=1).
        conn.execute(
            "INSERT INTO aave_v3_users \
                (id, market_id, address, e_mode, gho_discount, stk_aave_balance, \
                 isolation_mode_collateral_asset_id, isolation_mode_debt) \
             VALUES (1, 1, '0xuser1', 0, 0, NULL, NULL, '0')",
            [],
        )
        .unwrap();
        // Collateral position.
        conn.execute(
            "INSERT INTO aave_v3_collateral_positions (id, user_id, asset_id, balance, last_index) \
             VALUES (?1, 1, 1, ?2, ?3)",
            params![position_id, balance_str, last_index],
        )
        .unwrap();
    }

    /// Read a position's (balance, `last_index`) back as strings.
    fn position_state(db: &DegenbotDb, position_id: i64) -> (String, Option<String>) {
        let conn = db.lock();
        conn.query_row(
            "SELECT balance, last_index FROM aave_v3_collateral_positions WHERE id = ?1",
            [position_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .unwrap()
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_scaled_token_mint() {
        let db = fresh_db();
        seed_collateral_position_with_balance(&db, 1, "0", None);

        // A pre-computed mint delta of +3 RAY (the processor returned this).
        let events = vec![AaveChunkEvent::ScaledTokenMint {
            position: ScaledTokenPosition::Collateral,
            position_id: 1,
            balance_delta: I256::try_from(U256::from(3u8) * RAY).unwrap(),
            new_index: RAY,
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 9_000).unwrap();
            assert_eq!(report.scaled_token_mint, 1);
            tx.commit().unwrap();
        }

        let (balance, last_index) = position_state(&db, 1);
        assert_eq!(
            balance,
            (U256::from(3u8) * RAY).to_string(),
            "balance += delta"
        );
        assert_eq!(
            last_index,
            Some(RAY.to_string()),
            "last_index set to event index (was NULL → first event)"
        );
        assert_eq!(market_stamp(&db), Some(9_000));
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_scaled_token_burn() {
        let db = fresh_db();
        // Pre-credit with 5 RAY + last_index=RAY, so a burn of -3 RAY leaves 2 RAY.
        seed_collateral_position_with_balance(
            &db,
            1,
            &(U256::from(5u8) * RAY).to_string(),
            Some(&RAY.to_string()),
        );

        let events = vec![AaveChunkEvent::ScaledTokenBurn {
            position: ScaledTokenPosition::Collateral,
            position_id: 1,
            balance_delta: -I256::try_from(U256::from(3u8) * RAY).unwrap(),
            new_index: RAY,
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 10_000).unwrap();
            assert_eq!(report.scaled_token_burn, 1);
            tx.commit().unwrap();
        }

        let (balance, _last_index) = position_state(&db, 1);
        assert_eq!(
            balance,
            (U256::from(2u8) * RAY).to_string(),
            "balance after burn = 5 - 3 = 2 RAY"
        );
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_scaled_token_transfer() {
        let db = fresh_db();
        // Seed two collateral positions: from (id=1, balance=5 RAY) + to (id=2, balance=0).
        seed_collateral_position_with_balance(
            &db,
            1,
            &(U256::from(5u8) * RAY).to_string(),
            Some(&RAY.to_string()),
        );
        // The recipient also needs a position row; reuse the same seeding helper
        // but on a distinct position id (its user can be the same for the test).
        {
            let conn = db.lock();
            // The recipient needs a distinct user (the (user_id, asset_id)
            // unique key would otherwise fire).
            conn.execute(
                "INSERT INTO aave_v3_users \
                    (id, market_id, address, e_mode, gho_discount, stk_aave_balance, \
                     isolation_mode_collateral_asset_id, isolation_mode_debt) \
                 VALUES (2, 1, '0xuser2', 0, 0, NULL, NULL, '0')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_collateral_positions (id, user_id, asset_id, balance, last_index) \
                 VALUES (2, 2, 1, '0', NULL)",
                [],
            )
            .unwrap();
        }

        let events = vec![AaveChunkEvent::ScaledTokenTransfer {
            from_position_id: 1,
            to_position_id: 2,
            scaled_amount: U256::from(2u8) * RAY,
            transfer_index: RAY,
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 11_000).unwrap();
            assert_eq!(report.scaled_token_transfer, 1);
            tx.commit().unwrap();
        }

        let (from_balance, _) = position_state(&db, 1);
        let (to_balance, to_last_index) = position_state(&db, 2);
        assert_eq!(
            from_balance,
            (U256::from(3u8) * RAY).to_string(),
            "sender balance after transfer = 5 - 2 = 3 RAY"
        );
        assert_eq!(
            to_balance,
            (U256::from(2u8) * RAY).to_string(),
            "recipient balance after transfer = 0 + 2 = 2 RAY"
        );
        assert_eq!(
            to_last_index,
            Some(RAY.to_string()),
            "recipient last_index set to the transfer's index"
        );
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_last_index_max_with_prev() {
        // An incoming event with a LOWER index than the position's current
        // last_index must NOT clobber it (the max-with-prev guard).
        let db = fresh_db();
        // Pre-set last_index=3*RAY; the event's new_index (RAY) is lower.
        seed_collateral_position_with_balance(
            &db,
            1,
            "0",
            Some(&(U256::from(3u8) * RAY).to_string()),
        );

        let events = vec![AaveChunkEvent::ScaledTokenMint {
            position: ScaledTokenPosition::Collateral,
            position_id: 1,
            balance_delta: I256::try_from(RAY).unwrap(),
            new_index: RAY, // LOWER than the position's existing 3*RAY.
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            apply_aave_chunk_writes_on_conn(&tx, 1, &events, 12_000).unwrap();
            tx.commit().unwrap();
        }

        let (_, last_index) = position_state(&db, 1);
        assert_eq!(
            last_index,
            Some((U256::from(3u8) * RAY).to_string()),
            "lower-index event must NOT clobber the higher existing last_index"
        );
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_rolls_back_scaled_token_on_missing_position() {
        // A ScaledTokenMint targeting a non-existent position_id → MissingRow
        // → whole-chunk revert. We pair it with a valid asset row-creating
        // ReserveInitialized event to verify the rollback is all-or-nothing.
        let db = fresh_db();
        seed_collateral_position_with_balance(&db, 1, "0", None);

        // (a) valid ReserveInitialized (creates an asset row at underlying=999)
        //     + (b) invalid ScaledTokenMint on position_id=999.
        let events = vec![
            AaveChunkEvent::ReserveInitialized {
                market_id: 1,
                underlying_asset_id: 999,
                a_token_id: 2,
                a_token_revision: 1,
                v_token_id: 3,
                v_token_revision: 1,
                price_source: None,
            },
            AaveChunkEvent::ScaledTokenMint {
                position: ScaledTokenPosition::Collateral,
                position_id: 999, // no such row → MissingRow
                balance_delta: I256::try_from(RAY).unwrap(),
                new_index: RAY,
            },
        ];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let result = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 13_000);
            assert!(result.is_err(), "missing-position chunk must fail");
            // Don't commit — the dropped tx rolls back.
        }

        // (a) The stamp stayed at the seed value (None).
        assert_eq!(
            market_stamp(&db),
            None,
            "rolled-back chunk's stamp must not land"
        );
        // (b) The ReserveInitialized-written asset row did NOT land.
        let n: i64 = {
            let conn = db.lock();
            conn.query_row(
                "SELECT COUNT(*) FROM aave_v3_assets WHERE underlying_asset_id = 999",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            n, 0,
            "rolled-back chunk's asset-row creation must not be durable"
        );
        // (c) The collateral position's balance is still '0' (no mint landed).
        let (balance, _) = position_state(&db, 1);
        assert_eq!(balance, "0", "rolled-back chunk's mint must not be durable");
    }

    // ── RYKCC4 (SPECIALAPPLY): GHO + stkAAVE + Rewards apply fns ──────────

    /// Seed a bare `aave_v3_users` row at `user_id` (in market 1) with the
    /// CXRGX4 seeding defaults (`e_mode=0`, `gho_discount=0`, `stk_aave_balance=NULL`).
    /// Lighter than `seed_collateral_position_with_balance` (no erc20/asset
    /// parents) — the GHO/stkAAVE tests don't need them.
    fn seed_aave_v3_user(db: &DegenbotDb, user_id: i64) {
        let conn = db.lock();
        // `fresh_db` already inserted the market FK parent (id=1).
        conn.execute(
            "INSERT INTO aave_v3_users \
                (id, market_id, address, e_mode, gho_discount, stk_aave_balance, \
                 isolation_mode_collateral_asset_id, isolation_mode_debt) \
             VALUES (?1, 1, ?2, 0, 0, NULL, NULL, '0')",
            params![user_id, format!("0xuser{user_id}")],
        )
        .unwrap();
    }

    /// Seed a GHO token row (chain-unique) at `gho_token_id` with the given
    /// discount strategy + token attributes. The `token_id` + `v_token_id`
    /// FK parents are seeded as erc20 rows.
    fn seed_gho_token_row(
        db: &DegenbotDb,
        gho_token_id: i64,
        strategy: Option<&str>,
        discount_token: Option<&str>,
    ) {
        let conn = db.lock();
        for id in [10, 11] {
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
                params![id, format!("0xgho_par{id}")],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO aave_gho_tokens (id, token_id, v_token_id, \
                v_gho_discount_rate_strategy, v_gho_discount_token) \
             VALUES (?1, 10, 11, ?2, ?3)",
            params![gho_token_id, strategy, discount_token],
        )
        .unwrap();
    }

    /// Read a GHO token row's (strategy, `discount_token`) back.
    fn gho_token_state(db: &DegenbotDb, gho_token_id: i64) -> (Option<String>, Option<String>) {
        let conn = db.lock();
        conn.query_row(
            "SELECT v_gho_discount_rate_strategy, v_gho_discount_token \
             FROM aave_gho_tokens WHERE id = ?1",
            [gho_token_id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .unwrap()
    }

    /// Read a user's (`gho_discount`, `stk_aave_balance`) back.
    fn user_gho_stk_state(db: &DegenbotDb, user_id: i64) -> (i64, Option<String>) {
        let conn = db.lock();
        conn.query_row(
            "SELECT gho_discount, stk_aave_balance FROM aave_v3_users WHERE id = ?1",
            [user_id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .unwrap()
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_gho_discount_percent_updated() {
        let db = fresh_db();
        seed_aave_v3_user(&db, 1);

        let events = vec![AaveChunkEvent::GhoDiscountPercentUpdated {
            user_id: 1,
            new_discount_percent: 42,
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 1_000).unwrap();
            assert_eq!(report.gho_discount_percent_updated, 1);
            tx.commit().unwrap();
        }

        let (discount, _) = user_gho_stk_state(&db, 1);
        assert_eq!(discount, 42, "gho_discount set to the new percent");
        assert_eq!(market_stamp(&db), Some(1_000));
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_gho_discount_rate_strategy_updated() {
        let db = fresh_db();
        seed_gho_token_row(&db, 1, Some("0xold_strategy"), None);

        let events = vec![AaveChunkEvent::GhoDiscountRateStrategyUpdated {
            gho_token_id: 1,
            new_strategy: Some("0xnew_strategy".to_string()),
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 1_000).unwrap();
            assert_eq!(report.gho_discount_rate_strategy_updated, 1);
            tx.commit().unwrap();
        }

        let (strategy, _) = gho_token_state(&db, 1);
        assert_eq!(strategy.as_deref(), Some("0xnew_strategy"));
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_gho_discount_token_updated() {
        let db = fresh_db();
        seed_gho_token_row(&db, 1, None, Some("0xold_token"));

        // Clear the discount token (None).
        let events = vec![AaveChunkEvent::GhoDiscountTokenUpdated {
            gho_token_id: 1,
            new_discount_token: None,
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 1_000).unwrap();
            assert_eq!(report.gho_discount_token_updated, 1);
            tx.commit().unwrap();
        }

        let (_, discount_token) = gho_token_state(&db, 1);
        assert_eq!(discount_token, None, "discount token cleared to NULL");
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_stk_aave_staked() {
        let db = fresh_db();
        seed_aave_v3_user(&db, 1);
        // Pre-set stk_aave_balance = NULL (the CXRGX4 default).

        let amount = alloy::primitives::U256::from(1_000u64);
        let events = vec![AaveChunkEvent::StkAaveStaked { user_id: 1, amount }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 1_000).unwrap();
            assert_eq!(report.stk_aave_staked, 1);
            tx.commit().unwrap();
        }

        let (_, stk_balance) = user_gho_stk_state(&db, 1);
        assert_eq!(
            stk_balance.as_deref(),
            Some("1000"),
            "NULL balance treated as 0, then += amount (1000)"
        );
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_stk_aave_redeem() {
        let db = fresh_db();
        seed_aave_v3_user(&db, 1);
        // Pre-set stk_aave_balance = 5000.
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE aave_v3_users SET stk_aave_balance = '5000' WHERE id = 1",
                [],
            )
            .unwrap();
        }

        let events = vec![AaveChunkEvent::StkAaveRedeem {
            user_id: 1,
            amount: alloy::primitives::U256::from(3_000u64),
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 1_000).unwrap();
            assert_eq!(report.stk_aave_redeem, 1);
            tx.commit().unwrap();
        }

        let (_, stk_balance) = user_gho_stk_state(&db, 1);
        assert_eq!(stk_balance.as_deref(), Some("2000"), "5000 - 3000 = 2000");
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_stk_aave_redeem_underflow_errors() {
        let db = fresh_db();
        seed_aave_v3_user(&db, 1);
        // stk_aave_balance = 100; Redeem 200 → underflow → error.
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE aave_v3_users SET stk_aave_balance = '100' WHERE id = 1",
                [],
            )
            .unwrap();
        }

        let events = vec![AaveChunkEvent::StkAaveRedeem {
            user_id: 1,
            amount: alloy::primitives::U256::from(200u64),
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let result = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 1_000);
            assert!(
                result.is_err(),
                "redeem underflow must error (Python asserts >=0)"
            );
        }

        let (_, stk_balance) = user_gho_stk_state(&db, 1);
        assert_eq!(
            stk_balance.as_deref(),
            Some("100"),
            "balance untouched on error"
        );
        assert_eq!(market_stamp(&db), None, "stamp untouched on rollback");
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_rewards_claimed_noop() {
        let db = fresh_db();
        seed_aave_v3_user(&db, 1);

        // The no-op RewardsClaimed variant — the apply dispatch records the
        // count but writes nothing.
        let events = vec![AaveChunkEvent::RewardsClaimed {
            user_id: 1,
            reward_token_id: 99,
            claimer_id: 1,
            claimed_amount: alloy::primitives::U256::from(1_000u64),
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 7_777).unwrap();
            assert_eq!(report.rewards_claimed, 1, "counter incremented");
            tx.commit().unwrap();
        }

        // Stamp advances (the no-op still counts as a processed event in the chunk).
        assert_eq!(market_stamp(&db), Some(7_777));
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_rolls_back_gho_on_missing_user() {
        // A GhoDiscountPercentUpdated targeting a non-existent user_id →
        // MissingRow → whole-chunk revert. Paired with a valid StkAaveStaked
        // to verify the rollback is all-or-nothing.
        let db = fresh_db();
        seed_aave_v3_user(&db, 1);
        // Pre-set stk_aave_balance = NULL.

        let events = vec![
            AaveChunkEvent::StkAaveStaked {
                user_id: 1,
                amount: alloy::primitives::U256::from(500u64),
            },
            AaveChunkEvent::GhoDiscountPercentUpdated {
                user_id: 999, // no such row → MissingRow
                new_discount_percent: 50,
            },
        ];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let result = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 9_000);
            assert!(result.is_err(), "missing-user chunk must fail");
        }

        // (a) The stamp stayed at the seed value (None).
        assert_eq!(
            market_stamp(&db),
            None,
            "rolled-back chunk's stamp must not land"
        );
        // (b) The StkAaveStaked write did NOT land.
        let (_, stk_balance) = user_gho_stk_state(&db, 1);
        assert_eq!(
            stk_balance, None,
            "rolled-back chunk's stk_aave write must not be durable"
        );
    }

    // ── 6SWY4R-2b: the 6 missing-variant event apply tests ────────────────

    #[test]
    fn apply_upgraded_on_conn_updates_a_token_revision() {
        let db = fresh_db();
        seed_asset_row(&db, 1);
        DegenbotDb::apply_upgraded_on_conn(&db.lock(), 1, true, 3, None).unwrap();
        let rev: i64 = db
            .lock()
            .query_row(
                "SELECT a_token_revision FROM aave_v3_assets WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rev, 3);
        // v_token_revision is unchanged.
        let vrev: i64 = db
            .lock()
            .query_row(
                "SELECT v_token_revision FROM aave_v3_assets WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(vrev, 1, "v_token_revision untouched by an aToken upgrade");
    }

    #[test]
    fn apply_upgraded_on_conn_updates_v_token_revision() {
        let db = fresh_db();
        seed_asset_row(&db, 1);
        DegenbotDb::apply_upgraded_on_conn(&db.lock(), 1, false, 2, None).unwrap();
        let rev: i64 = db
            .lock()
            .query_row(
                "SELECT v_token_revision FROM aave_v3_assets WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rev, 2);
    }

    #[test]
    fn apply_upgraded_on_conn_missing_asset_errors() {
        let db = fresh_db();
        let err = DegenbotDb::apply_upgraded_on_conn(&db.lock(), 9999, true, 3, None).unwrap_err();
        assert!(matches!(err, degenbot_db::DbError::MissingRow(_)));
    }

    #[test]
    fn apply_upgraded_on_conn_gho_deprecation_clears_config_and_resets_users() {
        // The riskiest piece: the chunk-wide bulk UPDATE.
        let db = fresh_db();
        seed_asset_row(&db, 1);
        seed_gho_token_row(&db, 50, Some("0xold_strat"), Some("0xold_tok"));
        // Two users with non-zero GHO discount (one zero-discount user to
        // verify the WHERE clause skips them).
        seed_aave_v3_user(&db, 100);
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE aave_v3_users SET gho_discount = 15 WHERE id = 100",
                [],
            )
            .unwrap();
        }
        seed_aave_v3_user(&db, 101);
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE aave_v3_users SET gho_discount = 30 WHERE id = 101",
                [],
            )
            .unwrap();
        }
        // A zero-discount user (the bulk UPDATE must not touch it).
        seed_aave_v3_user(&db, 102);

        // Fire the deprecation (asset 1 is the GHO vToken's asset).
        DegenbotDb::apply_upgraded_on_conn(&db.lock(), 1, false, 4, Some(50)).unwrap();

        // The GHO token's discount config is cleared.
        let (strat, tok) = gho_token_state(&db, 50);
        assert_eq!(strat, None, "v_gho_discount_rate_strategy cleared");
        assert_eq!(tok, None, "v_gho_discount_token cleared");
        // The non-zero users are reset to 0.
        assert_eq!(user_gho_stk_state(&db, 100).0, 0, "user 100 reset");
        assert_eq!(user_gho_stk_state(&db, 101).0, 0, "user 101 reset");
        // The already-zero user is untouched (gho_discount still 0).
        assert_eq!(user_gho_stk_state(&db, 102).0, 0, "user 102 untouched");
    }

    #[test]
    fn apply_upgraded_on_conn_no_deprecation_leaves_users_alone() {
        let db = fresh_db();
        seed_asset_row(&db, 1);
        seed_aave_v3_user(&db, 100);
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE aave_v3_users SET gho_discount = 15 WHERE id = 100",
                [],
            )
            .unwrap();
        }
        // deprecated_gho_token_id = None → no bulk reset.
        DegenbotDb::apply_upgraded_on_conn(&db.lock(), 1, false, 2, None).unwrap();
        assert_eq!(
            user_gho_stk_state(&db, 100).0,
            15,
            "user discount untouched"
        );
    }

    #[test]
    fn apply_upgraded_on_conn_deprecation_missing_gho_token_errors() {
        let db = fresh_db();
        seed_asset_row(&db, 1);
        let err =
            DegenbotDb::apply_upgraded_on_conn(&db.lock(), 1, false, 4, Some(9999)).unwrap_err();
        assert!(matches!(err, degenbot_db::DbError::MissingRow(_)));
    }

    #[test]
    fn apply_contract_revision_updated_on_conn_sets_pool_revision() {
        let db = fresh_db();
        // Seed a POOL contract row.
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_contracts (id, market_id, name, address, revision) \
                 VALUES (1, 1, 'POOL', '0xpool', 1)",
                [],
            )
            .unwrap();
        }
        DegenbotDb::apply_contract_revision_updated_on_conn(&db.lock(), 1, "POOL", 2).unwrap();
        let (rev, addr): (i64, String) = db
            .lock()
            .query_row(
                "SELECT revision, address FROM aave_v3_contracts WHERE market_id = 1 AND name = 'POOL'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(rev, 2);
        // §4.2 parity: the address is NOT updated.
        assert_eq!(
            addr, "0xpool",
            "address untouched (Python updates revision only)"
        );
    }

    #[test]
    fn apply_contract_revision_updated_on_conn_missing_contract_errors() {
        let db = fresh_db();
        let err = DegenbotDb::apply_contract_revision_updated_on_conn(&db.lock(), 1, "POOL", 2)
            .unwrap_err();
        assert!(matches!(err, degenbot_db::DbError::MissingRow(_)));
    }

    #[test]
    fn apply_pool_data_provider_updated_on_conn_inserts_when_old_is_zero() {
        let db = fresh_db();
        DegenbotDb::apply_pool_data_provider_updated_on_conn(&db.lock(), 1, None, "0xnew_pdp")
            .unwrap();
        let (name, addr, rev): (String, String, Option<i64>) = db
            .lock()
            .query_row(
                "SELECT name, address, revision FROM aave_v3_contracts \
                 WHERE market_id = 1 AND address = '0xnew_pdp'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(name, "POOL_DATA_PROVIDER");
        assert_eq!(addr, "0xnew_pdp");
        assert_eq!(rev, None, "no revision on the INSERT path");
    }

    #[test]
    fn apply_pool_data_provider_updated_on_conn_updates_by_old_address() {
        let db = fresh_db();
        // Seed the existing POOL_DATA_PROVIDER row.
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_contracts (id, market_id, name, address, revision) \
                 VALUES (1, 1, 'POOL_DATA_PROVIDER', '0xold_pdp', NULL)",
                [],
            )
            .unwrap();
        }
        DegenbotDb::apply_pool_data_provider_updated_on_conn(
            &db.lock(),
            1,
            Some("0xold_pdp"),
            "0xnew_pdp",
        )
        .unwrap();
        let addr: String = db
            .lock()
            .query_row(
                "SELECT address FROM aave_v3_contracts WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(addr, "0xnew_pdp");
    }

    #[test]
    fn apply_pool_data_provider_updated_on_conn_update_path_missing_errors() {
        let db = fresh_db();
        let err = DegenbotDb::apply_pool_data_provider_updated_on_conn(
            &db.lock(),
            1,
            Some("0xnonexistent"),
            "0xnew_pdp",
        )
        .unwrap_err();
        assert!(matches!(err, degenbot_db::DbError::MissingRow(_)));
    }

    #[test]
    fn apply_contract_inserted_on_conn_inserts_address_set_row() {
        let db = fresh_db();
        // An AddressSet event: name from the ASCII id, no revision.
        DegenbotDb::apply_contract_inserted_on_conn(
            &db.lock(),
            1,
            "SOME_CONTRACT",
            "0xnew_addr",
            None,
        )
        .unwrap();
        let (name, rev): (String, Option<i64>) = db
            .lock()
            .query_row(
                "SELECT name, revision FROM aave_v3_contracts \
                 WHERE market_id = 1 AND address = '0xnew_addr'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "SOME_CONTRACT");
        assert_eq!(rev, None);
    }

    #[test]
    fn apply_contract_inserted_on_conn_inserts_proxy_created_row_with_revision() {
        let db = fresh_db();
        // A ProxyCreated event: name = POOL/POOL_CONFIGURATOR, with revision.
        DegenbotDb::apply_contract_inserted_on_conn(&db.lock(), 1, "POOL", "0xproxy", Some(2))
            .unwrap();
        let (name, rev): (String, Option<i64>) = db
            .lock()
            .query_row(
                "SELECT name, revision FROM aave_v3_contracts \
                 WHERE market_id = 1 AND address = '0xproxy'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "POOL");
        assert_eq!(rev, Some(2));
    }
}

//! The transactional Aave V3 chunk-write apply core + its atomicity tests.
//!
//! See the crate-level docs for the §3.4 atomicity invariant this file enforces.

use alloy::primitives::{Address, U256};
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
        /// When the new asset's underlying IS the GHO token, the Python's
        /// `_process_reserve_initialized_event` links the GHO token row to the
        /// new vToken: `aave_gho_tokens.v_token_id = v_token.id` (the FK the
        /// ULDUAC emitter guard resolves via `gho_asset.v_token_address`).
        /// `Some(gho_token_row_id)` when the link should fire; `None` for a
        /// regular reserve (2QGL6G / divergence #8).
        gho_link_token_id: Option<i64>,
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
    /// GHO discount-refresh signal (C3.3 — the (C) refresh wiring). Emitted by
    /// `build_gho_chunk_event` ALONGSIDE the `ScaledTokenMint`/`ScaledTokenBurn`
    /// when the GHO processor's `should_refresh_discount` is set (V1-V3 — the
    /// discount mechanism is active). NOT applied synchronously — the chunk
    /// loop's async POST-APPLY pass consumes it: `get_or_init_stk_aave_balance`
    /// (`balanceOf` `eth_call` at `block - 1` if `aave_v3_users.stk_aave_balance` is
    /// None) + `_refresh_discount_rate` (`debt_balance = ray_mul(scaled,
    /// index)` + [`calculate_gho_discount_rate`] → write
    /// `aave_v3_users.gho_discount`). `apply_chunk_events_on_conn` is a no-op for
    /// this variant (it carries no synchronously-applicable state). Mirrors
    /// Python's `token_processor._refresh_discount_rate` after a GHO
    /// borrow/accrual.
    GhoRefreshDiscount {
        /// The GHO debt position (`aave_v3_debt_positions.id`) whose
        /// `balance`/`last_index` were just updated — the refresh reads the
        /// POST-APPLY values from `conn`.
        position_id: i64,
    },
    /// `BalanceTransfer(from, to, value)` — aToken transfer between users
    /// (5Z3QQ2). Carries the resolved `from_position_id` + `to_position_id`
    /// (both collateral — `BalanceTransfer` is aToken-only) + the scaled
    /// amount + the transfer's index. The apply fn debits `from`, credits
    /// `to`, + reconciles both positions' `last_index`.
    ScaledTokenTransfer {
        from_position_id: i64,
        to_position_id: Option<i64>,
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
    /// stkAAVE ERC20 `Transfer(from, to, value)`. The canonical + only
    /// stkAAVE balance-mutation channel — Python's
    /// `process_stk_aave_transfer_event` processes EVERY `Transfer` event on
    /// `v_gho_discount_token`, including both zero-leg arms (the
    /// `Transfer(0→X)` mint + `Transfer(X→0)` burn) — the `ZERO_ADDRESS` leg is
    /// treated as a half-event (skip the zero side, always mutate the real
    /// user). This variant mirrors that exactly: each side is `None` iff the
    /// corresponding address is `ZERO_ADDRESS`, and the apply fn skips `None`
    /// + mutates the other.
    ///
    /// YMWN5V retirement (crash #3): the prior design shipped separate
    /// `StkAaveStaked`/`StkAaveRedeem` variants as proxies for the zero-leg
    /// Transfers (and dedupe-skipped the zero legs here). That required a now
    /// empirically-falsified invariant — every zero-leg Transfer must pair
    /// with a `Staked`/`Redeem` semantic event. Some actions emit ONLY the
    /// `Transfer(X→0)` event with no paired `Redeem` (verified via cast logs
    /// across the 16.59M→18M range), leaving the sender's `stk_aave_balance`
    /// stuck at its pre-burn cache value → wrong `calculate_gho_discount_rate`
    /// → `(C)` refresh over-applies discount → GHO-burn delta overshoots
    /// `prev_scaled_balance` → balance would go negative (the crash #3 byte-
    /// exact match: overshoot == `discount_scaled`). Retired per AGENTS.md
    /// (no backwards-compat shim for retired implementations).
    StkAaveTransfer {
        /// `aave_v3_users.id` of the sender (decremented by `amount`). `None`
        /// iff `from == ZERO_ADDRESS` (the mint-from-zero leg) — apply skips
        /// the from-leg entirely.
        from_user_id: Option<i64>,
        /// `aave_v3_users.id` of the recipient (incremented by `amount`).
        /// `None` iff `to == ZERO_ADDRESS` (the burn-to-zero leg) — apply
        /// skips the to-leg entirely.
        to_user_id: Option<i64>,
        /// The transferred amount.
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
    /// C3.3 (C refresh) — the GHO discount-refresh signals (the async
    /// post-apply pass consumes them (`balanceOf` + recompute `gho_discount`).
    pub gho_refresh_discount: usize,
    /// stkAAVE `Transfer(from, to, value)` — the canonical balance-mutation
    /// channel (YMWN5V retirement, crash #3): covers the zero-leg arms + the
    /// neither-zero case. `Staked`/`Redeem` semantic dispatchers were retired
    /// (their effect is now covered by the zero-leg Transfers).
    pub stk_aave_transfer: usize,
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

/// Dispatch each pre-decoded Aave V3 event to its `DegenbotDb::apply_*_on_conn`
/// writer under the caller's `Transaction` (borrowed as a `&Connection`),
/// accumulating a per-type count. Does NOT stamp `last_update_block` — the
/// caller owns the stamp (per-tx apply in [`process_chunk_on_conn`], or the
/// batched [`apply_aave_chunk_writes_on_conn`]).
///
/// GJQGKN: extracted from `apply_aave_chunk_writes_on_conn` so the per-tx
/// apply loop can write each tx's events to `conn` BEFORE the next tx's
/// dispatch/parse reads — fixing the two staleness surfaces (the prior tx's
/// `Upgraded` revision + scaled-token balances are visible via
/// read-your-own-writes within the `SQLite` txn, matching Python's per-tx ORM
/// apply). The `ScaledTokenProcessor` is stateless (its balances come from
/// `process_transaction`'s `conn` lookups), so per-tx apply is the only seam.
///
/// Pure, synchronous, transactional. NONE of: RPC, ABI decode, `pyo3`,
/// `database_path`, `open_for_writes`.
///
/// # Errors
///
/// Returns [`degenbot_db::DbError`] on any apply failure — the caller drops
/// the `Transaction` (rollback) on `Err`.
#[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
pub fn apply_chunk_events_on_conn(
    conn: &Connection,
    _market_id: i64,
    events: &[AaveChunkEvent],
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
                gho_link_token_id,
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
                    *gho_link_token_id,
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
            // C3.3 (C refresh): no synchronous apply — the chunk loop's async
            // post-apply pass consumes this (balanceOf + recompute
            // gho_discount). Counted in `report.gho_refresh_discount` so the
            // apply loop's exhaustiveness stays explicit.
            AaveChunkEvent::GhoRefreshDiscount { .. } => {
                report.gho_refresh_discount += 1;
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
            AaveChunkEvent::StkAaveTransfer {
                from_user_id,
                to_user_id,
                amount,
            } => {
                DegenbotDb::apply_stk_aave_transfer_on_conn(
                    conn,
                    *from_user_id,
                    *to_user_id,
                    *amount,
                )?;
                report.stk_aave_transfer += 1;
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
                // O4BOST: for the bootstrap contracts (`POOL`/
                // `POOL_CONFIGURATOR`) use the IDEMPOTENT variant so the chunk
                // loop's re-encounter of the bootstrap `ProxyCreated` events
                // (the bootstrap pass already applied them over
                // `[from_block, from_block + BOOTSTRAP_WINDOW]`, which overlaps
                // the chunk loop's first chunk) does NOT insert a duplicate
                // row. Other `ContractInserted` names (`POOL_DATA_PROVIDER`,
                // `PRICE_ORACLE`, `AddressSet`-decoded names) keep the
                // unconditional INSERT — parity with the Python which
                // unconditional-appends them.
                if name == "POOL" || name == "POOL_CONFIGURATOR" {
                    DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                        conn,
                        *ev_market_id,
                        name,
                        address,
                        *revision,
                    )?;
                } else {
                    DegenbotDb::apply_contract_inserted_on_conn(
                        conn,
                        *ev_market_id,
                        name,
                        address,
                        *revision,
                    )?;
                }
                report.contract_inserted += 1;
            }
        }
    }

    Ok(report)
}

/// Apply a chunk's worth of pre-decoded Aave V3 events under the caller's
/// `Transaction` (borrowed as a `&Connection`), then stamp
/// `aave_v3_markets.last_update_block = chunk_end_block` as the LAST write.
///
/// Thin wrapper over [`apply_chunk_events_on_conn`] (the per-event dispatch)
/// that then stamps the block. Kept as the public batched-apply entrypoint so
/// that the existing §3.4 atomicity tests stay GREEN.
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
#[allow(clippy::missing_errors_doc)]
pub fn apply_aave_chunk_writes_on_conn(
    conn: &Connection,
    market_id: i64,
    events: &[AaveChunkEvent],
    chunk_end_block: u64,
) -> Result<AaveChunkWriteReport, degenbot_db::DbError> {
    let mut report = apply_chunk_events_on_conn(conn, market_id, events)?;

    // Stamp `last_update_block` as the LAST write (§3.4 restart-invariant:
    // on rollback the stamp does NOT advance, so a restart re-processes the
    // chunk clean).
    let chunk_end_i64 = i64::try_from(chunk_end_block).unwrap_or(i64::MAX);
    DegenbotDb::set_market_last_update_block_on_conn(conn, market_id, chunk_end_i64)?;
    report.stamped_block = Some(chunk_end_block);

    Ok(report)
}

// ── the outer chunk loop (RPC-bound; the §4.4 atomicity owner — 6SWY4R-3) ──

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use alloy::rpc::types::Log;
use degenbot_core::errors::ProviderError;
use degenbot_db::aave::AaveGhoAsset;
use degenbot_db::DbError;
use degenbot_rpc::provider::{AlloyProvider, LogFetcher};

use crate::aave_fetch::{
    fetch_aave_chunk_logs, fetch_scaled_token_logs, fetch_stk_aave_logs,
    sort_logs_by_block_and_index, AaveFetchSpec,
};
use crate::config_dispatch::{
    build_discount_snapshot, dispatch_config_events, match_proxy_id, ConfigDispatchError,
    ProxyCreationResolution,
};
use crate::transaction_processor::{process_transaction, ProcessTxError};

/// The max RPC retries for the owned runtime's `AlloyProvider` (mirrors
/// `degenbot-pool-updater`'s `RPC_MAX_RETRIES`).
const RPC_MAX_RETRIES: u32 = 5;

/// The cold-boot bootstrap window (O4BOST). When `aave_v3_contracts` lacks the
/// `POOL`/`POOL_CONFIGURATOR` rows that `build_fetch_spec` requires, the
/// bootstrap pass fetches `ProxyCreated` events from the `POOL_ADDRESS_PROVIDER`
/// over `[from_block, from_block + BOOTSTRAP_WINDOW]` + applies them
/// idempotently. Mainnet lands the bootstrap `ProxyCreated` events at
/// `from_block + 57/+60/+66` (blocks 16291127/16291130/16291136 — `from_block`
/// is the deploy + 1 = 16291071); the 2 000-block window gives ample margin.
/// Non-mainnet markets with a longer deploy→`ProxyCreated` gap may need a
/// larger window (a `BootstrapFailed` error surfaces the miss).
const BOOTSTRAP_WINDOW: u64 = 2_000;

/// A per-chunk progress snapshot reported to [`ProgressSink`] at each chunk
/// boundary (after a successful commit OR a rollback). Mirrors
/// `degenbot-pool-updater::ChunkProgress`.
#[derive(Debug, Clone)]
pub struct AaveChunkProgress {
    pub chain_id: i64,
    pub market_id: i64,
    pub chunk_start: u64,
    pub chunk_end: u64,
    /// The total `AaveChunkEvent`s the apply fn wrote this chunk.
    pub events_applied: usize,
    /// `true` iff the chunk's transaction committed; `false` if it rolled
    /// back (an error mid-chunk → the whole chunk reverted → restart will
    /// re-process).
    pub committed: bool,
    /// The user addresses touched by ANY log in this chunk (topics[1]/[2]
    /// extracted as addresses). Drives the JGQHBX drive harness's per-chunk
    /// value-correctness gate via
    /// [`crate::verify::verify_touched_positions_on_conn`]: the harness's
    /// `progress_callback` receives this list + calls the verify fn against
    /// cand.db after the commit (small-set per-position RPC verification —
    /// multicall3 batching for the market-wide verify is BE474R-full).
    pub touched_user_addresses: Vec<Address>,
}

/// The sink the chunk loop reports per-chunk progress to. Implementations:
/// [`NoProgress`] (silent), a logging sink, or a `PyO3` callback sink (the
/// 6SWY4R-B seam). `report_chunk` is synchronous (called between chunks, off
/// the async path). Mirrors `degenbot-pool-updater::ProgressSink`.
pub trait ProgressSink: Send + Sync {
    fn report_chunk(&self, progress: &AaveChunkProgress);
}

/// A no-op [`ProgressSink`] — silent runs (the default when no sink is
/// supplied). Mirrors `degenbot-pool-updater::NoProgress`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn report_chunk(&self, _progress: &AaveChunkProgress) {}
}

/// The final report from a [`run_aave_update`] run. Mirrors
/// `degenbot-pool-updater::UpdateReport`.
#[derive(Debug, Default, Clone, Copy)]
pub struct AaveUpdateReport {
    pub chain_id: i64,
    pub market_id: i64,
    /// The first block processed (inclusive).
    pub from_block: u64,
    /// The last block the run advanced `last_update_block` to.
    pub to_block: u64,
    /// Total chunks committed.
    pub chunks_committed: usize,
    /// Total `AaveChunkEvent`s written across all chunks.
    pub total_events_applied: usize,
}

/// An error from [`run_aave_update`]. Mirrors `degenbot-pool-updater::RunError`
/// + adds the Aave dispatch/parse errors.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("database error: {0}")]
    Db(#[from] DbError),
    #[error("rpc error: {0}")]
    Provider(#[from] ProviderError),
    #[error("config dispatch error: {0}")]
    ConfigDispatch(#[from] ConfigDispatchError),
    #[error("transaction parse error: {0}")]
    ProcessTx(#[from] ProcessTxError),
    #[error("runtime error: {0}")]
    Runtime(#[from] std::io::Error),
    #[error("cancelled by cancel flag at chunk boundary")]
    Cancelled,
    #[error("market {0} not found")]
    MarketNotFound(i64),
    #[error("market {0} has no last_update_block — bootstrap the stamp before the loop")]
    NotBootstrapped(i64),
    #[error(
        "market {0} cold-boot bootstrap failed — POOL/POOL_CONFIGURATOR remain \
         missing after the ProxyCreated fetch over the bootstrap window"
    )]
    BootstrapFailed(i64),
    /// Pre-commit verification found divergences. The chunk's `Transaction`
    /// was DROPPED (rolled back) — `last_update_block` did NOT advance, so
    /// the next run re-processes the same chunk. Carries the divergence
    /// details so the caller can format them.
    #[error("verification failed at chunk {chunk_start}-{chunk_end}")]
    Verification {
        chunk_start: u64,
        chunk_end: u64,
        divergences: Vec<crate::verify::PositionDivergence>,
    },
}

/// One transaction's grouped logs. Sorted by `(block_number, first log_index)`
/// (the fetcher's `(block_number, log_index)` sort is preserved; the grouping
/// is stable). Mirrors the Python `_build_transaction_contexts` (utils.py:82).
#[derive(Debug)]
struct TxGroup<'a> {
    tx_hash: [u8; 32],
    block_number: u64,
    logs: Vec<&'a Log>,
}

/// Group a chunk's logs by `transactionHash`, preserving the
/// `(block_number, log_index)` sort. Mirrors the Python
/// `_build_transaction_contexts` (utils.py:82) — the events are pre-sorted,
/// then bucketed by `tx_hash`; the groups are emitted in first-seen order
/// (sorted by the group's first log's `(block_number, log_index)`, which the
/// fetcher's sort guarantees).
fn group_logs_by_tx(logs: &[Log]) -> Vec<TxGroup<'_>> {
    // The fetcher already sorted by (block_number, log_index), so a stable
    // insertion-order HashMap preserves chronological group order.
    let mut order: Vec<[u8; 32]> = Vec::new();
    let mut groups: HashMap<[u8; 32], TxGroup<'_>> = HashMap::new();
    for log in logs {
        let Some(tx_hash_b256) = log.transaction_hash else {
            // A log with no tx_hash — shouldn't happen for fetched logs. Treat
            // it as its own singleton group keyed by zero (mirrors the Python
            // which would KeyError on `event["transactionHash"]`).
            continue;
        };
        let tx_hash = tx_hash_b256.0;
        let block_number = log.block_number.unwrap_or(0);
        let entry = groups.entry(tx_hash).or_insert_with(|| {
            order.push(tx_hash);
            TxGroup {
                tx_hash,
                block_number,
                logs: Vec::new(),
            }
        });
        entry.logs.push(log);
    }
    order
        .into_iter()
        .map(|k| groups.remove(&k).expect("present in order"))
        .collect()
}

/// Cold-boot the `POOL`/`POOL_CONFIGURATOR` contract rows (O4BOST). On a fresh
/// market, `activate` seeds only the `POOL_ADDRESS_PROVIDER`; `build_fetch_spec`
/// hard-errors without `POOL`/`POOL_CONFIGURATOR`. This pass fetches the
/// `ProxyCreated` events from the `POOL_ADDRESS_PROVIDER` address over the
/// bootstrap window `[from_block, from_block + BOOTSTRAP_WINDOW]`, decodes each
/// via [`match_proxy_id`] (which RPCs the `POOL_REVISION()`/`CONFIGURATOR_REVISION()`
/// on the implementation address), and applies the resolved rows via
/// [`DegenbotDb::apply_contract_inserted_if_absent_on_conn`] (idempotent — so the
/// chunk loop's later re-encounter of the same `ProxyCreated` events is a
/// no-op). No-op on a warm boot (both rows already present). Mirrors the Python
/// `update_aave_market` Phase-1 bootstrap (commands.py:1010-1062).
///
/// # Errors
///
/// Returns [`RunError::NotBootstrapped`] if the `POOL_ADDRESS_PROVIDER` row is
/// missing (the caller must seed it via `activate`), or [`RunError::BootstrapFailed`]
/// if `POOL`/`POOL_CONFIGURATOR` remain missing after the fetch (e.g. the
/// bootstrap window was too small for a non-mainnet market).
async fn bootstrap_pool_contracts(
    db: &DegenbotDb,
    provider: &AlloyProvider,
    fetcher: &LogFetcher,
    market_id: i64,
    from_block: u64,
) -> Result<(), RunError> {
    // 1. Read the current contracts; skip if both bootstrap rows are present.
    let contracts = db.fetch_aave_contracts(market_id)?;
    let has_pool = contracts.iter().any(|c| c.name == "POOL");
    let has_configurator = contracts.iter().any(|c| c.name == "POOL_CONFIGURATOR");
    if has_pool && has_configurator {
        return Ok(()); // warm boot — nothing to do.
    }
    let address_provider = contracts
        .iter()
        .find(|c| c.name == "POOL_ADDRESS_PROVIDER")
        .ok_or(RunError::NotBootstrapped(market_id))?;
    let ap_address = address_provider.address;

    // 2. Fetch the PoolAddressProvider's events over the bootstrap window
    //    (async, no DB lock held across the `.await`). The fetch unions all
    //    address-provider topics; we filter to `ProxyCreated` below.
    let boot_end = from_block.saturating_add(BOOTSTRAP_WINDOW);
    let logs =
        crate::aave_fetch::fetch_address_provider_logs(fetcher, from_block, boot_end, ap_address)
            .await?;

    // 3. Decode + resolve each `ProxyCreated` (async RPC for the revision).
    //    `match_proxy_id` returns `None` for non-POOL/non-POOL_CONFIGURATOR ids
    //    (e.g. the `POOL_DATA_PROVIDER` proxy id) — those are skipped (the chunk
    //    loop's `PoolDataProviderUpdated`/`AddressSet` arms handle them).
    let mut resolutions: Vec<ProxyCreationResolution> = Vec::new();
    for log in &logs {
        let Some(degenbot_decoders::aave_event_decoder::DecodedAaveEvent::ProxyCreated(ev)) =
            degenbot_decoders::aave_event_decoder::decode_aave_log(log)
        else {
            continue;
        };
        if let Some(resolved) = match_proxy_id(
            &ev.id,
            &ev.proxy_address,
            &ev.implementation_address,
            provider,
            from_block,
        )
        .await?
        {
            resolutions.push(resolved);
        }
    }

    // 4. Apply the resolved rows idempotently in ONE transaction (so a partial
    //    bootstrap either commits all or none).
    if !resolutions.is_empty() {
        let mut guard = db.lock();
        let tx = guard.transaction().map_err(DbError::from)?;
        for r in &resolutions {
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &tx,
                market_id,
                &r.name,
                &r.address,
                Some(r.revision),
            )?;
        }
        tx.commit().map_err(DbError::from)?;
    }

    // 5. Re-verify: if either bootstrap row is STILL missing, the window was
    //    too small (or the market isn't mainnet-shaped) → surface the miss.
    let contracts2 = db.fetch_aave_contracts(market_id)?;
    let have_pool = contracts2.iter().any(|c| c.name == "POOL");
    let have_cfg = contracts2.iter().any(|c| c.name == "POOL_CONFIGURATOR");
    if !have_pool || !have_cfg {
        return Err(RunError::BootstrapFailed(market_id));
    }
    Ok(())
}

/// Resolve the [`AaveFetchSpec`] for `market_id`: the `POOL`/
/// `POOL_CONFIGURATOR` / `POOL_ADDRESS_PROVIDER` / `PRICE_ORACLE` contract
/// addresses (from `aave_v3_contracts`), the chain's aToken+vToken addresses, + the GHO
/// asset's stkAAVE address. Mirrors the Python `update_aave_market`'s contract
/// + `known_scaled_token_addresses` resolution (commands.py:1008-1058).
///
/// Returns `(spec, gho_asset)` — the `gho_asset` is the chain's GHO token row
/// (`None` for non-GHO markets); the orchestrator passes it to the discount
/// pre-pass + `process_transaction`.
#[allow(clippy::too_many_arguments)]
fn build_fetch_spec(
    db: &DegenbotDb,
    market_id: i64,
    chain_id: i64,
) -> Result<(AaveFetchSpec, Option<AaveGhoAsset>), RunError> {
    let contracts = db.fetch_aave_contracts(market_id)?;
    // Index by name (the Python `get_contract(market, name)` shape).
    let mut by_name: HashMap<&str, &degenbot_db::rows::AaveV3ContractRow> = HashMap::new();
    for c in &contracts {
        by_name.insert(c.name.as_str(), c);
    }
    let pool = by_name.get("POOL").ok_or_else(|| {
        RunError::Db(DbError::MissingRow(
            "POOL contract row not found for market {market_id}".to_string(),
        ))
    })?;
    let configurator = by_name.get("POOL_CONFIGURATOR").ok_or_else(|| {
        RunError::Db(DbError::MissingRow(
            "POOL_CONFIGURATOR contract row not found".to_string(),
        ))
    })?;
    let address_provider = by_name.get("POOL_ADDRESS_PROVIDER").ok_or_else(|| {
        RunError::Db(DbError::MissingRow(
            "POOL_ADDRESS_PROVIDER contract row not found".to_string(),
        ))
    })?;
    let oracle_address = by_name.get("PRICE_ORACLE").map(|c| c.address);

    let scaled_token_addresses: Vec<Address> = db
        .fetch_aave_scaled_token_addresses(chain_id)?
        .into_iter()
        .filter_map(|s| s.parse::<Address>().ok())
        .collect();

    // The GHO asset (chain-unique) + the stkAAVE address.
    let gho_asset = db.fetch_aave_gho_asset(chain_id)?;
    let stk_aave_address = gho_asset
        .as_ref()
        .and_then(|g| g.v_gho_discount_token.as_deref())
        .and_then(|s| s.parse::<Address>().ok());

    let spec = AaveFetchSpec {
        pool_address: pool.address,
        configurator_address: configurator.address,
        address_provider_address: address_provider.address,
        oracle_address,
        scaled_token_addresses,
        stk_aave_address,
    };
    Ok((spec, gho_asset))
}

/// Run the Aave V3 chunk-update loop for `market_id`, advancing
/// `aave_v3_markets.last_update_block` to `to_block` (or the chain tip if
/// `to_block` is `None`). The §3.4 atomicity owner.
///
/// Per market per chunk:
/// 1. `fetch_aave_chunk_logs` returns the raw `Vec<Log>` sorted by
///    `(block_number, log_index)`.
/// 2. `group_logs_by_tx` returns the per-tx groups (mirrors `_build_transaction_contexts`).
/// 3. Open ONE `Transaction`. For each tx group, re-resolve the GHO vToken
///    revision (GJQGKN per-tx, sees prior txs' `Upgraded` writes), build the
///    discount snapshot (RPC + the DB-cache path), dispatch the config events
///    (RPC for revisions and metadata plus the substrate lookups), apply THAT
///    tx's config events to `conn` (so the ops parser sees them), run
///    `process_transaction` (C3's operations parser, sync, substrate lookups),
///    and apply THAT tx's op events to `conn` (so tx N+1 sees them).
/// 4. Stamp `last_update_block = chunk_end` as the LAST write (end-of-chunk).
///    The caller's `Transaction` commits (or drops, rolling back). The stamp
///    is the LAST write.
///
/// # The §3.4 atomicity invariant (LOAD-BEARING)
///
/// ONE `Transaction` per chunk. Failure mid-chunk → drop the tx → the whole
/// chunk reverts → `last_update_block` unchanged → a restart re-processes the
/// chunk clean (no skipped blocks, no partial commit). The Transaction is
/// held open across the per-tx RPC (the discount pre-pass + the config dispatch
/// do substrate lookups + writes via `get_or_create_*` that MUST be atomic with
/// the chunk; the apply is the last step). The owned tokio runtime's
/// `block_on` polls the future on the calling thread, so the `!Send`
/// `&Transaction` borrow across `.await` is safe (single-thread poll).
///
/// # Owned runtime (D2)
///
/// MUST NOT be called from within an existing tokio runtime (panic on nested
/// `block_on`). Mirror `degenbot-pool-updater`'s constraint.
///
/// # §4.2-parity notes (flagged)
///
/// - **`treasury_address` is `None`**: `process_transaction` accepts it but the
///   current dispatch doesn't consume it (forward-compat). The Python resolves
///   it via the Pool's `RESERVE_TREASURY_ADDRESS()` RPC — not wired here.
/// - **`vtoken_revision` drift**: the discount pre-pass reads the GHO vToken's
///   revision at chunk-start (the in-chunk `Upgraded` write is DEFERRED to
///   Apply — §3.4). If an `Upgraded` event lands mid-chunk (the deprecation),
///   txs AFTER it would see the OLD revision → a non-zero discount instead of
///   0. In practice a vToken upgrade fires once per market lifetime, so the
///   drift is rare; flagged for the orchestrator's §4.2 review.
#[allow(
    clippy::await_holding_lock,
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]
pub fn run_aave_update(
    database_path: &Path,
    chain_id: i64,
    market_id: i64,
    to_block: Option<u64>,
    chunk_size: u64,
    rpc_url: &str,
    cancel: Arc<AtomicBool>,
    progress: Arc<dyn ProgressSink>,
    verify_chunk: bool,
) -> Result<AaveUpdateReport, RunError> {
    if chunk_size == 0 {
        return Err(RunError::Provider(ProviderError::InvalidBlockRange {
            from: 1,
            to: 0,
        }));
    }

    // Open ONE writeable handle for the whole run.
    let (db, _schema_state) = DegenbotDb::open_for_writes(database_path)?;

    // Resolve the market row (the `last_update_block` cursor).
    let market = db
        .fetch_aave_market_row(market_id)?
        .ok_or(RunError::MarketNotFound(market_id))?;
    if market.chain_id != chain_id {
        return Err(RunError::Db(DbError::Decode(format!(
            "market {market_id} chain_id {} != requested {chain_id}",
            market.chain_id
        ))));
    }
    let last_update_block = market
        .last_update_block
        .ok_or(RunError::NotBootstrapped(market_id))?;

    // Owned tokio runtime (D2). The fetches + the per-tx RPC run on it; the DB
    // writes are synchronous. MUST NOT be called from within an existing runtime.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let provider = rt.block_on(AlloyProvider::new(rpc_url, RPC_MAX_RETRIES))?;
    let provider = Arc::new(provider);
    let fetcher = LogFetcher::new(provider.clone(), chunk_size);

    // Resolve the chain tip if `to_block` is None.
    let last_block = match to_block {
        Some(n) => n,
        None => rt.block_on(provider.get_block_number())?,
    };

    let from_block = u64::try_from(last_update_block).unwrap_or(0) + 1;
    if from_block > last_block {
        // Already up to date — nothing to do.
        return Ok(AaveUpdateReport {
            chain_id,
            market_id,
            from_block: last_block,
            to_block: last_block,
            ..Default::default()
        });
    }

    // Cold-boot bootstrap (O4BOST). On a fresh market `activate` seeds only
    // `POOL_ADDRESS_PROVIDER`; `build_fetch_spec` below hard-errors if `POOL`/
    // `POOL_CONFIGURATOR` are missing. The bootstrap pass fetches the
    // `ProxyCreated` events from the `POOL_ADDRESS_PROVIDER` over the bootstrap
    // window + applies them idempotently (so the chunk loop's later
    // re-encounter of the same events is a no-op). No-op on a warm boot (both
    // rows already present). Mirrors the Python `update_aave_market` Phase-1
    // bootstrap (commands.py:1010-1062 + `_process_proxy_creation_event`).
    rt.block_on(bootstrap_pool_contracts(
        &db, &provider, &fetcher, market_id, from_block,
    ))?;

    // Build the fetch spec + the GHO asset (chain-unique). The per-chunk
    // loop's GJXURV refresh re-reads `scaled_token_addresses` +
    // `stk_aave_address` from the DB at the START of each chunk, so the
    // frozen run-start snapshot here is just the seed for chunk 1.
    let (mut spec, _gho_asset) = build_fetch_spec(&db, market_id, chain_id)?;
    let pool_address = spec.pool_address;
    let oracle_address = spec.oracle_address;

    let mut report = AaveUpdateReport {
        chain_id,
        market_id,
        from_block,
        to_block: last_block,
        ..Default::default()
    };

    let mut working_start = from_block;
    while working_start <= last_block {
        // Cooperative cancel at the chunk boundary (the most recent committed
        // chunk is durable; the next chunk's writes haven't started).
        if cancel.load(Ordering::Acquire) {
            return Err(RunError::Cancelled);
        }

        let chunk_end = last_block.min(working_start + chunk_size - 1);

        // 1. RPC fetch the chunk's logs (GIL-free, async, sorted by
        //    (block_number, log_index)).
        //
        // (a) W2S3WH: refresh the scaled-token address set from the DB at the
        //     START of each chunk — matches the Python's per-chunk
        //     `_get_all_scaled_token_addresses` (commands.py:1153). The frozen
        //     run-start set (build_fetch_spec above) misses assets created in
        //     PRIOR chunks (cross-chunk + run-spanning staleness). Read BEFORE
        //     the transaction (committed state — assets from prior chunks).
        //     The remaining same-chunk case (asset created mid-chunk) is
        //     handled by the (b) pre-scan below.
        spec.scaled_token_addresses = db
            .fetch_aave_scaled_token_addresses(chain_id)?
            .into_iter()
            .filter_map(|s| s.parse::<Address>().ok())
            .collect();
        // (a)' GJXURV: refresh `spec.stk_aave_address` from the DB at the
        //     START of each chunk — the W2S3WH sibling of
        //     `spec.scaled_token_addresses` above. The frozen run-start set
        //     (`build_fetch_spec` above) captures `v_gho_discount_token` from
        //     the cold-boot DB; on cold-boot states where `v_gho_discount_token
        //     IS NULL` (the on-chain `DiscountTokenUpdated` event that fills it
        //     fires mid-drive), the cached `None` short-circuits
        //     `fetch_stk_aave_logs` to an empty Vec for EVERY subsequent
        //     chunk — no stkAAVE Transfer events dispatched, no balance updates,
        //     the (C) refresh's on-chain backfill is the sole writer. Reading
        //     BEFORE the transaction sees committed state from prior chunks'
        //     DiscountTokenUpdated applies. Mirrors Python's per-chunk
        //     `_get_stk_aave_address` (would-be analog of
        //     `_get_all_scaled_token_addresses`).
        spec.stk_aave_address = db
            .fetch_aave_gho_asset(chain_id)?
            .as_ref()
            .and_then(|g| g.v_gho_discount_token.as_deref())
            .and_then(|s| s.parse().ok());
        let mut logs = rt.block_on(fetch_aave_chunk_logs(
            &spec,
            &fetcher,
            working_start,
            chunk_end,
        ))?;
        // (b) W2S3WH same-chunk staleness: an asset created mid-chunk (a
        //     `ReserveInitialized` in tx N + the first `Supply`/`Borrow` on
        //     it in tx N+M, same chunk) has its aToken/vToken NOT in the
        //     (just-refreshed) spec set — the asset doesn't exist until the
        //     `ReserveInitialized` dispatches inside `process_chunk_on_conn`.
        //     Pre-scan the frozen logs for `ReserveInitialized` events whose
        //     aToken/vToken isn't in the known set, re-fetch those tokens'
        //     scaled-token logs for the chunk range, + merge (de-dup by
        //     (block_number, log_index)). `process_chunk_on_conn` is
        //     UNCHANGED — the per-tx config-dispatch → ops interleave is
        //     preserved, so the `v_token_revision` conn reads stay per-tx-
        //     correct per I2RHGP Fix 2c (no rev-boundary regression — the
        //     rejected Option A two-pass split would have made tx N's ops see
        //     a later tx's `Upgraded`).
        let known: HashSet<Address> = spec.scaled_token_addresses.iter().copied().collect();
        let mut new_tokens: Vec<Address> = Vec::new();
        for log in &logs {
            if let Some(ev) = degenbot_decoders::aave_event_decoder::decode_reserve_initialized(log)
            {
                if !known.contains(&ev.a_token) {
                    new_tokens.push(ev.a_token);
                }
                if !known.contains(&ev.variable_debt_token) {
                    new_tokens.push(ev.variable_debt_token);
                }
            }
        }
        if !new_tokens.is_empty() {
            new_tokens.sort_unstable();
            new_tokens.dedup();
            let mut extra = rt.block_on(fetch_scaled_token_logs(
                &fetcher,
                working_start,
                chunk_end,
                &new_tokens,
            ))?;
            // De-dup by (block_number, log_index) — the re-fetch may overlap
            // the frozen fetch for tokens partially known (rare). Logs
            // missing either field sort last (shouldn't happen for fetched
            // logs — the fetcher fills both).
            let existing: HashSet<(u64, u64)> = logs
                .iter()
                .filter_map(|l| Some((l.block_number?, l.log_index?)))
                .collect();
            extra.retain(|l| {
                let key = (
                    l.block_number.unwrap_or(u64::MAX),
                    l.log_index.unwrap_or(u64::MAX),
                );
                !existing.contains(&key)
            });
            if !extra.is_empty() {
                logs.extend(extra);
                sort_logs_by_block_and_index(&mut logs);
            }
        }
        // (c) GJXURV same-chunk staleness for the discount token: a
        //     `DiscountTokenUpdated` event in tx N sets the new
        //     `v_gho_discount_token` mid-chunk — but `spec.stk_aave_address`
        //     was resolved from committed DB state BEFORE the chunk's dispatch
        //     (still `None` on a cold-boot where the event that first sets the
        //     token fires mid-drive). Pre-scan the frozen logs for
        //     `DiscountTokenUpdated`, re-resolve the new discount token, +
        //     re-fetch the stkAAVE Transfer/Staked/Redeem logs for the chunk
        //     range, merging with de-dup (same pattern as `(b)` above). The
        //     `process_chunk_on_conn` dispatch is UNCHANGED — the per-tx
        //     config-dispatch still applies `DiscountTokenUpdated` at its
        //     correct logIndex, so read-your-own-writes within the transaction
        //     is preserved.
        let mut discount_token_from_event: Option<Address> = None;
        for log in &logs {
            if let Some(ev) =
                degenbot_decoders::aave_event_decoder::decode_discount_token_updated(log)
            {
                discount_token_from_event = Some(ev.new_discount_token);
            }
        }
        if let Some(token) = discount_token_from_event.filter(|_| spec.stk_aave_address.is_none()) {
            let mut extra = rt.block_on(fetch_stk_aave_logs(
                &fetcher,
                working_start,
                chunk_end,
                Some(token),
            ))?;
            let existing: HashSet<(u64, u64)> = logs
                .iter()
                .filter_map(|l| Some((l.block_number?, l.log_index?)))
                .collect();
            extra.retain(|l| {
                let key = (
                    l.block_number.unwrap_or(u64::MAX),
                    l.log_index.unwrap_or(u64::MAX),
                );
                !existing.contains(&key)
            });
            if !extra.is_empty() {
                logs.extend(extra);
                sort_logs_by_block_and_index(&mut logs);
            }
        }

        let tx_groups = group_logs_by_tx(&logs);

        // 2. The single-transaction chunk write (§4.4 atomicity). The per-tx
        //    processing (discount pre-pass + config dispatch + parse) borrows
        //    the Transaction's `&Connection` for substrate lookups + writes —
        //    they MUST be atomic with the chunk's apply.
        let chunk_report = {
            let mut guard = db.lock();
            let tx = guard.transaction().map_err(DbError::from)?;
            let result = rt.block_on(process_chunk_on_conn(
                &tx,
                &provider,
                market_id,
                chain_id,
                pool_address,
                oracle_address,
                &tx_groups,
                chunk_end,
            ));
            match result {
                Ok(r) => {
                    // Pre-commit verification: if `verify_chunk` is set, run
                    // `verify_touched_positions_on_conn` on the
                    // (uncommitted) transaction. This catches divergences
                    // BEFORE the commit — a divergence drops `tx` (rollback)
                    // so `last_update_block` does NOT advance + the next run
                    // re-processes the same chunk. Without this, the bad
                    // commit would land first + the post-commit verify in
                    // the progress callback would find it but too late
                    // (the data is already durable).
                    if verify_chunk {
                        let touched: Vec<Address> =
                            r.touched_user_addresses.iter().copied().collect();
                        // Skip when no users were touched (matches the
                        // Python's `if not touched: return` — an empty
                        // chunk has nothing to verify). Passing `None`
                        // would verify ALL positions rather than none.
                        if !touched.is_empty() {
                            let divergences =
                                rt.block_on(crate::verify::verify_touched_positions_on_conn(
                                    &tx,
                                    &provider,
                                    market_id,
                                    chunk_end,
                                    Some(&touched),
                                ))?;
                            if !divergences.is_empty() {
                                // Drop `tx` (rollback) — the chunk's writes +
                                // the stamp advance are reverted.
                                drop(tx);
                                progress.report_chunk(&AaveChunkProgress {
                                    chain_id,
                                    market_id,
                                    chunk_start: working_start,
                                    chunk_end,
                                    events_applied: 0,
                                    committed: false,
                                    touched_user_addresses: Vec::new(),
                                });
                                return Err(RunError::Verification {
                                    chunk_start: working_start,
                                    chunk_end,
                                    divergences,
                                });
                            }
                        }
                    }
                    tx.commit().map_err(DbError::from)?;
                    r
                }
                Err(e) => {
                    // Drop `tx` (rollback) — the chunk's writes + the stamp
                    // advance are reverted; the committed prior chunks stand.
                    drop(tx);
                    progress.report_chunk(&AaveChunkProgress {
                        chain_id,
                        market_id,
                        chunk_start: working_start,
                        chunk_end,
                        events_applied: 0,
                        committed: false,
                        touched_user_addresses: Vec::new(),
                    });
                    return Err(e);
                }
            }
        };

        progress.report_chunk(&AaveChunkProgress {
            chain_id,
            market_id,
            chunk_start: working_start,
            chunk_end,
            events_applied: chunk_report.events_applied,
            committed: true,
            touched_user_addresses: chunk_report.touched_user_addresses.into_iter().collect(),
        });
        report.chunks_committed += 1;
        report.total_events_applied += chunk_report.events_applied;

        working_start = chunk_end + 1;
    }

    Ok(report)
}

/// The per-chunk processing core (the Transaction-borrowed, RPC-interspersed
/// body). Per tx group: re-resolve the GHO vToken revision, run the discount
/// pre-pass + the config-event dispatch + C3's `process_transaction`, then
/// apply THAT tx's events to `conn` via [`apply_chunk_events_on_conn`] BEFORE
/// the next tx's reads (GJQGKN per-tx apply — fixes the config-revision +
/// ops-balance staleness surfaces; matches Python's per-tx ORM session apply).
/// The `last_update_block` stamp is the LAST write (end-of-chunk). Held
/// inside the caller's `Transaction`; on `Err` the caller drops the tx
/// (rollback).
///
/// Extracted from [`run_aave_update`] to keep the loop fn readable + to
/// localize the `await_holding_lock` allow (the `&Transaction` borrow across
/// `.await` — safe under `block_on`'s single-thread poll).
#[allow(
    clippy::await_holding_lock,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::similar_names
)]
async fn process_chunk_on_conn(
    conn: &Connection,
    provider: &AlloyProvider,
    market_id: i64,
    chain_id: i64,
    pool_address: Address,
    oracle_address: Option<Address>,
    tx_groups: &[TxGroup<'_>],
    chunk_end: u64,
) -> Result<ChunkCoreReport, RunError> {
    // GJQGKN: per-tx apply within `conn`. Two staleness surfaces fixed —
    //   (1) config: `vtoken_revision` is now re-resolved per-tx from `conn`
    //       (read-your-own-writes sees the prior tx's `Upgraded` write),
    //       instead of the chunk-start snapshot that masked an in-chunk bump.
    //   (2) ops balances: each tx's events are applied to `conn` BEFORE the
    //       next tx's `build_discount_snapshot` / `dispatch_config_events` /
    //       `process_transaction` read — so tx N+1 sees tx N's writes (the
    //       `ScaledTokenProcessor` is stateless; its balances come from `conn`
    //       lookups, so per-tx apply is the only seam that matches Python's
    //       per-tx ORM session apply).
    // The stamp advance stays LAST (end-of-chunk), preserving the §3.4
    // restart-invariant (on rollback the stamp does NOT advance + the whole
    // chunk reverts).
    let mut events_applied_total: usize = 0;
    let mut touched_user_addresses: HashSet<Address> = HashSet::new();

    for group in tx_groups {
        let block_number = group.block_number;
        let tx_hashes_refs: Vec<&Log> = group.logs.clone();

        for log in &group.logs {
            let topics = log.topics();
            if let Some(t1) = topics.get(1) {
                touched_user_addresses.insert(Address::from_slice(&t1.as_slice()[12..]));
            }
            if let Some(t2) = topics.get(2) {
                touched_user_addresses.insert(Address::from_slice(&t2.as_slice()[12..]));
            }
        }

        // (0) Re-resolve the GHO asset from `conn` for THIS tx — sees the
        //     prior tx's `ReserveInitialized` write that set
        //     `aave_gho_tokens.v_token_id` (surface #3: the drive-startup
        //     snapshot had `v_token_address=None` when the GHO reserve wasn't
        //     yet initialized at coldboot, masking a mid-drive init + causing
        //     the GHO vToken's `Mint` to classify as plain `DebtMint` instead of
        //     `GhoDebtMint` → the borrow matcher found no DebtMint → NoMatch
        //     crash). Mirrors Python's lazy `tx_context.gho_vtoken_address`
        //     reload (re-reads the `v_token` relationship from the session on
        //     each tx's `_process_transaction` entry — line 81). The
        //     drive-startup `gho_asset`/addresses params are now only the
        //     coldboot seed; this per-tx fetch is authoritative.
        let gho_asset_tx = DegenbotDb::fetch_aave_gho_asset_on_conn(conn, chain_id)?;
        let gho_token_address_tx: Option<&str> = gho_asset_tx
            .as_ref()
            .and_then(|g| g.gho_token_address.as_deref());
        let gho_vtoken_address_tx: Option<&str> = gho_asset_tx
            .as_ref()
            .and_then(|g| g.v_token_address.as_deref());
        // C3.3 (C refresh): the chain's GHO discount-token (stkAAVE) address,
        //     re-resolved per-tx from `conn` (read-your-own-writes: a mid-run
        //     `DiscountTokenUpdated` bumps `v_gho_discount_token` here — the
        //     balanceOf must hit the NEW contract). `None` → the refresh is a
        //     no-op (no discount token configured).
        let discount_token_tx: Option<Address> = gho_asset_tx
            .as_ref()
            .and_then(|g| g.v_gho_discount_token.as_deref())
            .and_then(|s| s.parse().ok());

        // (a) Re-resolve the GHO vToken's revision from `conn` for THIS tx —
        //     sees the prior tx's in-chunk `Upgraded` write via
        //     read-your-own-writes (surface #1).
        let vtoken_revision: Option<u32> = match (gho_vtoken_address_tx, gho_asset_tx.as_ref()) {
            (Some(addr_str), Some(_)) => DegenbotDb::lookup_asset_by_token_address_on_conn(
                conn, market_id, addr_str, "v_token",
            )?
            .map(|row| row.v_token_revision),
            _ => None,
        };

        // (b) The discount pre-pass (RPC + the DB-cache path) — reads `conn`
        //     (sees prior txs' writes).
        let discounts = build_discount_snapshot(
            provider,
            &tx_hashes_refs,
            block_number,
            gho_vtoken_address_tx.and_then(|s| s.parse().ok()),
            vtoken_revision,
            market_id,
            conn,
        )
        .await?;

        // (c) The config-event dispatch (RPC + substrate lookups).
        let config_events = dispatch_config_events(
            provider,
            &tx_hashes_refs,
            market_id,
            chain_id,
            conn,
            pool_address,
            oracle_address,
            gho_asset_tx.as_ref(),
            block_number,
        )
        .await?;

        // (d) The config events were applied INTRA-dispatch (I2RHGP Fix 2c:
        //     `dispatch_config_events` applies each event to `conn` as it's
        //     dispatched, so a later config event's dispatch sees an earlier
        //     event's apply — e.g. `CollateralConfigurationChanged` sees the
        //     asset `ReserveInitialized` just created). By here the tx's config
        //     writes are already on `conn` (read-your-own-writes for the ops
        //     parser below). Matches Python's per-event apply order.
        events_applied_total += config_events.len();

        // (e) C3's operations parser (sync, substrate lookups) — reads `conn`
        //     (sees this tx's config writes + prior txs' writes) + uses the
        //     per-tx re-resolved GHO addresses (surface #3).
        let op_events = process_transaction(
            market_id,
            chain_id,
            pool_address,
            /* treasury_address */ None,
            gho_token_address_tx.and_then(|s| s.parse().ok()),
            gho_vtoken_address_tx.and_then(|s| s.parse().ok()),
            conn,
            &tx_hashes_refs,
            group.tx_hash,
            &discounts,
        )
        .map_err(|e| {
            eprintln!(
                "AAVE-PARSE-FAIL block={block_number} tx=0x{} err={e}",
                alloy::hex::encode(group.tx_hash)
            );
            e
        })?;

        // (f) Apply THIS tx's op events to `conn` — so tx N+1 sees them via
        //     read-your-own-writes (surface #2: the `Upgraded` revision bump +
        //     scaled-token balance deltas land before the next tx's reads).
        apply_chunk_events_on_conn(conn, market_id, &op_events)?;
        events_applied_total += op_events.len();

        // (f.4) DEBUG: per-tx touched-position trace (env-gated). When
        //     `DEGENBOT_AAVE_TX_TRACE=1`, emit one JSONL line per touched
        //     `position_id` to stderr, reading the POST-APPLY
        //     `(balance, last_index)` from `conn`. The per-tx differential-
        //     narrowing tool: a divergent (user, asset)'s trajectory pinpoints
        //     the exact tx where Rust's value first takes its final divergent
        //     value (e.g. the burn that should zero but leaves a ±1 residual).
        //     `position_id` is a stable surrogate; map it to (user, asset) via
        //     the final DB + the end-of-chunk compare's divergent list. This
        //     is the per-tx sibling of the per-tx apply (step f) — narrowing
        //     the comparison inside the chunk, one tx at a time, instead of
        //     deferring it to end-of-chunk. The end-of-chunk GREEN compare
        //     (exact-zero) remains the rigorous gate; this trace is the
        //     narrowing tool, not the gate.
        if std::env::var("DEGENBOT_AAVE_TX_TRACE").as_deref() == Ok("1") {
            let mut seen: std::collections::HashSet<(bool, i64)> = std::collections::HashSet::new();
            for ev in &op_events {
                match ev {
                    AaveChunkEvent::ScaledTokenMint {
                        position,
                        position_id,
                        ..
                    }
                    | AaveChunkEvent::ScaledTokenBurn {
                        position,
                        position_id,
                        ..
                    } => {
                        seen.insert((
                            matches!(*position, degenbot_db::ScaledTokenPosition::Debt),
                            *position_id,
                        ));
                    }
                    AaveChunkEvent::DebtPositionReset { position_id, .. } => {
                        seen.insert((true, *position_id));
                    }
                    AaveChunkEvent::ScaledTokenTransfer {
                        from_position_id,
                        to_position_id,
                        ..
                    } => {
                        seen.insert((false, *from_position_id));
                        if let Some(to) = to_position_id {
                            seen.insert((false, *to));
                        }
                    }
                    _ => {}
                }
            }
            let txhex = alloy::hex::encode(group.tx_hash);
            let mut ordered: Vec<(bool, i64)> = seen.into_iter().collect();
            ordered.sort_unstable();
            for (is_debt, pid) in ordered {
                let table = if is_debt {
                    "aave_v3_debt_positions"
                } else {
                    "aave_v3_collateral_positions"
                };
                let q = match table {
                    "aave_v3_debt_positions" => {
                        "SELECT balance, last_index FROM aave_v3_debt_positions WHERE id = ?1"
                    }
                    "aave_v3_collateral_positions" => {
                        "SELECT balance, last_index FROM aave_v3_collateral_positions WHERE id = ?1"
                    }
                    _ => continue,
                };
                let row: Option<(Option<String>, Option<String>)> = conn
                    .query_row(q, rusqlite::params![pid], |r| {
                        Ok((
                            r.get::<_, Option<String>>(0)?,
                            r.get::<_, Option<String>>(1)?,
                        ))
                    })
                    .ok();
                if let Some((bal, idx)) = row {
                    eprintln!(
                        "AAVE-TXTRACE {{\"block\":{},\"tx\":\"0x{}\",\"kind\":\"{}\",\"pos\":{},\"bal\":\"{}\",\"idx\":\"{}\"}}",
                        block_number,
                        txhex,
                        table,
                        pid,
                        bal.unwrap_or_default(),
                        idx.unwrap_or_default(),
                    );
                }
            }
        }

        // (f.5) C3.3 (C refresh): the async POST-APPLY discount-refresh pass.
        //     For each `GhoRefreshDiscount` signal this tx emitted (V1-V3 GHO
        //     mints/burns), recompute the user's `gho_discount` from the
        //     POST-APPLY debt balance + the user's stkAAVE balance
        //     (`balanceOf` `eth_call` at block-1 if `stk_aave_balance` is
        //     None). The refresh reads the POST-APPLY values (the apply above
        //     just landed them) + has the provider (this loop is async).
        //     Mirrors Python's `_refresh_discount_rate` +
        //     `get_or_init_stk_aave_balance` after a GHO borrow/accrual.
        for ev in &op_events {
            if let AaveChunkEvent::GhoRefreshDiscount { position_id } = ev {
                crate::config_dispatch::refresh_gho_discount(
                    provider,
                    conn,
                    market_id,
                    *position_id,
                    block_number,
                    discount_token_tx,
                )
                .await?;
            }
        }
    }

    // (g) Stamp `last_update_block` as the LAST write (end-of-chunk). The
    //     per-tx applies above are durable only when the caller's
    //     `Transaction` commits; on rollback the whole chunk (events + stamp)
    //     reverts (§3.4 restart-invariant).
    let chunk_end_i64 = i64::try_from(chunk_end).unwrap_or(i64::MAX);
    DegenbotDb::set_market_last_update_block_on_conn(conn, market_id, chunk_end_i64)?;

    Ok(ChunkCoreReport {
        events_applied: events_applied_total,
        touched_user_addresses,
    })
}

/// A small carrier for the per-chunk core's outcome (the event count + a hook
/// for richer per-type accounting in a future revision).
#[derive(Debug, Clone, Default)]
struct ChunkCoreReport {
    events_applied: usize,
    /// User addresses touched by ANY event in the chunk (topics[1]/[2] of every
    /// log extracted as addresses — cheap `O(num_logs * 2)` scan). Drives the
    /// JGQHBX drive harness's per-chunk value-correctness gate (the verify fn
    /// accepts a touched-users filter; verifying only touched users per chunk
    /// keeps the per-chunk RPC count bounded — multicall3 batching is the
    /// market-wide extension, BE474R-full).
    touched_user_addresses: HashSet<Address>,
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
            gho_link_token_id: None,
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
            gho_link_token_id: None,
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
                gho_link_token_id: None,
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
            to_position_id: Some(2),
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

    /// Crash #8 (RED→GREEN): a `ScaledTokenTransfer` whose recipient
    /// address is `ZERO_ADDRESS` (a burn-to-zero leg, the `Transfer(from=user,
    /// to=0x0, amount)` shape) must NOT create a 0x0 collateral position. The
    /// orchestrator's 19M→22M march diverged exactly here: Rust wrote BOTH
    /// legs unconditionally, creating an extra `aave_v3_collateral_positions`
    /// row for the 0-address user on rETH (balance=911746220 wei = dust) where
    /// Python ref had zero 0-address rows for rETH.
    ///
    /// Python's mirror filter lives in `transfers.py:_process_collateral_transfer`
    /// — the recipient block (`if scaled_event.target_address != ZERO_ADDRESS:`)
    /// is skipped when `to == ZERO_ADDRESS`. The SENDER side is written
    /// unconditionally (matches the 2 pre-existing 0-address rows on
    /// 0x83F2 / 0xD533 in both Python ref + Rust). The Rust equivalent: the
    /// dispatch path wraps `to_position_id: None` when `to_addr == ZERO_ADDRESS`,
    /// and the apply fn skips the recipient write.
    #[test]
    fn apply_aave_chunk_writes_on_conn_skips_zero_address_recipient_leg() {
        let db = fresh_db();
        // Seed only the SENDER (id=1, balance=5 RAY). The recipient position
        // is intentionally absent — the test asserts the apply path does NOT
        // insert a row for it.
        seed_collateral_position_with_balance(
            &db,
            1,
            &(U256::from(5u8) * RAY).to_string(),
            Some(&RAY.to_string()),
        );

        // The dispatch path passes `to_position_id: None` when
        // `to_addr == ZERO_ADDRESS` — the apply path must skip the recipient
        // write entirely (no INSERT into aave_v3_collateral_positions).
        let events = vec![AaveChunkEvent::ScaledTokenTransfer {
            from_position_id: 1,
            to_position_id: None,
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

        // Sender side is unaffected — debited as normal.
        let (from_balance, from_last_index) = position_state(&db, 1);
        assert_eq!(
            from_balance,
            (U256::from(3u8) * RAY).to_string(),
            "sender balance after transfer = 5 - 2 = 3 RAY (the zero-recipient skip is recipient-only)"
        );
        assert_eq!(
            from_last_index,
            Some(RAY.to_string()),
            "sender last_index advances to the transfer's index"
        );

        // NO collateral position row was created for the 0-address recipient.
        let zero_recipient_rows: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM aave_v3_collateral_positions WHERE id = 2",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            zero_recipient_rows, 0,
            "apply must not create a row for the zero-address recipient"
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
                gho_link_token_id: None,
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

    /// YMWN5V retirement — every zero-leg `Transfer(0→X)` mint + `Transfer(X→0)`
    /// burn must be processed as a half-event (decrement `from` for the
    /// burn, increment `to` for the mint), mirroring Python's
    /// `process_stk_aave_transfer_event` (which only skips the `ZERO_ADDRESS`
    /// side + always mutates the real user). The YMWN5V-era dispatch skipped
    /// both legs and relied on a paired `Staked`/`Redeem` semantic event that
    /// empirically does NOT always fire — leaving senders' `stk_aave_balance`
    /// stuck at the pre-burn cache value. Crash #3 root cause.
    #[test]
    fn apply_aave_chunk_writes_on_conn_stk_aave_transfer_to_zero_decrements_sender() {
        let db = fresh_db();
        seed_aave_v3_user(&db, 1);
        // Pre-set stk_aave_balance = 9086 (the on-chain-stale cache value at
        // the user's last GHO action — mirrors the crash #3 reproduction).
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE aave_v3_users SET stk_aave_balance = '9086624312799369058615' \
                 WHERE id = 1",
                [],
            )
            .unwrap();
        }

        // Transfer(user → 0x0) — the burn-to-zero leg. No paired Redeem fires
        // for this user (verified via cast logs across the drive range).
        let events = vec![AaveChunkEvent::StkAaveTransfer {
            from_user_id: Some(1),
            to_user_id: None, // ZERO_ADDRESS recipient — the burn leg.
            amount: alloy::primitives::U256::from_str_radix("9086624312799369058615", 10).unwrap(),
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 17_859_255).unwrap();
            assert_eq!(report.stk_aave_transfer, 1);
            tx.commit().unwrap();
        }

        // The sender's balance is decremented to 0 (mirrors the on-chain
        // post-burn reality verified via balanceOf at block 17859256).
        let (_, stk_balance) = user_gho_stk_state(&db, 1);
        assert_eq!(
            stk_balance.as_deref(),
            Some("0"),
            "burn-to-zero Transfer must decrement the sender's balance to 0"
        );
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_stk_aave_transfer_from_zero_increments_recipient() {
        let db = fresh_db();
        seed_aave_v3_user(&db, 1);
        // Pre-set stk_aave_balance = NULL (CXRGX4 default — a never-touched user).

        // Transfer(0x0 → user) — the mint-from-zero leg (Staked pattern).
        let events = vec![AaveChunkEvent::StkAaveTransfer {
            from_user_id: None, // ZERO_ADDRESS sender — the mint leg.
            to_user_id: Some(1),
            amount: alloy::primitives::U256::from(1_234u64),
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 1_000).unwrap();
            assert_eq!(report.stk_aave_transfer, 1);
            tx.commit().unwrap();
        }

        let (_, stk_balance) = user_gho_stk_state(&db, 1);
        assert_eq!(
            stk_balance.as_deref(),
            Some("1234"),
            "mint-from-zero Transfer must increment the recipient's balance"
        );
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_stk_aave_transfer_both_zero_is_noop() {
        let db = fresh_db();
        seed_aave_v3_user(&db, 1);
        // Degenerate Transfer(0x0 → 0x0) — neither side mutated, no error.
        let events = vec![AaveChunkEvent::StkAaveTransfer {
            from_user_id: None,
            to_user_id: None,
            amount: alloy::primitives::U256::from(7u64),
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 5_000).unwrap();
            assert_eq!(
                report.stk_aave_transfer, 1,
                "the event still counts — no mutation only"
            );
            tx.commit().unwrap();
        }

        let (_, stk_balance) = user_gho_stk_state(&db, 1);
        assert_eq!(stk_balance, None, "both-zero Transfer touches nothing");
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_stk_aave_transfer_both_legs() {
        let db = fresh_db();
        seed_aave_v3_user(&db, 1);
        seed_aave_v3_user(&db, 2);
        // user 1 (sender): balance = 5000; user 2 (recipient): balance = 0.
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE aave_v3_users SET stk_aave_balance = '5000' WHERE id = 1",
                [],
            )
            .unwrap();
            conn.execute(
                "UPDATE aave_v3_users SET stk_aave_balance = '0' WHERE id = 2",
                [],
            )
            .unwrap();
        }

        let amount = alloy::primitives::U256::from(3_000u64);
        let events = vec![AaveChunkEvent::StkAaveTransfer {
            from_user_id: Some(1),
            to_user_id: Some(2),
            amount,
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 1_000).unwrap();
            assert_eq!(report.stk_aave_transfer, 1);
            tx.commit().unwrap();
        }

        let (_, from_balance) = user_gho_stk_state(&db, 1);
        assert_eq!(from_balance.as_deref(), Some("2000"), "5000 - 3000 = 2000");
        let (_, to_balance) = user_gho_stk_state(&db, 2);
        assert_eq!(to_balance.as_deref(), Some("3000"), "0 + 3000 = 3000");
    }

    #[test]
    fn apply_aave_chunk_writes_on_conn_stk_aave_transfer_from_leg_underflow_errors() {
        let db = fresh_db();
        seed_aave_v3_user(&db, 1);
        seed_aave_v3_user(&db, 2);
        // sender balance = 100; transfer 200 → from-leg underflow → error.
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE aave_v3_users SET stk_aave_balance = '100' WHERE id = 1",
                [],
            )
            .unwrap();
        }

        let events = vec![AaveChunkEvent::StkAaveTransfer {
            from_user_id: Some(1),
            to_user_id: Some(2),
            amount: alloy::primitives::U256::from(200u64),
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let result = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 1_000);
            assert!(
                result.is_err(),
                "transfer from-leg underflow must error (mirrors Redeem)"
            );
        }

        // Neither leg is mutated on error (the whole chunk rolls back).
        let (_, from_balance) = user_gho_stk_state(&db, 1);
        assert_eq!(
            from_balance.as_deref(),
            Some("100"),
            "balance untouched on error"
        );
        let (_, to_balance) = user_gho_stk_state(&db, 2);
        assert_eq!(
            to_balance.as_deref(),
            None,
            "recipient balance untouched on error"
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

        // Paired with a StkAaveTransfer (mint-from-zero leg — to_user_id=Some(1))
        // to verify the rollback is all-or-nothing. The StkAaveStaked variant
        // was retired in the YMWN5V retirement; the mint arm is now a zero-leg
        // StkAaveTransfer with to_user_id=Some.
        let events = vec![
            AaveChunkEvent::StkAaveTransfer {
                from_user_id: None, // ZERO_ADDRESS sender (mint-from-zero).
                to_user_id: Some(1),
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

    // ── O4BOST: the idempotent `apply_contract_inserted_if_absent_on_conn`
    //    (the cold-bootstrap idempotency substrate).

    #[test]
    fn apply_contract_inserted_if_absent_on_conn_cold_inserts_and_returns_true() {
        let db = fresh_db();
        let inserted = DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            &db.lock(),
            1,
            "POOL",
            "0xproxy",
            Some(2),
        )
        .unwrap();
        assert!(inserted, "cold insert returns true");
        let (name, addr, rev): (String, String, Option<i64>) = db
            .lock()
            .query_row(
                "SELECT name, address, revision FROM aave_v3_contracts \
                 WHERE market_id = 1 AND name = 'POOL'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(name, "POOL");
        assert_eq!(addr, "0xproxy");
        assert_eq!(rev, Some(2));
    }

    #[test]
    fn apply_contract_inserted_if_absent_on_conn_warm_same_key_is_noop_returns_false() {
        let db = fresh_db();
        // Pre-seed the row (the bootstrap pass already applied it).
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_contracts (id, market_id, name, address, revision) \
                 VALUES (1, 1, 'POOL', '0xproxy', 2)",
                [],
            )
            .unwrap();
        }
        let inserted = DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            &db.lock(),
            1,
            "POOL",
            "0xproxy",
            Some(99), // a later revision — must NOT overwrite the canonical row.
        )
        .unwrap();
        assert!(!inserted, "warm same-key is a no-op (returns false)");
        // Count unchanged + revision is the canonical (Phase-1 wins).
        let (count, rev): (i64, i64) = db
            .lock()
            .query_row(
                "SELECT COUNT(*), revision FROM aave_v3_contracts \
                 WHERE market_id = 1 AND name = 'POOL' AND address = '0xproxy'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1, "no duplicate row");
        assert_eq!(rev, 2, "Phase-1 wins — revision untouched");
    }

    #[test]
    fn apply_contract_inserted_if_absent_on_conn_different_address_inserts_second_row() {
        // The idempotency key is (market_id, name, address) — NOT
        // (market_id, name). A ProxyCreated with a DIFFERENT address
        // (e.g. a re-deploy) inserts a second row.
        let db = fresh_db();
        DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            &db.lock(),
            1,
            "POOL",
            "0xproxy_v1",
            Some(1),
        )
        .unwrap();
        let inserted = DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            &db.lock(),
            1,
            "POOL",
            "0xproxy_v2",
            Some(2),
        )
        .unwrap();
        assert!(inserted, "different address → inserts (returns true)");
        let count: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM aave_v3_contracts \
                 WHERE market_id = 1 AND name = 'POOL'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "two rows for two distinct addresses");
    }

    #[test]
    fn apply_contract_inserted_if_absent_on_conn_different_revision_same_key_is_noop() {
        // Same (market_id, name, address) but a different revision → no-op
        // (the canonical row's revision stays as Phase-1 wrote it).
        let db = fresh_db();
        DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            &db.lock(),
            1,
            "POOL_CONFIGURATOR",
            "0xcfg",
            Some(1),
        )
        .unwrap();
        let inserted = DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            &db.lock(),
            1,
            "POOL_CONFIGURATOR",
            "0xcfg",
            Some(5),
        )
        .unwrap();
        assert!(!inserted, "same key → no-op");
        let rev: i64 = db
            .lock()
            .query_row(
                "SELECT revision FROM aave_v3_contracts \
                 WHERE market_id = 1 AND name = 'POOL_CONFIGURATOR' AND address = '0xcfg'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rev, 1, "Phase-1 revision wins");
    }

    // ── 6SWY4R-3: the `group_logs_by_tx` unit tests (the loop's pure
    //    tx-grouping seam — mirrors the Python `_build_transaction_contexts`).

    /// Build a minimal `alloy::rpc::types::Log` with the given tx hash,
    /// block number, + log index (the grouping key).
    fn make_tx_log(tx_hash: [u8; 32], block_number: u64, log_index: u64) -> Log {
        use alloy::primitives::{Bytes, Log as AlloyLog, B256};
        let inner = AlloyLog::new_unchecked(Address::ZERO, vec![], Bytes::new());
        Log {
            inner,
            block_hash: None,
            block_number: Some(block_number),
            block_timestamp: None,
            transaction_hash: Some(B256::from(tx_hash)),
            transaction_index: None,
            log_index: Some(log_index),
            removed: false,
        }
    }

    #[test]
    fn group_logs_by_tx_empty_returns_empty() {
        let logs: Vec<Log> = vec![];
        let groups = group_logs_by_tx(&logs);
        assert!(groups.is_empty());
    }

    #[test]
    fn group_logs_by_tx_single_tx_buckets_all_logs() {
        let tx = [0xaa; 32];
        let logs = vec![
            make_tx_log(tx, 100, 0),
            make_tx_log(tx, 100, 1),
            make_tx_log(tx, 100, 2),
        ];
        let groups = group_logs_by_tx(&logs);
        assert_eq!(groups.len(), 1, "one tx → one group");
        assert_eq!(groups[0].tx_hash, tx);
        assert_eq!(groups[0].logs.len(), 3);
        assert_eq!(groups[0].block_number, 100);
    }

    #[test]
    fn group_logs_by_tx_multi_tx_preserves_chronological_order() {
        // The fetcher pre-sorts by (block_number, log_index); the grouping is
        // stable → groups emitted in first-seen (chronological) order.
        let tx_a = [0x11; 32];
        let tx_b = [0x22; 32];
        let tx_c = [0x33; 32];
        let logs = vec![
            // block 100: tx_a's two logs, then tx_b's one.
            make_tx_log(tx_a, 100, 0),
            make_tx_log(tx_a, 100, 1),
            make_tx_log(tx_b, 100, 2),
            // block 101: tx_c's one log.
            make_tx_log(tx_c, 101, 0),
        ];
        let groups = group_logs_by_tx(&logs);
        assert_eq!(groups.len(), 3);
        // Chronological order: tx_a (block 100, log 0), tx_b (block 100, log 2),
        // tx_c (block 101, log 0).
        assert_eq!(groups[0].tx_hash, tx_a);
        assert_eq!(groups[0].block_number, 100);
        assert_eq!(groups[0].logs.len(), 2);
        assert_eq!(groups[1].tx_hash, tx_b);
        assert_eq!(groups[1].block_number, 100);
        assert_eq!(groups[1].logs.len(), 1);
        assert_eq!(groups[2].tx_hash, tx_c);
        assert_eq!(groups[2].block_number, 101);
    }

    #[test]
    fn group_logs_by_tx_skips_logs_without_tx_hash() {
        // A log with no transaction_hash — shouldn't happen for fetched logs,
        // but the grouping is defensive (mirrors the Python which would
        // KeyError on `event["transactionHash"]`).
        let tx = [0xaa; 32];
        let log_with_tx = make_tx_log(tx, 100, 0);
        let mut log_no_tx = make_tx_log([0x00; 32], 100, 1);
        log_no_tx.transaction_hash = None;
        let logs = vec![log_with_tx.clone(), log_no_tx, log_with_tx.clone()];
        let groups = group_logs_by_tx(&logs);
        assert_eq!(groups.len(), 1, "the no-tx-hash log is skipped");
        assert_eq!(groups[0].logs.len(), 2);
    }
}

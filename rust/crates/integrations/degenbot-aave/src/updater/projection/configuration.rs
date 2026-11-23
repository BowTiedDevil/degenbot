//! The configuration family of the canonical delta vocabulary (ADR-105 D4,
//! D7).
//!
//! Every effect here was an `ConfigurationEffect` before the vocabulary migrated:
//! the configuration resolver produced a separate write type whose apply step
//! was a second pipeline beside the operation family's deltas. A
//! `ConfigurationEffect` is now the payload of a
//! [`DeltaEffect::Configuration`](super::DeltaEffect::Configuration), so a
//! configuration change states its exact source position and the generation it
//! activated exactly as a scaled-balance movement does, and both families
//! persist through one writer.
//!
//! The payload carries the ids the resolver settled and the fields the effect
//! writes. It performs no acquisition and no storage access: resolution is
//! upstream, persistence downstream.

use alloy::primitives::U256;
use degenbot_core::canonical_head::LogRef;
use degenbot_db::ScaledTokenPosition;

use super::{ContractRole, GenerationEntity, GenerationRef, ProjectionError};

/// The generation vocabulary's revision fact: a contract's reported revision
/// that a `u32` cannot hold (a negative value, or one wider than the domain) is
/// a refusal — never a synthesized revision zero (ADR-105 D6, D11).
fn checked_revision(revision: i64) -> Result<u32, ProjectionError> {
    u32::try_from(revision).map_err(|_| ProjectionError::InvalidGenerationRevision { revision })
}

/// One configuration change, ready to reduce into a canonical delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigurationEffect {
    /// `CollateralConfigurationChanged(asset, config_bitmap)` — decode the
    /// bitmap into the `aave_v3_asset_configs` row.
    CollateralConfigurationChanged { asset_id: i64, config_bitmap: U256 },
    /// `EModeCategoryAdded(id, label, ltv, lt, bonus, price_source)` — upsert
    /// the `aave_v3_emode_categories` row.
    EModeCategoryAdded {
        market_id: i64,
        category_id: i64,
        ltv: u64,
        liquidation_threshold: u64,
        liquidation_bonus: u64,
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
    /// `ReserveUsedAsCollateral{Enabled,Disabled}(user, asset)` — set the
    /// `enabled` flag on the `aave_v3_user_collateral_configs` row.
    ReserveUsedAsCollateral {
        user_id: i64,
        asset_id: i64,
        enabled: bool,
    },
    /// `UserEModeSet(user, e_mode)` — set the user's `aave_v3_users.e_mode`.
    UserEModeSet { user_id: i64, e_mode: i64 },
    /// `PriceOracleUpdated(market, new_oracle)` — register/replace the
    /// `PRICE_ORACLE` contract row (the market's price endpoint).
    PriceOracleUpdated {
        market_id: i64,
        new_oracle_address: String,
    },
    /// `AssetSourceUpdated(asset, source)` — set the `aave_v3_assets
    /// .price_source` column.
    AssetSourceUpdated {
        asset_id: i64,
        source_address: String,
    },
    /// `ReserveDataUpdated(reserve, liquidityRate, stableBorrowRate,
    /// variableBorrowRate, liquidityIndex, variableBorrowIndex)` — the asset
    /// row's indices/rates. `stableBorrowRate` is deprecated on Aave V3 and
    /// dropped; the values are stored raw (27-decimal ray as decimal text).
    ReserveDataUpdated {
        asset_id: i64,
        liquidity_rate: U256,
        variable_borrow_rate: U256,
        liquidity_index: U256,
        variable_borrow_index: U256,
        block_number: u64,
    },
    /// `ReserveInitialized(asset, aToken, stableDebtToken, variableDebtToken,
    /// interestRateStrategy)` — the reserve's aToken/vToken generation, with
    /// the revisions resolved at the event's own block.
    ReserveInitialized {
        market_id: i64,
        underlying_asset_id: i64,
        a_token_id: i64,
        a_token_revision: i64,
        v_token_id: i64,
        v_token_revision: i64,
        price_source: Option<String>,
        /// `Some(gho_token_row_id)` when the new reserve's underlying IS the
        /// GHO token: the GHO row's `v_token_id` link fires.
        gho_link_token_id: Option<i64>,
    },
    /// GHO `DiscountPercentUpdated(user, oldPercent, newPercent)` — sets the
    /// user's `gho_discount` percentage.
    GhoDiscountPercentUpdated {
        user_id: i64,
        new_discount_percent: i64,
    },
    /// GHO `DiscountRateStrategyUpdated(oldStrategy, newStrategy)` — sets the
    /// chain-unique GHO token row's rate strategy.
    GhoDiscountRateStrategyUpdated {
        gho_token_id: i64,
        new_strategy: Option<String>,
    },
    /// GHO `DiscountTokenUpdated(oldToken, newToken)` — the discount-token
    /// generation replacement.
    GhoDiscountTokenUpdated {
        gho_token_id: i64,
        new_discount_token: Option<String>,
    },
    /// stkAAVE ERC20 `Transfer(from, to, value)` — the canonical and only
    /// stkAAVE balance-mutation channel, covering both zero-leg arms.
    StkAaveTransfer {
        from_user_id: Option<i64>,
        to_user_id: Option<i64>,
        amount: U256,
    },
    /// `RewardsController.RewardsClaimed` — no persisted effect; the variant
    /// exists for event accounting (rewards surface only through the stkAAVE
    /// token's `Transfer` events).
    RewardsClaimed {
        user_id: i64,
        reward_token_id: i64,
        claimer_id: i64,
        claimed_amount: U256,
    },
    /// `Upgraded(implementation)` on an aToken/vToken proxy — the scaled
    /// token's revision generation changes at this event.
    Upgraded {
        asset_id: i64,
        market_id: i64,
        is_a_token: bool,
        new_revision: i64,
        /// `Some(gho_token_id)` when the upgrade deprecates the GHO discount
        /// (the GHO vToken reaching revision 4+).
        deprecated_gho_token_id: Option<i64>,
    },
    /// `PoolUpdated`/`PoolConfiguratorUpdated(old, new)` — the contract
    /// row's `revision` (looked up by name).
    ContractRevisionUpdated {
        market_id: i64,
        contract_name: String,
        new_revision: i64,
    },
    /// `PoolDataProviderUpdated(old, new)` — the data-provider endpoint.
    PoolDataProviderUpdated {
        market_id: i64,
        old_address: Option<String>,
        new_address: String,
    },
    /// `AddressSet(id, old, new)` registration or `ProxyCreated(id, proxy,
    /// impl)` for a tracked role — insert the role's contract row.
    ContractInserted {
        market_id: i64,
        name: String,
        address: String,
        revision: Option<i64>,
    },
    /// `AddressSet(id, old, new)` rotation — replace the role's endpoint
    /// address in place.
    ContractAddressChanged {
        market_id: i64,
        name: String,
        old_address: String,
        new_address: String,
    },
}

impl ConfigurationEffect {
    /// The generation this effect activates or reads, in the view's
    /// generation-qualified vocabulary (ADR-105 D6).
    ///
    /// A replacement event activates the generation it names at its own
    /// position: the `Upgraded` of a scaled token, a contract role's
    /// registration/rotation/revision, the discount-token replacement. The
    /// stkAAVE transfer channel reads the discount-token generation the view
    /// currently holds, so a transfer after a same-chunk replacement is
    /// interpreted under the generation the replacement installed.
    ///
    /// Effects that only record facts (an e-mode category, a user's discount
    /// percent) activate no generation.
    /// # Errors
    ///
    /// [`ProjectionError::InvalidGenerationRevision`] when the effect carries a
    /// revision the generation vocabulary cannot hold: a negative or wider-than-
    /// `u32` revision is a refusal, never a synthesized revision zero (ADR-105
    /// D6, D11).
    pub fn generation(
        &self,
        chain_id: i64,
        source: LogRef,
    ) -> Result<Option<GenerationRef>, ProjectionError> {
        match self {
            Self::Upgraded {
                asset_id,
                is_a_token,
                new_revision,
                ..
            } => Ok(Some(GenerationRef::activated_at(
                GenerationEntity::ScaledToken {
                    asset_id: *asset_id,
                    position: if *is_a_token {
                        ScaledTokenPosition::Collateral
                    } else {
                        ScaledTokenPosition::Debt
                    },
                },
                Some(checked_revision(*new_revision)?),
                source,
            ))),
            Self::ContractRevisionUpdated {
                market_id,
                contract_name,
                new_revision,
            } => Ok(Self::contract_generation(
                *market_id,
                contract_name,
                Some(checked_revision(*new_revision)?),
                source,
            )),
            Self::ContractInserted {
                market_id,
                name,
                revision,
                ..
            } => Ok(Self::contract_generation(
                *market_id,
                name,
                revision.map(checked_revision).transpose()?,
                source,
            )),
            Self::ContractAddressChanged {
                market_id, name, ..
            } => Ok(Self::contract_generation(*market_id, name, None, source)),
            Self::PoolDataProviderUpdated { market_id, .. } => {
                Ok(Some(GenerationRef::activated_at(
                    GenerationEntity::Contract {
                        market_id: *market_id,
                        role: ContractRole::PoolDataProvider,
                    },
                    None,
                    source,
                )))
            }
            Self::PriceOracleUpdated { market_id, .. } => Ok(Some(GenerationRef::activated_at(
                GenerationEntity::Contract {
                    market_id: *market_id,
                    role: ContractRole::PriceOracle,
                },
                None,
                source,
            ))),
            Self::ReserveInitialized {
                gho_link_token_id: Some(_),
                ..
            } => Ok(Some(GenerationRef::activated_at(
                GenerationEntity::GhoDiscountLink { chain_id },
                None,
                source,
            ))),
            Self::GhoDiscountTokenUpdated { .. } => Ok(Some(GenerationRef::activated_at(
                GenerationEntity::DiscountToken { chain_id },
                None,
                source,
            ))),
            _ => Ok(None),
        }
    }

    /// The generation a contract role's address/revision fact activates.
    ///
    /// A role name outside the tracked set (`AddressSet` can name any
    /// component) activates no generation.
    fn contract_generation(
        market_id: i64,
        name: &str,
        revision: Option<u32>,
        source: LogRef,
    ) -> Option<GenerationRef> {
        ContractRole::from_contract_name(name).map(|role| {
            GenerationRef::activated_at(
                GenerationEntity::Contract { market_id, role },
                revision,
                source,
            )
        })
    }

    /// Project this effect onto the resolution read position (ADR-105 D4):
    /// every fact a later resolution of the same chunk reads is updated in the
    /// chunk substrate here, because the effect's SQL write is deferred to the
    /// chunk's one transaction. Only facts resolution actually reads are
    /// projected; the rest are write-only and need no read-model.
    ///
    /// # Errors
    ///
    /// Returns [`degenbot_db::DbError`] on any substrate failure.
    pub fn project(
        &self,
        substrate: &mut crate::updater::run::substrate::ChunkSubstrate,
        conn: &rusqlite::Connection,
    ) -> Result<(), degenbot_db::DbError> {
        match self {
            Self::PriceOracleUpdated {
                market_id,
                new_oracle_address,
            } => substrate.record_contract_address(*market_id, "PRICE_ORACLE", new_oracle_address),
            Self::PoolDataProviderUpdated {
                market_id,
                new_address,
                ..
            } => substrate.record_contract_address(*market_id, "POOL_DATA_PROVIDER", new_address),
            Self::ContractInserted {
                market_id,
                name,
                address,
                revision,
            } => {
                substrate.record_contract_address(*market_id, name, address);
                substrate.record_contract_insert(*market_id, name, *revision);
            }
            Self::ContractAddressChanged {
                market_id,
                name,
                new_address,
                ..
            } => substrate.record_contract_address(*market_id, name, new_address),
            Self::ContractRevisionUpdated {
                market_id,
                contract_name,
                new_revision,
            } => substrate.record_contract_revision(*market_id, contract_name, *new_revision),
            Self::Upgraded {
                asset_id,
                is_a_token,
                new_revision,
                deprecated_gho_token_id,
                ..
            } => {
                substrate.record_asset_revision(*asset_id, *is_a_token, *new_revision);
                if deprecated_gho_token_id.is_some() {
                    substrate.project_gho(|gho| {
                        gho.v_gho_discount_token = None;
                        gho.v_gho_discount_rate_strategy = None;
                    });
                }
            }
            Self::ReserveInitialized { .. } => {
                self.project_reserve_initialized(substrate, conn)?;
            }
            Self::GhoDiscountTokenUpdated {
                new_discount_token, ..
            } => {
                // A TRUE replacement invalidates the balances the old ledger
                // denominated; the initial `None -> Some` link has no old
                // generation to invalidate.
                let replaced = substrate
                    .gho_asset(conn)?
                    .and_then(|gho| gho.v_gho_discount_token)
                    .is_some();
                if replaced {
                    substrate.invalidate_stk_aave_balances();
                }
                substrate.project_gho(|gho| {
                    gho.v_gho_discount_token.clone_from(new_discount_token);
                });
            }
            Self::GhoDiscountPercentUpdated {
                user_id,
                new_discount_percent,
            } => substrate.record_user_gho_discount_by_id(*user_id, *new_discount_percent),
            Self::StkAaveTransfer {
                from_user_id,
                to_user_id,
                amount,
            } => {
                use crate::updater::run::substrate::StkAaveLeg;
                if let Some(user_id) = from_user_id {
                    substrate.project_stk_aave_leg(*user_id, *amount, StkAaveLeg::From)?;
                }
                if let Some(user_id) = to_user_id {
                    substrate.project_stk_aave_leg(*user_id, *amount, StkAaveLeg::To)?;
                }
            }
            Self::GhoDiscountRateStrategyUpdated { new_strategy, .. } => {
                substrate.project_gho(|gho| {
                    gho.v_gho_discount_rate_strategy.clone_from(new_strategy);
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// Project a `ReserveInitialized` event: the reserve's identity is created
    /// HERE, during resolution, so every later event of the chunk (and the
    /// parse that follows) resolves the new asset, its aToken/vToken bindings
    /// and its source. Persistence applies the same operation again,
    /// idempotently, so a retry converges on one row.
    fn project_reserve_initialized(
        &self,
        substrate: &mut crate::updater::run::substrate::ChunkSubstrate,
        conn: &rusqlite::Connection,
    ) -> Result<(), degenbot_db::DbError> {
        let Self::ReserveInitialized {
            market_id,
            underlying_asset_id,
            a_token_id,
            a_token_revision,
            v_token_id,
            v_token_revision,
            price_source,
            gho_link_token_id,
        } = self
        else {
            return Ok(());
        };
        let asset_row_id = degenbot_db::DegenbotDb::apply_reserve_initialized_on_conn(
            conn,
            *market_id,
            *underlying_asset_id,
            *a_token_id,
            *a_token_revision,
            *v_token_id,
            *v_token_revision,
            price_source.as_deref(),
            *gho_link_token_id,
        )?;
        substrate.refresh_asset_row(conn, asset_row_id)?;
        if gho_link_token_id.is_some() {
            substrate.project_gho(|gho| {
                if let Ok(address) = conn.query_row(
                    "SELECT address FROM erc20_tokens WHERE id = ?1",
                    [v_token_id],
                    |r| r.get::<_, String>(0),
                ) {
                    gho.v_token_address = Some(address);
                }
            });
        }
        Ok(())
    }

    /// The effect's canonical encoding — its variant name and the fields that
    /// determine the write. Nothing chunk-scoped participates, so two runs that
    /// reduce the same chain facts encode identically however the range was
    /// chunked.
    #[must_use]
    #[expect(clippy::too_many_lines)] // one encoding line per variant
    pub fn encode(&self) -> String {
        let addr = |a: &Option<String>| a.clone().unwrap_or_else(|| "none".to_string());
        match self {
            Self::CollateralConfigurationChanged {
                asset_id,
                config_bitmap,
            } => format!("collateral-configuration{{asset={asset_id},bitmap={config_bitmap}}}"),
            Self::EModeCategoryAdded {
                market_id,
                category_id,
                ltv,
                liquidation_threshold,
                liquidation_bonus,
                price_source,
                label,
            } => format!(
                "emode-category-added{{market={market_id},category={category_id},ltv={ltv},lt={liquidation_threshold},bonus={liquidation_bonus},source={},label={label}}}",
                addr(price_source)
            ),
            Self::EModeAssetCategoryChanged {
                asset_id,
                new_category_id,
            } => format!("emode-asset-category{{asset={asset_id},category={new_category_id}}}"),
            Self::AssetCollateralInEModeChanged {
                asset_id,
                category_id,
                is_collateral,
            } => format!(
                "asset-collateral-in-emode{{asset={asset_id},category={category_id},collateral={is_collateral}}}"
            ),
            Self::ReserveUsedAsCollateral {
                user_id,
                asset_id,
                enabled,
            } => format!(
                "reserve-used-as-collateral{{user={user_id},asset={asset_id},enabled={enabled}}}"
            ),
            Self::UserEModeSet { user_id, e_mode } => {
                format!("user-emode{{user={user_id},emode={e_mode}}}")
            }
            Self::PriceOracleUpdated {
                market_id,
                new_oracle_address,
            } => format!("price-oracle-updated{{market={market_id},oracle={new_oracle_address}}}"),
            Self::AssetSourceUpdated {
                asset_id,
                source_address,
            } => format!("asset-source-updated{{asset={asset_id},source={source_address}}}"),
            Self::ReserveDataUpdated {
                asset_id,
                liquidity_rate,
                variable_borrow_rate,
                liquidity_index,
                variable_borrow_index,
                ..
            } => format!(
                "reserve-data-updated{{asset={asset_id},lr={liquidity_rate},vbr={variable_borrow_rate},li={liquidity_index},vbi={variable_borrow_index}}}"
            ),
            Self::ReserveInitialized {
                market_id,
                underlying_asset_id,
                a_token_id,
                a_token_revision,
                v_token_id,
                v_token_revision,
                price_source,
                gho_link_token_id,
            } => format!(
                "reserve-initialized{{market={market_id},underlying={underlying_asset_id},a-token={a_token_id}@{a_token_revision},v-token={v_token_id}@{v_token_revision},source={},gho-link={}}}",
                addr(price_source),
                gho_link_token_id.map_or_else(|| "none".to_string(), |id| id.to_string())
            ),
            Self::GhoDiscountPercentUpdated {
                user_id,
                new_discount_percent,
            } => format!(
                "gho-discount-percent-updated{{user={user_id},percent={new_discount_percent}}}"
            ),
            Self::GhoDiscountRateStrategyUpdated {
                gho_token_id,
                new_strategy,
            } => format!(
                "gho-discount-rate-strategy-updated{{gho={gho_token_id},strategy={}}}",
                addr(new_strategy)
            ),
            Self::GhoDiscountTokenUpdated {
                gho_token_id,
                new_discount_token,
            } => format!(
                "gho-discount-token-updated{{gho={gho_token_id},token={}}}",
                addr(new_discount_token)
            ),
            Self::StkAaveTransfer {
                from_user_id,
                to_user_id,
                amount,
            } => format!(
                "stk-aave-transfer{{from={},to={},amount={amount}}}",
                from_user_id.map_or_else(|| "none".to_string(), |id| id.to_string()),
                to_user_id.map_or_else(|| "none".to_string(), |id| id.to_string())
            ),
            Self::RewardsClaimed {
                user_id,
                reward_token_id,
                claimer_id,
                claimed_amount,
            } => format!(
                "rewards-claimed{{user={user_id},reward={reward_token_id},claimer={claimer_id},amount={claimed_amount}}}"
            ),
            Self::Upgraded {
                asset_id,
                is_a_token,
                new_revision,
                deprecated_gho_token_id,
                ..
            } => format!(
                "upgraded{{asset={asset_id},leg={},revision={new_revision},deprecated-gho={}}}",
                if *is_a_token { "collateral" } else { "debt" },
                deprecated_gho_token_id.map_or_else(|| "none".to_string(), |id| id.to_string())
            ),
            Self::ContractRevisionUpdated {
                market_id,
                contract_name,
                new_revision,
            } => format!(
                "contract-revision-updated{{market={market_id},name={contract_name},revision={new_revision}}}"
            ),
            Self::PoolDataProviderUpdated {
                market_id,
                old_address,
                new_address,
            } => format!(
                "pool-data-provider-updated{{market={market_id},old={},new={new_address}}}",
                addr(old_address)
            ),
            Self::ContractInserted {
                market_id,
                name,
                address,
                revision,
            } => format!(
                "contract-inserted{{market={market_id},name={name},address={address},revision={}}}",
                revision.map_or_else(|| "none".to_string(), |r| r.to_string())
            ),
            Self::ContractAddressChanged {
                market_id,
                name,
                old_address,
                new_address,
            } => format!(
                "contract-address-changed{{market={market_id},name={name},old={old_address},new={new_address}}}"
            ),
        }
    }
}

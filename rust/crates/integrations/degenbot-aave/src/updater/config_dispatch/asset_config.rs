//! The per-asset config-event handlers.
//!
//! The 8 sync (no-RPC) dispatchers — `ReserveDataUpdated`, `UserEModeSet`,
//! `ReserveUsedAsCollateral`, `PriceOracleUpdated`, `AssetSourceUpdated`,
//! `EModeCategoryAdded`, `EModeAssetCategoryChanged`,
//! `AssetCollateralInEModeChanged` — resolve ids against the chunk substrate
//! and emit their [`ConfigurationEffect`]s. The 2 async RPC resolutions:
//! [`resolve_reserve_initialized`] (the 3-token ERC20 metadata burst, the
//! EIP-1967 implementation-slot reads, the context-memoized
//! `ATOKEN_REVISION()`/`DEBT_TOKEN_REVISION()` reads and `getSourceOfAsset` — folded into ONE
//! transport pass when 2+ remain pending) and
//! [`resolve_collateral_configuration`] (`getConfiguration(address)` fetched
//! from the Pool at the event block, never trusted from the event fields).
//! The metadata burst rides the shared `degenbot_rpc::erc20` read.
//!
//! Surface table: `tests/fixtures/cassettes/wave4/UPGRADE-MAP.md` §(A)
//! row 9 (the per-`ReserveInitialized` EIP-1967 pair + the two impl revision
//! reads, NOT memoized away) — and the row-6 ordering this feeds: the loop
//! applies each emitted `ReserveInitialized` before a later config event in
//! the same transaction dispatches.

#![expect(clippy::doc_markdown)]

use crate::projection::ConfigurationEffect;
use alloy::primitives::{Address, U256};
use degenbot_rpc::chain_evidence::BlockReads;

use super::context::ChunkContext;
use super::{
    checksum, decode_address_return, encode_single_address_call, word0_to_u256, ConfigDispatchError,
};

/// The EIP-1967 implementation storage slot
/// (`bytes32(uint256(keccak256("eip1967.proxy.implementation")) - 1)`) —
/// read via `get_storage_at` to resolve a proxy's logic contract. Written as
/// the canonical hex and parsed at compile time (a typo fails the build
/// rather than silently mis-routing proxy resolution).
pub(crate) const EIP_1967_IMPLEMENTATION_SLOT: U256 = match U256::from_str_radix(
    "360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc",
    16,
) {
    Ok(v) => v,
    Err(_) => panic!("EIP-1967 implementation slot hex is a valid U256"),
};

// ── the 8 sync config handlers (no RPC — testable with in-memory DB) ───────

/// `ReserveDataUpdated` → [`ConfigurationEffect::ReserveDataUpdated`]. A missing
/// asset lifts to a `DecodeShape` error — the orchestrator pre-seeds assets
/// via `ReserveInitialized`.
pub(super) fn dispatch_reserve_data_updated(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3ReserveDataUpdatedEvent,
) -> Result<ConfigurationEffect, ConfigDispatchError> {
    let block_number = ctx.block_number();
    let market_id = ctx.market_id();
    let addr_str = checksum(&decoded.reserve);
    let asset = ctx
        .substrate_mut()
        .lookup_asset_row("underlying", &addr_str)?
        .ok_or_else(|| {
            ConfigDispatchError::DecodeShape(format!(
                "ReserveDataUpdated: no asset for underlying {addr_str} in market {market_id}"
            ))
        })?;
    Ok(ConfigurationEffect::ReserveDataUpdated {
        asset_id: asset.id,
        liquidity_rate: decoded.liquidity_rate,
        variable_borrow_rate: decoded.variable_borrow_rate,
        liquidity_index: decoded.liquidity_index,
        variable_borrow_index: decoded.variable_borrow_index,
        block_number,
    })
}

/// `UserEModeSet(user, categoryId)` → [`ConfigurationEffect::UserEModeSet`].
pub(super) fn dispatch_user_e_mode_set(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3UserEModeSetEvent,
) -> Result<ConfigurationEffect, ConfigDispatchError> {
    let addr_str = checksum(&decoded.user);
    let user_id = ctx.substrate_mut().user_id_or_create(&addr_str, 0)?;
    Ok(ConfigurationEffect::UserEModeSet {
        user_id,
        e_mode: i64::from(decoded.category_id),
    })
}

/// `ReserveUsedAsCollateral{Enabled,Disabled}` →
/// [`ConfigurationEffect::ReserveUsedAsCollateral`]. Mirrors
/// `_process_reserve_used_as_collateral_enabled_event` /
/// `_process_reserve_used_as_collateral_disabled_event`.
pub(super) fn dispatch_reserve_used_as_collateral(
    ctx: &mut ChunkContext<'_>,
    reserve: Address,
    user: Address,
    enabled: bool,
) -> Result<ConfigurationEffect, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let user_str = checksum(&user);
    let user_id = ctx.substrate_mut().user_id_or_create(&user_str, 0)?;
    let asset_str = checksum(&reserve);
    let asset = ctx
        .substrate_mut()
        .lookup_asset_row("underlying", &asset_str)?
        .ok_or_else(|| {
            ConfigDispatchError::DecodeShape(format!(
                "ReserveUsedAsCollateral: no asset for underlying {asset_str} in market {market_id}"
            ))
        })?;
    Ok(ConfigurationEffect::ReserveUsedAsCollateral {
        user_id,
        asset_id: asset.id,
        enabled,
    })
}

/// `PriceOracleUpdated(old, new)` → [`ConfigurationEffect::PriceOracleUpdated`].
/// An existing oracle lifts to an idempotent overwrite — the apply fn
/// handles the INSERT-not-UPDATE guard.
//
// The arm keeps the uniform `Result` shape the dispatch match relies on
// (`Some(dispatch_*(ctx, ev)?)`); this handler itself cannot fail.
#[expect(clippy::unnecessary_wraps)]
pub(super) fn dispatch_price_oracle_updated(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3ConfigAddressPairEvent,
) -> Result<ConfigurationEffect, ConfigDispatchError> {
    Ok(ConfigurationEffect::PriceOracleUpdated {
        market_id: ctx.market_id(),
        new_oracle_address: checksum(&decoded.new_address),
    })
}

/// `AssetSourceUpdated(asset, source)` → [`ConfigurationEffect::AssetSourceUpdated`].
pub(super) fn dispatch_asset_source_updated(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3AssetSourceUpdatedEvent,
) -> Result<Option<ConfigurationEffect>, ConfigDispatchError> {
    let asset_str = checksum(&decoded.asset);
    let Some(asset) = ctx
        .substrate_mut()
        .lookup_asset_row("underlying", &asset_str)?
    else {
        // Cold-boot tolerance: on a fresh-market cold-boot, the
        // `AssetSourceUpdated` for a not-yet-initialized reserve can precede
        // its `ReserveInitialized` within the same tx (mainnet block
        // 16496792: `AssetSourceUpdated` at logIdx 409, `ReserveInitialized`
        // at logIdx 413). Skip the event — the later `ReserveInitialized`
        // creates the asset + its `getSourceOfAsset(underlying)` RPC at
        // `block_number` reads the on-chain current source (which this
        // `AssetSourceUpdated` just set), recovering the `price_source`.
        // No data loss: a later `AssetSourceUpdated` for an asset that by
        // then exists dispatches normally (the unchanged path below).
        return Ok(None);
    };
    Ok(Some(ConfigurationEffect::AssetSourceUpdated {
        asset_id: asset.id,
        source_address: checksum(&decoded.source),
    }))
}

/// `EModeCategoryAdded(id, label, ltv, lt, bonus, oracle)` →
/// [`ConfigurationEffect::EModeCategoryAdded`]. Mirrors
/// `_process_e_mode_category_added_event`.
//
// Same uniform-arm rationale as `dispatch_price_oracle_updated`.
#[expect(clippy::unnecessary_wraps)]
pub(super) fn dispatch_e_mode_category_added(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3EModeCategoryAddedEvent,
) -> Result<ConfigurationEffect, ConfigDispatchError> {
    Ok(ConfigurationEffect::EModeCategoryAdded {
        market_id: ctx.market_id(),
        category_id: i64::from(decoded.category_id),
        ltv: decoded.ltv.to::<u64>(),
        liquidation_threshold: decoded.liquidation_threshold.to::<u64>(),
        liquidation_bonus: decoded.liquidation_bonus.to::<u64>(),
        // Parity: the Python gold's decode treats the indexed `oracle` event
        // field as a non-empty HexAddress *string*, whose string-truthiness is
        // ALWAYS true — even for the zero-address — so the gold stores
        // `get_checksum_address(Address::ZERO)` = the literal zero-address
        // checksum string when the event emitted `oracle = Address::ZERO`.
        // To honor byte-exact gold-parity on `aave_v3_emode_categories
        // .price_source` (the 16591070 parity gate criterion), unconditionally
        // checksum the decoded oracle (zero-oracle → the lowercase 42-char
        // zero-address string). The DB write layer receives
        // `Some("0x0000...0000")` and stores it as the column text.
        price_source: Some(checksum(&decoded.oracle)),
        label: decoded.label.clone(),
    })
}

/// `EModeAssetCategoryChanged(asset, old, new)` →
/// [`ConfigurationEffect::EModeAssetCategoryChanged`] (the older variant).
/// The `e_mode_category_id` is set unconditionally (`None` when
/// `new_category_id == 0`). NB: the apply fn takes `new_category_id: i64` +
/// zero means "clear".
pub(super) fn dispatch_e_mode_asset_category_changed(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3EModeAssetCategoryChangedEvent,
) -> Result<ConfigurationEffect, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let asset_str = checksum(&decoded.asset);
    let asset = ctx
        .substrate_mut()
        .lookup_asset_row("underlying", &asset_str)?
        .ok_or_else(|| {
            ConfigDispatchError::DecodeShape(format!(
            "EModeAssetCategoryChanged: no asset for underlying {asset_str} in market {market_id}"
        ))
        })?;
    Ok(ConfigurationEffect::EModeAssetCategoryChanged {
        asset_id: asset.id,
        new_category_id: i64::from(decoded.new_category_id),
    })
}

/// `AssetCollateralInEModeChanged(asset, category, is_collateral)` →
/// [`ConfigurationEffect::AssetCollateralInEModeChanged`] (the newer Aave 3.4+
/// variant).
pub(super) fn dispatch_asset_collateral_in_emode_changed(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3AssetCollateralInEModeChangedEvent,
) -> Result<ConfigurationEffect, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let asset_str = checksum(&decoded.asset);
    let asset = ctx
        .substrate_mut()
        .lookup_asset_row("underlying", &asset_str)?
    .ok_or_else(|| {
        ConfigDispatchError::DecodeShape(format!(
            "AssetCollateralInEModeChanged: no asset for underlying {asset_str} in market {market_id}"
        ))
    })?;
    Ok(ConfigurationEffect::AssetCollateralInEModeChanged {
        asset_id: asset.id,
        category_id: i64::from(decoded.category_id),
        is_collateral: decoded.collateral,
    })
}

// ── the 2 async RPC handlers ───────────────────────────────────────────────

/// `CollateralConfigurationChanged(asset)` →
/// [`ConfigurationEffect::CollateralConfigurationChanged`] with the RPC-fetched
/// `getConfiguration(address)` bitmap. The event's `ltv`/`threshold`/`bonus`
/// fields are NOT trusted — the full bitmap is RPC-fetched from the Pool
/// contract at the event's block (a pool upgrade can emit stale values).
/// The apply fn decodes the bitmap.
pub(super) async fn resolve_collateral_configuration(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3CollateralConfigurationChangedEvent,
) -> Result<ConfigurationEffect, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let pool_address = ctx.pool_address();
    let asset_str = checksum(&decoded.asset);
    let asset = ctx
        .substrate_mut()
        .lookup_asset_row("underlying", &asset_str)?
        .ok_or_else(|| {
            ConfigDispatchError::DecodeShape(format!(
                "CollateralConfigurationChanged: no asset for underlying {asset_str} in market {market_id}"
            ))
        })?;
    // getConfiguration(address) — 4-byte selector + 32-byte address.
    let calldata = encode_single_address_call("getConfiguration(address)", &decoded.asset);
    let ret = ctx.reads()?.call(pool_address, calldata).await?;
    let config_bitmap = word0_to_u256(&ret).ok_or_else(|| {
        ConfigDispatchError::DecodeShape(
            "CollateralConfigurationChanged: getConfiguration returned < 32 bytes".to_string(),
        )
    })?;
    Ok(ConfigurationEffect::CollateralConfigurationChanged {
        asset_id: asset.id,
        config_bitmap,
    })
}

/// `ReserveInitialized(asset, aToken, stableDebtToken, variableDebtToken, …)`
/// → [`ConfigurationEffect::ReserveInitialized`] with the seed's interned ids and
/// revisions plus the RPC-resolved price source.
///
/// The reserve's three ERC-20 rows, its asset row and both implementation
/// revisions were acquired and interned by the chunk's seed, so this resolution
/// names ids the database already holds and issues exactly one read: the
/// reserve's `getSourceOfAsset(asset)` on the PRICE_ORACLE contract as the
/// resolution read position has it (a same-chunk `PriceOracleUpdated` is
/// visible). The price source reaches the row through the effect's delta; the
/// chunk's write transaction is the only place it is persisted.
///
/// # Errors
///
/// A seed miss refuses with [`crate::updater::run::seed::IncompleteChunkSeed`];
/// a reverting or malformed `getSourceOfAsset` read refuses the chunk.
pub(super) async fn resolve_reserve_initialized(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3ReserveInitializedEvent,
    oracle_address: Address,
) -> Result<ConfigurationEffect, ConfigDispatchError> {
    let reserve = ctx.reserve(&checksum(&decoded.asset))?;
    let ret = ctx
        .reads()?
        .call(
            oracle_address,
            encode_single_address_call("getSourceOfAsset(address)", &decoded.asset),
        )
        .await?;
    let price_source = decode_address_return(&ret);
    let gho_link_token_id = ctx
        .gho_asset()
        .filter(|gho| {
            gho.gho_token_address
                .eq_ignore_ascii_case(&checksum(&decoded.asset))
        })
        .map(|gho| gho.id);
    Ok(ConfigurationEffect::ReserveInitialized {
        market_id: ctx.market_id(),
        underlying_asset_id: reserve.underlying_id,
        a_token_id: reserve.a_token_id,
        a_token_revision: reserve.facts.a_token_revision,
        v_token_id: reserve.v_token_id,
        v_token_revision: reserve.facts.v_token_revision,
        price_source,
        gho_link_token_id,
    })
}

/// Read the EIP-1967 implementation slot for `proxy` at the reads' exact block
/// and decode the address.
///
/// # Errors
///
/// A transport failure or unserved exact read refuses the acquisition; a slot
/// word the runtime cannot decode cannot occur (the read is a 32-byte word).
pub(crate) async fn read_implementation_slot(
    reads: &BlockReads<'_>,
    proxy: &Address,
) -> Result<Address, ConfigDispatchError> {
    let slot = reads
        .storage_at(*proxy, EIP_1967_IMPLEMENTATION_SLOT)
        .await?;
    let word = slot.to_be_bytes::<32>();
    Ok(Address::from_slice(&word[12..]))
}

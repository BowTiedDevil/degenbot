//! The per-asset config-event handlers.
//!
//! The 8 sync (no-RPC) dispatchers — `ReserveDataUpdated`, `UserEModeSet`,
//! `ReserveUsedAsCollateral`, `PriceOracleUpdated`, `AssetSourceUpdated`,
//! `EModeCategoryAdded`, `EModeAssetCategoryChanged`,
//! `AssetCollateralInEModeChanged` — resolve ids against the chunk substrate
//! and emit their [`AaveChunkEvent`]s. The 2 async RPC resolutions:
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

use crate::run::AaveChunkEvent;
use alloy::primitives::{Address, U256};
use degenbot_db::DegenbotDb;
use degenbot_rpc::erc20;
use degenbot_rpc::provider::AlloyProvider;

use super::context::{BatchRead, BatchReadResult, ChunkContext};
use super::{
    checksum, decode_address_return, discount_to_i64, encode_single_address_call, word0_to_u256,
    ConfigDispatchError,
};

/// The EIP-1967 implementation storage slot (the canonically-fixed
/// `bytes32(uint256(keccak256("eip1967.proxy.implementation")) - 1)`).
/// The ERC-1967 implementation slot — read via
/// `get_storage_at` to resolve a proxy's logic contract. Written as the
/// canonical hex and parsed at compile time (a typo fails the build rather
/// than silently mis-routing proxy resolution).
pub(super) const EIP_1967_IMPLEMENTATION_SLOT: U256 = match U256::from_str_radix(
    "360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc",
    16,
) {
    Ok(v) => v,
    Err(_) => panic!("EIP-1967 implementation slot hex is a valid U256"),
};

// ── the 8 sync config handlers (no RPC — testable with in-memory DB) ───────

/// `ReserveDataUpdated` → [`AaveChunkEvent::ReserveDataUpdated`]. Mirrors
/// `event_handlers._process_reserve_data_update_event` (the
/// `assert asset_in_db is not None` lifts to a `DecodeShape` error — the
/// orchestrator pre-seeds assets via `ReserveInitialized`).
pub(super) fn dispatch_reserve_data_updated(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3ReserveDataUpdatedEvent,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let block_number = ctx.block_number();
    let conn = ctx.conn();
    let addr_str = checksum(&decoded.reserve);
    let asset = ctx
        .substrate_mut()
        .lookup_asset_row(conn, market_id, "underlying", &addr_str)?
        .ok_or_else(|| {
            ConfigDispatchError::DecodeShape(format!(
                "ReserveDataUpdated: no asset for underlying {addr_str} in market {market_id}"
            ))
        })?;
    Ok(AaveChunkEvent::ReserveDataUpdated {
        asset_id: asset.id,
        liquidity_rate: decoded.liquidity_rate,
        variable_borrow_rate: decoded.variable_borrow_rate,
        liquidity_index: decoded.liquidity_index,
        variable_borrow_index: decoded.variable_borrow_index,
        block_number,
    })
}

/// `UserEModeSet(user, categoryId)` → [`AaveChunkEvent::UserEModeSet`].
/// Mirrors `event_handlers._process_user_e_mode_set_event`.
pub(super) fn dispatch_user_e_mode_set(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3UserEModeSetEvent,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let conn = ctx.conn();
    let addr_str = checksum(&decoded.user);
    let user_id = ctx
        .substrate_mut()
        .user_id_or_create(conn, market_id, &addr_str, 0)?;
    Ok(AaveChunkEvent::UserEModeSet {
        user_id,
        e_mode: i64::from(decoded.category_id),
    })
}

/// `ReserveUsedAsCollateral{Enabled,Disabled}` →
/// [`AaveChunkEvent::ReserveUsedAsCollateral`]. Mirrors
/// `_process_reserve_used_as_collateral_enabled_event` /
/// `_process_reserve_used_as_collateral_disabled_event`.
pub(super) fn dispatch_reserve_used_as_collateral(
    ctx: &mut ChunkContext<'_>,
    reserve: Address,
    user: Address,
    enabled: bool,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let conn = ctx.conn();
    let user_str = checksum(&user);
    let user_id = ctx
        .substrate_mut()
        .user_id_or_create(conn, market_id, &user_str, 0)?;
    let asset_str = checksum(&reserve);
    let asset = ctx
        .substrate_mut()
        .lookup_asset_row(conn, market_id, "underlying", &asset_str)?
        .ok_or_else(|| {
            ConfigDispatchError::DecodeShape(format!(
                "ReserveUsedAsCollateral: no asset for underlying {asset_str} in market {market_id}"
            ))
        })?;
    Ok(AaveChunkEvent::ReserveUsedAsCollateral {
        user_id,
        asset_id: asset.id,
        enabled,
    })
}

/// `PriceOracleUpdated(old, new)` → [`AaveChunkEvent::PriceOracleUpdated`].
/// Mirrors `_process_price_oracle_updated_event` (the `assert existing_oracle
/// is None` lifts to an idempotent overwrite — the apply fn handles the
/// INSERT-not-UPDATE guard).
//
// The arm keeps the uniform `Result` shape the dispatch match relies on
// (`Some(dispatch_*(ctx, ev)?)`); this handler itself cannot fail.
#[expect(clippy::unnecessary_wraps)]
pub(super) fn dispatch_price_oracle_updated(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3ConfigAddressPairEvent,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    Ok(AaveChunkEvent::PriceOracleUpdated {
        market_id: ctx.market_id(),
        new_oracle_address: checksum(&decoded.new_address),
    })
}

/// `AssetSourceUpdated(asset, source)` → [`AaveChunkEvent::AssetSourceUpdated`].
/// Mirrors `_process_asset_source_updated_event`.
pub(super) fn dispatch_asset_source_updated(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3AssetSourceUpdatedEvent,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let conn = ctx.conn();
    let asset_str = checksum(&decoded.asset);
    let Some(asset) =
        ctx.substrate_mut()
            .lookup_asset_row(conn, market_id, "underlying", &asset_str)?
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
    Ok(Some(AaveChunkEvent::AssetSourceUpdated {
        asset_id: asset.id,
        source_address: checksum(&decoded.source),
    }))
}

/// `EModeCategoryAdded(id, label, ltv, lt, bonus, oracle)` →
/// [`AaveChunkEvent::EModeCategoryAdded`]. Mirrors
/// `_process_e_mode_category_added_event`.
//
// Same uniform-arm rationale as `dispatch_price_oracle_updated`.
#[expect(clippy::unnecessary_wraps)]
pub(super) fn dispatch_e_mode_category_added(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3EModeCategoryAddedEvent,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    Ok(AaveChunkEvent::EModeCategoryAdded {
        market_id: ctx.market_id(),
        category_id: i64::from(decoded.category_id),
        ltv: decoded.ltv.to::<u64>(),
        liquidation_threshold: decoded.liquidation_threshold.to::<u64>(),
        liquidation_bonus: decoded.liquidation_bonus.to::<u64>(),
        // Parity match: Python's `_process_e_mode_category_added_event`
        // (event_handlers.py:269,277) does
        //   `price_source = get_checksum_address(oracle) if oracle else None`
        // but web3.py decodes the indexed `oracle` event field as a non-empty
        // HexAddress *string*, whose string-truthiness is ALWAYS true — even
        // for the zero-address — so Python stores
        // `get_checksum_address(Address::ZERO)` = the literal zero-address
        // checksum string when the event emitted `oracle = Address::ZERO`.
        // To honor byte-exact gold-parity on `aave_v3_emode_categories
        // .price_source` (the 16591070 parity gate criterion), unconditionally
        // checksum the decoded oracle (zero-oracle → the lowercase 42-char
        // zero-address string). The DB write layer receives
        // `Some("0x0000...0000")` and stores it as the column text — matching
        // Python gold's stored representation.
        price_source: Some(checksum(&decoded.oracle)),
        label: decoded.label.clone(),
    })
}

/// `EModeAssetCategoryChanged(asset, old, new)` →
/// [`AaveChunkEvent::EModeAssetCategoryChanged`] (the older variant).
/// The `e_mode_category_id` is set unconditionally (`None` when
/// `new_category_id == 0`). NB: the apply fn takes `new_category_id: i64` +
/// zero means "clear".
pub(super) fn dispatch_e_mode_asset_category_changed(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3EModeAssetCategoryChangedEvent,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let conn = ctx.conn();
    let asset_str = checksum(&decoded.asset);
    let asset = ctx
        .substrate_mut()
        .lookup_asset_row(conn, market_id, "underlying", &asset_str)?
        .ok_or_else(|| {
            ConfigDispatchError::DecodeShape(format!(
            "EModeAssetCategoryChanged: no asset for underlying {asset_str} in market {market_id}"
        ))
        })?;
    Ok(AaveChunkEvent::EModeAssetCategoryChanged {
        asset_id: asset.id,
        new_category_id: i64::from(decoded.new_category_id),
    })
}

/// `AssetCollateralInEModeChanged(asset, category, is_collateral)` →
/// [`AaveChunkEvent::AssetCollateralInEModeChanged`] (the newer Aave 3.4+
/// variant).
pub(super) fn dispatch_asset_collateral_in_emode_changed(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3AssetCollateralInEModeChangedEvent,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let conn = ctx.conn();
    let asset_str = checksum(&decoded.asset);
    let asset = ctx
        .substrate_mut()
        .lookup_asset_row(conn, market_id, "underlying", &asset_str)?
    .ok_or_else(|| {
        ConfigDispatchError::DecodeShape(format!(
            "AssetCollateralInEModeChanged: no asset for underlying {asset_str} in market {market_id}"
        ))
    })?;
    Ok(AaveChunkEvent::AssetCollateralInEModeChanged {
        asset_id: asset.id,
        category_id: i64::from(decoded.category_id),
        is_collateral: decoded.collateral,
    })
}

// ── the 2 async RPC handlers ───────────────────────────────────────────────

/// `CollateralConfigurationChanged(asset)` →
/// [`AaveChunkEvent::CollateralConfigurationChanged`] with the RPC-fetched
/// `getConfiguration(address)` bitmap. Mirrors
/// `_process_collateral_configuration_changed_event`. The Python does NOT
/// trust the event's `ltv`/`threshold`/`bonus` fields — it RPC-fetches the
/// full bitmap from the Pool contract at the event's block (a pool upgrade
/// can emit stale values). The apply fn decodes the bitmap.
pub(super) async fn resolve_collateral_configuration(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3CollateralConfigurationChangedEvent,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let block_number = ctx.block_number();
    let pool_address = ctx.pool_address();
    let conn = ctx.conn();
    let asset_str = checksum(&decoded.asset);
    let asset = ctx
        .substrate_mut()
        .lookup_asset_row(conn, market_id, "underlying", &asset_str)?
        .ok_or_else(|| {
            ConfigDispatchError::DecodeShape(format!(
                "CollateralConfigurationChanged: no asset for underlying {asset_str} in market {market_id}"
            ))
        })?;
    // getConfiguration(address) — 4-byte selector + 32-byte address.
    let calldata = encode_single_address_call("getConfiguration(address)", &decoded.asset);
    let ret = ctx
        .provider()
        .eth_call(&pool_address, calldata, Some(block_number))
        .await?;
    let config_bitmap = word0_to_u256(&ret).ok_or_else(|| {
        ConfigDispatchError::DecodeShape(
            "CollateralConfigurationChanged: getConfiguration returned < 32 bytes".to_string(),
        )
    })?;
    Ok(AaveChunkEvent::CollateralConfigurationChanged {
        asset_id: asset.id,
        config_bitmap,
    })
}

/// `ReserveInitialized(asset, aToken, stableDebtToken, variableDebtToken, …)`
/// → [`AaveChunkEvent::ReserveInitialized`] with the RPC-resolved revisions +
/// price source. Mirrors `event_handlers._process_asset_initialization_event`:
/// the 3 erc20 tokens (asset/aToken/vToken) are `get_or_create`d with their
/// RPC-fetched metadata, and per EIP-1967 the aToken/vToken implementation
/// slot → `ATOKEN_REVISION()` / `DEBT_TOKEN_REVISION()` `eth_call` +
/// `getSourceOfAsset(address)` `eth_call` on the PRICE_ORACLE contract.
///
/// The metadata step ports `erc20_utils._fetch_erc20_token_metadata`: the
/// name/symbol/decimals tuple rides the ctx's injected provider (ADR-068 D5 —
/// the core never builds a transport), ONE Multicall3 pass for the three
/// tokens, and each field is `None` when its reads fail (the reference's
/// per-field tolerance — an unrecorded or reverting read is not a chunk
/// fault). The `get_or_create_erc20_token_on_conn` contract carries the
/// idempotency: a row that already holds metadata keeps it — only its null
/// cells are backfilled — so a re-dispatch never overwrites a resolved value.
pub(super) async fn resolve_reserve_initialized(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3ReserveInitializedEvent,
    oracle_address: Address,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let chain_id = ctx.chain_id();
    let block_number = ctx.block_number();
    let conn = ctx.conn();
    // The GHO asset through the ctx-owned always-fresh accessor, for the
    // GHO-vToken-FK link below: the fresh read mirrors Python's live ORM read
    // of `gho_asset.token.address` (a cache hit until an apply marks it dirty).
    let gho_asset = ctx.gho_asset()?;
    // 1. get_or_create the 3 erc20 token rows (underlying / aToken / vToken),
    //    RPC-fetching the name/symbol/decimals from the chain (the
    //    standalone-Rust-core constraint — Rust owns the loop, no PyO3 FFI for
    //    metadata). The 3 tokens' fields ride ONE Multicall3 pass through the
    //    shared `degenbot_rpc::erc20` read (same block, mutually independent
    //    reads; the per-field selector fallbacks are part of the batch — both
    //    spellings are issued and the decision consumes the batched results
    //    in the sequential logic's exact order). Each field lands on the DB's
    //    nullable column; `decimals` keeps the reference's `i64` width.
    let underlying_str = checksum(&decoded.asset);
    let a_token_str = checksum(&decoded.a_token);
    let v_token_str = checksum(&decoded.variable_debt_token);
    let metadata = fold_metadata_rows(
        erc20::read_batch(
            ctx.provider(),
            &[decoded.asset, decoded.a_token, decoded.variable_debt_token],
            erc20::MetadataShape::LowerUpperStopOnCallError,
            Some(block_number),
        )
        .await,
    )?;
    let [(underlying_name, underlying_symbol, underlying_decimals), (a_name, a_symbol, a_decimals), (v_name, v_symbol, v_decimals)] =
        metadata;
    let underlying_asset_id = DegenbotDb::get_or_create_erc20_token_on_conn(
        conn,
        chain_id,
        &underlying_str,
        underlying_name.as_deref(),
        underlying_symbol.as_deref(),
        underlying_decimals,
    )?;
    let a_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
        conn,
        chain_id,
        &a_token_str,
        a_name.as_deref(),
        a_symbol.as_deref(),
        a_decimals,
    )?;
    let v_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
        conn,
        chain_id,
        &v_token_str,
        v_name.as_deref(),
        v_symbol.as_deref(),
        v_decimals,
    )?;

    // 2. EIP-1967: resolve the aToken + vToken implementation addresses.
    //    `get_storage_at` has no Multicall3 shape — these two stay sequential.
    let atoken_impl =
        read_implementation_slot(ctx.provider(), &decoded.a_token, block_number).await?;
    let vtoken_impl =
        read_implementation_slot(ctx.provider(), &decoded.variable_debt_token, block_number)
            .await?;

    // 3. + 4. ATOKEN_REVISION() / DEBT_TOKEN_REVISION() on the implementations
    //    + getSourceOfAsset(address) on the PRICE_ORACLE — three
    //    mutually-independent same-block reads folded into ONE transport pass
    //    when 2+ remain after the memo (a lone remaining call keeps the exact
    //    sequential shape; see `eth_calls_batched_or_direct`). The context owns
    //    the memo keying for the two revision slots.
    let pending = [
        BatchRead::Revision {
            target: atoken_impl,
            sig: "ATOKEN_REVISION()",
        },
        BatchRead::Revision {
            target: vtoken_impl,
            sig: "DEBT_TOKEN_REVISION()",
        },
        BatchRead::Plain {
            target: oracle_address,
            calldata: encode_single_address_call("getSourceOfAsset(address)", &decoded.asset),
        },
    ];
    let results = ctx.read_revision_batch(&pending).await?;
    let [BatchReadResult::Revision(a_token_revision), BatchReadResult::Revision(v_token_revision), BatchReadResult::Plain(source_return)] =
        results.as_slice()
    else {
        return Err(ConfigDispatchError::DecodeShape(
            "ReserveInitialized: the batched read returned an unexpected slot shape".to_string(),
        ));
    };
    let price_source = decode_address_return(source_return);

    Ok(AaveChunkEvent::ReserveInitialized {
        market_id,
        underlying_asset_id,
        a_token_id,
        a_token_revision: discount_to_i64(*a_token_revision),
        v_token_id,
        v_token_revision: discount_to_i64(*v_token_revision),
        price_source,
        // 5. GHO-vToken-FK link: when the asset IS the GHO token, the GHO token
        //    row's `v_token_id` points at this vToken. The FK is
        //    the precondition for the discount-event emitter guard (which compares
        //    against `gho_asset.v_token_address`, resolved via the FK).
        gho_link_token_id: gho_asset
            .as_ref()
            .filter(|g| g.gho_token_address.as_deref() == Some(underlying_str.as_str()))
            .map(|g| g.id),
    })
}

/// A token's stored metadata fields — `(name, symbol, decimals)` — each
/// absent when the token's call yielded no decodable value.
type MetadataFields = (Option<String>, Option<String>, Option<i64>);

/// The `[underlying, aToken, vToken]` metadata triple the dispatch consumes.
type MetadataTriple = [MetadataFields; 3];

/// Fold the metadata burst's per-token rows — one [`MetadataFields`] per
/// input address — into the [`MetadataTriple`] the dispatch consumes.
/// `erc20::read_batch` contracts one row per input address; any other count
/// is an explicit [`ConfigDispatchError`], never three null records over the
/// token rows.
fn fold_metadata_rows(
    rows: Vec<erc20::TokenMetadata>,
) -> Result<MetadataTriple, ConfigDispatchError> {
    rows.into_iter()
        .map(|meta| (meta.name, meta.symbol, meta.decimals.map(u64::cast_signed)))
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| {
            ConfigDispatchError::DecodeShape(
                "ReserveInitialized: expected exactly three metadata rows".to_string(),
            )
        })
}

/// Read the EIP-1967 implementation slot for `proxy` at `block_number` +
/// decode the address.
async fn read_implementation_slot(
    provider: &AlloyProvider,
    proxy: &Address,
    block_number: u64,
) -> Result<Address, ConfigDispatchError> {
    let slot = provider
        .get_storage_at(proxy, EIP_1967_IMPLEMENTATION_SLOT, Some(block_number))
        .await?;
    // The slot is a 32-byte word; the address is the low 20 bytes.
    Ok(Address::from_slice(&slot.as_slice()[12..]))
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The metadata burst's fold accepts exactly the three rows the batched
    /// read contracts — one per token — and fails loudly on any other count,
    /// never emitting three null records over the token rows.
    #[test]
    fn fold_metadata_rows_requires_exactly_three_rows() {
        let rows = vec![
            erc20::TokenMetadata {
                name: Some("Dai".into()),
                symbol: Some("DAI".into()),
                decimals: Some(18),
            },
            erc20::TokenMetadata::default(),
            erc20::TokenMetadata {
                decimals: Some(6),
                ..Default::default()
            },
        ];
        let folded = fold_metadata_rows(rows).unwrap();
        assert_eq!(folded[0].0.as_deref(), Some("Dai"));
        assert_eq!(folded[0].2, Some(18));
        assert_eq!(folded[1].0, None);
        assert_eq!(folded[2].2, Some(6));
        assert!(fold_metadata_rows(Vec::new()).is_err());
        assert!(fold_metadata_rows(vec![erc20::TokenMetadata::default(); 2]).is_err());
        assert!(fold_metadata_rows(vec![erc20::TokenMetadata::default(); 4]).is_err());
    }
}

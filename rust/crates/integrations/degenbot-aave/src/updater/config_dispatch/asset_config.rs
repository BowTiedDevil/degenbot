//! The per-asset config-event handlers.
//!
//! The 8 sync (no-RPC) dispatchers — `ReserveDataUpdated`, `UserEModeSet`,
//! `ReserveUsedAsCollateral`, `PriceOracleUpdated`, `AssetSourceUpdated`,
//! `EModeCategoryAdded`, `EModeAssetCategoryChanged`,
//! `AssetCollateralInEModeChanged` — resolve ids against the chunk substrate
//! and emit their [`AaveChunkEvent`]s. The 2 async RPC resolutions:
//! [`resolve_reserve_initialized`] (the 3-token ERC20 metadata burst, the
//! EIP-1967 implementation-slot reads, the memo-carrying
//! `ATOKEN_REVISION()`/`DEBT_TOKEN_REVISION()` reads and `getSourceOfAsset` — folded into ONE
//! transport pass when 2+ remain pending) and
//! [`resolve_collateral_configuration`] (`getConfiguration(address)` fetched
//! from the Pool at the event block, never trusted from the event fields),
//! plus the ERC20 metadata fetch family they ride.
//!
//! Surface table: `tests/fixtures/cassettes/wave4/UPGRADE-MAP.md` §(A)
//! row 9 (the per-`ReserveInitialized` EIP-1967 pair + the two impl revision
//! reads, NOT memoized away) — and the row-6 ordering this feeds: the loop
//! applies each emitted `ReserveInitialized` before a later config event in
//! the same transaction dispatches.

#![expect(clippy::missing_errors_doc, clippy::doc_markdown)]

use crate::run::AaveChunkEvent;
use crate::updater::run::substrate::ChunkSubstrate;
use alloy::primitives::{Address, Bytes, U256};
use degenbot_db::aave::AaveGhoAsset;
use degenbot_db::DegenbotDb;
use degenbot_rpc::multicall3::{multicall3_batch, MulticallResult};
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::Connection;

use super::context::ChunkContext;
use super::{
    checksum, decimals_from_batch, decode_address_return, decode_string_return, discount_to_i64,
    encode_no_arg_call, encode_single_address_call, eth_calls_batched_or_direct, revision_selector,
    string_field_from_batch, word0_to_u256, ConfigDispatchError,
};

/// The EIP-1967 implementation storage slot (the canonically-fixed
/// `bytes32(uint256(keccak256("eip1967.proxy.implementation")) - 1)`).
/// Mirrors the Python `ERC_1967_IMPLEMENTATION_SLOT` — read via
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
pub fn dispatch_reserve_data_updated(
    market_id: i64,
    block_number: u64,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3ReserveDataUpdatedEvent,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let addr_str = checksum(&decoded.reserve);
    let asset = substrate
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
pub fn dispatch_user_e_mode_set(
    market_id: i64,
    block_number: u64,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3UserEModeSetEvent,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let addr_str = checksum(&decoded.user);
    let user_id = substrate.user_id_or_create(conn, market_id, &addr_str, 0)?;
    let _ = block_number;
    Ok(AaveChunkEvent::UserEModeSet {
        user_id,
        e_mode: i64::from(decoded.category_id),
    })
}

/// `ReserveUsedAsCollateral{Enabled,Disabled}` →
/// [`AaveChunkEvent::ReserveUsedAsCollateral`]. Mirrors
/// `_process_reserve_used_as_collateral_enabled_event` /
/// `_process_reserve_used_as_collateral_disabled_event`.
pub fn dispatch_reserve_used_as_collateral(
    market_id: i64,
    reserve: Address,
    user: Address,
    enabled: bool,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let user_str = checksum(&user);
    let user_id = substrate.user_id_or_create(conn, market_id, &user_str, 0)?;
    let asset_str = checksum(&reserve);
    let asset = substrate
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
pub fn dispatch_price_oracle_updated(
    market_id: i64,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3ConfigAddressPairEvent,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    Ok(AaveChunkEvent::PriceOracleUpdated {
        market_id,
        new_oracle_address: checksum(&decoded.new_address),
    })
}

/// `AssetSourceUpdated(asset, source)` → [`AaveChunkEvent::AssetSourceUpdated`].
/// Mirrors `_process_asset_source_updated_event`.
pub fn dispatch_asset_source_updated(
    market_id: i64,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3AssetSourceUpdatedEvent,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    let asset_str = checksum(&decoded.asset);
    let Some(asset) = substrate.lookup_asset_row(conn, market_id, "underlying", &asset_str)? else {
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
pub fn dispatch_e_mode_category_added(
    market_id: i64,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3EModeCategoryAddedEvent,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    Ok(AaveChunkEvent::EModeCategoryAdded {
        market_id,
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
/// Mirrors the Python's unconditionally-set `e_mode_category_id` (`None` when
/// `new_category_id == 0`). NB: the apply fn takes `new_category_id: i64` +
/// zero means "clear".
pub fn dispatch_e_mode_asset_category_changed(
    market_id: i64,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3EModeAssetCategoryChangedEvent,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let asset_str = checksum(&decoded.asset);
    let asset = substrate
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
pub fn dispatch_asset_collateral_in_emode_changed(
    market_id: i64,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3AssetCollateralInEModeChangedEvent,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let asset_str = checksum(&decoded.asset);
    let asset = substrate.lookup_asset_row(conn, market_id, "underlying", &asset_str)?
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
pub async fn resolve_collateral_configuration(
    provider: &AlloyProvider,
    pool_address: Address,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3CollateralConfigurationChangedEvent,
    market_id: i64,
    block_number: u64,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let asset_str = checksum(&decoded.asset);
    let asset = substrate.lookup_asset_row(conn, market_id, "underlying", &asset_str)?
    .ok_or_else(|| {
        ConfigDispatchError::DecodeShape(format!(
            "CollateralConfigurationChanged: no asset for underlying {asset_str} in market {market_id}"
        ))
    })?;
    // getConfiguration(address) — 4-byte selector + 32-byte address.
    let calldata = encode_single_address_call("getConfiguration(address)", &decoded.asset);
    let ret = provider
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
/// per EIP-1967, the aToken/vToken implementation slot → `ATOKEN_REVISION()`
/// / `DEBT_TOKEN_REVISION()` `eth_call` + `getSourceOfAsset(address)` `eth_call`
/// on the PRICE_ORACLE contract.
///
/// NB: the 3 erc20 tokens (asset/aToken/vToken) are `get_or_create`d with
/// `None` metadata (name/symbol/decimals) — the Python RPC-fetches these via
/// `_fetch_erc20_token_metadata`, which is a parity-gate gap flagged for -2b / a
/// follow-up (the standalone Rust consumer needs the metadata fetch too; the
/// substrate `get_or_create_erc20_token_on_conn` takes metadata as a caller
/// param, so this is consistent with the existing contract — the gap is
/// isolated to this fn's token resolution).
pub(crate) async fn resolve_reserve_initialized(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3ReserveInitializedEvent,
    oracle_address: Address,
    gho_asset: Option<&AaveGhoAsset>,
    block_number: u64,
    conn: &Connection,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    // 1. get_or_create the 3 erc20 token rows (underlying / aToken / vToken),
    //    RPC-fetching the name/symbol/decimals from the chain (the
    //    standalone-Rust-core constraint — Rust owns the loop, no PyO3 FFI for
    //    metadata). Port of `erc20_utils._fetch_erc20_token_metadata`. The 3
    //    tokens' fields ride ONE Multicall3 pass (same block, mutually
    //    independent reads; the per-field selector fallbacks are part of the
    //    batch — both spellings are issued and the decision consumes the
    //    batched results in the sequential logic's exact order).
    let underlying_str = checksum(&decoded.asset);
    let a_token_str = checksum(&decoded.a_token);
    let v_token_str = checksum(&decoded.variable_debt_token);
    let metadata = fetch_erc20_metadata_batched(
        ctx.provider(),
        [
            &decoded.asset,
            &decoded.a_token,
            &decoded.variable_debt_token,
        ],
        block_number,
    )
    .await;
    let [(underlying_name, underlying_symbol, underlying_decimals), (a_name, a_symbol, a_decimals), (v_name, v_symbol, v_decimals)] =
        metadata;
    let underlying_asset_id = DegenbotDb::get_or_create_erc20_token_on_conn(
        conn,
        ctx.chain_id(),
        &underlying_str,
        underlying_name.as_deref(),
        underlying_symbol.as_deref(),
        underlying_decimals,
    )?;
    let a_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
        conn,
        ctx.chain_id(),
        &a_token_str,
        a_name.as_deref(),
        a_symbol.as_deref(),
        a_decimals,
    )?;
    let v_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
        conn,
        ctx.chain_id(),
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
    //    sequential shape; see `eth_calls_batched_or_direct`).
    let (a_token_revision, v_token_revision, price_source) =
        read_reserve_init_revisions_and_source(
            ctx,
            atoken_impl,
            vtoken_impl,
            &decoded.asset,
            &oracle_address,
            block_number,
        )
        .await?;

    Ok(AaveChunkEvent::ReserveInitialized {
        market_id: ctx.market_id(),
        underlying_asset_id,
        a_token_id,
        a_token_revision: discount_to_i64(a_token_revision),
        v_token_id,
        v_token_revision: discount_to_i64(v_token_revision),
        price_source,
        // 5. GHO-vToken-FK link (divergence #8): mirror the Python's
        //    `if asset_address == gho_asset.token.address: gho_token_entry
        //    .v_token_id = v_token.id` (event_handlers.py:689-698). The FK is
        //    the precondition for the discount-event emitter guard (which compares
        //    against `gho_asset.v_token_address`, resolved via the FK).
        gho_link_token_id: gho_asset
            .filter(|g| g.gho_token_address.as_deref() == Some(underlying_str.as_str()))
            .map(|g| g.id),
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

/// Which pending same-block read one `ReserveInitialized` dispatch slot
/// feeds — the two memo-carrying revision reads + the always-pending price
/// source read.
#[derive(Debug)]
enum PendingRead {
    /// `ATOKEN_REVISION()` on the aToken implementation — carries the memo
    /// key to record the answered value under.
    ATokenRevision((Address, [u8; 4], u64)),
    /// `DEBT_TOKEN_REVISION()` on the vToken implementation — same key shape.
    VTokenRevision((Address, [u8; 4], u64)),
    /// `getSourceOfAsset(address)` on the PRICE_ORACLE (not a revision — no
    /// memo; it is a distinct contract read at the same block).
    SourceOfAsset,
}

/// The `ReserveInitialized` dispatch's step-3+4 reads —
/// `ATOKEN_REVISION()` / `DEBT_TOKEN_REVISION()` on the two implementations +
/// `getSourceOfAsset(underlying)` on the oracle — folded into ONE transport
/// pass when 2+ remain after the memo (the price-source read is always
/// pending, so the batch runs whenever either revision misses; when both
/// revisions are memo hits the lone source call keeps the exact sequential
/// shape). Revision results are recorded into `memo` under their
/// `(implementation, selector, block)` keys.
///
/// # Errors
///
/// Propagates the batch's error contract (see
/// [`eth_calls_batched_or_direct`]).
async fn read_reserve_init_revisions_and_source(
    ctx: &mut ChunkContext<'_>,
    atoken_impl: Address,
    vtoken_impl: Address,
    underlying: &Address,
    oracle_address: &Address,
    block_number: u64,
) -> Result<(U256, U256, Option<String>), ConfigDispatchError> {
    let a_selector = revision_selector("ATOKEN_REVISION()");
    let v_selector = revision_selector("DEBT_TOKEN_REVISION()");
    let mut a_revision = ctx.memo().get(&atoken_impl, a_selector, block_number);
    let mut v_revision = ctx.memo().get(&vtoken_impl, v_selector, block_number);

    let mut pending: Vec<(Address, Bytes, PendingRead)> = Vec::new();
    if a_revision.is_none() {
        pending.push((
            atoken_impl,
            encode_no_arg_call("ATOKEN_REVISION()"),
            PendingRead::ATokenRevision((atoken_impl, a_selector, block_number)),
        ));
    }
    if v_revision.is_none() {
        pending.push((
            vtoken_impl,
            encode_no_arg_call("DEBT_TOKEN_REVISION()"),
            PendingRead::VTokenRevision((vtoken_impl, v_selector, block_number)),
        ));
    }
    let source_calldata = encode_single_address_call("getSourceOfAsset(address)", underlying);
    pending.push((*oracle_address, source_calldata, PendingRead::SourceOfAsset));

    let pairs: Vec<(Address, Bytes)> = pending.iter().map(|(t, d, _)| (*t, d.clone())).collect();
    let rets = eth_calls_batched_or_direct(ctx.provider(), &pairs, block_number).await?;
    let mut source = None;
    for ((_, _, slot), ret) in pending.iter().zip(rets) {
        match slot {
            PendingRead::ATokenRevision((t, s, b)) => {
                let v = word0_to_u256(&ret).unwrap_or(U256::ZERO);
                ctx.memo().insert(t, *s, *b, v);
                a_revision = Some(v);
            }
            PendingRead::VTokenRevision((t, s, b)) => {
                let v = word0_to_u256(&ret).unwrap_or(U256::ZERO);
                ctx.memo().insert(t, *s, *b, v);
                v_revision = Some(v);
            }
            PendingRead::SourceOfAsset => source = decode_address_return(&ret),
        }
    }
    // Both revision slots are filled by construction — a memo hit or the
    // batched read above (the source slot is always pending, so the batch
    // always runs). The guard keeps the invariant loud rather than assumed.
    match (a_revision, v_revision) {
        (Some(a), Some(v)) => Ok((a, v, source)),
        _ => Err(ConfigDispatchError::DecodeShape(
            "ReserveInitialized: a revision slot was left unfilled after the batched reads"
                .to_string(),
        )),
    }
}

// ── ERC20 metadata fetch (name/symbol/decimals) — the dynamic-string ABI decode ──

/// RPC-fetch an ERC20 token's `name()`/`symbol()` (a `string` return — the
/// dynamic ABI: 32-byte offset + 32-byte length + data padded to 32-byte
/// multiples). Falls back to the bytes32-decode + null-strip (older tokens —
/// some USDT-style tokens return bytes32 instead of a dynamic string). Attempts
/// the lowercase selector first, then the uppercase fallback. Port of
/// `erc20_utils._try_fetch_token_string` (erc20_utils.py:23-58).
///
/// Returns `None` when all fetch attempts fail (the Python's
/// `contextlib.suppress(Exception)` catches every error).
/// RPC-fetch the full ERC20 metadata tuple `(name, symbol, decimals)` for a
/// token. Port of `erc20_utils._fetch_erc20_token_metadata` (erc20_utils.py:85+).
/// Each field is `None` when its fetch attempts fail (the Python's
/// `contextlib.suppress(Exception)` per-field tolerance).
pub(crate) async fn fetch_erc20_metadata(
    provider: &AlloyProvider,
    token: &Address,
    block_number: u64,
) -> (Option<String>, Option<String>, Option<i64>) {
    let name = fetch_erc20_string_metadata(provider, token, "name()", "NAME()", block_number).await;
    let symbol =
        fetch_erc20_string_metadata(provider, token, "symbol()", "SYMBOL()", block_number).await;
    let decimals = fetch_erc20_decimals(provider, token, block_number).await;
    (name, symbol, decimals)
}

/// RPC-fetch `(name, symbol, decimals)` for SEVERAL tokens in ONE Multicall3
/// `aggregate3` pass — the `ReserveInitialized` metadata burst (3 tokens ×
/// 3 fields × 2 selector spellings = 18 same-block reads → 1 round trip).
///
/// Per-field semantics mirror [`fetch_erc20_metadata`]'s sequential logic
/// exactly (`string_field_from_batch` / `decimals_from_batch`): a failed
/// lower-spelling result (the revert/transport class the sequential path
/// sees as an `eth_call` error) makes the field `None` WITHOUT consulting
/// the fallback spelling — the sequential `.ok()?` early-return — and a
/// succeeded-but-undecodable return falls through to the fallback spelling.
/// BOTH spellings are always issued (`allowFailure = true`): the per-field
/// decision consumes the batched results, so the values are identical to
/// the sequential shape; only the request set always carries the fallback
/// spelling (read-only calls; a recorder records the batched shape whole).
pub(crate) async fn fetch_erc20_metadata_batched(
    provider: &AlloyProvider,
    tokens: [&Address; 3],
    block_number: u64,
) -> [(Option<String>, Option<String>, Option<i64>); 3] {
    const FIELD_FNS: [&str; 6] = [
        "name()",
        "NAME()",
        "symbol()",
        "SYMBOL()",
        "decimals()",
        "DECIMALS()",
    ];
    let mut calls: Vec<(Address, Bytes)> = Vec::with_capacity(tokens.len() * FIELD_FNS.len());
    for token in tokens {
        for func in FIELD_FNS {
            calls.push((*token, encode_no_arg_call(func)));
        }
    }
    // `multicall3_batch` already degrades a failed/malformed batch to
    // sequential per-call `eth_call`s mapped to per-item `success` flags; an
    // `Err` can only mean its own encode step failed (defensively
    // unreachable) — mirror it as all-failed results, which the per-field
    // decisions turn into the sequential shape's `None` fields.
    let results: Vec<MulticallResult> =
        match multicall3_batch(provider, &calls, Some(block_number)).await {
            Ok(r) => r,
            Err(_) => calls
                .iter()
                .map(|_| MulticallResult {
                    success: false,
                    return_data: Bytes::new(),
                })
                .collect(),
        };
    let field = |i: usize| -> (Option<String>, Option<String>, Option<i64>) {
        let base = i * FIELD_FNS.len();
        let name = string_field_from_batch(&results[base], &results[base + 1]);
        let symbol = string_field_from_batch(&results[base + 2], &results[base + 3]);
        let decimals = decimals_from_batch(&results[base + 4], &results[base + 5]);
        (name, symbol, decimals)
    };
    [field(0), field(1), field(2)]
}

async fn fetch_erc20_string_metadata(
    provider: &AlloyProvider,
    token: &Address,
    lower_func: &str,
    upper_func: &str,
    block_number: u64,
) -> Option<String> {
    for func in [lower_func, upper_func] {
        let calldata = encode_no_arg_call(func);
        let ret = provider
            .eth_call(token, calldata, Some(block_number))
            .await
            .ok()?;
        if let Some(s) = decode_string_return(&ret) {
            return Some(s);
        }
    }
    None
}

/// Decode an ABI dynamic `string` return: the first word is the offset to the
/// length + data (always 0x20 — the data begins at byte 32); the second word
/// is the byte length; the data follows, right-padded to a 32-byte multiple.
/// Mirrors `eth_abi.abi.decode(["string"], data)`. Returns `None` when the
/// return is malformed (too short / the offset isn't 0x20 / the length
/// overruns the return).
pub(crate) fn decode_dynamic_string(ret: &[u8]) -> Option<String> {
    if ret.len() < 64 {
        return None;
    }
    let offset = U256::from_be_slice(&ret[..32]);
    if offset != U256::from(0x20u32) {
        return None;
    }
    let length = U256::from_be_slice(&ret[32..64]).to::<usize>();
    if 64 + length > ret.len() {
        return None;
    }
    let s = String::from_utf8_lossy(&ret[64..64 + length]).into_owned();
    // The Python's `str(value)` preserves the bytes; the bytes32 fallback does
    // the null-strip. For the dynamic-string path, the length is exact — no
    // trailing NULs to strip.
    Some(s)
}

/// RPC-fetch an ERC20 token's `decimals()` (a `uint256` return). Attempts the
/// lowercase selector first, then the uppercase fallback. Port of
/// `erc20_utils._try_fetch_token_uint256` (erc20_utils.py:61-84). Returns
/// `None` when all fetch attempts fail.
async fn fetch_erc20_decimals(
    provider: &AlloyProvider,
    token: &Address,
    block_number: u64,
) -> Option<i64> {
    for func in ["decimals()", "DECIMALS()"] {
        let calldata = encode_no_arg_call(func);
        let ret = provider
            .eth_call(token, calldata, Some(block_number))
            .await
            .ok()?;
        if let Some(v) = word0_to_u256(&ret) {
            return Some(v.to::<u64>().cast_signed());
        }
    }
    None
}

//! The config-event direct-decode dispatch for the Rust-owned Aave V3 chunk
//! loop (`run_aave_update`, epic `AZGJUN`, task `6SWY4R`-2a).
//!
//! This module owns the orchestrator's per-transaction config-event handling:
//! loop over a tx's logs, decode each via
//! [`degenbot_decoders::aave_event_decoder::decode_aave_log`], + for the 10
//! enum-covered config event types, resolve ids via the `DegenbotDb`
//! substrate (`get_or_create_*_on_conn` / `lookup_asset_by_*_on_conn`) +
//! emit [`AaveChunkEvent`] variants (mirror of Python
//! `transaction_processor.py::_process_transaction` L328-430 — the Phase 1+2
//! config-event dispatch). The apply step (`-3`'s `apply_aave_chunk_writes_on_conn`)
//! consumes the emitted variants on the chunk `Transaction` (§3.4 atomicity).
//!
//! # 6SWY4R-2a scope (this file)
//!
//! The **10 enum-covered** config events (the ones with existing
//! [`AaveChunkEvent`] variants + `apply_*_on_conn` fns):
//! - 8 sync handlers (no RPC): `ReserveDataUpdated`, `UserEModeSet`,
//!   `ReserveUsedAsCollateral{Enabled,Disabled}`, `PriceOracleUpdated`,
//!   `AssetSourceUpdated`, `EModeCategoryAdded`, `EModeAssetCategoryChanged`,
//!   `AssetCollateralInEModeChanged`, + the 3 GHO discount events
//!   (`DiscountPercentUpdated`, `DiscountTokenUpdated`,
//!   `DiscountRateStrategyUpdated`).
//! - 2 async RPC handlers: [`resolve_reserve_initialized`] (EIP-1967 +
//!   revisions + `getSourceOfAsset`) + [`resolve_collateral_configuration`]
//!   (`getConfiguration(address)`).
//! - The discount pre-pass: [`build_discount_snapshot`] (the 3-way hybrid).
//!
//! # 6SWY4R-2b scope (NOT this file — the 6 missing-variant events)
//!
//! The 6 config events with NO `AaveChunkEvent` variant + NO apply fn (a
//! §4.2-parity gap to land in -2b): `Upgraded` (incl. the GHO-discount
//! deprecation side effect), `PoolUpdated`, `PoolConfiguratorUpdated`,
//! `PoolDataProviderUpdated`, `AddressSet`, `ProxyCreated`. The dispatch loop
//! ([`dispatch_config_events`]) skips these + logs them as "not yet ported".
//!
//! # The dispatch flow (mirror of Python `_process_transaction` L320-430)
//!
//! The Python's `_process_transaction` loops over the tx's UNASSIGNED events
//! (those not matched to an Operation by the parser) + dispatches by topic0.
//! In Rust, the config events are NEVER operation-assigned (the parser only
//! assigns Mint/Burn/BalanceTransfer/Transfer/Pool-event Supply/Borrow/etc.),
//! so this module loops over ALL tx logs (the orchestrator may narrow to
//! `parser.unassigned_events` in `-3`; the decode + dispatch is identical).
//!
//! # §3.4 atomicity
//!
//! The dispatch takes `conn: &Connection` (the chunk's single `Transaction`).
//! All `get_or_create_*` / `lookup_*` substrate calls run on it — the §3.4
//! invariant (ONE `Transaction` per chunk) holds. The dispatch emits
//! [`AaveChunkEvent`]s; the apply step (`-3`) consumes them on the same conn.
//! No `Transaction` lifecycle here — this file owns neither `commit` nor
//! `drop`.

#![allow(clippy::missing_errors_doc, clippy::doc_markdown)]

use std::collections::HashMap;

use alloy::primitives::{keccak256, Address, Bytes, U256};
use degenbot_db::aave::AaveGhoAsset;
use degenbot_db::{DbError, DebtPositionRefreshContext, DegenbotDb};
use degenbot_decoders::aave_event_decoder::{decode_aave_log, DecodedAaveEvent};
use degenbot_evm_math::ray_mul;
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;

use crate::gho_processor::calculate_gho_discount_rate;
use crate::run::{apply_chunk_events_on_conn, AaveChunkEvent};

/// The GHO-discount deprecation revision (V4+). Mirrors the Python
/// `GHO_DISCOUNT_DEPRECATION_REVISION = 4`. At vToken revision ≥ this value,
/// the discount mechanism is deprecated → the effective discount is always 0
/// (`build_discount_snapshot` path #3; `is_discount_supported` gate).
const GHO_DISCOUNT_DEPRECATION_REVISION: u32 = 4;

/// The EIP-1967 implementation storage slot (the canonically-fixed
/// `bytes32(uint256(keccak256("eip1967.proxy.implementation")) - 1)`).
/// Mirrors the Python `ERC_1967_IMPLEMENTATION_SLOT` — read via
/// `get_storage_at` to resolve a proxy's logic contract. Written as the
/// canonical hex and parsed at compile time (a typo fails the build rather
/// than silently mis-routing proxy resolution).
const EIP_1967_IMPLEMENTATION_SLOT: U256 = match U256::from_str_radix(
    "360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc",
    16,
) {
    Ok(v) => v,
    Err(_) => panic!("EIP-1967 implementation slot hex is a valid U256"),
};

// ── error ──────────────────────────────────────────────────────────────────

/// A config-event dispatch failure.
#[derive(Debug, thiserror::Error)]
pub enum ConfigDispatchError {
    /// A substrate (`DegenbotDb::`) failure.
    #[error("substrate error: {0}")]
    Substrate(#[from] DbError),
    /// A decoded-event shape failure (missing topic, malformed data — the
    /// decoder returned a struct but a required field is the zero address or
    /// a missing id).
    #[error("decode shape error: {0}")]
    DecodeShape(String),
    /// An RPC failure from a `ReserveInitialized` / `CollateralConfiguration`
    /// resolution.
    #[error("rpc error: {0}")]
    Rpc(#[from] degenbot_core::errors::ProviderError),
    /// The vToken revision could not be resolved (needed for the
    /// `is_discount_supported` gate).
    #[error("missing vtoken revision for market {0}")]
    MissingVtokenRevision(i64),
}

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
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let addr_str = checksum(&decoded.reserve);
    let asset = DegenbotDb::lookup_asset_by_underlying_address_on_conn(conn, market_id, &addr_str)?
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
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let addr_str = checksum(&decoded.user);
    let user_id = DegenbotDb::get_or_create_user_on_conn(conn, market_id, &addr_str, 0)?;
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
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let user_str = checksum(&user);
    let user_id = DegenbotDb::get_or_create_user_on_conn(conn, market_id, &user_str, 0)?;
    let asset_str = checksum(&reserve);
    let asset =
        DegenbotDb::lookup_asset_by_underlying_address_on_conn(conn, market_id, &asset_str)?
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
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    let asset_str = checksum(&decoded.asset);
    let Some(asset) =
        DegenbotDb::lookup_asset_by_underlying_address_on_conn(conn, market_id, &asset_str)?
    else {
        // I2RHGP Fix 2b (tolerance): on a fresh-market cold-boot, the
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
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let asset_str = checksum(&decoded.asset);
    let asset =
        DegenbotDb::lookup_asset_by_underlying_address_on_conn(conn, market_id, &asset_str)?
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
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let asset_str = checksum(&decoded.asset);
    let asset = DegenbotDb::lookup_asset_by_underlying_address_on_conn(
        conn,
        market_id,
        &asset_str,
    )?
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

/// `DiscountPercentUpdated(user, old, new)` →
/// [`AaveChunkEvent::GhoDiscountPercentUpdated`]. Mirrors
/// `_process_discount_percent_updated_event` (`newDiscountPercent` is topic[2]
/// — the decoder already extracted it).
pub fn dispatch_discount_percent_updated(
    market_id: i64,
    block_number: u64,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3DiscountPercentUpdatedEvent,
    conn: &Connection,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let user_str = checksum(&decoded.user);
    let user_id = DegenbotDb::get_or_create_user_on_conn(conn, market_id, &user_str, 0)?;
    let _ = block_number;
    Ok(AaveChunkEvent::GhoDiscountPercentUpdated {
        user_id,
        new_discount_percent: discount_to_i64(decoded.new_discount_percent),
    })
}

/// Discount-token (stkAAVE) ERC20 `Transfer(from, to, value)` →
/// [`AaveChunkEvent::StkAaveTransfer`] — the canonical + only stkAAVE
/// balance-mutation channel. Mirrors `stkaave.process_stk_aave_transfer_event`
/// EXACTLY: processes EVERY Transfer event (including both zero-leg arms),
/// treating `from == ZERO_ADDRESS` / `to == ZERO_ADDRESS` as a half-event
/// (skip the zero side, always mutate the real user) via `Option<i64>`
/// user_ids that are `None` iff the corresponding address is `ZERO_ADDRESS`.
///
/// Scoping:
/// - Returns `None` if the emitter is NOT `gho_asset.v_gho_discount_token` —
///   aToken/vToken/scaled-token Transfers are handled by the ops path
///   (`process_transaction`); only the stkAAVE discount token is this fn's
///   scope (matches the Python `assert contract_address == discount_token`).
///
/// YMWN5V retirement (crash #3): the prior design dedupe-skipped the zero-leg
/// here + processed the paired `Staked`/`Redeem` semantic events via separate
/// dispatch fns. That required an empirically-falsified invariant — every
/// zero-leg Transfer must pair with a semantic event. Some actions emit ONLY
/// the `Transfer(X→0)` event with no paired `Redeem` (verified via cast logs
/// across the 16.59M→18M range), leaving the sender's `stk_aave_balance`
/// stuck at its pre-burn cache value → wrong `calculate_gho_discount_rate`
/// → GHO-burn delta overshoots `prev_scaled_balance` → crashes. The dispatch
/// handlers are now retired (the Python never processed Staked/Redeem for
/// balance mutation — they're fetched only for classification in
/// `fetch_stk_aave_events`); all balance mutation flows through this fn now.
pub fn dispatch_stk_aave_transfer(
    market_id: i64,
    block_number: u64,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3Erc20TransferEvent,
    gho_asset: Option<&AaveGhoAsset>,
    conn: &Connection,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    let _ = block_number;
    // Scope: only the discount-token (stkAAVE) emitter.
    let Some(g) = gho_asset else {
        return Ok(None);
    };
    let Some(token_str) = g.v_gho_discount_token.as_deref() else {
        return Ok(None);
    };
    let Ok(discount_token) = token_str.parse::<Address>() else {
        return Ok(None);
    };
    if decoded.token_address != discount_token {
        return Ok(None); // not a stkAAVE Transfer → the ops path handles it.
    }
    // Matches the Python `if from_address == to_address: return`.
    if decoded.from == decoded.to {
        return Ok(None);
    }
    // Half-event handling for the zero-leg (YMWN5V retirement): skip the
    // ZERO_ADDRESS side, always resolve + mutate the real user. Mirrors
    // `process_stk_aave_transfer_event` — the Python skips ZERO_ADDRESS
    // entirely (`from_user=None`/`to_user=None` collapses to a no-op on that
    // side). The prior design dedupe-skipped the zero-leg here + processed the
    // paired Staked/Redeem semantic events (those dispatch fns are now
    // retired); that required an empirically-falsified invariant — every
    // zero-leg Transfer must pair with a semantic event. Some actions emit
    // ONLY the `Transfer(X→0)` event with no paired `Redeem` (verified via
    // cast logs across the 16.59M→18M range), leaving the sender's balance
    // stuck at the pre-burn cache value → wrong `calculate_gho_discount_rate`
    // → GHO-burn delta overshoots `prev_scaled_balance` → crash #3.
    let from_user_id = if decoded.from == Address::ZERO {
        None // the mint-from-zero leg — skipped at apply.
    } else {
        Some(DegenbotDb::get_or_create_user_on_conn(
            conn,
            market_id,
            &checksum(&decoded.from),
            0,
        )?)
    };
    let to_user_id = if decoded.to == Address::ZERO {
        None // the burn-to-zero leg — skipped at apply.
    } else {
        Some(DegenbotDb::get_or_create_user_on_conn(
            conn,
            market_id,
            &checksum(&decoded.to),
            0,
        )?)
    };
    // The degenerate `Transfer(0x0 → 0x0)` lands both `None` (no real user) —
    // still emitted (counts as processed) but the apply is a no-op.
    Ok(Some(AaveChunkEvent::StkAaveTransfer {
        from_user_id,
        to_user_id,
        amount: decoded.value,
    }))
}

/// [`dispatch_stk_aave_transfer`] wrapper that re-resolves the GHO asset
/// FRESH from `conn` AND pre-apply backfills each side's `stk_aave_balance`
/// from on-chain `balanceOf(user)` at `block_number - 1` when the column is
/// `NULL`. GJXURV (crash #4): the per-tx `gho_asset` snapshot is taken once
/// before `dispatch_config_events`'s per-event loop; a same-tx
/// `DiscountTokenUpdated` at an earlier logIndex bumps `v_gho_discount_token`
/// AFTER the snapshot, so a stale `None` snapshot would make dispatch skip the
/// matched transfer (the W2S3WH-sibling per-chunk refresh of
/// `spec.stk_aave_address` handles cross-chunk staleness from the cold-boot
/// `NULL`; this handles same-tx staleness). The pre-apply backfill mirrors
/// Python's `get_or_init_stk_aave_balance` (stkaave.py:116-122): without it,
/// `apply_stk_aave_transfer_on_conn` treats `NULL` as `U256::ZERO` and applies
/// `0 ± amount`, omitting the pre-event on-chain balance. For the crash
/// user's mint-then-burn pattern, `burn_value == on_chain_initial +
/// mint_value` — without the `on_chain_initial` backfill, the burn apply
/// underflows → chunk-rollback → wrong balance → GHO-burn overshoot crash
/// (byte-exact magnitude). The backfill is a no-op when the column is already
/// non-`NULL` (set by a prior same-tx Transfer or the (C) refresh).
pub(crate) async fn dispatch_stk_aave_transfer_with_backfill(
    provider: &AlloyProvider,
    conn: &Connection,
    market_id: i64,
    chain_id: i64,
    block_number: u64,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3Erc20TransferEvent,
    gho_asset: Option<&AaveGhoAsset>,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    // None-guard: no GHO market row at tx start — skip (a chain-wide fetch can
    // surface events from chains without a seeded GHO market).
    if gho_asset.is_none() {
        return Ok(None);
    }
    // Re-resolve gho_asset FRESH from conn (read-your-own-writes — a same-tx
    // DiscountTokenUpdated at an earlier logIndex bumps v_gho_discount_token
    // AFTER the per-tx snapshot was taken). Mirrors
    // `dispatch_discount_token_updated_with_fresh_resolution`.
    let fresh = DegenbotDb::fetch_aave_gho_asset_on_conn(conn, chain_id)?;
    let Some(g) = fresh.as_ref() else {
        return Ok(None);
    };
    // Re-use the sync dispatch fn for the emitter-validation guard +
    // StkAaveTransfer emission (skip the re-resolution since we just did it).
    let event = dispatch_stk_aave_transfer(market_id, block_number, decoded, Some(g), conn)?;
    let Some(AaveChunkEvent::StkAaveTransfer {
        from_user_id,
        to_user_id,
        amount: _,
    }) = event
    else {
        return Ok(event);
    };
    // Pre-apply backfill of either side when the column is `NULL` — mirrors
    // Python's `get_or_init_stk_aave_balance` at `block_number - 1`.
    let discount_token: Address = match g
        .v_gho_discount_token
        .as_deref()
        .and_then(|s| s.parse().ok())
    {
        Some(addr) => addr,
        None => {
            return Ok(Some(AaveChunkEvent::StkAaveTransfer {
                from_user_id,
                to_user_id,
                amount: decoded.value,
            }))
        }
    };
    if let Some(uid) = from_user_id {
        backfill_user_stk_aave_balance_if_none(provider, conn, uid, block_number, discount_token)
            .await?;
    }
    if let Some(uid) = to_user_id {
        backfill_user_stk_aave_balance_if_none(provider, conn, uid, block_number, discount_token)
            .await?;
    }
    Ok(Some(AaveChunkEvent::StkAaveTransfer {
        from_user_id,
        to_user_id,
        amount: decoded.value,
    }))
}

/// RPC-fetch `balanceOf(user)` at `block_number - 1` and store via
/// [`DegenbotDb::set_user_stk_aave_balance_on_conn`] when the user's
/// `stk_aave_balance` column is `NULL`. Mirrors Python's
/// `get_or_init_stk_aave_balance` (stkaave.py:24-39) — a `NULL` column means
/// the user was created by an earlier event but never touched by a stkAAVE
/// `Staked`/`Transfer` event YET; the on-chain value at the previous block is
/// the authoritative balance before any current-block events. No-op when the
/// column is already populated (the apply will mutate the cached value).
pub(crate) async fn backfill_user_stk_aave_balance_if_none(
    provider: &AlloyProvider,
    conn: &Connection,
    user_id: i64,
    block_number: u64,
    discount_token: Address,
) -> Result<(), ConfigDispatchError> {
    let row: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT address, stk_aave_balance FROM aave_v3_users WHERE id = ?1",
            [user_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(DbError::Sqlite)?;
    let Some((addr_str, balance_str)) = row else {
        return Err(ConfigDispatchError::DecodeShape(format!(
            "aave_v3_users id={user_id} (backfill target — row vanished)"
        )));
    };
    // Non-NULL → caller (apply_stk_aave_transfer_*) will use the cached value.
    if balance_str.is_some() {
        return Ok(());
    }
    let user_addr: Address = addr_str.parse().map_err(|_| {
        ConfigDispatchError::DecodeShape(format!("bad user_address in backfill: {addr_str}"))
    })?;
    let calldata = encode_single_address_call("balanceOf(address)", &user_addr);
    let ret = provider
        .eth_call(
            &discount_token,
            calldata,
            Some(block_number.saturating_sub(1)),
        )
        .await?;
    let balance = word0_to_u256(&ret).unwrap_or(alloy::primitives::U256::ZERO);
    DegenbotDb::set_user_stk_aave_balance_on_conn(conn, user_id, balance)?;
    Ok(())
}

/// `DiscountTokenUpdated(old, new)` → [`AaveChunkEvent::GhoDiscountTokenUpdated`].
/// Mirrors `_process_discount_token_updated_event`.
///
/// # Emitter-validation guard
///
/// `fetch_discount_config_logs` is chain-wide (no address filter — the events
/// emit from any contract). The load-bearing Python guard
/// `if gho_asset.v_token is None or gho_asset.v_token.address != event["address"]:
/// return` filters spurious chain-wide discount events; this dispatch mirrors
/// it: returns `Ok(None)` when the GHO vToken FK is missing OR the decoded
/// emitter (`decoded.v_token_address` = `log.address()`) doesn't match
/// `gho_asset.v_token_address`. The apply fn never sees the skip.
pub fn dispatch_discount_token_updated(
    gho_asset: &AaveGhoAsset,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3DiscountTokenUpdatedEvent,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    if gho_asset.v_token_address.as_deref() != Some(&checksum(&decoded.v_token_address)) {
        return Ok(None);
    }
    Ok(Some(AaveChunkEvent::GhoDiscountTokenUpdated {
        gho_token_id: gho_asset.id,
        new_discount_token: Some(checksum(&decoded.new_discount_token)),
    }))
}

/// `DiscountRateStrategyUpdated(old, new)` →
/// [`AaveChunkEvent::GhoDiscountRateStrategyUpdated`]. Mirrors
/// `_process_discount_rate_strategy_updated_event`.
///
/// Same emitter-validation guard as [`dispatch_discount_token_updated`] — see
/// its docs; the chain-wide fetch + the Python's `event["address"]` guard
/// apply identically (ULDUAC / divergence #2).
pub fn dispatch_discount_rate_strategy_updated(
    gho_asset: &AaveGhoAsset,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3DiscountRateStrategyUpdatedEvent,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    if gho_asset.v_token_address.as_deref() != Some(&checksum(&decoded.v_token_address)) {
        return Ok(None);
    }
    Ok(Some(AaveChunkEvent::GhoDiscountRateStrategyUpdated {
        gho_token_id: gho_asset.id,
        new_strategy: Some(checksum(&decoded.new_strategy)),
    }))
}

/// [`dispatch_discount_token_updated`] wrapper that re-resolves the GHO vToken
/// address FRESH from `conn` before the emitter-validation guard
/// (read-your-own-writes — a same-tx `ReserveInitialized` at an earlier
/// logIndex links `v_token_id` AFTER the per-tx `gho_asset` snapshot was
/// taken, so the snapshot's `v_token_address` is stale `None` → the guard
/// would skip the INITIAL-set event where `old=None→stkAAVE`). Mirrors
/// Python's live ORM (`gho_asset.v_token` lazy-loads the just-applied FK).
/// The `gho_asset` param is the None-guard (no GHO market row at tx start
/// = skip).
fn dispatch_discount_token_updated_with_fresh_resolution(
    gho_asset: Option<&AaveGhoAsset>,
    ev: &degenbot_decoders::aave_event_decoder::AaveV3DiscountTokenUpdatedEvent,
    conn: &Connection,
    chain_id: i64,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    // None-guard: no GHO market row at tx start = skip (the chain-wide fetch
    // can surface events from chains without a seeded GHO market).
    if gho_asset.is_none() {
        return Ok(None);
    }
    // Re-resolve FRESH from conn (read-your-own-writes — a same-tx
    // `ReserveInitialized` at an earlier logIndex links `v_token_id` AFTER the
    // per-tx snapshot was taken, so the snapshot's `v_token_address` is stale
    // `None` → the guard would skip the INITIAL-set event where
    // `old=None→stkAAVE`). Mirrors Python's live ORM (`gho_asset.v_token`
    // lazy-loads the just-applied FK). The discount-config events are rare
    // (a handful over the whole drive), so the per-event conn lookup is
    // negligible.
    let fresh = DegenbotDb::fetch_aave_gho_asset_on_conn(conn, chain_id)?;
    match fresh.as_ref() {
        Some(g) => dispatch_discount_token_updated(g, ev),
        None => Ok(None),
    }
}

/// [`dispatch_discount_rate_strategy_updated`] wrapper with the same
/// read-your-own-writes re-resolution as
/// [`dispatch_discount_token_updated_with_fresh_resolution`] — see its docs.
fn dispatch_discount_rate_strategy_updated_with_fresh_resolution(
    gho_asset: Option<&AaveGhoAsset>,
    ev: &degenbot_decoders::aave_event_decoder::AaveV3DiscountRateStrategyUpdatedEvent,
    conn: &Connection,
    chain_id: i64,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    if gho_asset.is_none() {
        return Ok(None);
    }
    let fresh = DegenbotDb::fetch_aave_gho_asset_on_conn(conn, chain_id)?;
    match fresh.as_ref() {
        Some(g) => dispatch_discount_rate_strategy_updated(g, ev),
        None => Ok(None),
    }
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
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let asset_str = checksum(&decoded.asset);
    let asset = DegenbotDb::lookup_asset_by_underlying_address_on_conn(
        conn,
        market_id,
        &asset_str,
    )?
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
/// `_fetch_erc20_token_metadata`, which is a §4.2 gap flagged for -2b / a
/// follow-up (the standalone Rust consumer needs the metadata fetch too; the
/// substrate `get_or_create_erc20_token_on_conn` takes metadata as a caller
/// param, so this is consistent with the existing contract — the gap is
/// isolated to this fn's token resolution).
#[expect(clippy::too_many_arguments)] // mirrors the Python event arg list 1:1
pub async fn resolve_reserve_initialized(
    provider: &AlloyProvider,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3ReserveInitializedEvent,
    market_id: i64,
    chain_id: i64,
    oracle_address: Address,
    gho_asset: Option<&AaveGhoAsset>,
    block_number: u64,
    conn: &Connection,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    // 1. get_or_create the 3 erc20 token rows (underlying / aToken / vToken),
    //    RPC-fetching the name/symbol/decimals from the chain (the
    //    standalone-Rust-core constraint — Rust owns the loop, no PyO3 FFI for
    //    metadata). Port of `erc20_utils._fetch_erc20_token_metadata`.
    let underlying_str = checksum(&decoded.asset);
    let (name, symbol, decimals) =
        fetch_erc20_metadata(provider, &decoded.asset, block_number).await;
    let underlying_asset_id = DegenbotDb::get_or_create_erc20_token_on_conn(
        conn,
        chain_id,
        &underlying_str,
        name.as_deref(),
        symbol.as_deref(),
        decimals,
    )?;
    let a_token_str = checksum(&decoded.a_token);
    let (name, symbol, decimals) =
        fetch_erc20_metadata(provider, &decoded.a_token, block_number).await;
    let a_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
        conn,
        chain_id,
        &a_token_str,
        name.as_deref(),
        symbol.as_deref(),
        decimals,
    )?;
    let v_token_str = checksum(&decoded.variable_debt_token);
    let (name, symbol, decimals) =
        fetch_erc20_metadata(provider, &decoded.variable_debt_token, block_number).await;
    let v_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
        conn,
        chain_id,
        &v_token_str,
        name.as_deref(),
        symbol.as_deref(),
        decimals,
    )?;

    // 2. EIP-1967: resolve the aToken + vToken implementation addresses.
    let atoken_impl = read_implementation_slot(provider, &decoded.a_token, block_number).await?;
    let vtoken_impl =
        read_implementation_slot(provider, &decoded.variable_debt_token, block_number).await?;

    // 3. ATOKEN_REVISION() / DEBT_TOKEN_REVISION() on the implementations.
    let a_token_revision =
        read_uint256_return(provider, &atoken_impl, "ATOKEN_REVISION()", block_number).await?;
    let v_token_revision = read_uint256_return(
        provider,
        &vtoken_impl,
        "DEBT_TOKEN_REVISION()",
        block_number,
    )
    .await?;

    // 4. getSourceOfAsset(address) on the PRICE_ORACLE contract.
    let source_calldata = encode_single_address_call("getSourceOfAsset(address)", &decoded.asset);
    let source_ret = provider
        .eth_call(&oracle_address, source_calldata, Some(block_number))
        .await?;
    let price_source = decode_address_return(&source_ret);

    Ok(AaveChunkEvent::ReserveInitialized {
        market_id,
        underlying_asset_id,
        a_token_id,
        a_token_revision: discount_to_i64(a_token_revision),
        v_token_id,
        v_token_revision: discount_to_i64(v_token_revision),
        price_source,
        // 5. GHO-vToken-FK link (2QGL6G / divergence #8): mirror the Python's
        //    `if asset_address == gho_asset.token.address: gho_token_entry
        //    .v_token_id = v_token.id` (event_handlers.py:689-698). The FK is
        //    the precondition for the ULDUAC emitter guard (which compares
        //    against `gho_asset.v_token_address`, resolved via the FK).
        gho_link_token_id: gho_asset
            .filter(|g| g.gho_token_address.as_deref() == Some(underlying_str.as_str()))
            .map(|g| g.id),
    })
}

// ── the discount pre-pass (the 3-way hybrid) ───────────────────────────────

/// Build the per-tx GHO discount snapshot — the `HashMap<Address, U256>` that
/// `process_transaction` (C3) consumes as the `discounts` parameter. Mirrors
/// the Python `_process_transaction` L140-210 pre-pass: scan the tx's GHO
/// vToken Mint/Burn events, + for each user, resolve the START discount (the
/// value in effect at tx start, BEFORE any `DISCOUNT_PERCENT_UPDATED` in this
/// tx — the `GhoDiscountContext` applies the in-tx overrides via
/// `effective_discount`).
///
/// # The 3-way hybrid
///
/// - **path #1 (DB-cache):** the user exists in `aave_v3_users` → read
///   `gho_discount` (the snapshot at tx start). Mirrors the Python
///   `gho_users.get(user_address)` → `user.gho_discount`.
/// - **path #2 (RPC):** the user is NOT in the DB → `getDiscountPercent(user)`
///   `eth_call` at the tx's `block_number` (the user has an existing GHO debt
///   position first encountered in this chunk's tx range). Mirrors the
///   Python `raw_call(getDiscountPercent)`. Only when `vtoken_revision < 4`.
/// - **path #3 (V4+ deprecation):** `vtoken_revision >= 4` → the discount
///   mechanism is deprecated → 0. Mirrors the Python
///   `is_discount_supported(session, market)` gate (revision < 4).
///
/// Returns an empty map when there's no GHO vToken (`gho_vtoken_address` is
/// `None`) or the revision couldn't be resolved.
pub async fn build_discount_snapshot(
    provider: &AlloyProvider,
    tx_logs: &[&alloy::rpc::types::Log],
    block_number: u64,
    gho_vtoken_address: Option<Address>,
    vtoken_revision: Option<u32>,
    market_id: i64,
    conn: &Connection,
) -> Result<HashMap<Address, U256>, ConfigDispatchError> {
    use degenbot_decoders::aave_event_decoder::{AAVE_BURN_TOPIC, AAVE_MINT_TOPIC};

    let Some(vtoken_addr) = gho_vtoken_address else {
        return Ok(HashMap::new());
    };
    let Some(rev) = vtoken_revision else {
        return Ok(HashMap::new());
    };
    let discount_supported = rev < GHO_DISCOUNT_DEPRECATION_REVISION;

    let mut snapshot: HashMap<Address, U256> = HashMap::new();
    for log in tx_logs {
        let topics = log.topics();
        if topics.is_empty() || log.address() != vtoken_addr {
            continue;
        }
        let user = if topics[0] == AAVE_MINT_TOPIC && topics.len() >= 3 {
            // Mint: topics[2] = onBehalfOf (the user).
            topic_to_address(topics[2])
        } else if topics[0] == AAVE_BURN_TOPIC && topics.len() >= 2 {
            // Burn: topics[1] = from (the user).
            topic_to_address(topics[1])
        } else {
            continue;
        };
        if snapshot.contains_key(&user) {
            continue; // first-encounter wins (matches the Python `if user_address not in user_discounts`)
        }
        // path #1: DB-cache.
        let user_str = checksum(&user);
        if let Some(db_discount) =
            DegenbotDb::fetch_user_gho_discount_by_address_on_conn(conn, market_id, &user_str)?
        {
            snapshot.insert(user, U256::from(u64::try_from(db_discount).unwrap_or(0)));
            continue;
        }
        // path #3: V4+ → 0 (must check before path #2 — the Python gates path
        // #2 with `is_discount_supported`).
        if !discount_supported {
            snapshot.insert(user, U256::ZERO);
            continue;
        }
        // path #2: RPC getDiscountPercent(user) at block_number.
        let calldata = encode_single_address_call("getDiscountPercent(address)", &user);
        let ret = provider
            .eth_call(&vtoken_addr, calldata, Some(block_number))
            .await?;
        let discount = word0_to_u256(&ret).unwrap_or(U256::ZERO);
        snapshot.insert(user, discount);
    }
    Ok(snapshot)
}

// ── the dispatch loop ──────────────────────────────────────────────────────

/// The per-tx config-event dispatch loop. Mirrors Python
/// `_process_transaction` L328-430: loop over `tx_logs`, decode each via
/// [`decode_aave_log`], + for the 10 enum-covered config event types,
/// resolve ids + emit [`AaveChunkEvent`] variants. Skips non-config events
/// (Supply/Borrow/Mint/Burn/Transfer — the Operation events C3's
/// `process_transaction` handles) + the 6 missing-variant events (-2b's
/// scope: `Upgraded`/`PoolUpdated`/`PoolConfiguratorUpdated`/
/// `PoolDataProviderUpdated`/`AddressSet`/`ProxyCreated`).
#[expect(clippy::too_many_arguments)] // mirrors the Python event arg list 1:1
pub async fn dispatch_config_events(
    provider: &AlloyProvider,
    tx_logs: &[&alloy::rpc::types::Log],
    market_id: i64,
    chain_id: i64,
    conn: &Connection,
    pool_address: Address,
    oracle_address: Option<Address>,
    gho_asset: Option<&AaveGhoAsset>,
    block_number: u64,
) -> Result<Vec<AaveChunkEvent>, ConfigDispatchError> {
    let mut events = Vec::new();
    for log in tx_logs {
        let Some(decoded) = decode_aave_log(log) else {
            continue; // not an Aave event (may be an unrelated ERC20/etc.).
        };
        if let Some(ev) = dispatch_single_config_event(
            &decoded,
            provider,
            market_id,
            chain_id,
            conn,
            pool_address,
            oracle_address,
            gho_asset,
            block_number,
        )
        .await?
        {
            // I2RHGP Fix 2c (intra-dispatch apply): apply each config event
            // to `conn` AS it's dispatched (in logIndex order), so a later
            // config event's dispatch sees an earlier event's apply — e.g.
            // `CollateralConfigurationChanged` (logIdx 419) sees the asset
            // `ReserveInitialized` (logIdx 413) just created. Matches the
            // Python's per-event apply (intra-tx read-your-own-writes). The
            // chunk loop's batch apply (the former step (d)) is removed.
            apply_chunk_events_on_conn(conn, market_id, std::slice::from_ref(&ev))?;
            events.push(ev);
        }
    }
    Ok(events)
}

/// Resolve the `PRICE_ORACLE` contract address for `ReserveInitialized`
/// dispatch (I2RHGP Fix 1b). The spec's `cached` `oracle_address` is
/// captured once before the chunk loop (`build_fetch_spec`) + can be `None`
/// when the `PRICE_ORACLE` row is registered mid-loop via a
/// `PriceOracleUpdated` event (mainnet: block 16291126, chunk 1) — AFTER
/// `build_fetch_spec` ran. When `None`, look it up FRESH on `conn`
/// (read-your-own-writes within the chunk + prior chunks' committed writes).
/// The `PRICE_ORACLE` row, once registered, is durable across chunks; the
/// stale spec cache is the only gap.
fn resolve_reserve_oracle_address(
    conn: &Connection,
    market_id: i64,
    cached: Option<Address>,
) -> Result<Address, ConfigDispatchError> {
    if let Some(o) = cached {
        return Ok(o);
    }
    DegenbotDb::fetch_aave_oracle_address_on_conn(conn, market_id)?
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| {
            ConfigDispatchError::DecodeShape(
                "ReserveInitialized: no PRICE_ORACLE contract resolved".to_string(),
            )
        })
}

/// Dispatch a single decoded config event to its handler + return the emitted
/// [`AaveChunkEvent`] (`None` for skipped/non-config events). Extracted from
/// [`dispatch_config_events`] to keep the loop fn under the 100-line
/// `clippy::too_many_lines` limit (the 14-arm match is naturally one unit).
#[expect(clippy::too_many_arguments)] // mirrors the Python event arg list 1:1
async fn dispatch_single_config_event(
    decoded: &DecodedAaveEvent,
    provider: &AlloyProvider,
    market_id: i64,
    chain_id: i64,
    conn: &Connection,
    pool_address: Address,
    oracle_address: Option<Address>,
    gho_asset: Option<&AaveGhoAsset>,
    block_number: u64,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    let ev = match decoded {
        // ── the 8 sync handlers (no RPC) ──
        DecodedAaveEvent::ReserveDataUpdated(ev) => Some(dispatch_reserve_data_updated(
            market_id,
            block_number,
            ev,
            conn,
        )?),
        DecodedAaveEvent::UserEModeSet(ev) => {
            Some(dispatch_user_e_mode_set(market_id, block_number, ev, conn)?)
        }
        DecodedAaveEvent::ReserveUsedAsCollateralEnabled(ev) => Some(
            dispatch_reserve_used_as_collateral(market_id, ev.reserve, ev.user, true, conn)?,
        ),
        DecodedAaveEvent::ReserveUsedAsCollateralDisabled(ev) => Some(
            dispatch_reserve_used_as_collateral(market_id, ev.reserve, ev.user, false, conn)?,
        ),
        DecodedAaveEvent::PriceOracleUpdated(ev) => {
            Some(dispatch_price_oracle_updated(market_id, ev)?)
        }
        DecodedAaveEvent::AssetSourceUpdated(ev) => {
            dispatch_asset_source_updated(market_id, ev, conn)?
        }
        DecodedAaveEvent::EModeCategoryAdded(ev) => {
            Some(dispatch_e_mode_category_added(market_id, ev)?)
        }
        DecodedAaveEvent::EModeAssetCategoryChanged(ev) => {
            Some(dispatch_e_mode_asset_category_changed(market_id, ev, conn)?)
        }
        DecodedAaveEvent::AssetCollateralInEModeChanged(ev) => Some(
            dispatch_asset_collateral_in_emode_changed(market_id, ev, conn)?,
        ),
        DecodedAaveEvent::DiscountPercentUpdated(ev) => Some(dispatch_discount_percent_updated(
            market_id,
            block_number,
            ev,
            conn,
        )?),
        DecodedAaveEvent::DiscountTokenUpdated(ev) => {
            dispatch_discount_token_updated_with_fresh_resolution(gho_asset, ev, conn, chain_id)?
        }
        DecodedAaveEvent::DiscountRateStrategyUpdated(ev) => {
            dispatch_discount_rate_strategy_updated_with_fresh_resolution(
                gho_asset, ev, conn, chain_id,
            )?
        }
        // ── the 2 async RPC handlers ──
        DecodedAaveEvent::CollateralConfigurationChanged(ev) => Some(
            resolve_collateral_configuration(
                provider,
                pool_address,
                ev,
                market_id,
                block_number,
                conn,
            )
            .await?,
        ),
        DecodedAaveEvent::ReserveInitialized(ev) => {
            let oracle = resolve_reserve_oracle_address(conn, market_id, oracle_address)?;
            Some(
                resolve_reserve_initialized(
                    provider,
                    ev,
                    market_id,
                    chain_id,
                    oracle,
                    gho_asset,
                    block_number,
                    conn,
                )
                .await?,
            )
        }
        // ── stkAAVE Staked/Redeem semantic events: NO balance-mutation
        // dispatch. YMWN5V-retired (crash #3): the prior design processed
        // these as proxies for the zero-leg Transfers; the Python never
        // did (Staked/Redeem are fetched only for classification in
        // `fetch_stk_aave_events`). The decoders stay (harmless, available
        // for future classification) — what goes is the balance-mutation
        // proxy. Balance mutation flows through the Transfer arm below. ──
        DecodedAaveEvent::Staked(_) | DecodedAaveEvent::Redeem(_) => None,
        // ── stkAAVE `Transfer` arm (covers the zero-leg arms + the
        // neither-zero case via Option<i64>; scoped to the discount token). ──
        DecodedAaveEvent::Erc20Transfer(ev) => {
            dispatch_stk_aave_transfer_with_backfill(
                provider,
                conn,
                market_id,
                chain_id,
                block_number,
                ev,
                gho_asset,
            )
            .await?
        }
        // ── 6SWY4R-2b: the 6 missing-variant config events ──────────────────
        // Delegated to `resolve_missing_variant_event` to keep this fn under
        // the 100-line `clippy::too_many_lines` limit.
        //
        // Operation events (Supply/Borrow/Mint/Burn/Transfer/...) + the
        // 6 missing-variant events both fall through to the `_` arm. The
        // `resolve_missing_variant_event` fn matches on the 6 missing-variant
        // variants; for operation events it returns `Ok(None)`.
        _ => {
            resolve_missing_variant_event(
                decoded,
                provider,
                market_id,
                gho_asset,
                block_number,
                conn,
            )
            .await?
        }
    };
    Ok(ev)
}

/// Resolve the 6 missing-variant config events (6SWY4R-2b). Extracted from
/// [`dispatch_single_config_event`] to keep that fn under the 100-line
/// `clippy::too_many_lines` limit. Returns `Ok(None)` for non-missing-variant
/// events (operation events) + for `ProxyCreated` when the `id` doesn't match
/// `POOL`/`POOL_CONFIGURATOR`.
///
/// # Events
///
/// - `Upgraded` — RPC `ATOKEN_REVISION()`/`DEBT_TOKEN_REVISION()` + the
///   GHO-discount-deprecation side effect.
/// - `PoolUpdated`/`PoolConfiguratorUpdated` — RPC `*_REVISION()` on the new
///   address → `ContractRevisionUpdated` (revision ONLY — §4.2 parity).
/// - `PoolDataProviderUpdated` — pure-decode (INSERT when old==zero, else
///   UPDATE-by-old-address).
/// - `AddressSet` — assert old==zero; ASCII-decode the bits32 id.
/// - `ProxyCreated` — match the id against the right-padded ASCII `b"POOL"`/
///   `b"POOL_CONFIGURATOR"`; RPC the revision on the impl address.
async fn resolve_missing_variant_event(
    decoded: &DecodedAaveEvent,
    provider: &AlloyProvider,
    market_id: i64,
    gho_asset: Option<&AaveGhoAsset>,
    block_number: u64,
    conn: &Connection,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    let ev = match decoded {
        DecodedAaveEvent::Upgraded(ev) => {
            Some(resolve_upgraded(provider, ev, market_id, gho_asset, block_number, conn).await?)
        }
        DecodedAaveEvent::PoolUpdated(ev) => Some(
            resolve_contract_revision_updated(
                provider,
                ev.new_address,
                "POOL",
                "POOL_REVISION()",
                market_id,
                block_number,
            )
            .await?,
        ),
        DecodedAaveEvent::PoolConfiguratorUpdated(ev) => Some(
            resolve_contract_revision_updated(
                provider,
                ev.new_address,
                "POOL_CONFIGURATOR",
                "CONFIGURATOR_REVISION()",
                market_id,
                block_number,
            )
            .await?,
        ),
        DecodedAaveEvent::PoolDataProviderUpdated(ev) => {
            Some(AaveChunkEvent::PoolDataProviderUpdated {
                market_id,
                old_address: (ev.old_address != Address::ZERO).then(|| checksum(&ev.old_address)),
                new_address: checksum(&ev.new_address),
            })
        }
        DecodedAaveEvent::AddressSet(ev) => {
            if ev.old_address != Address::ZERO {
                return Err(ConfigDispatchError::DecodeShape(format!(
                    "AddressSet: expected old_address == zero, got {}",
                    checksum(&ev.old_address)
                )));
            }
            let name = strip_trailing_nulls_from_ascii(&ev.id);
            Some(AaveChunkEvent::ContractInserted {
                market_id,
                name,
                address: checksum(&ev.new_address),
                revision: None,
            })
        }
        DecodedAaveEvent::ProxyCreated(ev) => {
            if let Some(resolved) = match_proxy_id(
                &ev.id,
                &ev.proxy_address,
                &ev.implementation_address,
                provider,
                block_number,
            )
            .await?
            {
                Some(AaveChunkEvent::ContractInserted {
                    market_id,
                    name: resolved.name,
                    address: resolved.address,
                    revision: Some(resolved.revision),
                })
            } else {
                None
            }
        }
        // Non-missing-variant events (operation events + the 10 already-handled
        // config events — unreachable from `dispatch_single_config_event`'s `_`
        // arm for the 10, reachable for the operation events).
        _ => None,
    };
    Ok(ev)
}

// ── RPC helpers (hand-encoded calldata — no degenbot-abi helper, per -2 decision) ──

/// Encode a single-`address`-arg `eth_call`: 4-byte selector
/// (`keccak256(sig)[0..4]`) + 32-byte left-padded address. For
/// `getConfiguration(address)`, `getSourceOfAsset(address)`,
/// `getDiscountPercent(address)`.
fn encode_single_address_call(sig: &str, addr: &Address) -> Bytes {
    let selector = &keccak256(sig.as_bytes())[..4];
    let mut calldata = Vec::with_capacity(36);
    calldata.extend_from_slice(selector);
    calldata.extend_from_slice(&[0u8; 12]); // left-pad the address to 32 bytes
    calldata.extend_from_slice(addr.as_slice());
    Bytes::from(calldata)
}

/// Encode a no-arg `eth_call`: 4-byte selector only. For
/// `ATOKEN_REVISION()`, `DEBT_TOKEN_REVISION()`.
pub(crate) fn encode_no_arg_call(sig: &str) -> Bytes {
    Bytes::from(keccak256(sig.as_bytes())[..4].to_vec())
}

/// Pad a 20-byte address to a 32-byte topic (left-padded with zeros). For
/// constructing fixture log topics for the discount pre-pass tests.
#[cfg(test)]
fn pad_address(addr: &Address) -> alloy::primitives::B256 {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(addr.as_slice());
    alloy::primitives::B256::from(word)
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

/// Read a `uint256`-returning no-arg call (`ATOKEN_REVISION()`,
/// `DEBT_TOKEN_REVISION()`).
async fn read_uint256_return(
    provider: &AlloyProvider,
    target: &Address,
    sig: &str,
    block_number: u64,
) -> Result<U256, ConfigDispatchError> {
    let calldata = encode_no_arg_call(sig);
    let ret = provider
        .eth_call(target, calldata, Some(block_number))
        .await?;
    Ok(word0_to_u256(&ret).unwrap_or(U256::ZERO))
}

/// Decode the first 32-byte word of an `eth_call` return as a `U256`.
fn word0_to_u256(ret: &[u8]) -> Option<U256> {
    if ret.len() < 32 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&ret[0..32]);
    Some(U256::from_be_bytes::<32>(buf))
}

/// Decode an `eth_call` return as an `address` (the low 20 bytes of the first
/// 32-byte word). Returns `None` when the return is < 32 bytes (shouldn't
/// happen for a well-formed `getSourceOfAsset` / oracle return).
fn decode_address_return(ret: &[u8]) -> Option<String> {
    if ret.len() < 32 {
        return None;
    }
    let addr = Address::from_slice(&ret[12..32]);
    if addr.is_zero() {
        None
    } else {
        Some(checksum(&addr))
    }
}

/// EIP-55 checksum an address (for DB storage + RPC addresses).
fn checksum(addr: &Address) -> String {
    degenbot_core::address_utils::address_to_checksum_string(addr)
}

/// Decode the first topic word as an `Address` (the low 20 bytes).
fn topic_to_address(topic: alloy::primitives::B256) -> Address {
    Address::from_slice(&topic.as_slice()[12..])
}

/// Clamp a `U256` discount/revision percent to `i64` (the `aave_v3_users
/// .gho_discount` / `*_revision` column type). Mirrors the Python's implicit
/// `int(...)` (the Aave protocol caps discount at 100%, revisions at single
/// digits).
fn discount_to_i64(v: U256) -> i64 {
    v.to::<u64>().try_into().unwrap_or(i64::MAX)
}

// ── C3.3: the (C) discount-refresh post-apply pass ───────────────────

/// The (C) discount-refresh post-apply pass. For one `GhoRefreshDiscount`
/// signal (one V1-V3 GHO mint/burn that set `should_refresh_discount`),
/// recompute `aave_v3_users.gho_discount` from the POST-APPLY debt balance +
/// the user's stkAAVE balance (mirrors Python's `_refresh_discount_rate` +
/// `get_or_init_stk_aave_balance`).
///
/// 1. Read the debt position's `(user_id, user_address, scaled_balance,
///    last_index, stk_aave_balance)` from `conn` — the POST-apply values
///    (the `ScaledTokenMint`/`Burn` apply landed them just before this call).
/// 2. `get_or_init_stk_aave_balance`: if `stk_aave_balance` is `None` (the
///    user never touched by a stkAAVE `Staked`/`Transfer` yet — the 890
///    `None` + the 1042 missing), `balanceOf(address)` `eth_call` on the
///    discount-token (stkAAVE) at **`block_number - 1`** (Python reads at the
///    PREVIOUS block — the balance check is BEFORE any events in the current
///    block; an off-by-one here causes a divergence class) → SET
///    `aave_v3_users.stk_aave_balance`.
/// 3. `debt_balance = ray_mul(scaled_balance, last_index, None)` (HALF_UP —
///    Python's `math_libs.ray_mul` default; matches the byte-exact
///    `accrue_debt_on_action` path).
/// 4. `gho_discount = calculate_gho_discount_rate(debt_balance,
///    stk_aave_balance)` → `i64`.
/// 5. Write `aave_v3_users.gho_discount` (the DB-cache path #1 of
///    `build_discount_snapshot` reads this on the next tx → correct discount
///    → GHO debt accrual converges).
///
/// `discount_token` is the chain's (freshly-per-tx-resolved)
/// `v_gho_discount_token` — `None` (no discount token configured) → no-op.
/// `market_id` is unused (the refresh target is `aave_v3_users.id`).
#[expect(clippy::missing_errors_doc)]
pub async fn refresh_gho_discount(
    provider: &AlloyProvider,
    conn: &Connection,
    #[expect(unused_variables)] market_id: i64,
    position_id: i64,
    block_number: u64,
    discount_token: Option<Address>,
) -> Result<(), ConfigDispatchError> {
    let Some(discount_token) = discount_token else {
        return Ok(());
    };
    // 1. POST-apply debt-position context.
    let ctx: DebtPositionRefreshContext =
        DegenbotDb::lookup_debt_position_refresh_context_on_conn(conn, position_id)?;
    // 2. get_or_init_stk_aave_balance (balanceOf at block-1 if None).
    let stk_aave_balance = if let Some(b) = ctx.stk_aave_balance {
        b
    } else {
        let user_addr: Address = ctx.user_address.parse().map_err(|_| {
            ConfigDispatchError::DecodeShape(format!(
                "bad user_address in refresh: {}",
                ctx.user_address
            ))
        })?;
        let calldata = encode_single_address_call("balanceOf(address)", &user_addr);
        let ret = provider
            .eth_call(
                &discount_token,
                calldata,
                Some(block_number.saturating_sub(1)),
            )
            .await?;
        let balance = word0_to_u256(&ret).unwrap_or(U256::ZERO);
        DegenbotDb::set_user_stk_aave_balance_on_conn(conn, ctx.user_id, balance)?;
        balance
    };
    // 3. debt_balance = ray_mul(scaled, last_index) — HALFUP (Python default).
    let debt_index = ctx.last_index.unwrap_or(U256::ZERO);
    let debt_balance = ray_mul(ctx.scaled_balance, debt_index, None).map_err(|e| {
        ConfigDispatchError::DecodeShape(format!("ray_mul overflow in refresh: {e}"))
    })?;
    // 4. + 5. compute + write gho_discount.
    let rate = calculate_gho_discount_rate(debt_balance, stk_aave_balance).map_err(|e| {
        ConfigDispatchError::DecodeShape(format!("calculate_gho_discount_rate: {e}"))
    })?;
    DegenbotDb::apply_gho_discount_percent_updated_on_conn(
        conn,
        ctx.user_id,
        discount_to_i64(rate),
    )?;
    Ok(())
}

// ── 6SWY4R-2b: the 6 missing-variant event resolvers ─────────────────────

/// The right-padded ASCII bytes32 id `b"POOL"` (4 bytes + 28 zeros). The
/// Python's `eth_abi.abi.encode(["bytes32"], [b"POOL"])` — §4.2 finding:
/// NOT `keccak256("POOL")`, the protocol emits the right-padded ASCII string.
const POOL_PROXY_ID: [u8; 32] = {
    let mut id = [0u8; 32];
    id[0] = b'P';
    id[1] = b'O';
    id[2] = b'O';
    id[3] = b'L';
    id
};
/// The right-padded ASCII bytes32 id `b"POOL_CONFIGURATOR"` (17 bytes + 15
/// zeros).
const POOL_CONFIGURATOR_PROXY_ID: [u8; 32] = {
    let mut id = [0u8; 32];
    let name = b"POOL_CONFIGURATOR";
    let mut i = 0;
    while i < name.len() {
        id[i] = name[i];
        i += 1;
    }
    id
};

/// The result of matching a `ProxyCreated` event's id against the two known
/// proxy ids — the resolved contract name + address (the proxy address) + the
/// RPC-fetched revision.
pub(crate) struct ProxyCreationResolution {
    pub(crate) name: String,
    pub(crate) address: String,
    pub(crate) revision: i64,
}

/// Resolve an `Upgraded` event (the riskiest piece — 6SWY4R-2b). Port of
/// `_process_scaled_token_upgrade_event` (event_handlers.py:848-940).
///
/// 1. The event's `proxy_address` (the emitter) is matched against existing
///    `aave_v3_assets.a_token` first, then `v_token` (the Python's
///    `get_asset_by_token_type` sequence). If neither matches, returns `Err`
///    ("Unreachable code path" — the Python raises `ValueError`).
/// 2. The new implementation's revision is RPC'd (`ATOKEN_REVISION()` for
///    aToken / `DEBT_TOKEN_REVISION()` for vToken) at `block_number`.
/// 3. For a vToken upgrade, the GHO-discount-deprecation fires when the
///    upgraded vToken IS the GHO vToken (`gho_asset.v_token_address` matches)
///    and the new revision ≥ `GHO_DISCOUNT_DEPRECATION_REVISION` (4). It clears
///    `v_gho_discount_token`/`v_gho_discount_rate_strategy` and bulk-resets
///    all users' `gho_discount` to 0 (the apply fn does the writes).
async fn resolve_upgraded(
    provider: &AlloyProvider,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3UpgradedEvent,
    market_id: i64,
    gho_asset: Option<&AaveGhoAsset>,
    block_number: u64,
    conn: &Connection,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let proxy_str = checksum(&decoded.proxy_address);
    // 1. asset lookup: a_token first, then v_token.
    let a_asset =
        DegenbotDb::lookup_asset_by_token_address_on_conn(conn, market_id, &proxy_str, "a_token")?;
    let (asset_id, is_a_token) = if let Some(row) = a_asset {
        (row.id, true)
    } else {
        let v_asset = DegenbotDb::lookup_asset_by_token_address_on_conn(
            conn, market_id, &proxy_str, "v_token",
        )?;
        let Some(row) = v_asset else {
            return Err(ConfigDispatchError::DecodeShape(format!(
                "Upgraded: proxy {proxy_str} is neither a known aToken nor vToken \
                 (Python: 'Unreachable code path')"
            )));
        };
        (row.id, false)
    };
    // 2. RPC the revision on the new implementation.
    let rev_fn = if is_a_token {
        "ATOKEN_REVISION()"
    } else {
        "DEBT_TOKEN_REVISION()"
    };
    let new_revision =
        read_uint256_return(provider, &decoded.implementation, rev_fn, block_number).await?;
    let new_revision_i64 = discount_to_i64(new_revision);
    // 3. GHO-discount-deprecation (vToken only).
    let deprecated_gho_token_id = if is_a_token {
        None
    } else {
        let matches_gho_vtoken = gho_asset
            .and_then(|g| g.v_token_address.as_deref())
            .is_some_and(|addr| addr == proxy_str);
        if matches_gho_vtoken
            && u32::try_from(new_revision_i64).is_ok_and(|r| r >= GHO_DISCOUNT_DEPRECATION_REVISION)
        {
            gho_asset.map(|g| g.id)
        } else {
            None
        }
    };
    Ok(AaveChunkEvent::Upgraded {
        asset_id,
        market_id,
        is_a_token,
        new_revision: new_revision_i64,
        deprecated_gho_token_id,
    })
}

/// Resolve a `PoolUpdated`/`PoolConfiguratorUpdated` event: RPC the revision fn
/// on the new address + emit `ContractRevisionUpdated`. Port of
/// `_update_contract_revision` (event_handlers.py:944-974). The `contract_name`
/// is "POOL"/"POOL_CONFIGURATOR"; the `revision_fn` is
/// "POOL_REVISION()"/"CONFIGURATOR_REVISION()".
async fn resolve_contract_revision_updated(
    provider: &AlloyProvider,
    new_address: Address,
    contract_name: &str,
    revision_fn: &str,
    market_id: i64,
    block_number: u64,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let revision = read_uint256_return(provider, &new_address, revision_fn, block_number).await?;
    Ok(AaveChunkEvent::ContractRevisionUpdated {
        market_id,
        contract_name: contract_name.to_string(),
        new_revision: discount_to_i64(revision),
    })
}

/// Match a `ProxyCreated` event's `id` against the two known proxy ids (the
/// right-padded ASCII `b"POOL"`/`b"POOL_CONFIGURATOR"`). On a match, RPC the
/// revision fn on the implementation address + return the resolved
/// `ProxyCreationResolution` (the contract name + the proxy address + the
/// revision). On no match → `Ok(None)` (the Python returns early when the id
/// doesn't match the expected proxy_id).
pub(crate) async fn match_proxy_id(
    id: &alloy::primitives::B256,
    proxy_address: &Address,
    implementation_address: &Address,
    provider: &AlloyProvider,
    block_number: u64,
) -> Result<Option<ProxyCreationResolution>, ConfigDispatchError> {
    let (name, rev_fn) = if id.as_slice() == POOL_PROXY_ID {
        ("POOL", "POOL_REVISION()")
    } else if id.as_slice() == POOL_CONFIGURATOR_PROXY_ID {
        ("POOL_CONFIGURATOR", "CONFIGURATOR_REVISION()")
    } else {
        return Ok(None);
    };
    let revision =
        read_uint256_return(provider, implementation_address, rev_fn, block_number).await?;
    Ok(Some(ProxyCreationResolution {
        name: name.to_string(),
        address: checksum(proxy_address),
        revision: discount_to_i64(revision),
    }))
}

/// ASCII-decode a bytes32 + strip the trailing NUL bytes. Port of the Python's
/// `contract_id_bytes.decode("ascii").strip("\\x00")` (the `AddressSet` event's
/// `id` topic). The bytes2 is left-aligned ASCII (the protocol writes the
/// contract name into the bytes32 + right-pads with zeros).
fn strip_trailing_nulls_from_ascii(id: &alloy::primitives::B256) -> String {
    let bytes = id.as_slice();
    let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
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
        if let Some(s) = decode_dynamic_string(&ret) {
            return Some(s);
        }
        // Fallback: bytes32 decode (older tokens). Take the first 32 bytes +
        // strip the trailing NULs.
        if ret.len() >= 32 {
            let s = String::from_utf8_lossy(&ret[..32])
                .trim_end_matches('\0')
                .to_string();
            if !s.is_empty() {
                return Some(s);
            }
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

#[expect(clippy::expect_used, clippy::panic)]
#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, Log as AlloyLog, B256};
    use alloy::rpc::types::Log;
    use degenbot_db::connection::DegenbotDb;
    use degenbot_decoders::aave_event_decoder::{
        AaveV3DiscountRateStrategyUpdatedEvent, AaveV3DiscountTokenUpdatedEvent,
    };
    use rusqlite::params;
    use std::path::Path;

    /// An in-memory writeable DB seeded with: a market (id=1, chain=1) + 1
    /// asset (underlying `0xUND`, aToken `0xATK`, vToken `0xVTK`) + 1 user
    /// (`0xUSR`, gho_discount=15) + a PRICE_ORACLE contract.
    fn db_seeded() -> (DegenbotDb, Vec<u8>) {
        let (db, _state) = DegenbotDb::open_for_writes(Path::new(":memory:")).unwrap();
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
             VALUES (1, 1, 'm', 1, NULL)",
            [],
        )
        .unwrap();
        // 3 erc20 tokens at stable addresses.
        let underlying =
            Address::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let a_token = Address::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);
        let v_token = Address::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3]);
        let addr_str = |a: Address| degenbot_core::address_utils::address_to_checksum_string(&a);
        for (id, a) in [(1_i64, underlying), (2, a_token), (3, v_token)] {
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
                params![id, addr_str(a)],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO aave_v3_assets \
                (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                 v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                 borrow_index, borrow_rate) \
             VALUES (1, 1, 1, 2, 1, 3, 2, '0', '0', '0', '0')",
            [],
        )
        .unwrap();
        let user_addr = Address::repeat_byte(0x42);
        conn.execute(
            "INSERT INTO aave_v3_users (market_id, address, e_mode, gho_discount, isolation_mode_debt) \
             VALUES (1, ?1, 0, 15, '0')",
            params![addr_str(user_addr)],
        )
        .unwrap();
        // PRICE_ORACLE contract.
        let oracle_addr = Address::repeat_byte(0x99);
        conn.execute(
            "INSERT INTO aave_v3_contracts (id, market_id, name, address, revision) \
             VALUES (1, 1, 'PRICE_ORACLE', ?1, 1)",
            params![addr_str(oracle_addr)],
        )
        .unwrap();
        drop(conn);
        // Return the underlying address bytes for test fixture building.
        let _ = (underlying, a_token, v_token);
        (db, vec![])
    }

    #[test]
    fn encode_single_address_call_is_selector_plus_padded_address() {
        let addr = Address::from([0xaa; 20]);
        let calldata = encode_single_address_call("getSourceOfAsset(address)", &addr);
        assert_eq!(calldata.len(), 36, "4-byte selector + 32-byte address");
        // the address is in the last 20 bytes (left-padded by 12 zeros).
        assert_eq!(&calldata[16..36], addr.as_slice());
        // the selector is the first 4 bytes of keccak256("getSourceOfAsset(address)").
        let expected_selector = &keccak256("getSourceOfAsset(address)".as_bytes())[..4];
        assert_eq!(&calldata[0..4], expected_selector);
    }

    #[test]
    fn encode_no_arg_call_is_selector_only() {
        let calldata = encode_no_arg_call("ATOKEN_REVISION()");
        assert_eq!(calldata.len(), 4);
        let expected = &keccak256("ATOKEN_REVISION()".as_bytes())[..4];
        assert_eq!(&calldata[..], expected);
    }

    #[test]
    fn word0_to_u256_decodes_first_32_bytes() {
        let mut bytes = vec![0u8; 64];
        bytes[29] = 0x01;
        bytes[30] = 0x02;
        bytes[31] = 0x03;
        let v = word0_to_u256(&bytes).unwrap();
        assert_eq!(v, U256::from(0x01_0203));
    }

    #[test]
    fn word0_to_u256_returns_none_for_short_return() {
        assert!(word0_to_u256(&[0u8; 16]).is_none());
    }

    #[test]
    fn decode_address_return_zero_address_yields_none() {
        let ret = vec![0u8; 32];
        assert!(decode_address_return(&ret).is_none());
    }

    #[test]
    fn decode_address_return_nonzero_yields_checksummed() {
        let mut ret = vec![0u8; 32];
        ret[31] = 0x42;
        let s = decode_address_return(&ret).unwrap();
        assert!(s.starts_with("0x"));
        assert_eq!(s.len(), 42);
        // the last byte of the address is 0x42.
        assert!(s.ends_with("42"));
    }

    #[test]
    fn eip_1967_slot_matches_canonical_constant() {
        // The canonical EIP-1967 slot is
        // 0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc.
        let mut expected = [0u8; 32];
        // parse the hex.
        let hex = "360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc";
        for (i, byte) in hex.as_bytes().chunks(2).enumerate() {
            expected[i] = u8::from_str_radix(std::str::from_utf8(byte).unwrap(), 16).unwrap();
        }
        assert_eq!(
            EIP_1967_IMPLEMENTATION_SLOT,
            U256::from_be_bytes::<32>(expected),
            "the EIP-1967 implementation slot is the canonically-fixed value"
        );
    }

    #[test]
    fn build_discount_snapshot_path1_db_cache_reads_gho_discount() {
        // path #1: the user exists in aave_v3_users with gho_discount=15.
        // The snapshot should hold that value without any RPC (no provider
        // needed). We build a Mint log for the GHO vToken + the user.
        let (db, _seed) = db_seeded();
        let conn = db.lock();
        let vtoken_addr =
            Address::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3]);
        let user_addr = Address::repeat_byte(0x42);
        // AAVE_MINT_TOPIC — the mint event with topic[2] = onBehalfOf (the user).
        let mint_topic = degenbot_decoders::aave_event_decoder::AAVE_MINT_TOPIC;
        let inner = AlloyLog::new_unchecked(
            vtoken_addr,
            vec![mint_topic, B256::ZERO, pad_address(&user_addr)],
            Bytes::new(),
        );
        let log = Log {
            inner,
            block_hash: None,
            block_number: Some(100),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: Some(0),
            removed: false,
        };
        // vtoken_revision = 2 (< 4, so discount_supported); but path #1
        // resolves before the gate is checked. Build the snapshot via tokio.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let provider = dummy_provider(&rt);
        let snapshot = rt
            .block_on(build_discount_snapshot(
                // no live provider — path #1 doesn't RPC.
                &provider,
                &[&log],
                100,
                Some(vtoken_addr),
                Some(2),
                1,
                &conn,
            ))
            .unwrap();
        assert_eq!(snapshot.get(&user_addr), Some(&U256::from(15)));
        drop(conn);
    }

    #[test]
    fn build_discount_snapshot_no_vtoken_returns_empty() {
        let (db, _) = db_seeded();
        let conn = db.lock();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let provider = dummy_provider(&rt);
        let snapshot = rt
            .block_on(build_discount_snapshot(
                &provider,
                &[],
                100,
                None,
                Some(2),
                1,
                &conn,
            ))
            .unwrap();
        assert!(snapshot.is_empty());
    }

    #[test]
    fn build_discount_snapshot_v4_plus_zeros_path3() {
        // path #3: vtoken_revision >= 4 → discount deprecated → 0. The user
        // doesn't exist in DB (path #1 fails) → path #3 (not path #2 RPC).
        let (db, _seed) = db_seeded();
        let conn = db.lock();
        let vtoken_addr =
            Address::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3]);
        let fresh_user = Address::repeat_byte(0x77);
        let mint_topic = degenbot_decoders::aave_event_decoder::AAVE_MINT_TOPIC;
        let inner = AlloyLog::new_unchecked(
            vtoken_addr,
            vec![mint_topic, B256::ZERO, pad_address(&fresh_user)],
            Bytes::new(),
        );
        let log = Log {
            inner,
            block_hash: None,
            block_number: Some(100),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: Some(0),
            removed: false,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let provider = dummy_provider(&rt);
        let snapshot = rt
            .block_on(build_discount_snapshot(
                &provider,
                &[&log],
                100,
                Some(vtoken_addr),
                Some(4), // V4+ — discount deprecated.
                1,
                &conn,
            ))
            .unwrap();
        assert_eq!(snapshot.get(&fresh_user), Some(&U256::ZERO));
        drop(conn);
    }

    /// A dummy `AlloyProvider` pointing at a dead URL. Used for path #1 / #3
    /// tests (which short-circuit before any `eth_call`). Path #2 tests need
    /// a live node + are `#[ignore]`-gated.
    fn dummy_provider(rt: &tokio::runtime::Runtime) -> AlloyProvider {
        // Construct pointing at a dead URL; the path #1 / #3 tests never RPC.
        rt.block_on(AlloyProvider::new("http://127.0.0.1:1", 1))
            .expect("a dead-URL provider constructs without contact")
    }

    // ── the 8 sync dispatch handler tests ───────────────────────────────

    #[test]
    fn dispatch_reserve_data_updated_resolves_asset_id() {
        let (db, _) = db_seeded();
        let conn = db.lock();
        let underlying =
            Address::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let ev = degenbot_decoders::aave_event_decoder::AaveV3ReserveDataUpdatedEvent {
            pool_address: Address::ZERO,
            reserve: underlying,
            liquidity_rate: U256::from(100),
            stable_borrow_rate: U256::ZERO,
            variable_borrow_rate: U256::from(200),
            liquidity_index: U256::from(300),
            variable_borrow_index: U256::from(400),
        };
        let chunk_ev = dispatch_reserve_data_updated(1, 100, &ev, &conn).unwrap();
        match chunk_ev {
            AaveChunkEvent::ReserveDataUpdated {
                asset_id,
                liquidity_rate,
                variable_borrow_rate,
                liquidity_index,
                variable_borrow_index,
                block_number,
            } => {
                assert_eq!(asset_id, 1);
                assert_eq!(liquidity_rate, U256::from(100));
                assert_eq!(variable_borrow_rate, U256::from(200));
                assert_eq!(liquidity_index, U256::from(300));
                assert_eq!(variable_borrow_index, U256::from(400));
                assert_eq!(block_number, 100);
            }
            other => panic!("expected ReserveDataUpdated, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_reserve_data_updated_missing_asset_errors() {
        let (db, _) = db_seeded();
        let conn = db.lock();
        let ev = degenbot_decoders::aave_event_decoder::AaveV3ReserveDataUpdatedEvent {
            pool_address: Address::ZERO,
            reserve: Address::repeat_byte(0xff), // no asset at this underlying.
            liquidity_rate: U256::ZERO,
            stable_borrow_rate: U256::ZERO,
            variable_borrow_rate: U256::ZERO,
            liquidity_index: U256::ZERO,
            variable_borrow_index: U256::ZERO,
        };
        assert!(dispatch_reserve_data_updated(1, 100, &ev, &conn).is_err());
    }

    /// I2RHGP Fix 2b: a fresh-market cold-boot can see `AssetSourceUpdated`
    /// for a not-yet-initialized reserve (it precedes `ReserveInitialized`
    /// within the same tx on mainnet block 16496792). The handler must SKIP
    /// (return `Ok(None)`) rather than error — the later `ReserveInitialized`
    /// creates the asset + its `getSourceOfAsset` RPC recovers the source.
    #[test]
    fn dispatch_asset_source_updated_missing_asset_skips_returns_none() {
        let (db, _) = db_seeded();
        let conn = db.lock();
        let ev = degenbot_decoders::aave_event_decoder::AaveV3AssetSourceUpdatedEvent {
            oracle_address: Address::repeat_byte(0x99),
            asset: Address::repeat_byte(0xff), // no asset at this underlying.
            source: Address::repeat_byte(0x81),
        };
        let out = dispatch_asset_source_updated(1, &ev, &conn).unwrap();
        assert!(
            out.is_none(),
            "expected skip (None) for missing asset, got {out:?}"
        );
    }

    /// The unchanged path: when the asset EXISTS, `AssetSourceUpdated`
    /// dispatches normally (the later-update case). Guards against Fix 2b
    /// over-skipping.
    #[test]
    fn dispatch_asset_source_updated_existing_asset_dispatches() {
        let (db, _) = db_seeded();
        let conn = db.lock();
        let underlying =
            Address::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let ev = degenbot_decoders::aave_event_decoder::AaveV3AssetSourceUpdatedEvent {
            oracle_address: Address::repeat_byte(0x99),
            asset: underlying, // db_seeded's asset id 1.
            source: Address::repeat_byte(0x81),
        };
        let out = dispatch_asset_source_updated(1, &ev, &conn).unwrap();
        match out {
            Some(AaveChunkEvent::AssetSourceUpdated {
                asset_id,
                source_address,
            }) => {
                assert_eq!(asset_id, 1);
                assert!(source_address.starts_with("0x81"));
            }
            other => panic!("expected Some(AssetSourceUpdated), got {other:?}"),
        }
    }

    #[test]
    fn dispatch_user_e_mode_set_creates_user_and_sets_emode() {
        let (db, _) = db_seeded();
        let conn = db.lock();
        let fresh_user = Address::repeat_byte(0xab);
        let ev = degenbot_decoders::aave_event_decoder::AaveV3UserEModeSetEvent {
            pool_address: Address::ZERO,
            user: fresh_user,
            category_id: 2,
        };
        let chunk_ev = dispatch_user_e_mode_set(1, 100, &ev, &conn).unwrap();
        match chunk_ev {
            AaveChunkEvent::UserEModeSet { e_mode, .. } => assert_eq!(e_mode, 2),
            other => panic!("expected UserEModeSet, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_price_oracle_updated_emits_new_address() {
        let new_oracle = Address::repeat_byte(0x55);
        let ev = degenbot_decoders::aave_event_decoder::AaveV3ConfigAddressPairEvent {
            address_provider: Address::ZERO,
            old_address: Address::ZERO,
            new_address: new_oracle,
        };
        let chunk_ev = dispatch_price_oracle_updated(1, &ev).unwrap();
        match chunk_ev {
            AaveChunkEvent::PriceOracleUpdated {
                market_id,
                new_oracle_address,
            } => {
                assert_eq!(market_id, 1);
                assert_eq!(new_oracle_address.len(), 42);
            }
            other => panic!("expected PriceOracleUpdated, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_e_mode_category_added_emits_full_fields() {
        let oracle = Address::repeat_byte(0x33);
        let ev = degenbot_decoders::aave_event_decoder::AaveV3EModeCategoryAddedEvent {
            pool_configurator_address: Address::ZERO,
            category_id: 1,
            ltv: U256::from(8000),
            liquidation_threshold: U256::from(8500),
            liquidation_bonus: U256::from(10500),
            oracle,
            label: "stablecoins".to_string(),
        };
        let chunk_ev = dispatch_e_mode_category_added(1, &ev).unwrap();
        match chunk_ev {
            AaveChunkEvent::EModeCategoryAdded {
                market_id,
                category_id,
                ltv,
                liquidation_threshold,
                liquidation_bonus,
                price_source,
                label,
            } => {
                assert_eq!(market_id, 1);
                assert_eq!(category_id, 1);
                assert_eq!(ltv, 8000);
                assert_eq!(liquidation_threshold, 8500);
                assert_eq!(liquidation_bonus, 10500);
                assert!(price_source.is_some(), "oracle is non-zero → Some");
                assert_eq!(label, "stablecoins");
            }
            other => panic!("expected EModeCategoryAdded, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_e_mode_category_added_zero_oracle_yields_zero_address_string() {
        // Parity-directed behavior: when EModeCategoryAdded emits
        // `oracle = Address::ZERO` (the canonical Aave V3 "no specific price
        // oracle" sentinel), Rust MUST emit `price_source =
        // Some("0x0000000000000000000000000000000000000000")` — matching Python's
        // `_process_e_mode_category_added_event` (event_handlers.py:269,277)
        // which, via web3's truthiness check on the decoded address string,
        // stores `get_checksum_address(zero_address)`.
        // Without this, Rust stores NULL — a 1-row byte-divergence vs Python
        // gold on `aave_v3_emode_categories.price_source` (surfaced by the
        // full-schema-diff at the 16591070 parity gate).
        let ev = degenbot_decoders::aave_event_decoder::AaveV3EModeCategoryAddedEvent {
            pool_configurator_address: Address::ZERO,
            category_id: 1,
            ltv: U256::from(8000),
            liquidation_threshold: U256::from(8500),
            liquidation_bonus: U256::from(10500),
            oracle: Address::ZERO,
            label: String::new(),
        };
        let chunk_ev = dispatch_e_mode_category_added(1, &ev).unwrap();
        match chunk_ev {
            AaveChunkEvent::EModeCategoryAdded { price_source, .. } => {
                assert_eq!(
                    price_source.as_deref(),
                    Some("0x0000000000000000000000000000000000000000"),
                    "zero oracle → Some(zero-address checksum string), matching Python gold"
                );
            }
            other => panic!("expected EModeCategoryAdded, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_discount_percent_updated_clamps_to_i64() {
        let (db, _) = db_seeded();
        let conn = db.lock();
        let user = Address::repeat_byte(0x42);
        let ev = degenbot_decoders::aave_event_decoder::AaveV3DiscountPercentUpdatedEvent {
            v_token_address: Address::ZERO,
            user,
            old_discount_percent: U256::from(10),
            new_discount_percent: U256::from(25),
        };
        let chunk_ev = dispatch_discount_percent_updated(1, 100, &ev, &conn).unwrap();
        match chunk_ev {
            AaveChunkEvent::GhoDiscountPercentUpdated {
                new_discount_percent,
                ..
            } => {
                assert_eq!(new_discount_percent, 25);
            }
            other => panic!("expected GhoDiscountPercentUpdated, got {other:?}"),
        }
    }

    #[test]
    fn is_discount_supported_gate_is_revision_lt_4() {
        // §4.2-parity pin: the gate is `revision < 4` (matches the Python
        // `revision is not None and revision < GHO_DISCOUNT_DEPRECATION_REVISION`).
        let _: u32 = GHO_DISCOUNT_DEPRECATION_REVISION;
        let supported = [1_u32, 2, 3]
            .iter()
            .all(|r| *r < GHO_DISCOUNT_DEPRECATION_REVISION);
        let deprecated = [4_u32, 5, 6]
            .iter()
            .all(|r| *r >= GHO_DISCOUNT_DEPRECATION_REVISION);
        assert!(supported, "V1/V2/V3 support discount");
        assert!(deprecated, "V4+ deprecates");
    }

    // ── 6SWY4R-2b: the 6 missing-variant pure-decode tests ──────────────

    #[test]
    fn strip_trailing_nulls_from_ascii_decodes_pool() {
        // The AddressSet `id` = right-padded ASCII bytes32 (§4.2 finding: NOT
        // keccak256). The Python: `contract_id_bytes.decode("ascii").strip("\0")`.
        let mut id = [0u8; 32];
        id[..4].copy_from_slice(b"POOL");
        assert_eq!(strip_trailing_nulls_from_ascii(&B256::from(id)), "POOL");
    }

    #[test]
    fn strip_trailing_nulls_from_ascii_decodes_long_string() {
        let mut id = [0u8; 32];
        id[..17].copy_from_slice(b"POOL_CONFIGURATOR");
        assert_eq!(
            strip_trailing_nulls_from_ascii(&B256::from(id)),
            "POOL_CONFIGURATOR"
        );
    }

    #[test]
    fn strip_trailing_nulls_from_ascii_all_zeros_yields_empty() {
        assert_eq!(strip_trailing_nulls_from_ascii(&B256::ZERO), "");
    }

    #[test]
    fn proxy_id_consts_are_right_padded_ascii_not_keccak() {
        // §4.2 finding: the Python's
        // `eth_abi.abi.encode(["bytes32"], [b"POOL"])` is right-padded ASCII,
        // NOT `keccak256("POOL")`. Pin this so a future refactor doesn't
        // accidentally switch to keccak256 (which would break the proxy-id
        // match for the Aave PoolAddressesProvider).
        assert_eq!(&POOL_PROXY_ID[..4], b"POOL");
        assert!(POOL_PROXY_ID[4..].iter().all(|&b| b == 0));
        assert_eq!(&POOL_CONFIGURATOR_PROXY_ID[..17], b"POOL_CONFIGURATOR");
        assert!(POOL_CONFIGURATOR_PROXY_ID[17..].iter().all(|&b| b == 0));
    }

    #[test]
    fn decode_dynamic_string_decodes_name() {
        // ABI dynamic string: 32-byte offset (0x20) + 32-byte length + data
        // padded to a 32-byte multiple.
        let mut ret = vec![0u8; 96];
        // offset = 0x20 at [0..32].
        ret[31] = 0x20;
        // length = 4 at [32..64].
        ret[63] = 4;
        // data = "DAI" at [64..68] (the length is 4 → "DAI\0"? no, length=4 →
        // 4 bytes; use "DAI\0" → but we want a printable. Use "WETH" = 4 bytes).
        ret[64..68].copy_from_slice(b"WETH");
        assert_eq!(decode_dynamic_string(&ret), Some("WETH".to_string()));
    }

    #[test]
    fn decode_dynamic_string_returns_none_for_short_return() {
        // A bytes32-only return (older tokens) is too short for the dynamic
        // string decode → None (the bytes32 fallback handles it).
        assert_eq!(decode_dynamic_string(&[0u8; 32]), None);
    }

    #[test]
    fn decode_dynamic_string_returns_none_for_non_0x20_offset() {
        let mut ret = vec![0u8; 96];
        ret[31] = 0x40; // offset ≠ 0x20 → malformed.
        ret[63] = 4;
        ret[64..68].copy_from_slice(b"WETH");
        assert_eq!(decode_dynamic_string(&ret), None);
    }

    #[test]
    fn decode_dynamic_string_returns_none_for_length_overrun() {
        let mut ret = vec![0u8; 96];
        ret[31] = 0x20;
        ret[63] = 0xff; // length overruns the return.
        assert_eq!(decode_dynamic_string(&ret), None);
    }

    // ── ULDUAC: discount-event emitter-validation guard (divergence #2) ────────
    // The dispatch returns Ok(None) when (a) the GHO asset has no vToken FK
    // (the Python's `gho_asset.v_token is None` guard) or (b) the decoded
    // emitter (log.address) doesn't match gho_asset.v_token_address (the
    // Python's `v_token.address != event["address"]` guard). Only an emitting
    // log from the matching vToken yields a chunk event.
    fn sample_gho_asset(v_token_address: Option<&str>) -> AaveGhoAsset {
        AaveGhoAsset {
            id: 1,
            token_id: 10,
            v_token_id: Some(11),
            gho_token_address: Some(checksum(&Address::ZERO)),
            v_token_address: v_token_address.map(str::to_string),
            v_gho_discount_rate_strategy: None,
            v_gho_discount_token: None,
        }
    }

    /// Discount-config gap (fork A, post-WCRWL3): the per-tx `gho_asset`
    /// snapshot is taken BEFORE the same-tx `ReserveInitialized` (which links
    /// `v_token_id`) applies. The `dispatch_discount_token_updated` guard
    /// checks `v_token_address` (resolved from the FK) — with the stale
    /// snapshot it's `None` → the INITIAL-set event (old=None→stkAAVE) is
    /// SKIPPED. The `_with_fresh_resolution` wrapper re-resolves from `conn`
    /// (read-your-own-writes, mirrors Python's live ORM) so the guard sees
    /// the just-linked FK. On mainnet both events fire in tx 0xae8e542d… at
    /// block 17699249 (ReserveInitialized logIdx 85, DiscountTokenUpdated
    /// logIdx 91) — same tx, so the per-tx snapshot is necessarily stale.
    #[test]
    fn discount_token_updated_fresh_resolution_sees_intra_tx_v_token_link() {
        let (db, _state) = DegenbotDb::open_for_writes(Path::new(":memory:")).unwrap();
        let gho_vtoken = Address::from([0xa0; 20]);
        let new_discount_token = Address::from([0xc; 20]);
        let addr_str = |a: Address| degenbot_core::address_utils::address_to_checksum_string(&a);
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (1, 1, 'm', 1, NULL)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES \
                 (10, 1, ?1), (11, 1, ?2)",
                params![addr_str(Address::ZERO), addr_str(gho_vtoken)],
            )
            .unwrap();
            // aave_gho_tokens row with v_token_id=NULL — the pre-
            // ReserveInitialized coldboot state.
            conn.execute(
                "INSERT INTO aave_gho_tokens (id, token_id, v_token_id) VALUES (1, 10, NULL)",
                [],
            )
            .unwrap();
        }
        // The STALE per-tx snapshot: resolved BEFORE ReserveInitialized
        // applied → v_token_address is None (v_token_id is NULL).
        let conn = db.lock();
        let stale = DegenbotDb::fetch_aave_gho_asset_on_conn(&conn, 1)
            .unwrap()
            .expect("GHO asset row seeded");
        assert_eq!(
            stale.v_token_address, None,
            "pre-link snapshot: v_token_address None (v_token_id NULL)"
        );
        // Simulate the same-tx ReserveInitialized apply (links v_token_id=11).
        conn.execute(
            "UPDATE aave_gho_tokens SET v_token_id = 11 WHERE id = 1",
            [],
        )
        .unwrap();
        let ev = AaveV3DiscountTokenUpdatedEvent {
            v_token_address: gho_vtoken,       // the GHO vToken emitter
            old_discount_token: Address::ZERO, // the INITIAL set (old=None)
            new_discount_token,
        };
        // The wrapper re-resolves from conn → sees the link → guard passes.
        let r = dispatch_discount_token_updated_with_fresh_resolution(Some(&stale), &ev, &conn, 1)
            .unwrap();
        match r {
            Some(AaveChunkEvent::GhoDiscountTokenUpdated {
                gho_token_id,
                new_discount_token: ndt,
            }) => {
                assert_eq!(gho_token_id, 1);
                assert_eq!(ndt.as_deref(), Some(addr_str(new_discount_token).as_str()));
            }
            other => panic!("expected Some(GhoDiscountTokenUpdated), got {other:?}"),
        }
    }

    #[test]
    fn dispatch_discount_token_updated_returns_none_when_emitter_mismatches() {
        let gho_vtoken = Address::from([0xa0; 20]);
        let gho_asset = sample_gho_asset(Some(&checksum(&gho_vtoken)));
        // An off-market emitter (any other contract) — the chain-wide fetch can
        // surface these; the Python guard drops them.
        let off_emitter = Address::from([0xb; 20]);
        let ev = AaveV3DiscountTokenUpdatedEvent {
            v_token_address: off_emitter,
            old_discount_token: Address::ZERO,
            new_discount_token: Address::from([0xc; 20]),
        };
        assert!(dispatch_discount_token_updated(&gho_asset, &ev)
            .unwrap()
            .is_none());
    }

    #[test]
    fn dispatch_discount_token_updated_returns_none_when_vtoken_fk_missing() {
        // gho_asset.v_token is None — the Python's first guard.
        let gho_asset = sample_gho_asset(None);
        let ev = AaveV3DiscountTokenUpdatedEvent {
            v_token_address: Address::from([0xa0; 20]),
            old_discount_token: Address::ZERO,
            new_discount_token: Address::from([0xc; 20]),
        };
        assert!(dispatch_discount_token_updated(&gho_asset, &ev)
            .unwrap()
            .is_none());
    }

    #[test]
    fn dispatch_discount_token_updated_emits_when_emitter_matches() {
        let gho_vtoken = Address::from([0xa0; 20]);
        let gho_asset = sample_gho_asset(Some(&checksum(&gho_vtoken)));
        let new_tok = Address::from([0xc; 20]);
        let ev = AaveV3DiscountTokenUpdatedEvent {
            v_token_address: gho_vtoken,
            old_discount_token: Address::ZERO,
            new_discount_token: new_tok,
        };
        match dispatch_discount_token_updated(&gho_asset, &ev).unwrap() {
            Some(AaveChunkEvent::GhoDiscountTokenUpdated {
                gho_token_id,
                new_discount_token,
            }) => {
                assert_eq!(gho_token_id, 1);
                assert_eq!(
                    new_discount_token.as_deref(),
                    Some(checksum(&new_tok).as_str())
                );
            }
            other => panic!("expected Some(GhoDiscountTokenUpdated), got {other:?}"),
        }
    }

    #[test]
    fn dispatch_discount_rate_strategy_updated_returns_none_when_emitter_mismatches() {
        let gho_vtoken = Address::from([0xa0; 20]);
        let gho_asset = sample_gho_asset(Some(&checksum(&gho_vtoken)));
        let off_emitter = Address::from([0xb; 20]);
        let ev = AaveV3DiscountRateStrategyUpdatedEvent {
            v_token_address: off_emitter,
            old_strategy: Address::ZERO,
            new_strategy: Address::from([0xd; 20]),
        };
        assert!(dispatch_discount_rate_strategy_updated(&gho_asset, &ev)
            .unwrap()
            .is_none());
    }

    #[test]
    fn dispatch_discount_rate_strategy_updated_emits_when_emitter_matches() {
        let gho_vtoken = Address::from([0xa0; 20]);
        let gho_asset = sample_gho_asset(Some(&checksum(&gho_vtoken)));
        let new_strat = Address::from([0xd; 20]);
        let ev = AaveV3DiscountRateStrategyUpdatedEvent {
            v_token_address: gho_vtoken,
            old_strategy: Address::ZERO,
            new_strategy: new_strat,
        };
        match dispatch_discount_rate_strategy_updated(&gho_asset, &ev).unwrap() {
            Some(AaveChunkEvent::GhoDiscountRateStrategyUpdated {
                gho_token_id,
                new_strategy,
            }) => {
                assert_eq!(gho_token_id, 1);
                assert_eq!(new_strategy.as_deref(), Some(checksum(&new_strat).as_str()));
            }
            other => panic!("expected Some(GhoDiscountRateStrategyUpdated), got {other:?}"),
        }
    }
}

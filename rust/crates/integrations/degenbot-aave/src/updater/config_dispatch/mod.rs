//! The config-event direct-decode dispatch for the Rust-owned Aave V3 chunk
//! loop (`run_aave_update`).
//!
//! This module owns the orchestrator's per-transaction config-event handling:
//! loop over a tx's logs, decode each via
//! [`degenbot_decoders::aave_event_decoder::decode_aave_log`], + for the 10
//! enum-covered config event types, resolve ids via the `DegenbotDb`
//! substrate (`get_or_create_*_on_conn` / `lookup_asset_by_*_on_conn`) +
//! emit [`AaveChunkEvent`] variants (mirror of Python
//! `transaction_processor.py::_process_transaction` L328-430 — the Phase 1+2
//! config-event dispatch). The apply step (`-3`'s `apply_aave_chunk_writes_on_conn`)
//! consumes the emitted variants on the chunk `Transaction` (chunk
//! atomicity).
//!
//! # Scope (this module)
//!
//! The dispatch loop + the shared decode/RPC helpers every resolver rides
//! (the checksum/calldata encoders, the batch-or-direct multicall seam, the
//! word/string decoders). [`dispatch_config_events`] routes each decoded
//! event to its resolver + applies each emitted event to the chunk `conn`
//! AS it is dispatched (logIndex order), so a later event's dispatch sees an
//! earlier event's apply — surface-table row 6
//! (`tests/fixtures/cassettes/wave4/UPGRADE-MAP.md` §(A)).
//!
//! The resolvers live in the child modules, one seam each:
//! - [`context`]: the per-chunk dispatch context ([`ChunkContext`] + the
//!   block-pinned [`RevisionMemo`]) + the apply→bump→dispatch ordering
//!   (rows 3, 7, 11).
//! - [`revision`]: the upgrade-transition revision resolvers — `Upgraded`,
//!   `PoolUpdated`/`PoolConfiguratorUpdated`, `ProxyCreated` (rows 4, 5, 11).
//! - [`discount`]: the GHO-discount channel — the per-tx snapshot pre-pass,
//!   the discount-config dispatchers, the stkAAVE transfer path (row 7's
//!   consumer side; §(C) W6).
//! - [`asset_config`]: the per-asset config handlers — the 8 sync
//!   dispatchers + the `ReserveInitialized`/`CollateralConfigurationChanged`
//!   RPC resolutions + the ERC20 metadata family (row 9).
//!
//! Six config events route through [`resolve_missing_variant_event`] —
//! `Upgraded`, `PoolUpdated`, `PoolConfiguratorUpdated`,
//! `PoolDataProviderUpdated`, `AddressSet`, `ProxyCreated` — the
//! upgrade-transition resolvers in [`revision`] + the pure-decode arms here.
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
//! # Chunk atomicity
//!
//! The dispatch takes `conn: &Connection` (the chunk's single `Transaction`).
//! All `get_or_create_*` / `lookup_*` substrate calls run on it — the
//! chunk-atomicity
//! invariant (ONE `Transaction` per chunk) holds. The dispatch emits
//! [`AaveChunkEvent`]s; the apply step (`-3`) consumes them on the same conn.
//! No `Transaction` lifecycle here — this file owns neither `commit` nor
//! `drop`.

mod asset_config;
mod context;
mod discount;
mod revision;

use super::run::substrate::ChunkSubstrate;
use crate::run::AaveChunkEvent;
use alloy::primitives::{keccak256, Address, Bytes, U256};
use degenbot_db::aave::AaveGhoAsset;
use degenbot_db::{DbError, DegenbotDb};
use degenbot_decoders::aave_event_decoder::{decode_aave_log, DecodedAaveEvent};
use degenbot_rpc::multicall3::{multicall3_batch, MulticallResult};
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::Connection;

pub(crate) use asset_config::{
    decode_dynamic_string, fetch_erc20_metadata, resolve_reserve_initialized,
};
pub use asset_config::{
    dispatch_asset_collateral_in_emode_changed, dispatch_asset_source_updated,
    dispatch_e_mode_asset_category_changed, dispatch_e_mode_category_added,
    dispatch_price_oracle_updated, dispatch_reserve_data_updated,
    dispatch_reserve_used_as_collateral, dispatch_user_e_mode_set,
    resolve_collateral_configuration,
};
pub(crate) use context::{ChunkContext, ChunkSpan, RevisionMemo};
pub(crate) use discount::dispatch_stk_aave_transfer_with_backfill;
pub use discount::{
    build_discount_snapshot, dispatch_discount_percent_updated,
    dispatch_discount_rate_strategy_updated, dispatch_discount_token_updated,
    dispatch_stk_aave_transfer, refresh_gho_discount,
};
use discount::{
    dispatch_discount_rate_strategy_updated_with_fresh_resolution,
    dispatch_discount_token_updated_with_fresh_resolution,
};
pub(crate) use revision::{match_proxy_id, ProxyCreationResolution};
use revision::{resolve_contract_revision_updated, resolve_upgraded};

/// The GHO-discount deprecation revision (V4+). Mirrors the Python
/// `GHO_DISCOUNT_DEPRECATION_REVISION = 4`. At vToken revision ≥ this value,
/// the discount mechanism is deprecated → the effective discount is always 0
/// (`build_discount_snapshot` path #3; `is_discount_supported` gate).
const GHO_DISCOUNT_DEPRECATION_REVISION: u32 = 4;

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

/// The 4-byte selector of a revision signature (the memo key's middle lane).
fn revision_selector(sig: &str) -> [u8; 4] {
    let hash = keccak256(sig.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
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
pub(crate) async fn dispatch_config_events(
    ctx: &mut ChunkContext<'_>,
    tx_logs: &[&alloy::rpc::types::Log],
    conn: &Connection,
    gho_asset: Option<&AaveGhoAsset>,
    block_number: u64,
    substrate: &mut ChunkSubstrate,
) -> Result<Vec<AaveChunkEvent>, ConfigDispatchError> {
    let mut events = Vec::new();
    for log in tx_logs {
        let Some(decoded) = decode_aave_log(log) else {
            continue; // not an Aave event (may be an unrelated ERC20/etc.).
        };
        if let Some(ev) = dispatch_single_config_event(
            &decoded,
            ctx,
            conn,
            gho_asset,
            block_number,
            &mut *substrate,
        )
        .await?
        {
            // Intra-dispatch apply: apply each config event
            // to `conn` AS it's dispatched (in logIndex order), so a later
            // config event's dispatch sees an earlier event's apply — e.g.
            // `CollateralConfigurationChanged` (logIdx 419) sees the asset
            // `ReserveInitialized` (logIdx 413) just created. Matches the
            // Python's per-event apply (intra-tx read-your-own-writes). The
            // chunk loop's batch apply (the former step (d)) is removed.
            ctx.apply_events(conn, std::slice::from_ref(&ev), &mut *substrate)?;
            events.push(ev);
        }
    }
    Ok(events)
}

/// Resolve the `PRICE_ORACLE` contract address for `ReserveInitialized`
/// dispatch. The spec's `cached` `oracle_address` is
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
async fn dispatch_single_config_event(
    decoded: &DecodedAaveEvent,
    ctx: &mut ChunkContext<'_>,
    conn: &Connection,
    gho_asset: Option<&AaveGhoAsset>,
    block_number: u64,
    substrate: &mut ChunkSubstrate,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let ev = match decoded {
        // ── the 8 sync handlers (no RPC) ──
        DecodedAaveEvent::ReserveDataUpdated(ev) => Some(dispatch_reserve_data_updated(
            market_id,
            block_number,
            ev,
            conn,
            substrate,
        )?),
        DecodedAaveEvent::UserEModeSet(ev) => Some(dispatch_user_e_mode_set(
            market_id,
            block_number,
            ev,
            conn,
            substrate,
        )?),
        DecodedAaveEvent::ReserveUsedAsCollateralEnabled(ev) => {
            Some(dispatch_reserve_used_as_collateral(
                market_id, ev.reserve, ev.user, true, conn, substrate,
            )?)
        }
        DecodedAaveEvent::ReserveUsedAsCollateralDisabled(ev) => {
            Some(dispatch_reserve_used_as_collateral(
                market_id, ev.reserve, ev.user, false, conn, substrate,
            )?)
        }
        DecodedAaveEvent::PriceOracleUpdated(ev) => {
            Some(dispatch_price_oracle_updated(market_id, ev)?)
        }
        DecodedAaveEvent::AssetSourceUpdated(ev) => {
            dispatch_asset_source_updated(market_id, ev, conn, substrate)?
        }
        DecodedAaveEvent::EModeCategoryAdded(ev) => {
            Some(dispatch_e_mode_category_added(market_id, ev)?)
        }
        DecodedAaveEvent::EModeAssetCategoryChanged(ev) => Some(
            dispatch_e_mode_asset_category_changed(market_id, ev, conn, substrate)?,
        ),
        DecodedAaveEvent::AssetCollateralInEModeChanged(ev) => Some(
            dispatch_asset_collateral_in_emode_changed(market_id, ev, conn, substrate)?,
        ),
        DecodedAaveEvent::DiscountPercentUpdated(ev) => Some(dispatch_discount_percent_updated(
            market_id,
            block_number,
            ev,
            conn,
            substrate,
        )?),
        DecodedAaveEvent::DiscountTokenUpdated(ev) => {
            dispatch_discount_token_updated_with_fresh_resolution(gho_asset, ev, conn, substrate)?
        }
        DecodedAaveEvent::DiscountRateStrategyUpdated(ev) => {
            dispatch_discount_rate_strategy_updated_with_fresh_resolution(
                gho_asset, ev, conn, substrate,
            )?
        }
        // ── the 2 async RPC handlers ──
        DecodedAaveEvent::CollateralConfigurationChanged(ev) => Some(
            resolve_collateral_configuration(
                ctx.provider(),
                ctx.pool_address(),
                ev,
                market_id,
                block_number,
                conn,
                substrate,
            )
            .await?,
        ),
        DecodedAaveEvent::ReserveInitialized(ev) => {
            let oracle = resolve_reserve_oracle_address(conn, market_id, ctx.oracle_address())?;
            Some(resolve_reserve_initialized(ctx, ev, oracle, gho_asset, block_number, conn).await?)
        }
        // ── stkAAVE Staked/Redeem semantic events: NO balance-mutation
        // dispatch. Crash #3: the prior design processed
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
                ctx.provider(),
                conn,
                market_id,
                block_number,
                ev,
                gho_asset,
                substrate,
            )
            .await?
        }
        // ── the 6 missing-variant config events ──────────────────
        // Delegated to `resolve_missing_variant_event` to keep this fn under
        // the 100-line `clippy::too_many_lines` limit.
        //
        // Operation events (Supply/Borrow/Mint/Burn/Transfer/...) + the
        // 6 missing-variant events both fall through to the `_` arm. The
        // `resolve_missing_variant_event` fn matches on the 6 missing-variant
        // variants; for operation events it returns `Ok(None)`.
        _ => {
            resolve_missing_variant_event(decoded, ctx, gho_asset, block_number, conn, substrate)
                .await?
        }
    };
    Ok(ev)
}

/// Resolve the 6 missing-variant config events. Extracted from
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
///   address → `ContractRevisionUpdated` (revision ONLY — the parity gate).
/// - `PoolDataProviderUpdated` — pure-decode (INSERT when old==zero, else
///   UPDATE-by-old-address).
/// - `AddressSet` — assert old==zero; ASCII-decode the bits32 id.
/// - `ProxyCreated` — match the id against the right-padded ASCII `b"POOL"`/
///   `b"POOL_CONFIGURATOR"`; RPC the revision on the impl address.
async fn resolve_missing_variant_event(
    decoded: &DecodedAaveEvent,
    ctx: &mut ChunkContext<'_>,
    gho_asset: Option<&AaveGhoAsset>,
    block_number: u64,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<Option<AaveChunkEvent>, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let ev = match decoded {
        DecodedAaveEvent::Upgraded(ev) => {
            Some(resolve_upgraded(ctx, ev, gho_asset, block_number, conn, substrate).await?)
        }
        DecodedAaveEvent::PoolUpdated(ev) => Some(
            resolve_contract_revision_updated(
                ctx,
                ev.new_address,
                "POOL",
                "POOL_REVISION()",
                block_number,
            )
            .await?,
        ),
        DecodedAaveEvent::PoolConfiguratorUpdated(ev) => Some(
            resolve_contract_revision_updated(
                ctx,
                ev.new_address,
                "POOL_CONFIGURATOR",
                "CONFIGURATOR_REVISION()",
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
            if let Some(resolved) = ctx
                .resolve_proxy_created(
                    &ev.id,
                    &ev.proxy_address,
                    &ev.implementation_address,
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

// ── the multicall batching seam ────────────────────────────────────────────

/// Run a set of same-block read-only `eth_call`s in ONE transport pass when
/// 2+ are pending, or as the exact single sequential `eth_call` when exactly
/// one is — the config dispatch's batching seam.
///
/// The batch rides [`degenbot_rpc::multicall3::multicall3_batch`]
/// (`aggregate3`, `allowFailure = true` — the liquidity verifier's idiom). A
/// failed sub-call is a HARD error here, never a silent default: the
/// sequential reads this replaces propagate an `eth_call` failure as
/// [`ConfigDispatchError::Rpc`] (the chunk rolls back), and the batch
/// preserves that contract — only the failure payload is coarser (Multicall3
/// reports revert/no-answer as `success = false` with empty return data; the
/// provider's per-call error body is not recoverable through the batch). A
/// batch of one degrades to the direct call so a single-call dispatch keeps
/// its exact wire shape — a 1-call `aggregate3` wrapper would change the
/// recorded request set for no round-trip gain.
///
/// # Errors
///
/// Propagates the direct `eth_call`'s error (the 1-call shape) or fails on
/// any batch sub-call that reverted or went unanswered.
async fn eth_calls_batched_or_direct(
    provider: &AlloyProvider,
    calls: &[(Address, Bytes)],
    block_number: u64,
) -> Result<Vec<Bytes>, ConfigDispatchError> {
    match calls {
        [] => Ok(Vec::new()),
        [(target, data)] => {
            let ret = provider
                .eth_call(target, data.clone(), Some(block_number))
                .await?;
            Ok(vec![ret])
        }
        many => {
            let results = multicall3_batch(provider, many, Some(block_number)).await?;
            let mut out = Vec::with_capacity(results.len());
            for (i, r) in results.into_iter().enumerate() {
                if !r.success {
                    return Err(ConfigDispatchError::Rpc(
                        degenbot_core::errors::ProviderError::RpcError {
                            code: -32000,
                            message: format!(
                                "multicall3 sub-call {i} of {} reverted or went unanswered                                  (target {}); the sequential reads would have failed the chunk                                  the same way",
                                many.len(),
                                checksum(&many[i].0)
                            ),
                        },
                    ));
                }
                out.push(r.return_data);
            }
            Ok(out)
        }
    }
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

/// Clamp a `U256` discount/revision percent to `i64` (the `aave_v3_users
/// .gho_discount` / `*_revision` column type). Mirrors the Python's implicit
/// `int(...)` (the Aave protocol caps discount at 100%, revisions at single
/// digits).
fn discount_to_i64(v: U256) -> i64 {
    v.to::<u64>().try_into().unwrap_or(i64::MAX)
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

/// The `(name|symbol)` decision over one field's batched `[lower, upper]`
/// selector results — the exact sequential order of
/// [`fetch_erc20_string_metadata`]: a failed lower result (the revert /
/// transport class) returns `None` WITHOUT consulting the upper spelling
/// (the sequential `.ok()?` early-return); a succeeded-but-undecodable lower
/// return falls through to the upper spelling; each spelling decodes the
/// dynamic `string` form before the bytes32 fallback.
fn string_field_from_batch(lower: &MulticallResult, upper: &MulticallResult) -> Option<String> {
    if !lower.success {
        return None;
    }
    decode_string_return(&lower.return_data).or_else(|| {
        if !upper.success {
            return None;
        }
        decode_string_return(&upper.return_data)
    })
}

/// The `decimals` decision over one field's batched `[lower, upper]` results
/// — mirrors [`fetch_erc20_decimals`]'s sequential order with the same
/// failed-lower early-`None`.
fn decimals_from_batch(lower: &MulticallResult, upper: &MulticallResult) -> Option<i64> {
    if !lower.success {
        return None;
    }
    word0_to_u256(&lower.return_data)
        .map(|v| v.to::<u64>().cast_signed())
        .or_else(|| {
            if !upper.success {
                return None;
            }
            word0_to_u256(&upper.return_data).map(|v| v.to::<u64>().cast_signed())
        })
}

/// The per-spelling string decode shared by the sequential and batched
/// metadata paths: dynamic `string` first, then the bytes32 fallback (older
/// tokens — first 32 bytes, trailing NULs stripped, non-empty).
fn decode_string_return(ret: &[u8]) -> Option<String> {
    if let Some(s) = decode_dynamic_string(ret) {
        return Some(s);
    }
    if ret.len() >= 32 {
        let s = String::from_utf8_lossy(&ret[..32])
            .trim_end_matches('\0')
            .to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

#[expect(clippy::doc_markdown, clippy::expect_used, clippy::panic)]
#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::asset_config::EIP_1967_IMPLEMENTATION_SLOT;
    use super::revision::{POOL_CONFIGURATOR_PROXY_ID, POOL_PROXY_ID};
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
        // resolves before the gate is checked. Build the snapshot via the
        // SHARED process runtime (degenbot_core::runtime::get_runtime()) —
        // no ad-hoc test runtime; the shared low-latency budget is exactly
        // right for a test bench.
        let provider = dummy_provider();
        let snapshot = degenbot_core::runtime::get_runtime()
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
        let provider = dummy_provider();
        let snapshot = degenbot_core::runtime::get_runtime()
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
        let provider = dummy_provider();
        let snapshot = degenbot_core::runtime::get_runtime()
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
    fn dummy_provider() -> AlloyProvider {
        // Construct pointing at a dead URL; the path #1 / #3 tests never RPC.
        // Rides the shared process runtime — tests are exactly the callers
        // its low-latency budget serves.
        degenbot_core::runtime::get_runtime()
            .block_on(AlloyProvider::new("http://127.0.0.1:1", 1))
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
        let chunk_ev =
            dispatch_reserve_data_updated(1, 100, &ev, &conn, &mut ChunkSubstrate::lazy(1))
                .unwrap();
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
        assert!(
            dispatch_reserve_data_updated(1, 100, &ev, &conn, &mut ChunkSubstrate::lazy(1))
                .is_err()
        );
    }

    /// A fresh-market cold-boot can see `AssetSourceUpdated`
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
        let out =
            dispatch_asset_source_updated(1, &ev, &conn, &mut ChunkSubstrate::lazy(1)).unwrap();
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
        let out =
            dispatch_asset_source_updated(1, &ev, &conn, &mut ChunkSubstrate::lazy(1)).unwrap();
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
        let chunk_ev =
            dispatch_user_e_mode_set(1, 100, &ev, &conn, &mut ChunkSubstrate::lazy(1)).unwrap();
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
        let chunk_ev =
            dispatch_discount_percent_updated(1, 100, &ev, &conn, &mut ChunkSubstrate::lazy(1))
                .unwrap();
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
        // Parity-gate pin: the gate is `revision < 4` (matches the Python
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

    // ── the 6 missing-variant pure-decode tests ──────────────

    #[test]
    fn strip_trailing_nulls_from_ascii_decodes_pool() {
        // The AddressSet `id` = right-padded ASCII bytes32 (a parity-gate finding: NOT
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
        // Parity-gate finding: the Python's
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

    // ── Discount-event emitter-validation guard (divergence #2) ────────
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

    /// Discount-config gap (fork A): the per-tx `gho_asset`
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
        let r = dispatch_discount_token_updated_with_fresh_resolution(
            Some(&stale),
            &ev,
            &conn,
            &mut ChunkSubstrate::lazy(1),
        )
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

    // ── the revision memo + the multicall batching seam ─────────────────
    //
    // The offline provider seam: an in-memory cassette ledger built from a
    // JSON literal (the recorder's own format) + the replay transport — the
    // same D5 injection the committed-corpus replay suites use, so the
    // served/request counters are the SAME measurement the replay gates read.

    use degenbot_rpc::cassette::Cassette;
    use degenbot_rpc::cassette_replay::CassetteReplayTransport;
    use degenbot_rpc::multicall3::{encode_aggregate3, MULTICALL3_ADDRESS};

    /// The independent `eth_abi`-encoded `(bool,bytes)[]` aggregate3 return
    /// for two 32-byte successes (`0x…2a`, `0x…2b`) — the multicall3 module's
    /// reference-vector precedent (computed with a DIFFERENT ABI encoder).
    const AGGREGATE3_RETURN_2_SUCCESSES: &str = "00000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000c0000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000002a000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000002b";

    /// An in-memory cassette from `(ledger key, result hex)` pairs — the
    /// recorder's JSON format parsed through the crate's own parser, with
    /// the write digest computed over the same compact response JSON the
    /// recorder digests.
    fn test_cassette(entries: Vec<(String, String)>) -> Cassette {
        let body: Vec<String> = entries
            .into_iter()
            .map(|(key, result_hex)| {
                let response_json = format!(r#"{{"result":"{result_hex}"}}"#);
                let digest = alloy::primitives::keccak256(response_json.as_bytes()).to_string();
                // The ledger key is itself a JSON STRING in the file form —
                // the embedded quotes escape (the keys are pure ASCII, so
                // `escape_default` produces exactly the recorder's `\"`).
                let escaped_key = key.escape_default().to_string();
                format!(r#""{escaped_key}": {{"response": {response_json}, "digest": "{digest}"}}"#)
            })
            .collect();
        let json = format!(
            "{{\"schema\":\"degenbot.cassette/v1\",\"chain_id\":1,             \"provenance\":{{\"source\":\"test\",\"recorded_at\":\"1970-01-01T00:00:00Z\",             \"span\":{{\"from_block\":0,\"to_block\":0}}}},\"entries\":{{{}}}}}",
            body.join(",")
        );
        Cassette::from_json_str(&json).expect("the in-memory test cassette parses")
    }

    /// The canonical ledger key for an `eth_call`: the compact JSON pair the
    /// recorder writes — the calldata under `input` in its ALREADY-CANONICAL
    /// form (the caller canonicalizes), the target lowercase (a ≥17-digit
    /// verbatim hex address), and the block tag in the canonical
    /// decimal-string form.
    fn eth_call_key(canonical_input: &str, to_hex: &str, block: u64) -> String {
        format!(
            "[\"eth_call\",[{{\"input\":\"{canonical_input}\",\"to\":\"{to_hex}\"}},\"{block}\"]]"
        )
    }

    /// The canonical form of a 4-byte selector-only calldata — the cassette
    /// short-hex precision rule applied to the selector the code encodes: a
    /// minimal-form short quantity (≤16 hex digits, no leading zero, `0x0`
    /// aside) DECIMALIZES; a leading-zero digit marks a NON-minimal byte-hex
    /// that stays verbatim. `ATOKEN_REVISION()` = `0x0bd7ad3b` (leading zero
    /// → verbatim) and `DEBT_TOKEN_REVISION()` = `0xb9a7b622` (→ decimal)
    /// cover both classes. Mirrors `canonical_hex_quantity` (the committed
    /// corpus guard in `degenbot-rpc::cassette` pins the rule).
    fn canonical_selector_input(sig: &str) -> String {
        let selector = revision_selector(sig);
        let digits = alloy::hex::encode(selector);
        if digits.starts_with('0') {
            // Non-minimal byte-hex — stays verbatim (decimalizing would not
            // re-hex back to the same wire string).
            format!("0x{digits}")
        } else {
            u64::from_str_radix(&digits, 16)
                .expect("an 8-hex-digit selector parses as u64")
                .to_string()
        }
    }

    /// The verbatim (≥17-digit) form of a selector+address calldata —
    /// canonicalization leaves it untouched (the recorded getDiscountPercent
    /// entry's shape).
    fn address_arg_input(sig: &str, user: &Address) -> String {
        format!("0x{}{}", alloy::hex::encode(revision_selector(sig)), {
            let mut word = [0u8; 32];
            word[12..].copy_from_slice(user.as_slice());
            alloy::hex::encode(word)
        })
    }

    /// A 32-byte big-endian word as a `0x`-prefixed hex string (the wire form
    /// an `eth_call` result carries).
    fn word_result_hex(v: u64) -> String {
        format!("0x{v:064x}")
    }

    fn batch_success(data: &[u8]) -> MulticallResult {
        MulticallResult {
            success: true,
            return_data: Bytes::from(data.to_vec()),
        }
    }

    fn batch_failure() -> MulticallResult {
        MulticallResult {
            success: false,
            return_data: Bytes::new(),
        }
    }

    /// An ABI dynamic-`string` return: the 0x20 offset word, the length word,
    /// the data right-padded to a 32-byte multiple.
    fn dynamic_string_return(s: &str) -> Vec<u8> {
        let len = u8::try_from(s.len()).expect("the test strings fit one length word");
        let mut v = vec![0u8; 64];
        v[31] = 0x20;
        v[63] = len;
        v.extend_from_slice(s.as_bytes());
        while !v.len().is_multiple_of(32) {
            v.push(0);
        }
        v
    }

    #[test]
    fn string_field_from_batch_mirrors_the_sequential_fallback_order() {
        // lower succeeds with a dynamic string — the upper spelling is never
        // consulted (and a garbage upper must not matter).
        assert_eq!(
            string_field_from_batch(
                &batch_success(&dynamic_string_return("WETH")),
                &batch_failure()
            ),
            Some("WETH".to_string())
        );
        // lower succeeds but is undecodable as a string (32 zero bytes — the
        // bytes32 fallback strips to empty) → falls through to the upper
        // spelling.
        assert_eq!(
            string_field_from_batch(
                &batch_success(&[0u8; 32]),
                &batch_success(&dynamic_string_return("MKR"))
            ),
            Some("MKR".to_string())
        );
        // A bytes32-returning lower spelling (older tokens) decodes without
        // the upper spelling.
        let mut bytes32 = [0u8; 32];
        bytes32[..3].copy_from_slice(b"DAI");
        assert_eq!(
            string_field_from_batch(&batch_success(&bytes32), &batch_failure()),
            Some("DAI".to_string())
        );
        // A FAILED lower result (the revert/transport class the sequential
        // path saw as an `eth_call` error) returns None WITHOUT consulting
        // the upper spelling — the sequential `.ok()?` early-return.
        assert_eq!(
            string_field_from_batch(
                &batch_failure(),
                &batch_success(&dynamic_string_return("WETH"))
            ),
            None
        );
        // A succeeded-but-undecodable lower + a failed upper → None.
        assert_eq!(
            string_field_from_batch(&batch_success(&[0u8; 32]), &batch_failure()),
            None
        );
    }

    #[test]
    fn decimals_from_batch_mirrors_the_sequential_fallback_order() {
        let word = |v: u64| {
            let mut w = [0u8; 32];
            w[24..32].copy_from_slice(&v.to_be_bytes());
            w
        };
        // lower succeeds → the upper spelling is never consulted.
        assert_eq!(
            decimals_from_batch(&batch_success(&word(18)), &batch_failure()),
            Some(18)
        );
        // lower succeeds but is undecodable (short return) → the upper
        // spelling answers.
        assert_eq!(
            decimals_from_batch(&batch_success(&[0u8; 16]), &batch_success(&word(6))),
            Some(6)
        );
        // A FAILED lower result returns None WITHOUT the upper spelling.
        assert_eq!(
            decimals_from_batch(&batch_failure(), &batch_success(&word(18))),
            None
        );
        assert_eq!(
            decimals_from_batch(&batch_success(&[0u8; 16]), &batch_failure()),
            None
        );
    }

    #[test]
    fn revision_memo_second_same_key_read_serves_no_new_entry() {
        let target = Address::repeat_byte(0x11);
        let input = encode_no_arg_call("ATOKEN_REVISION()");
        // The wire calldata is the 4-byte selector; its canonical ledger-key
        // form is the DECIMALIZED quantity (the short-hex precision rule).
        assert_eq!(
            input.as_ref(),
            revision_selector("ATOKEN_REVISION()"),
            "the memo read issues the selector-only calldata"
        );
        let key = eth_call_key(
            &canonical_selector_input("ATOKEN_REVISION()"),
            &format!("0x{}", alloy::hex::encode(target)),
            100,
        );
        let transport =
            CassetteReplayTransport::new(test_cassette(vec![(key, word_result_hex(7))]));
        let provider = transport.as_alloy_provider();
        degenbot_core::runtime::get_runtime().block_on(async {
            let mut memo = RevisionMemo::new();
            let v1 = memo
                .read(&provider, &target, "ATOKEN_REVISION()", 100)
                .await
                .unwrap();
            let v2 = memo
                .read(&provider, &target, "ATOKEN_REVISION()", 100)
                .await
                .unwrap();
            assert_eq!(v1, U256::from(7));
            assert_eq!(v2, U256::from(7), "the memo hit returns the recorded value");
            let snap = transport.served_snapshot();
            assert_eq!(
                snap.served, 1,
                "the second same-(impl, selector, block) read must be a memo hit, not a new RPC"
            );
            assert_eq!(snap.requests, 1, "no extra request left the transport");
            // A different block is a different key — the memo never serves a
            // read across blocks (the `Upgraded` safety lane).
            let miss = memo
                .read(&provider, &target, "ATOKEN_REVISION()", 101)
                .await;
            assert!(
                miss.is_err(),
                "an unrecorded key is a loud miss, never a memo hit"
            );
            assert_eq!(
                transport.served_snapshot().served,
                1,
                "the miss served no ledger entry"
            );
        });
    }

    #[test]
    fn batched_calls_single_call_keeps_the_direct_wire_shape() {
        let target = Address::repeat_byte(0x22);
        // A 36-byte selector+address calldata (the recorded getDiscountPercent
        // shape) stays verbatim under canonicalization — the exact form the
        // committed corpus's single eth_call entry carries.
        let user = Address::repeat_byte(0x33);
        let input = encode_single_address_call("getDiscountPercent(address)", &user);
        let key = eth_call_key(
            &address_arg_input("getDiscountPercent(address)", &user),
            &format!("0x{}", alloy::hex::encode(target)),
            100,
        );
        let transport =
            CassetteReplayTransport::new(test_cassette(vec![(key, word_result_hex(9))]));
        let provider = transport.as_alloy_provider();
        degenbot_core::runtime::get_runtime().block_on(async {
            let calls = vec![(target, input)];
            let rets = eth_calls_batched_or_direct(&provider, &calls, 100)
                .await
                .unwrap();
            assert_eq!(word0_to_u256(&rets[0]), Some(U256::from(9)));
            let snap = transport.served_snapshot();
            assert_eq!(
                snap.requests, 1,
                "a single pending call must stay the exact direct eth_call —                  no aggregate3 wrapper attempt"
            );
            assert_eq!(snap.served, 1);
        });
    }

    #[test]
    fn batched_calls_two_calls_fold_into_one_aggregate3_entry() {
        let a = Address::repeat_byte(0xaa);
        let b = Address::repeat_byte(0xbb);
        let calls = vec![
            (a, encode_no_arg_call("ATOKEN_REVISION()")),
            (b, encode_no_arg_call("DEBT_TOKEN_REVISION()")),
        ];
        let encoded = encode_aggregate3(&calls).unwrap();
        let key = eth_call_key(
            &format!("0x{}", alloy::hex::encode(encoded.as_ref())),
            &format!("0x{}", alloy::hex::encode(MULTICALL3_ADDRESS)),
            100,
        );
        let transport = CassetteReplayTransport::new(test_cassette(vec![(
            key,
            format!("0x{AGGREGATE3_RETURN_2_SUCCESSES}"),
        )]));
        let provider = transport.as_alloy_provider();
        degenbot_core::runtime::get_runtime().block_on(async {
            let rets = eth_calls_batched_or_direct(&provider, &calls, 100)
                .await
                .unwrap();
            assert_eq!(rets.len(), 2);
            assert_eq!(word0_to_u256(&rets[0]), Some(U256::from(42)));
            assert_eq!(word0_to_u256(&rets[1]), Some(U256::from(43)));
            let snap = transport.served_snapshot();
            assert_eq!(
                snap.requests, 1,
                "2+ same-block independent calls fold into ONE aggregate3 request"
            );
            assert_eq!(snap.served, 1, "the batch serves exactly one ledger entry");
        });
    }

    #[test]
    fn batched_calls_fall_back_to_sequential_when_the_batch_is_unrecorded() {
        // The degradation contract: on a chain (or against a ledger) without
        // the aggregate3 entry, the batch request misses and the per-call
        // fallback serves the DIRECT entries — values correct, but the
        // request set carries the failed wrapper (requests 3, served 2).
        // This is why the batch only fires where 2+ calls are pending: the
        // recorded shapes of single-call dispatches never change.
        let a = Address::repeat_byte(0xaa);
        let b = Address::repeat_byte(0xbb);
        let calls = vec![
            (a, encode_no_arg_call("ATOKEN_REVISION()")),
            (b, encode_no_arg_call("DEBT_TOKEN_REVISION()")),
        ];
        // Each direct entry's key carries the selector's CANONICAL form (the
        // short-hex precision rule: `ATOKEN_REVISION()`'s leading-zero
        // selector stays verbatim, `DEBT_TOKEN_REVISION()`'s decimalizes).
        let sigs = ["ATOKEN_REVISION()", "DEBT_TOKEN_REVISION()"];
        let direct_keys: Vec<(String, String)> = calls
            .iter()
            .zip(sigs)
            .map(|((t, _), sig)| {
                (
                    eth_call_key(
                        &canonical_selector_input(sig),
                        &format!("0x{}", alloy::hex::encode(*t)),
                        100,
                    ),
                    word_result_hex(5),
                )
            })
            .collect();
        let transport = CassetteReplayTransport::new(test_cassette(direct_keys));
        let provider = transport.as_alloy_provider();
        degenbot_core::runtime::get_runtime().block_on(async {
            let rets = eth_calls_batched_or_direct(&provider, &calls, 100)
                .await
                .unwrap();
            assert_eq!(rets.len(), 2);
            assert_eq!(word0_to_u256(&rets[0]), Some(U256::from(5)));
            assert_eq!(word0_to_u256(&rets[1]), Some(U256::from(5)));
            let snap = transport.served_snapshot();
            assert_eq!(
                snap.requests, 3,
                "the failed aggregate3 attempt + the two fallback eth_calls"
            );
            assert_eq!(snap.served, 2, "both direct entries served");
        });
    }
}

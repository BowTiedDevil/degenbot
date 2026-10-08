//! The upgrade-transition revision resolvers.
//!
//! `Upgraded` → resolve the asset (aToken row first, then vToken; neither →
//! `Err`), RPC the new implementation's revision fn at the tx block, and flag
//! the GHO-discount deprecation when the upgraded vToken IS the GHO vToken
//! and the new revision ≥ 4 (the apply side in `run::apply` does the column
//! bump, the GHO clears, and the bulk user reset). `PoolUpdated` /
//! `PoolConfiguratorUpdated` → RPC the revision fn on the NEW address and
//! update ONLY `aave_v3_contracts.revision`, never the address (the parity
//! gate). `ProxyCreated` → match the right-padded ASCII proxy id and RPC the
//! revision on the implementation through the caller's memo.
//!
//! Surface table: `tests/fixtures/cassettes/wave4/UPGRADE-MAP.md` §(A)
//! row 4 (resolution order and the deprecation gate), row 5 (revision-only
//! contract update), and row 11 ([`match_proxy_id`] serves BOTH the chunk
//! dispatch (through [`ChunkContext::resolve_proxy_created`]) and the
//! cold-boot bootstrap pass, which owns its own [`RevisionMemo`] instance —
//! the block lane keeps the keying correct across the two owners).

#![expect(clippy::doc_markdown)]

use crate::run::AaveChunkEvent;
use crate::updater::run::substrate::ChunkSubstrate;
use alloy::primitives::Address;
use degenbot_db::aave::AaveGhoAsset;
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::Connection;

use super::context::{ChunkContext, RevisionMemo};
use super::{checksum, discount_to_i64, ConfigDispatchError, GHO_DISCOUNT_DEPRECATION_REVISION};

// ── the 6 missing-variant event resolvers ─────────────────────

/// The right-padded ASCII bytes32 id `b"POOL"` (4 bytes + 28 zeros). The
/// Python's `eth_abi.abi.encode(["bytes32"], [b"POOL"])` — a parity-gate
/// finding:
/// NOT `keccak256("POOL")`, the protocol emits the right-padded ASCII string.
pub(super) const POOL_PROXY_ID: [u8; 32] = {
    let mut id = [0u8; 32];
    id[0] = b'P';
    id[1] = b'O';
    id[2] = b'O';
    id[3] = b'L';
    id
};
/// The right-padded ASCII bytes32 id `b"POOL_CONFIGURATOR"` (17 bytes + 15
/// zeros).
pub(super) const POOL_CONFIGURATOR_PROXY_ID: [u8; 32] = {
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

/// Resolve an `Upgraded` event (the riskiest piece). Port of
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
pub(super) async fn resolve_upgraded(
    ctx: &mut ChunkContext<'_>,
    decoded: &degenbot_decoders::aave_event_decoder::AaveV3UpgradedEvent,
    gho_asset: Option<&AaveGhoAsset>,
    block_number: u64,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let market_id = ctx.market_id();
    let proxy_str = checksum(&decoded.proxy_address);
    // 1. asset lookup: a_token first, then v_token.
    let a_asset = substrate.lookup_asset_row(conn, market_id, "a_token", &proxy_str)?;
    let (asset_id, is_a_token) = if let Some(row) = a_asset {
        (row.id, true)
    } else {
        let v_asset = substrate.lookup_asset_row(conn, market_id, "v_token", &proxy_str)?;
        let Some(row) = v_asset else {
            return Err(ConfigDispatchError::DecodeShape(format!(
                "Upgraded: proxy {proxy_str} is neither a known aToken nor vToken \
                 (Python: 'Unreachable code path')"
            )));
        };
        (row.id, false)
    };
    // 2. RPC the revision on the new implementation (memoized per
    //    (implementation, selector, block) — an `Upgraded` fires once per
    //    contract per market lifetime, so the memo's role here is the
    //    re-encounter case: a rolled-and-replayed span or a same-block
    //    sibling event re-reading the same implementation).
    let rev_fn = if is_a_token {
        "ATOKEN_REVISION()"
    } else {
        "DEBT_TOKEN_REVISION()"
    };
    let new_revision = ctx
        .read_revision(&decoded.implementation, rev_fn, block_number)
        .await?;
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
        market_id: ctx.market_id(),
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
pub(super) async fn resolve_contract_revision_updated(
    ctx: &mut ChunkContext<'_>,
    new_address: Address,
    contract_name: &str,
    revision_fn: &str,
    block_number: u64,
) -> Result<AaveChunkEvent, ConfigDispatchError> {
    let revision = ctx
        .read_revision(&new_address, revision_fn, block_number)
        .await?;
    Ok(AaveChunkEvent::ContractRevisionUpdated {
        market_id: ctx.market_id(),
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
///
/// Takes the [`RevisionMemo`] directly (not a [`ChunkContext`]) because the
/// cold-boot bootstrap pass is a pre-chunk phase that owns its own memo
/// instance — the block lane keeps the keying correct across both owners.
pub(crate) async fn match_proxy_id(
    memo: &mut RevisionMemo,
    provider: &AlloyProvider,
    id: &alloy::primitives::B256,
    proxy_address: &Address,
    implementation_address: &Address,
    block_number: u64,
) -> Result<Option<ProxyCreationResolution>, ConfigDispatchError> {
    let (name, rev_fn) = if id.as_slice() == POOL_PROXY_ID {
        ("POOL", "POOL_REVISION()")
    } else if id.as_slice() == POOL_CONFIGURATOR_PROXY_ID {
        ("POOL_CONFIGURATOR", "CONFIGURATOR_REVISION()")
    } else {
        return Ok(None);
    };
    let revision = memo
        .read(provider, implementation_address, rev_fn, block_number)
        .await?;
    Ok(Some(ProxyCreationResolution {
        name: name.to_string(),
        address: checksum(proxy_address),
        revision: discount_to_i64(revision),
    }))
}

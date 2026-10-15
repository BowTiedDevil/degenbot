//! The pool-refresh core entry: fetch -> compare -> apply, per family, keyed
//! by `pool_id`.
//!
//! The update path's ONE core entry. The PyO3 shell's `Pool.refresh_via`
//! pymethod and the Python driver's `Bot.update` are thin calls onto this;
//! the per-family choreography that used to live on the retired V2/V3/V4
//! builder `update()` methods (and their Python twins) lives here instead, so
//! a new family is a new arm in this dispatcher — never a new Python branch.
//!
//! Family dispatch keys on the registered family tag
//! ([`pool_ops::pool_family`]), the same tag `BotState` already exposes; the
//! fetch half composes the core construction-I/O choreography
//! (`pool_builder::choreography`) over the shared [`ConstructionIo`], and the
//! apply half drives the state owner's typed dispatchers
//! (`apply_sync_by_pool_id` / `apply_swap_by_pool_id` /
//! `apply_balance_update_by_pool_id`).
//!
//! # Block discipline
//!
//! The fetch block defaults to the current chain head (the historical
//! `_update_pool` behaviour). An update whose block is strictly older than the
//! pool's own `update_block` is refused as [`RefreshOutcome::Unchanged`]: the
//! core apply paths stamp the block they are handed, so replaying an older
//! observation would walk a pool's clock backwards. The Python companions'
//! `external_update` raise their historical `ExternalUpdateError` for the same
//! condition; the refresh entry has no journal to rewind and simply declines.

use alloy::primitives::U256;
use degenbot_core::errors::ProviderError;
use degenbot_pools::spec_bounds::narrow_v2_reserve;
use degenbot_substrate::state_lock::{LockSite, StateLock};
use degenbot_substrate::BotState;

use crate::bot_core::construction_io::ConstructionIo;
use crate::bot_core::pool_builder::choreography;
use crate::bot_core::pool_builder::curve_choreography;
use crate::bot_core::pool_ops;

/// Whether a refresh changed the pool's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// A differing on-chain state was applied.
    Changed,
    /// The on-chain state matched the pool's state (or the requested block was
    /// older than the pool's clock); nothing was applied.
    Unchanged,
}

/// Why a refresh refused.
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// The id is not registered.
    #[error("pool {0} is not registered")]
    UnknownPool(u64),
    /// The registered family has no refresh path.
    #[error("pool {pool_id} family {family:?} has no refresh path")]
    UnsupportedFamily {
        /// The requested pool id.
        pool_id: u64,
        /// The registered family tag.
        family: &'static str,
    },
    /// The pool's on-chain identity is missing from its registered state.
    #[error("pool {pool_id} is missing the identity its family refreshes from")]
    MissingIdentity {
        /// The requested pool id.
        pool_id: u64,
    },
    /// A fetched value fell outside its on-chain storage bound.
    #[error("pool {pool_id} refresh read a value outside its spec bound: {message}")]
    Spec {
        /// The requested pool id.
        pool_id: u64,
        /// The spec-bound failure, rendered.
        message: String,
    },
    /// The fetch failed.
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// Resolve the fetch block: the caller's explicit block, else the chain head.
async fn resolve_block(io: &ConstructionIo, block: Option<u64>) -> Result<u64, ProviderError> {
    match block {
        Some(block) => Ok(block),
        None => io.get_block_number().await,
    }
}

/// Fetch a pool's current on-chain state and apply it to `BotState` when it
/// differs.
///
/// # Errors
///
/// Returns [`RefreshError`] for an unregistered pool, a family without a
/// refresh path, a missing identity, a spec-bound violation on a fetched
/// reserve, or an RPC/decode failure.
pub async fn refresh_pool(
    state: &StateLock<BotState>,
    pool_id: u64,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RefreshOutcome, RefreshError> {
    let family = pool_ops::pool_family(state, pool_id).ok_or(RefreshError::UnknownPool(pool_id))?;
    match family {
        "v2" | "aerodrome-v2" => refresh_reserve_pair(state, pool_id, io, block).await,
        "v3" => refresh_v3(state, pool_id, io, block).await,
        "v4" => refresh_v4(state, pool_id, io, block).await,
        "curve" => refresh_curve(state, pool_id, io, block).await,
        "balancer-weighted" => refresh_balancer(state, pool_id, io, block, false).await,
        "balancer-stable" => refresh_balancer(state, pool_id, io, block, true).await,
        other => Err(RefreshError::UnsupportedFamily {
            pool_id,
            family: other,
        }),
    }
}

/// V2 / Aerodrome V2: `getReserves()` against the registered reserve pair.
async fn refresh_reserve_pair(
    state: &StateLock<BotState>,
    pool_id: u64,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RefreshOutcome, RefreshError> {
    let block = resolve_block(io, block).await?;
    let address = {
        let guard = state.read_at(LockSite::Orchestrator);
        guard
            .pool_slice::<degenbot_pools::v2_state::V2PoolIdentity>(pool_id)
            .map(|identity| identity.address)
            .or_else(|| {
                guard
                    .pool_slice::<degenbot_pools::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                        pool_id,
                    )
                    .map(|identity| identity.address)
            })
            .ok_or(RefreshError::MissingIdentity { pool_id })?
    };
    let (reserve0, reserve1) = choreography::fetch_v2_reserves(io, address, Some(block)).await?;
    let narrow = |value: U256, field: &'static str| {
        narrow_v2_reserve(value, field).map_err(|violation| RefreshError::Spec {
            pool_id,
            message: violation.to_string(),
        })
    };
    let new0 = narrow(reserve0, "reserve0")?;
    let new1 = narrow(reserve1, "reserve1")?;

    let mut guard = state.write_at(LockSite::Orchestrator);
    let (current0, current1, update_block) = guard
        .pool_slice::<degenbot_pools::v2_state::V2PoolState>(pool_id)
        .map(|s| (s.reserve0, s.reserve1, s.update_block))
        .or_else(|| {
            guard
                .pool_slice::<degenbot_pools::aerodrome_v2_state::AerodromeV2PoolState>(pool_id)
                .map(|s| (s.reserve0, s.reserve1, s.update_block))
        })
        .ok_or(RefreshError::MissingIdentity { pool_id })?;
    if block < update_block || (current0 == new0 && current1 == new1) {
        return Ok(RefreshOutcome::Unchanged);
    }
    let _ = guard.apply_sync_by_pool_id(pool_id, new0, new1, block);
    Ok(RefreshOutcome::Changed)
}

/// V3: `slot0()` + `liquidity()`.
async fn refresh_v3(
    state: &StateLock<BotState>,
    pool_id: u64,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RefreshOutcome, RefreshError> {
    let block = resolve_block(io, block).await?;
    let address = {
        let guard = state.read_at(LockSite::Orchestrator);
        guard
            .pool_slice::<degenbot_pools::v3_state::V3PoolIdentity>(pool_id)
            .map(|identity| identity.address)
            .ok_or(RefreshError::MissingIdentity { pool_id })?
    };
    let (sqrt_price_x96, tick_raw, liquidity_raw) =
        choreography::fetch_v3_slot0_liquidity(io, address, Some(block)).await?;
    let tick = i32::try_from(tick_raw).unwrap_or(0);
    let liquidity = liquidity_raw.to::<u128>();

    let mut guard = state.write_at(LockSite::Orchestrator);
    let (current_sqrt, current_liquidity, current_tick, update_block) = guard
        .pool_slice::<degenbot_pools::v3_state::V3PoolState>(pool_id)
        .map(|s| (s.sqrt_price_x96, s.liquidity, s.tick, s.update_block))
        .ok_or(RefreshError::MissingIdentity { pool_id })?;
    if block < update_block
        || (current_sqrt == sqrt_price_x96
            && current_liquidity == liquidity
            && current_tick == tick)
    {
        return Ok(RefreshOutcome::Unchanged);
    }
    let _ = guard.apply_swap_by_pool_id(pool_id, sqrt_price_x96, liquidity, tick, block, &[]);
    Ok(RefreshOutcome::Changed)
}

/// V4: `getSlot0(bytes32)` + `getLiquidity(bytes32)` on the manager's state
/// view.
async fn refresh_v4(
    state: &StateLock<BotState>,
    pool_id: u64,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RefreshOutcome, RefreshError> {
    let block = resolve_block(io, block).await?;
    let (state_view, v4_pool_id) = {
        let guard = state.read_at(LockSite::Orchestrator);
        let identity = guard
            .pool_slice::<degenbot_pools::v4_state::V4PoolIdentity>(pool_id)
            .ok_or(RefreshError::MissingIdentity { pool_id })?;
        let state_view = guard
            .state_view_for_pool(pool_id)
            .ok_or(RefreshError::MissingIdentity { pool_id })?;
        (state_view, identity.pool_id)
    };
    let (sqrt_price_x96, tick_raw, _protocol_fee, _lp_fee, liquidity_raw) =
        choreography::fetch_v4_slot0_liquidity(io, state_view, v4_pool_id, Some(block)).await?;
    let tick = i32::try_from(tick_raw).unwrap_or(0);
    let liquidity = liquidity_raw.to::<u128>();

    let mut guard = state.write_at(LockSite::Orchestrator);
    let (current_sqrt, current_liquidity, current_tick, update_block) = guard
        .pool_slice::<degenbot_pools::v4_state::V4PoolState>(pool_id)
        .map(|s| (s.sqrt_price_x96, s.liquidity, s.tick, s.update_block))
        .ok_or(RefreshError::MissingIdentity { pool_id })?;
    if block < update_block
        || (current_sqrt == sqrt_price_x96
            && current_liquidity == liquidity
            && current_tick == tick)
    {
        return Ok(RefreshOutcome::Unchanged);
    }
    let _ = guard.apply_swap_by_pool_id(pool_id, sqrt_price_x96, liquidity, tick, block, &[]);
    Ok(RefreshOutcome::Changed)
}

/// Curve `StableSwap`: `balances(uint256)` over the pool's coin count.
async fn refresh_curve(
    state: &StateLock<BotState>,
    pool_id: u64,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RefreshOutcome, RefreshError> {
    let block = resolve_block(io, block).await?;
    let (address, coin_count) = {
        let guard = state.read_at(LockSite::Orchestrator);
        guard
            .pool_slice::<degenbot_pools::curve_state::CurvePoolIdentity>(pool_id)
            .map(|identity| (identity.address, identity.tokens.len()))
            .ok_or(RefreshError::MissingIdentity { pool_id })?
    };
    let balances =
        curve_choreography::fetch_curve_balances(io, address, coin_count, Some(block)).await?;
    apply_balances(state, pool_id, balances, block, |guard, pool_id| {
        guard
            .pool_slice::<degenbot_pools::curve_state::CurvePoolState>(pool_id)
            .map(|s| (s.balances.to_vec(), s.update_block))
    })
}

/// Balancer V2 weighted / stable: `getPoolTokens(bytes32)` on the Vault.
async fn refresh_balancer(
    state: &StateLock<BotState>,
    pool_id: u64,
    io: &ConstructionIo,
    block: Option<u64>,
    stable: bool,
) -> Result<RefreshOutcome, RefreshError> {
    let (vault, v4_style_id) = {
        let guard = state.read_at(LockSite::Orchestrator);
        if stable {
            guard
                .pool_slice::<degenbot_pools::balancer_stable_state::BalancerStablePoolIdentity>(pool_id)
                .map(|identity| (identity.vault, identity.pool_id))
        } else {
            guard
                .pool_slice::<degenbot_pools::balancer_weighted_state::BalancerWeightedPoolIdentity>(pool_id)
                .map(|identity| (identity.vault, identity.pool_id))
        }
        .ok_or(RefreshError::MissingIdentity { pool_id })?
    };
    let (_tokens, balances) =
        choreography::fetch_balancer_vault_tokens(io, vault, &v4_style_id, block).await?;
    // The historical Balancer update stamped `block_number or 0` (the driver's
    // resolved block is `None` when the caller gave none), so the refresh
    // preserves that stamp rather than inventing a head default.
    let apply_block = block.unwrap_or(0);
    apply_balances(
        state,
        pool_id,
        balances,
        apply_block,
        move |guard, pool_id| {
            if stable {
                guard
                    .pool_slice::<degenbot_pools::balancer_stable_state::BalancerStablePoolState>(
                        pool_id,
                    )
                    .map(|s| (s.balances.to_vec(), s.update_block))
            } else {
                guard
                    .pool_slice::<degenbot_pools::balancer_weighted_state::BalancerWeightedPoolState>(pool_id)
                    .map(|s| (s.balances.to_vec(), s.update_block))
            }
        },
    )
}

/// The shared balance-vector half: compare the fetched vector against the
/// registered one under one write guard and apply it when it differs.
fn apply_balances<F>(
    state: &StateLock<BotState>,
    pool_id: u64,
    balances: Vec<U256>,
    block: u64,
    read_current: F,
) -> Result<RefreshOutcome, RefreshError>
where
    F: FnOnce(&mut BotState, u64) -> Option<(Vec<U256>, u64)>,
{
    let mut guard = state.write_at(LockSite::Orchestrator);
    let (current, update_block) =
        read_current(&mut guard, pool_id).ok_or(RefreshError::MissingIdentity { pool_id })?;
    if block < update_block || current == balances {
        return Ok(RefreshOutcome::Unchanged);
    }
    let _ = guard.apply_balance_update_by_pool_id(pool_id, balances, block);
    Ok(RefreshOutcome::Changed)
}

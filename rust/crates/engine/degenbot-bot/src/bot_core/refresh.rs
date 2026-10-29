//! The pool-refresh core entry: fetch -> compare -> apply, per family, keyed
//! by `pool_id`.
//!
//! The update path's ONE core entry. The PyO3 shell's `Pool.refresh_via`
//! pymethod and the Python driver's `Bot.update` are thin calls onto this;
//! the per-family choreography that used to live on the retired V2/V3/V4
//! builder `update()` methods (and their Python twins) lives here instead, so
//! a new family is a new arm in this dispatcher — never a new Python branch.
//!
//! Family dispatch keys on [`RegisteredPoolFamily`], read typed from the state
//! owner (`BotState::registered_family`) and matched exhaustively, so adding a
//! family is a compile error here rather than a runtime gap; the
//! fetch half is the one call step's read family (`degenbot_rpc::abi`)
//! over the shared [`ConstructionIo`], and the
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
use degenbot_pools::narrow_v2_reserve;
use degenbot_pools::RegisteredPoolFamily;
use degenbot_rpc::abi::{
    ContractRead, IBalancerVault, IUniswapV3Pool, IUniswapV4StateView, Slot0Prefix, V2Reserves,
    V4Slot0,
};
use degenbot_substrate::state_lock::{LockSite, StateLock};
use degenbot_substrate::BotState;

use crate::bot_core::construction_io::ConstructionIo;
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
/// Returns [`RefreshError`] for an unregistered pool, a missing identity, a
/// spec-bound violation on a fetched reserve, or an RPC/decode failure.
pub async fn refresh_pool(
    state: &StateLock<BotState>,
    pool_id: u64,
    io: &ConstructionIo,
    block: Option<u64>,
) -> Result<RefreshOutcome, RefreshError> {
    // The family comes typed from the state owner — the one reader of record —
    // and this match is exhaustive over `RegisteredPoolFamily`. A family added
    // to the enum is a compile error right here, never a runtime fallthrough.
    let family =
        pool_ops::registered_family(state, pool_id).ok_or(RefreshError::UnknownPool(pool_id))?;
    match family {
        RegisteredPoolFamily::V2 | RegisteredPoolFamily::AerodromeV2 => {
            refresh_reserve_pair(state, pool_id, io, block).await
        }
        RegisteredPoolFamily::V3 => refresh_v3(state, pool_id, io, block).await,
        RegisteredPoolFamily::V4 => refresh_v4(state, pool_id, io, block).await,
        RegisteredPoolFamily::Curve => refresh_curve(state, pool_id, io, block).await,
        RegisteredPoolFamily::BalancerWeighted => {
            refresh_balancer(state, pool_id, io, block, false).await
        }
        RegisteredPoolFamily::BalancerStable => {
            refresh_balancer(state, pool_id, io, block, true).await
        }
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
            .pool_slice::<degenbot_pools::V2PoolIdentity>(pool_id)
            .map(|identity| identity.address)
            .or_else(|| {
                guard
                    .pool_slice::<degenbot_pools::AerodromeV2PoolIdentity>(pool_id)
                    .map(|identity| identity.address)
            })
            .ok_or(RefreshError::MissingIdentity { pool_id })?
    };
    let v2 = V2Reserves::read(io, address, Some(block)).await?;
    let (reserve0, reserve1) = (v2.reserve0, v2.reserve1);
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
        .pool_slice::<degenbot_pools::V2PoolState>(pool_id)
        .map(|s| (s.reserve0, s.reserve1, s.update_block))
        .or_else(|| {
            guard
                .pool_slice::<degenbot_pools::AerodromeV2PoolState>(pool_id)
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
            .pool_slice::<degenbot_pools::V3PoolIdentity>(pool_id)
            .map(|identity| identity.address)
            .ok_or(RefreshError::MissingIdentity { pool_id })?
    };
    let slot0 = Slot0Prefix::read(io, address, Some(block)).await?;
    let liquidity_raw = io
        .read(address, IUniswapV3Pool::liquidityCall {}, Some(block))
        .await?;
    let (sqrt_price_x96, tick_raw) = (slot0.sqrt_price_x96, slot0.tick);
    let tick = i32::try_from(tick_raw).unwrap_or(0);
    let liquidity = liquidity_raw;

    let mut guard = state.write_at(LockSite::Orchestrator);
    let (current_sqrt, current_liquidity, current_tick, update_block) = guard
        .pool_slice::<degenbot_pools::V3PoolState>(pool_id)
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
            .pool_slice::<degenbot_pools::V4PoolIdentity>(pool_id)
            .ok_or(RefreshError::MissingIdentity { pool_id })?;
        let state_view = guard
            .state_view_for_pool(pool_id)
            .ok_or(RefreshError::MissingIdentity { pool_id })?;
        (state_view, identity.pool_id)
    };
    let slot0 = V4Slot0::read(io, state_view, v4_pool_id, Some(block)).await?;
    let liquidity_raw = io
        .read(
            state_view,
            IUniswapV4StateView::getLiquidityCall {
                poolId: v4_pool_id.into(),
            },
            Some(block),
        )
        .await?;
    let (sqrt_price_x96, tick_raw) = (slot0.sqrt_price_x96, slot0.tick);
    let tick = i32::try_from(tick_raw).unwrap_or(0);
    let liquidity = liquidity_raw;

    let mut guard = state.write_at(LockSite::Orchestrator);
    let (current_sqrt, current_liquidity, current_tick, update_block) = guard
        .pool_slice::<degenbot_pools::V4PoolState>(pool_id)
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
            .pool_slice::<degenbot_pools::CurvePoolIdentity>(pool_id)
            .map(|identity| (identity.address, identity.tokens.len()))
            .ok_or(RefreshError::MissingIdentity { pool_id })?
    };
    let balances =
        curve_choreography::fetch_curve_balances(io, address, coin_count, Some(block)).await?;
    apply_balances(state, pool_id, balances, block, |guard, pool_id| {
        guard
            .pool_slice::<degenbot_pools::CurvePoolState>(pool_id)
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
                .pool_slice::<degenbot_pools::BalancerStablePoolIdentity>(pool_id)
                .map(|identity| (identity.vault, identity.pool_id))
        } else {
            guard
                .pool_slice::<degenbot_pools::BalancerWeightedPoolIdentity>(pool_id)
                .map(|identity| (identity.vault, identity.pool_id))
        }
        .ok_or(RefreshError::MissingIdentity { pool_id })?
    };
    let balances = io
        .read(
            vault,
            IBalancerVault::getPoolTokensCall {
                poolId: v4_style_id.into(),
            },
            block,
        )
        .await?
        .balances;
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
                    .pool_slice::<degenbot_pools::BalancerStablePoolState>(pool_id)
                    .map(|s| (s.balances.to_vec(), s.update_block))
            } else {
                guard
                    .pool_slice::<degenbot_pools::BalancerWeightedPoolState>(pool_id)
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

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used, clippy::unwrap_used)]

    use alloy::primitives::aliases::U112;
    use alloy::primitives::{Address, Bytes};
    use async_trait::async_trait;
    use degenbot_substrate::RegisterV2PoolParams;

    use super::*;
    use crate::bot_core::construction_io::{ConstructionIo, NoDb, RpcConstruction};

    /// An RPC that refuses every read. The family arms are then observable
    /// without a chain: an arm that runs stops at its fetch, so the failure is
    /// a [`RefreshError::Provider`] — never a family refusal.
    struct RefusingRpc;

    fn unavailable() -> ProviderError {
        ProviderError::ConnectionFailed {
            message: "no node in a unit test".to_string(),
        }
    }

    #[async_trait]
    impl RpcConstruction for RefusingRpc {
        async fn get_block_number(&self) -> Result<u64, ProviderError> {
            Ok(1)
        }

        async fn get_block(
            &self,
            _block_number: u64,
        ) -> Result<Option<degenbot_rpc::provider::EthBlock>, ProviderError> {
            Ok(None)
        }

        async fn get_block_timestamp(
            &self,
            _block_number: u64,
        ) -> Result<Option<u64>, ProviderError> {
            Ok(None)
        }

        async fn get_code(
            &self,
            _address: Address,
            _block_number: Option<u64>,
        ) -> Result<Bytes, ProviderError> {
            Err(unavailable())
        }

        async fn get_balance(
            &self,
            _address: Address,
            _block_number: Option<u64>,
        ) -> Result<U256, ProviderError> {
            Err(unavailable())
        }

        async fn call(
            &self,
            _to: Address,
            _data: Bytes,
            _block_number: Option<u64>,
        ) -> Result<Bytes, ProviderError> {
            Err(unavailable())
        }
    }

    fn io() -> ConstructionIo {
        ConstructionIo::new(std::sync::Arc::new(NoDb), std::sync::Arc::new(RefusingRpc))
    }

    /// A core holding one registered V2 pool, keyed by the id registration
    /// returns (the dispatch reads the family off the registry, never an
    /// argument).
    fn v2_state() -> (StateLock<BotState>, u64) {
        let state = StateLock::new(BotState::new());
        let pool_id = state
            .write_at(LockSite::Orchestrator)
            .register_v2_pool(&RegisterV2PoolParams {
                address: Address::from([0xccu8; 20]),
                token0: Address::from([0xa0u8; 20]),
                token1: Address::from([0xa1u8; 20]),
                reserve0: U112::from(1_000u64),
                reserve1: U112::from(2_000u64),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                factory: Address::from([0xf0u8; 20]),
                update_block: 100,
                variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                stable_swap: false,
                fee_denominator: None,
                ..Default::default()
            })
            .expect("test setup: V2 registration");
        (state, pool_id)
    }

    #[tokio::test]
    async fn an_unregistered_id_is_refused_by_the_typed_family_read() {
        // The family read is the dispatch's admission gate: `None` is the
        // unregistered pool, not a family the dispatcher cannot name.
        let state = StateLock::new(BotState::new());
        let error = refresh_pool(&state, 7, &io(), Some(1))
            .await
            .expect_err("an unregistered id has nothing to refresh");
        assert!(matches!(error, RefreshError::UnknownPool(7)));
    }

    #[tokio::test]
    async fn the_dispatch_reaches_the_registered_familys_fetch() {
        // The V2 arm runs: the refusing node fails the `getReserves()` fetch.
        let (state, pool_id) = v2_state();
        let error = refresh_pool(&state, pool_id, &io(), Some(1))
            .await
            .expect_err("the refusing node fails the fetch");
        assert!(
            matches!(error, RefreshError::Provider(_)),
            "expected the V2 fetch to run, got {error:?}"
        );
    }

    /// An RPC serving one canned `getReserves()` return for every call — the
    /// compare half needs only the fetched pair to be scripted per case.
    struct ReservesRpc {
        reserve0: u64,
        reserve1: u64,
    }

    #[async_trait]
    impl RpcConstruction for ReservesRpc {
        async fn get_block_number(&self) -> Result<u64, ProviderError> {
            Ok(100)
        }

        async fn get_block(
            &self,
            _block_number: u64,
        ) -> Result<Option<degenbot_rpc::provider::EthBlock>, ProviderError> {
            Ok(None)
        }

        async fn get_block_timestamp(
            &self,
            _block_number: u64,
        ) -> Result<Option<u64>, ProviderError> {
            Ok(None)
        }

        async fn get_code(
            &self,
            _address: Address,
            _block_number: Option<u64>,
        ) -> Result<Bytes, ProviderError> {
            Ok(Bytes::new())
        }

        async fn get_balance(
            &self,
            _address: Address,
            _block_number: Option<u64>,
        ) -> Result<U256, ProviderError> {
            Ok(U256::ZERO)
        }

        async fn call(
            &self,
            _to: Address,
            _data: Bytes,
            _block_number: Option<u64>,
        ) -> Result<Bytes, ProviderError> {
            // getReserves() head: reserve0, reserve1, blockTimestampLast.
            let mut bytes = Vec::with_capacity(96);
            bytes.extend_from_slice(&U256::from(self.reserve0).to_be_bytes::<32>());
            bytes.extend_from_slice(&U256::from(self.reserve1).to_be_bytes::<32>());
            bytes.extend_from_slice(&[0u8; 32]);
            Ok(Bytes::from(bytes))
        }
    }

    fn reserves_io(reserve0: u64, reserve1: u64) -> ConstructionIo {
        ConstructionIo::new(
            std::sync::Arc::new(NoDb),
            std::sync::Arc::new(ReservesRpc { reserve0, reserve1 }),
        )
    }

    /// The reserve-pair half of the choreography: a fetched pair that differs
    /// from the registered one is applied and reports `Changed`; a pair that
    /// matches (or an observation older than the pool's own clock) applies
    /// nothing and reports `Unchanged`.
    #[tokio::test]
    async fn a_differing_reserve_pair_is_applied_and_a_matching_one_is_unchanged() {
        let (state, pool_id) = v2_state(); // 1000 / 2000, clock at block 100

        assert_eq!(
            refresh_pool(&state, pool_id, &reserves_io(5_000, 6_000), Some(101))
                .await
                .unwrap(),
            RefreshOutcome::Changed,
            "a differing pair is applied"
        );
        let (reserve0, reserve1, update_block) = {
            let guard = state.read_at(LockSite::Orchestrator);
            let pool = guard
                .pool_slice::<degenbot_pools::V2PoolState>(pool_id)
                .expect("registered");
            (pool.reserve0, pool.reserve1, pool.update_block)
        };
        assert_eq!((reserve0, reserve1), (U112::from(5_000), U112::from(6_000)));
        assert_eq!(update_block, 101, "the observation block is stamped");

        assert_eq!(
            refresh_pool(&state, pool_id, &reserves_io(5_000, 6_000), Some(102))
                .await
                .unwrap(),
            RefreshOutcome::Unchanged,
            "a matching pair applies nothing"
        );

        assert_eq!(
            refresh_pool(&state, pool_id, &reserves_io(1, 2), Some(100))
                .await
                .unwrap(),
            RefreshOutcome::Unchanged,
            "an observation at/below the pool clock is refused, never replayed"
        );
        let guard = state.read_at(LockSite::Orchestrator);
        let pool = guard
            .pool_slice::<degenbot_pools::V2PoolState>(pool_id)
            .expect("registered");
        assert_eq!(
            (pool.reserve0, pool.reserve1),
            (U112::from(5_000), U112::from(6_000))
        );
        assert_eq!(
            pool.update_block, 101,
            "the refused observation moved nothing"
        );
    }
}

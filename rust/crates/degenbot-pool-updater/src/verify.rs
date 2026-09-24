//! Updater adapters for the shared contract-facing liquidity-map verifier.
//!
//! The updater owns only its typed divergence projection and its rollback
//! policy. The V3/V4 reads, slot derivation, and map comparison live in
//! `degenbot-rpc` so CLI, updater, and bot cannot drift into parallel
//! implementations.

use alloy::primitives::{Address, B256, U128, U256};
use degenbot_core::errors::ProviderError;
use degenbot_db::{ComputedLiquidityUpdate, DegenbotDb};
use degenbot_rpc::liquidity_verifier::{
    verify_liquidity_map, LiquidityMap, LiquidityMapDivergence, LiquidityMapTarget,
    LiquidityMapVerifyError,
};
use degenbot_rpc::provider::AlloyProvider;
use hashbrown::HashMap;
use std::str::FromStr;

use crate::run::RunError;

/// A named, deterministic fact proving that an updater map differs from chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiquidityDivergence {
    /// A tick exists on only one side of the map.
    TickPresence {
        /// Tick index.
        tick: i32,
        /// Whether the updater map contains the tick.
        stored: bool,
        /// Whether chain state contains the tick.
        observed: bool,
    },
    /// Gross liquidity differs for a tick present on both sides.
    TickGross {
        /// Tick index.
        tick: i32,
        /// Supplied gross liquidity.
        expected: U128,
        /// Chain gross liquidity.
        actual: U128,
    },
    /// Net liquidity differs for a tick present on both sides.
    TickNet {
        /// Tick index.
        tick: i32,
        /// Supplied net liquidity.
        expected: alloy::primitives::I256,
        /// Chain net liquidity.
        actual: alloy::primitives::I256,
    },
    /// A bitmap word differs from chain state.
    BitmapWord {
        /// Signed bitmap word position.
        word: i32,
        /// Supplied bitmap.
        expected: U256,
        /// Chain bitmap.
        actual: U256,
    },
}

impl From<LiquidityMapDivergence> for LiquidityDivergence {
    fn from(value: LiquidityMapDivergence) -> Self {
        match value {
            LiquidityMapDivergence::TickPresence {
                tick,
                stored,
                observed,
            } => Self::TickPresence {
                tick,
                stored,
                observed,
            },
            LiquidityMapDivergence::TickGross {
                tick,
                expected,
                actual,
            } => Self::TickGross {
                tick,
                expected,
                actual,
            },
            LiquidityMapDivergence::TickNet {
                tick,
                expected,
                actual,
            } => Self::TickNet {
                tick,
                expected: alloy::primitives::I256::try_from(expected)
                    .unwrap_or(alloy::primitives::I256::ZERO),
                actual: alloy::primitives::I256::try_from(actual)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
            LiquidityMapDivergence::BitmapWord {
                word,
                expected,
                actual,
            } => Self::BitmapWord {
                word,
                expected,
                actual,
            },
        }
    }
}

fn shared_map(computed: &ComputedLiquidityUpdate) -> Result<LiquidityMap, RunError> {
    let mut ticks = HashMap::with_capacity(computed.tick_data.len());
    for (&tick, value) in &computed.tick_data {
        let gross = U128::try_from(value.liquidity_gross).map_err(|_| {
            RunError::Provider(ProviderError::DecodingError {
                message: format!("tick {tick} gross does not fit uint128"),
            })
        })?;
        let net = i128::try_from(value.liquidity_net).map_err(|_| {
            RunError::Provider(ProviderError::DecodingError {
                message: format!("tick {tick} net does not fit int128"),
            })
        })?;
        ticks.insert(
            tick,
            degenbot_pools::TickInfo {
                liquidity_gross: gross,
                liquidity_net: net,
                block: 0,
            },
        );
    }
    let mut bitmaps = HashMap::with_capacity(computed.tick_bitmap.len());
    for (&word, value) in &computed.tick_bitmap {
        bitmaps.insert(word, value.bitmap);
    }
    Ok(LiquidityMap::tracked_with_spacing(
        ticks,
        bitmaps,
        computed.tick_spacing,
    ))
}

fn map_error(error: &LiquidityMapVerifyError) -> RunError {
    let message = error.to_string();
    let provider = match error {
        LiquidityMapVerifyError::Decode { .. } | LiquidityMapVerifyError::SparseInput => {
            ProviderError::DecodingError { message }
        }
        LiquidityMapVerifyError::Read { .. } => ProviderError::Other { message },
    };
    RunError::Provider(provider)
}

fn project(
    result: Result<Vec<LiquidityMapDivergence>, LiquidityMapVerifyError>,
) -> Result<Vec<LiquidityDivergence>, RunError> {
    result
        .map(|facts| facts.into_iter().map(LiquidityDivergence::from).collect())
        .map_err(|error| map_error(&error))
}

/// Verify a V3 computed map through the shared verifier.
///
/// # Errors
///
/// Returns [`RunError::Provider`] for shared read/decode failures and
/// [`RunError::Verification`] is raised by the caller-specific gate when facts
/// are non-empty.
pub async fn verify_v3_liquidity_map_on_chain(
    provider: &AlloyProvider,
    pool_address: Address,
    computed: &ComputedLiquidityUpdate,
    block_number: u64,
) -> Result<Vec<LiquidityDivergence>, RunError> {
    project(
        verify_liquidity_map(
            provider,
            LiquidityMapTarget::V3(pool_address),
            &shared_map(computed)?,
            Some(block_number),
        )
        .await,
    )
}

/// Verify a V4 computed map through the shared verifier. The target is the
/// `PoolManager` and `PoolId`; `StateView` is intentionally not an input.
///
/// # Errors
///
/// Returns [`RunError::Provider`] for shared read/decode failures. A non-empty
/// fact list is returned to the caller for its rollback policy.
pub async fn verify_v4_liquidity_map_on_chain(
    provider: &AlloyProvider,
    pool_manager_address: Address,
    pool_id: B256,
    computed: &ComputedLiquidityUpdate,
    block_number: u64,
) -> Result<Vec<LiquidityDivergence>, RunError> {
    project(
        verify_liquidity_map(
            provider,
            LiquidityMapTarget::V4 {
                pool_manager: pool_manager_address,
                pool_id,
            },
            &shared_map(computed)?,
            Some(block_number),
        )
        .await,
    )
}

/// Context for market-wide committed-map verification.
pub struct FullVerifyCtx<'a> {
    /// HTTP provider used for on-chain reads.
    pub provider: &'a AlloyProvider,
    /// Runtime used by the synchronous updater boundary.
    pub rt: &'static tokio::runtime::Runtime,
    /// Block at which committed maps are compared.
    pub block_number: u64,
    /// Chain containing V3 pools.
    pub chain_id: i64,
    /// Chain key used to resolve V4 manager rows.
    pub pool_manager_chain: i64,
}

/// Verify every committed V3 and V4 map in the transaction's DB view.
///
/// The first divergent pool becomes the caller's rollback signal. The shared
/// verifier owns only map facts; this function preserves updater-specific
/// transaction and `last_update_block` policy.
///
/// # Errors
///
/// Returns database, provider, or caller-specific verification errors.
pub fn verify_all_pools_committed_on_conn(
    conn: &rusqlite::Connection,
    ctx: &FullVerifyCtx<'_>,
) -> Result<(), RunError> {
    let v3_addresses = DegenbotDb::fetch_v3_pool_addresses_on_conn(conn, ctx.chain_id)?;
    for addr in &v3_addresses {
        let addr_str = addr.to_checksum(None);
        let Some(state) =
            DegenbotDb::fetch_v3_pool_update_state_on_conn(conn, ctx.chain_id, &addr_str)?
        else {
            continue;
        };
        let (tick_bitmap, tick_data) =
            DegenbotDb::fetch_v3_liquidity_map_on_conn(conn, state.pool_id)?;
        let computed = ComputedLiquidityUpdate {
            pool_id: state.pool_id,
            tick_spacing: state.tick_spacing,
            tick_data,
            tick_bitmap,
            last_event: None,
        };
        let divergences = ctx.rt.block_on(verify_v3_liquidity_map_on_chain(
            ctx.provider,
            *addr,
            &computed,
            ctx.block_number,
        ))?;
        if !divergences.is_empty() {
            return Err(RunError::Verification {
                pool: addr_str,
                block_number: ctx.block_number,
                divergences,
            });
        }
    }

    let v4_hashes = DegenbotDb::fetch_v4_pool_hashes_on_conn(conn, ctx.chain_id)?;
    for (pool_hash, manager) in &v4_hashes {
        let Some(state) = DegenbotDb::fetch_v4_pool_update_state_on_conn(
            conn,
            pool_hash,
            ctx.pool_manager_chain,
        )?
        else {
            continue;
        };
        let (tick_bitmap, tick_data) =
            DegenbotDb::fetch_v4_liquidity_map_on_conn(conn, state.pool_id)?;
        let computed = ComputedLiquidityUpdate {
            pool_id: state.pool_id,
            tick_spacing: state.tick_spacing,
            tick_data,
            tick_bitmap,
            last_event: None,
        };
        let pool_id =
            B256::from_str(pool_hash.strip_prefix("0x").unwrap_or(pool_hash)).map_err(|e| {
                RunError::Provider(ProviderError::DecodingError {
                    message: format!("v4 full-verify: bad pool_hash {pool_hash:?}: {e}"),
                })
            })?;
        let divergences = ctx.rt.block_on(verify_v4_liquidity_map_on_chain(
            ctx.provider,
            *manager,
            pool_id,
            &computed,
            ctx.block_number,
        ))?;
        if !divergences.is_empty() {
            return Err(RunError::Verification {
                pool: pool_hash.clone(),
                block_number: ctx.block_number,
                divergences,
            });
        }
    }
    Ok(())
}

/// Whether a market-wide verification interval boundary is crossed.
pub(crate) fn should_run_full_verify_at_interval(
    working_start: u64,
    chunk_end: u64,
    interval: Option<u64>,
) -> bool {
    let Some(n) = interval else { return false };
    if n == 0 || chunk_end == 0 {
        return false;
    }
    working_start.saturating_sub(1) / n != chunk_end / n
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use degenbot_db::{ApplyBitmapAtWord, ApplyLiquidityAtTick};
    use hashbrown::HashMap;

    fn computed() -> ComputedLiquidityUpdate {
        let mut tick_data = HashMap::new();
        tick_data.insert(
            0,
            ApplyLiquidityAtTick {
                liquidity_gross: U128::from(1),
                liquidity_net: alloy::primitives::I256::try_from(1i128).unwrap(),
                block: 0,
            },
        );
        let mut tick_bitmap = HashMap::new();
        tick_bitmap.insert(
            0,
            ApplyBitmapAtWord {
                bitmap: U256::from(1),
                block: 0,
            },
        );
        ComputedLiquidityUpdate {
            pool_id: 1,
            tick_spacing: 1,
            tick_data,
            tick_bitmap,
            last_event: None,
        }
    }

    #[test]
    fn updater_projection_preserves_shared_typed_facts() {
        let facts = vec![
            LiquidityMapDivergence::TickPresence {
                tick: 1,
                stored: true,
                observed: false,
            },
            LiquidityMapDivergence::TickGross {
                tick: 2,
                expected: U128::from(3),
                actual: U128::from(4),
            },
            LiquidityMapDivergence::TickNet {
                tick: 3,
                expected: -1,
                actual: 2,
            },
            LiquidityMapDivergence::BitmapWord {
                word: 4,
                expected: U256::from(5),
                actual: U256::from(6),
            },
        ];
        let projected: Vec<LiquidityDivergence> = facts.into_iter().map(Into::into).collect();
        assert_eq!(
            projected[0],
            LiquidityDivergence::TickPresence {
                tick: 1,
                stored: true,
                observed: false
            }
        );
        assert_eq!(projected.len(), 4);
    }

    #[test]
    fn empty_computed_map_is_a_tracked_full_map() {
        let map = shared_map(&ComputedLiquidityUpdate {
            tick_data: HashMap::new(),
            tick_bitmap: HashMap::new(),
            ..computed()
        })
        .unwrap();
        assert!(map.ticks.is_empty());
        assert!(map.bitmaps.is_empty());
    }
}

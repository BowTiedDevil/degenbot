//! The command-stream path-info projection: a hop list over `BotState` pool
//! identities becomes the executor's `composers::PathInfo` (the
//! encode-relay flatten).
//!
//! The projection resolves every hop's identity from the shared [`BotState`]
//! and feeds the SAME `composers::PathInfo` to the SAME
//! `degenbot_executor::composers::encode_cmd_stream`, so byte-for-byte
//! encoder parity reduces to "does the projection build identical `HopInfo`
//! field values?" — pinned by the `degenbot-executor` 59-fn golden suite +
//! the `tests/rust-seam/` dispatch spy tests.
//!
//! # Unsupported hop families
//!
//! `composers::HopInfo` has only `V2`/`V3`/`V4` variants. The solver's
//! `HopType` enum additionally covers `SolidlyStable`, `BalancerWeighted`,
//! `BalancerStable`, `CurveStableswap` — for which the command-stream encoder
//! has no arm (`composers.rs` "2-hop Solidly / mixed-V2-V3-with-Solidly: not
//! supported"). The projection returns
//! [`PathInfoBuildError::UnsupportedHopType`] for these — matching the
//! pre-flatten `extract_hop` behavior, which rejected `SolidlyHopInfo` with
//! "hop must be V2HopInfo/V3HopInfo/V4HopInfo". No new encoding capability is
//! added; the pre-existing Solidly/Balancer/Curve encoding gap is preserved.

use ::degenbot_executor::composers::{HopInfo, PathInfo};
use ::degenbot_solvers::mixed::{HopType, MixedPoolRef};
use thiserror::Error;

use crate::executor_hop::{v2_hop, v3_hop, v4_hop, V2Fee, V2FeeRefusal};
use crate::BotState;

/// Why [`build_path_info`] could not build a `PathInfo`.
#[derive(Debug, Error)]
pub enum PathInfoBuildError {
    /// A hop's `pool_key` is not registered in the associated `BotState`.
    #[error("pool_id {pool_id} is not registered in the associated BotState")]
    PoolNotRegistered { pool_id: u64 },
    /// The hop's family has no command-stream encoder arm (Solidly /
    /// Balancer / Curve). Matches the pre-flatten `extract_hop` rejection.
    #[error(
        "hop_type {hop_type:?} (pool_id {pool_id}) is not supported by the command-stream encoder"
    )]
    UnsupportedHopType { hop_type: HopType, pool_id: u64 },
    /// The registered V2 fee cannot be represented by the executor.
    #[error("pool_id {pool_id} has an invalid V2 fee: {reason}")]
    InvalidV2Fee {
        pool_id: u64,
        #[source]
        reason: V2FeeRefusal,
    },
}

/// Build the `composers::PathInfo` for a hop list straight off the shared
/// core — the ENGINE-LOCK-FREE form: the inline-sim hook runs
/// in the SOLVE WORKER while the calling cycle holds the engine `Mutex`, so
/// it must NEVER re-enter the engine lock. The projection resolves every
/// hop's identity from the core (engine-then-core discipline: the caller
/// takes the core read directly, holding no engine state).
///
/// # Errors
/// A hop's pool identity is unregistered (or its family has no encoder arm)
/// — the exact [`PathInfoBuildError`] the engine-side projection raises.
pub fn build_path_info(
    core: &BotState,
    pools: &[MixedPoolRef],
) -> Result<PathInfo, PathInfoBuildError> {
    let mut hops = Vec::with_capacity(pools.len());
    for pool_ref in pools {
        hops.push(build_hop_info(core, pool_ref)?);
    }
    Ok(PathInfo::new(hops))
}

/// Resolve one registered hop to its encoder descriptor.
///
/// `pool_ref.pool_key` is the `BotState` `pool_id` (set at `register_path`
/// time — see the engine's `lifecycle::register_path`: `pool_key: hop.pool_id`).
fn build_hop_info(core: &BotState, pool_ref: &MixedPoolRef) -> Result<HopInfo, PathInfoBuildError> {
    match pool_ref.hop_type {
        HopType::V2 => {
            let id = core.get_v2_identity(pool_ref.pool_key).ok_or(
                PathInfoBuildError::PoolNotRegistered {
                    pool_id: pool_ref.pool_key,
                },
            )?;
            // The Python `build_hops_from_pools` computes
            // `fee = int(pool.fee_token0 * 10000)` where the Python
            // `fee_token0` is the FEE fraction `Fraction(denom - gamma, denom)`
            // derived from the Rust `(gamma_numer, fee_denom)` pair (see
            // `v2_liquidity_pool.py` — `self._fee_token0 = Fraction(denom - gamma, denom)`).
            // `int()` truncates toward zero; both operands are non-negative so
            // this is floor division. The projection matches by construction.
            let (gamma, denom) = if pool_ref.zero_for_one {
                id.fee_token0
            } else {
                id.fee_token1
            };
            let fee = V2Fee::from_retained(gamma, denom).map_err(|reason| {
                PathInfoBuildError::InvalidV2Fee {
                    pool_id: pool_ref.pool_key,
                    reason,
                }
            })?;
            Ok(v2_hop(
                id.address,
                id.token0,
                id.token1,
                fee,
                pool_ref.zero_for_one,
            ))
        }
        HopType::V3 => {
            let id = core.get_v3_identity(pool_ref.pool_key).ok_or(
                PathInfoBuildError::PoolNotRegistered {
                    pool_id: pool_ref.pool_key,
                },
            )?;
            Ok(v3_hop(
                id.address,
                id.token0,
                id.token1,
                id.fee,
                pool_ref.zero_for_one,
            ))
        }
        HopType::V4 => {
            let id = core.get_v4_identity(pool_ref.pool_key).ok_or(
                PathInfoBuildError::PoolNotRegistered {
                    pool_id: pool_ref.pool_key,
                },
            )?;
            Ok(v4_hop(
                id.pool_manager,
                alloy::primitives::B256::new(id.pool_id),
                id.pool_key.currency0,
                id.pool_key.currency1,
                id.pool_key.fee,
                id.pool_key.tick_spacing,
                id.pool_key.hooks,
                pool_ref.zero_for_one,
            ))
        }
        HopType::SolidlyStable
        | HopType::BalancerWeighted
        | HopType::BalancerStable
        | HopType::CurveStableswap => Err(PathInfoBuildError::UnsupportedHopType {
            hop_type: pool_ref.hop_type,
            pool_id: pool_ref.pool_key,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hop's family has no command-stream encoder arm: the projection
    /// refuses rather than silently dropping the hop.
    #[test]
    fn unsupported_hop_type_is_refused_by_the_encoder_projection() {
        let core = crate::BotState::new();
        let pools = [MixedPoolRef {
            hop_type: HopType::SolidlyStable,
            pool_key: 0,
            zero_for_one: true,
        }];
        assert!(matches!(
            build_path_info(&core, &pools),
            Err(PathInfoBuildError::UnsupportedHopType {
                hop_type: HopType::SolidlyStable,
                pool_id: 0,
            })
        ));
    }
}

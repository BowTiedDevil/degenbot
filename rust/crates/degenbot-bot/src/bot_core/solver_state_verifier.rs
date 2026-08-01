//! Solver-state accuracy gate — diff the solver's stored pool state against
//! the chain (the ADR-005 Option-A state-accuracy assertion).
//!
//! The mutation-nonce staleness gate (`candidate_is_stale`) verifies that a
//! pool's local state was *updated* — it proves the nonce changed, NOT that
//! the per-hop state handed to the solver/encoder *equals* the chain. If the
//! solver runs on a desynced snapshot (a missed log, an unhandled reorg, or a
//! directly-storage-mutated pool), it emits "impossible" `hop_outputs` and all
//! post-hoc (sim / balance) analysis is moot. This module closes that gap:
//! for each hop, `eth_call`s the canonical on-chain scalar state at the solve
//! block and diffs it against the `BotState` snapshot `resolve_path` consumed.
//!
//! Verified fields are the MUTABLE scalar state the solver predicts from:
//! V2 `(reserve0, reserve1)`, V3/V4 `(sqrtPriceX96, liquidity, tick)` — the
//! same fields the existing `liquidity_verifier` deliberately does NOT touch
//! (it only checks the immutable-init tick *map*). Env-gated; on any mismatch
//! the caller panics immediately (see `AV42C7` Option A).

use alloy::primitives::{Address, I256, U256};
use degenbot_rpc::abi::{fetch_v2_reserves, fetch_v3_slot0_liquidity, fetch_v4_pool_state};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_solvers::mixed::{HopType, MixedPoolRef};

use super::BotState;

/// The solver's stored per-hop scalar state, pre-extracted from `BotState`
/// so the raw (non-`Clone`, non-`Send`) state can be dropped BEFORE the async
/// on-chain reads begin — the pump must not hold a `parking_lot` read guard
/// across an `.await`.
#[derive(Debug, Clone)]
pub struct SolverHopScalarState {
    pub hop_type: HopType,
    /// V2: `(pair, reserve0, reserve1)`.
    pub v2: Option<(Address, U256, U256)>,
    /// V3: `(pool, sqrt_price_x96, liquidity, tick)`.
    pub v3: Option<(Address, U256, u128, i32)>,
    /// V4: `(pool_manager, pool_id, sqrt_price_x96, liquidity, tick)`.
    pub v4: Option<(Address, [u8; 32], U256, u128, i32)>,
}

/// Extract the solver's stored per-hop scalar state from `BotState` (the
/// states `resolve_path` consumed). Run this INSIDE the core read-guard scope;
/// return its result, drop the guard, then pass to
/// [`verify_solver_hop_states`]. Hops whose family is not a CL/V2 scalar diff
/// (Solidly / Balancer / Curve) or whose state is missing are skipped.
#[must_use]
pub fn extract_solver_hop_states(
    core: &BotState,
    pools: &[MixedPoolRef],
) -> Vec<SolverHopScalarState> {
    pools
        .iter()
        .map(|pool_ref| SolverHopScalarState {
            hop_type: pool_ref.hop_type,
            v2: match pool_ref.hop_type {
                HopType::V2 => core
                    .get_v2_pool_state(pool_ref.pool_key)
                    .zip(core.get_v2_identity(pool_ref.pool_key))
                    .map(|(state, identity)| {
                        (
                            identity.address,
                            U256::from(state.reserve0),
                            U256::from(state.reserve1),
                        )
                    }),
                _ => None,
            },
            v3: match pool_ref.hop_type {
                HopType::V3 => core
                    .get_v3_pool(pool_ref.pool_key)
                    .zip(core.get_v3_identity(pool_ref.pool_key))
                    .map(|(state, identity)| {
                        (
                            identity.address,
                            state.sqrt_price_x96,
                            state.liquidity,
                            state.tick,
                        )
                    }),
                _ => None,
            },
            v4: match pool_ref.hop_type {
                HopType::V4 => core
                    .get_v4_pool(pool_ref.pool_key)
                    .zip(core.get_v4_identity(pool_ref.pool_key))
                    .map(|(state, identity)| {
                        (
                            identity.pool_manager,
                            identity.pool_id,
                            state.sqrt_price_x96,
                            state.liquidity,
                            state.tick,
                        )
                    }),
                _ => None,
            },
        })
        .collect()
}

/// A per-hop chain-vs-solver state mismatch (or read failure), rendered as an
/// actionable message the pump can panic with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolverStateMismatch {
    /// The human-readable failure detail (the panic message body).
    pub message: String,
}

impl std::fmt::Display for SolverStateMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SolverStateMismatch {}

/// Whether a V2 pair's solver-stored reserves match the on-chain reserves.
/// `reserve0`/`reserve1` are uint112 on-chain; compared exactly.
#[must_use]
pub fn v2_state_matches(
    solver_reserve0: U256,
    solver_reserve1: U256,
    chain_reserve0: U256,
    chain_reserve1: U256,
) -> bool {
    solver_reserve0 == chain_reserve0 && solver_reserve1 == chain_reserve1
}

/// Whether a V3/V4 pool's solver-stored CL scalar state matches the on-chain
/// `slot0`+`liquidity` state. The solver's predictions are a deterministic
/// function of `(sqrtPriceX96, liquidity, tick)`, so an exact diff of all
/// three is the correct accuracy oracle.
#[must_use]
pub fn cl_state_matches(
    solver_sqrt_price_x96: U256,
    solver_liquidity: u128,
    solver_tick: i32,
    chain_sqrt_price_x96: U256,
    chain_liquidity: U256,
    chain_tick: I256,
) -> bool {
    solver_sqrt_price_x96 == chain_sqrt_price_x96
        && U256::from(solver_liquidity) == chain_liquidity
        && solver_tick == chain_tick.as_i32()
}

/// Verify that every hop's solver-stored scalar state (`extract_solver_hop_states`)
/// matches the chain at `block` (the solve block). Fails fast on the FIRST
/// mismatch or any read error.
///
/// V2 reads `getReserves`, V3 `slot0`+`liquidity`, and V4 `getPool` on the
/// hop's own `PoolManager` — no external `StateView` address required. Non-CL hop
/// families (Solidly / Balancer / Curve) are skipped — their solve state is
/// not a simple scalar slot0/getReserves diff.
///
/// # Errors
///
/// Returns [`SolverStateMismatch`] on any hop whose stored state diverges
/// from the chain at `block`, or on an `eth_call` transport/decode failure.
#[allow(clippy::too_many_lines)]
pub async fn verify_solver_hop_states(
    provider: &AlloyProvider,
    hops: &[SolverHopScalarState],
    block: u64,
) -> Result<(), SolverStateMismatch> {
    for (i, hop) in hops.iter().enumerate() {
        match hop.hop_type {
            HopType::V2 => {
                let Some((pool, s0, s1)) = hop.v2 else {
                    continue;
                };
                let (c0, c1) = fetch_v2_reserves(provider, &pool, Some(block))
                    .await
                    .map_err(|e| mismatch(i, &format!("V2 eth_call at block {block}: {e}")))?;
                if !v2_state_matches(s0, s1, c0, c1) {
                    return Err(mismatch(
                        i,
                        &format!(
                            "V2 pool {pool} at block {block} state mismatch: solver \
                             reserve0/1 = ({s0},{s1}), on-chain = ({c0},{c1})"
                        ),
                    ));
                }
            }
            HopType::V3 => {
                let Some((pool, s_sqrt, s_liq, s_tick)) = hop.v3 else {
                    continue;
                };
                let (c_sqrt, c_tick, c_liq) =
                    fetch_v3_slot0_liquidity(provider, &pool, Some(block))
                        .await
                        .map_err(|e| mismatch(i, &format!("V3 eth_call at block {block}: {e}")))?;
                if !cl_state_matches(s_sqrt, s_liq, s_tick, c_sqrt, c_liq, c_tick) {
                    return Err(mismatch(
                        i,
                        &format!(
                            "V3 pool {pool} at block {block} state mismatch: solver \
                             (sqrt={s_sqrt}, liq={s_liq}, tick={s_tick}), on-chain \
                             (sqrt={c_sqrt}, liq={c_liq}, tick={c_tick})"
                        ),
                    ));
                }
            }
            HopType::V4 => {
                let Some((pm, pool_id, s_sqrt, s_liq, s_tick)) = hop.v4 else {
                    continue;
                };
                let (c_sqrt, c_tick, c_liq) =
                    fetch_v4_pool_state(provider, &pm, &pool_id, Some(block))
                        .await
                        .map_err(|e| mismatch(i, &format!("V4 eth_call at block {block}: {e}")))?;
                if !cl_state_matches(s_sqrt, s_liq, s_tick, c_sqrt, c_liq, c_tick) {
                    return Err(mismatch(
                        i,
                        &format!(
                            "V4 pool {pm} (id {:02x}…) at block {block} state mismatch: \
                             solver (sqrt={s_sqrt}, liq={s_liq}, tick={s_tick}), on-chain \
                             (sqrt={c_sqrt}, liq={c_liq}, tick={c_tick})",
                            pool_id[0]
                        ),
                    ));
                }
            }
            // Solidly / Balancer / Curve — no scalar diff in scope; pass-through.
            _ => {}
        }
    }
    Ok(())
}

fn mismatch(index: usize, message: &str) -> SolverStateMismatch {
    SolverStateMismatch {
        message: format!("hop {index}: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{I256, U256};

    #[test]
    fn v2_match_and_mismatch() {
        let (s0, s1) = (U256::from(1000u64), U256::from(2000u64));
        // Matching chain state → true.
        assert!(v2_state_matches(s0, s1, s0, s1));
        // Off-by-one on either reserve → false.
        assert!(!v2_state_matches(s0, s1, s0 + U256::from(1), s1));
        assert!(!v2_state_matches(s0, s1, s0, s1 - U256::from(1)));
        // Both wrong → false.
        assert!(!v2_state_matches(s0, s1, U256::from(9), U256::from(9)));
    }

    #[test]
    fn cl_match_and_mismatch() {
        let sqrt = U256::from(2u128.pow(100));
        let liq = 1_000_000u128;
        let tick = -12345i32;
        // Matching → true.
        assert!(cl_state_matches(
            sqrt,
            liq,
            tick,
            sqrt,
            U256::from(liq),
            I256::unchecked_from(tick)
        ));
        // sqrt off → false.
        assert!(!cl_state_matches(
            sqrt,
            liq,
            tick,
            sqrt + U256::from(1),
            U256::from(liq),
            I256::unchecked_from(tick)
        ));
        // liquidity off → false.
        assert!(!cl_state_matches(
            sqrt,
            liq,
            tick,
            sqrt,
            U256::from(liq) - U256::from(1),
            I256::unchecked_from(tick)
        ));
        // tick off → false.
        assert!(!cl_state_matches(
            sqrt,
            liq,
            tick,
            sqrt,
            U256::from(liq),
            I256::unchecked_from(tick + 1)
        ));
    }
}

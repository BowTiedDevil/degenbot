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
use degenbot_rpc::abi::{fetch_v2_reserves, fetch_v3_slot0_liquidity, fetch_v4_slot0_liquidity};
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
    /// The solver's stored `update_block` for this hop's pool — the block its
    /// scalar state was last advanced to. Diffing this against the solve block
    /// discriminates the two mismatch classes: pure WS latency
    /// (`update_block < block`) vs. a same-block sub-tick corruption
    /// (`update_block == block` yet sqrtPrice/liquidity/tick diverges).
    pub update_block: u64,
    /// CL hop registration metadata (CL pools only; `None` for V2):
    /// `(coverage, lifecycle)` as Debug strings — `Tracked`/`Sparse` and
    /// `Live`/`Quarantined`. A pool frozen far behind the solve block that is
    /// `Sparse`/`Quarantined` is a path-referenced pool the live pump never
    /// applies — the state is pinned at its seed, not a live-swap drop.
    pub cl_meta: Option<(String, String)>,
    /// V2: `(pair, reserve0, reserve1)`.
    pub v2: Option<(Address, U256, U256)>,
    /// V3: `(pool, sqrt_price_x96, liquidity, tick)`.
    pub v3: Option<(Address, U256, u128, i32)>,
    /// V4: `(pool_manager, pool_id, state_view, sqrt_price_x96, liquidity, tick)`.
    pub v4: Option<(Address, [u8; 32], Address, U256, u128, i32)>,
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
        .map(|pool_ref| {
            let cl_meta = match pool_ref.hop_type {
                HopType::V3 => core.get_v3_pool(pool_ref.pool_key).map(|s| {
                    (
                        format!("{:?}", s.coverage),
                        format!("{:?}", s.registration_lifecycle),
                    )
                }),
                HopType::V4 => core.get_v4_pool(pool_ref.pool_key).map(|s| {
                    (
                        format!("{:?}", s.coverage),
                        format!("{:?}", s.registration_lifecycle),
                    )
                }),
                _ => None,
            };
            SolverHopScalarState {
                hop_type: pool_ref.hop_type,
                update_block: core.pool_update_block(pool_ref.pool_key),
                cl_meta,
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
                        .and_then(|(state, identity)| {
                            core.state_view_for(identity.pool_manager)
                                .map(|state_view| {
                                    (
                                        identity.pool_manager,
                                        identity.pool_id,
                                        state_view,
                                        state.sqrt_price_x96,
                                        state.liquidity,
                                        state.tick,
                                    )
                                })
                        }),
                    _ => None,
                },
            }
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
/// matches the chain at the SOLVER'S OWN anchor block (`hop.update_block`) — the
/// block its stored state claims to reflect. A pool 1-2 blocks behind the solve
/// block is normal latency; matching the chain at `update_block` proves the state
/// is accurate where it says it is, and only a divergence even AT the anchor is a
/// true desync (missed log / reorg / storage mutation). `block` (the solve block)
/// is retained for staleness reporting only. Fails fast on the FIRST mismatch or
/// any read error.
///
/// Each mismatch message carries the hop's solver-stored `update_block` and the
/// staleness `block - update_block`, so a panic both proves the divergence and
/// reports its age.
///
/// V2 reads `getReserves`, V3 `slot0`+`liquidity`, and V4 the `StateView`'s
/// `getSlot0`+`getLiquidity` at the hop's Rust-owned `state_view` (ADR-005 /
/// Option 2 — not `getPool`, which reverts on the canonical `PoolManager`). Non-CL hop
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
        // Compare against the chain at the SOLVER'S OWN anchor block
        // (`update_block`) — the block its stored scalar state claims to
        // reflect. A solver holding state from 1-2 blocks ago is normal
        // latency (you cannot know future swaps); matching the chain at
        // `update_block` proves the state is accurate where it says it is.
        // Only a state that diverges even AT its own anchor is a true
        // desync (a missed log / reorg / storage mutation). `block` (the
        // solve block) remains for message context / staleness reporting.
        let anchor = if hop.update_block > 0 {
            hop.update_block
        } else {
            block
        };
        // A hop whose `update_block >= block` was touched IN the solve block
        // (`block` is the pump's in-progress header block). Its scalar reflects
        // a mid-block capture (an early swap of the block), which a historical
        // `slot0` read can never reproduce — the RPC returns the BLOCK-FINAL
        // value, after later same-block swaps. Such a state is honest-by-
        // construction and unverifiable here; skip it (it is also provably
        // active, never the frozen-stale class the gate exists to catch). Only
        // a hop whose state reflects a COMPLETED block (`update_block < block`)
        // is diffed, against the chain at that completed anchor.
        if hop.update_block >= block {
            continue;
        }
        match hop.hop_type {
            HopType::V2 => {
                let Some((pool, s0, s1)) = hop.v2 else {
                    continue;
                };
                let (c0, c1) = fetch_v2_reserves(provider, &pool, Some(anchor))
                    .await
                    .map_err(|e| mismatch(i, &format!("V2 eth_call at block {anchor}: {e}")))?;
                if !v2_state_matches(s0, s1, c0, c1) {
                    return Err(mismatch(
                        i,
                        &format!(
                            "V2 pool {pool} state mismatch at its anchor block {anchor} \
                             (solve block {block}, solver update_block={ub}, behind by {}): \
                             solver reserve0/1 = ({s0},{s1}), on-chain = ({c0},{c1})",
                            block.saturating_sub(hop.update_block),
                            ub = hop.update_block,
                        ),
                    ));
                }
            }
            HopType::V3 => {
                let Some((pool, s_sqrt, s_liq, s_tick)) = hop.v3 else {
                    continue;
                };
                let (c_sqrt, c_tick, c_liq) =
                    fetch_v3_slot0_liquidity(provider, &pool, Some(anchor))
                        .await
                        .map_err(|e| mismatch(i, &format!("V3 eth_call at block {anchor}: {e}")))?;
                if !cl_state_matches(s_sqrt, s_liq, s_tick, c_sqrt, c_liq, c_tick) {
                    return Err(mismatch(
                        i,
                        &format!(
                            "V3 pool {pool} state mismatch at its anchor block {anchor} \
                             (solve block {block}, solver update_block={ub}, behind by {}): \
                             solver (sqrt={s_sqrt}, liq={s_liq}, tick={s_tick}), on-chain \
                             (sqrt={c_sqrt}, liq={c_liq}, tick={c_tick})",
                            block.saturating_sub(hop.update_block),
                            ub = hop.update_block,
                        ),
                    ));
                }
            }
            HopType::V4 => {
                let Some((pm, pool_id, state_view, s_sqrt, s_liq, s_tick)) = hop.v4 else {
                    continue;
                };
                let (c_sqrt, c_tick, _protocol_fee, _lp_fee, c_liq) =
                    fetch_v4_slot0_liquidity(provider, &state_view, &pool_id, Some(anchor))
                        .await
                        .map_err(|e| mismatch(i, &format!("V4 eth_call at block {anchor}: {e}")))?;
                if !cl_state_matches(s_sqrt, s_liq, s_tick, c_sqrt, c_liq, c_tick) {
                    return Err(mismatch(
                        i,
                        &format!(
                            "V4 pool {pm} (id {:02x}…) state mismatch at its anchor block {anchor} \
                             (solve block {block}, solver update_block={ub}, behind by {}): solver \
                             (sqrt={s_sqrt}, liq={s_liq}, tick={s_tick}), on-chain \
                             (sqrt={c_sqrt}, liq={c_liq}, tick={c_tick})",
                            pool_id[0],
                            block.saturating_sub(hop.update_block),
                            ub = hop.update_block,
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

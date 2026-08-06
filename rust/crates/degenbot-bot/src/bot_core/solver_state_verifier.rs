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
//!
//! The anchor diff (each hop at its OWN `update_block`) catches reorgs /
//! same-block sub-tick corruption but tolerates normal latency — a pool honest
//! at an OLD anchor can still be a frozen snapshot whose on-chain price moved
//! past it (missed swap events). The staleness re-check closes that gap: for a
//! CL hop whose `update_block` trails the solve block by more than
//! [`MAX_CL_STALENESS_BLOCKS`], a FRESH solve-block read that still diverges is
//! reported as a stale/desynced hop (the PancakeSwap-V3 non-canonical-Swap-
//! topic0 root cause — see `docs/exploration-no-profit-crash.md`).

use alloy::primitives::{Address, I256, U256};
use degenbot_rpc::abi::{fetch_v2_reserves, fetch_v3_slot0_liquidity, fetch_v4_slot0_liquidity};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_solvers::mixed::{HopType, MixedPoolRef};

use super::liquidity_verifier::{verify_v3_pool, verify_v4_pool, LiquidityVerifyError};
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
    /// The **liquidity** clock (`tick_data_block`, two-stamp OB7UNY) — the
    /// block this hop's tick map reflects. A CL hop whose `tick_data_block`
    /// lags its `update_block` is the staged-clock desync class (`0x5653`: a
    /// fresh price but a stale liquidity map) that the scalar-only ADR-021
    /// anchor diff cannot see — this surface makes it observable. Falls back
    /// to `update_block` for families with no tick-data clock and to `0` for
    /// an unregistered hop.
    pub tick_data_block: u64,
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
    /// The solver's **live CL tick map**, cloned out of `BotState` under the
    /// short read-guard so the solve-time tick-map fidelity probe
    /// ([`probe_cl_tick_map_fidelity`] → `verify_v3_pool` / `verify_v4_pool`)
    /// can run after the guard is dropped. `None` for V2/non-CL hops.
    ///
    /// The scalar gate ([`verify_solver_hop_states`] scalar diff) is blind to
    /// a snapshot whose tick map is missing positions entirely OFF the active
    /// range: active liquidity (`Σ net ≤ current tick`) is unchanged by a
    /// missing off-range position, yet a solve that CROSSES into the missing
    /// region under-counts liquidity and over-predicts output (the UO3JM4
    /// thin-tick-map class — ADR-021 D3). This map is the surface that makes
    /// that class observable at solve time.
    pub cl_tick_map: Option<ClTickMapSnapshot>,
}

/// A CL hop's solver-stored tick map, cloned under the short read-guard (see
/// [`SolverHopScalarState::cl_tick_map`]). The probe verifies it against
/// on-chain at the hop's **tick-data anchor** (`tick_data_block` — NOT the
/// price clock `update_block`, OB7UNY), because the map reflects the
/// liquidity clock. Verified against the tick-data anchor the map claims to
/// reflect, a 1-2 block price lag or a never-advanced liquidity clock does
/// not false-trip; only a genuinely desynced map (missed Mint/Burn, ghost
/// tick, decode bug) diverges.
#[derive(Clone, Debug)]
pub struct ClTickMapSnapshot {
    /// Tick spacing (immutable — lets the probe compress ticks into bitmap
    /// words).
    pub tick_spacing: i32,
    /// The solver's current active tick — seeds the ±2-word bitmap scan.
    pub active_tick: i32,
    /// The tick bookkeeping map: tick index → `(liquidity_gross, liquidity_net)`.
    pub tick_data: std::collections::HashMap<i32, degenbot_pools::TickInfo>,
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
                tick_data_block: core.pool_tick_data_block(pool_ref.pool_key),
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
                cl_tick_map: match pool_ref.hop_type {
                    HopType::V3 => core
                        .get_v3_pool(pool_ref.pool_key)
                        .zip(core.get_v3_identity(pool_ref.pool_key))
                        .map(|(state, identity)| ClTickMapSnapshot {
                            tick_spacing: identity.tick_spacing,
                            active_tick: state.tick,
                            tick_data: state.tick_data.clone(),
                        }),
                    HopType::V4 => core
                        .get_v4_pool(pool_ref.pool_key)
                        .zip(core.get_v4_identity(pool_ref.pool_key))
                        .map(|(state, identity)| ClTickMapSnapshot {
                            tick_spacing: identity.pool_key.tick_spacing,
                            active_tick: state.tick,
                            tick_data: state.tick_data.clone(),
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

/// The solver's anchor block to diff a hop against: its own `update_block`
/// when the pool has been updated (`> 0`), else the solve block (a never-
/// updated pool is verified at the solve block for want of a better anchor).
#[must_use]
fn solver_anchor_block(update_block: u64, block: u64) -> u64 {
    if update_block > 0 {
        update_block
    } else {
        block
    }
}

/// Maximum acceptable lag (in blocks) of a CL pool's stored `update_block`
/// behind the solve block before it is considered suspicious. The anchor-match
/// (exact diff at the hop's OWN `update_block`) tolerates 1-2 blocks of normal
/// pump latency, so this threshold is only reached by a genuinely stale hop
/// (missed swap events / a non-canonical Swap topic0 that the pump never
/// decodes, e.g. the PancakeSwap-V3 family — see the no-profit exploration
/// doc). `is_cl_pool_stale` at the solve block (a fresh read) then confirms
/// whether on-chain actually MOVED past the solver's snapshot; a quiet-but-
/// correct pool (chain unchanged) is not flagged.
const MAX_CL_STALENESS_BLOCKS: u64 = 3;

/// Whether a CL hop's stored `update_block` is old enough to warrant a
/// fresh solve-block re-check. Skips never-updated pools (`update_block == 0`,
/// verified at the solve block via the anchor path) and pools at/after the
/// solve block (skipped by `skip_in_progress_hop`).
#[must_use]
fn is_cl_pool_stale(update_block: u64, block: u64) -> bool {
    update_block > 0 && block.saturating_sub(update_block) > MAX_CL_STALENESS_BLOCKS
}

/// Whether a CL hop's stored price-clock `update_block` is AHEAD of the solve
/// `block` — the two-stamp PRICE clock running past the block being solved (the
/// backfill/dispatch race, live path 10956: solved 25677777 with a 25677789
/// price). The `>=` in `skip_in_progress_hop` groups this with the legitimate
/// mid-block `update_block == block` case and silently SKIPS it, so the
/// verification never sees a future-priced pool. Ahead is NEVER legitimate and
/// must be rejected loudly. Mirror of `is_cl_pool_stale` (which only handles
/// the behind case).
#[must_use]
fn is_future_price(update_block: u64, block: u64) -> bool {
    update_block > block
}

/// Whether a hop at `update_block` must be SKIPPED when verifying at solve
/// `block`: skip hops touched IN the in-progress block (`update_block >= block`)
/// — their scalar reflects a mid-block capture (an early swap of the block,
/// e.g. 0xE0554a @ 25658682 captured swap #1) that a historical `slot0` read
/// (block-final) can never reproduce. Only hops whose state reflects a
/// COMPLETED block (`update_block < block`) are diffed, against the chain at
/// that completed anchor.
#[must_use]
fn skip_in_progress_hop(update_block: u64, block: u64) -> bool {
    update_block >= block
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
        let anchor = solver_anchor_block(hop.update_block, block);
        // FUTURE-PRICE guard (belts + suspenders): `block` is the PUMP-promoted
        // solve block (`active_block = max(current_block, pool_state_head)`),
        // so no hop's `update_block` can exceed it (head is the max). This
        // fires only in a genuinely impossible case now — NOT the
        // backfill/dispatch race. Promoted at the pump (BO5FBS) instead of the
        // verifier re-anchoring at head, but the guard stays to catch a
        // mid-solve state advance or a bypassing caller.
        // Originally the solve block was the lagging drain clock and a hop at
        // head (path 10956: solved 25677777 with a 25677789 price) was LIVE
        // state, not a future price; aborting on it killed a capturable
        // opportunity (B2 — the block_pump promotes at head first).
        if is_future_price(hop.update_block, block) {
            return Err(mismatch(
                i,
                &format!(
                    "CL hop FUTURE at solve block {block} (solver update_block={}, ahead by {} blocks): \
                     the price clock runs past the block being solved — solving with a future price is \
                     never legitimate",
                    hop.update_block,
                    hop.update_block.saturating_sub(block),
                ),
            ));
        }
        // A hop whose `update_block >= block` was touched IN the solve block
        // (`block` is the pump's in-progress header block) is honest-by-
        // construction and unverifiable via historical slot0 — skip it. Only
        // a hop whose state reflects a COMPLETED block is diffed.
        if skip_in_progress_hop(hop.update_block, block) {
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
                // Staleness re-check: a hop accurate at its OWN (old) anchor can
                // still be a frozen snapshot whose on-chain price moved past it
                // (missed swap events — e.g. a PancakeSwap-V3 pool whose Swap
                // topic0 the pump does not decode). Compare against a FRESH
                // solve-block read; only flag when the solve-block state diverges
                // AND the hop is genuinely old. A quiet-but-correct pool (on-chain
                // unchanged) is not flagged.
                if is_cl_pool_stale(hop.update_block, block) {
                    let (b_sqrt, b_tick, b_liq) =
                        fetch_v3_slot0_liquidity(provider, &pool, Some(block))
                            .await
                            .map_err(|e| {
                                mismatch(i, &format!("V3 solve-block eth_call at {block}: {e}"))
                            })?;
                    if !cl_state_matches(s_sqrt, s_liq, s_tick, b_sqrt, b_liq, b_tick) {
                        return Err(mismatch(
                            i,
                            &format!(
                                "V3 pool {pool} STALE at solve block {block} (solver \
                                 update_block={ub}, behind by {} blocks): solver snapshot \
                                 (sqrt={s_sqrt}, liq={s_liq}, tick={s_tick}) no longer matches \
                                 on-chain at {block} (sqrt={b_sqrt}, liq={b_liq}, tick={b_tick}); \
                                 likely missed swap events (non-canonical Swap topic0)\n",
                                block.saturating_sub(hop.update_block),
                                ub = hop.update_block,
                            ),
                        ));
                    }
                }
                // Solve-time tick-map fidelity probe (ADR-021 D3): verify the
                // solver's LIVE tick map against on-chain at the tick-data
                // anchor. The scalar diff above only catches active-liquidity
                // divergence (Σ net ≤ current tick); a snapshot missing an
                // OFF-range position passes it yet under-counts liquidity once
                // a solve crosses into the missing region — the UO3JM4
                // over-prediction class.
                if let Some(cl) = hop.cl_tick_map.as_ref() {
                    probe_cl_tick_map_fidelity(
                        provider,
                        hop,
                        cl,
                        pool,
                        Address::ZERO,
                        [0u8; 32],
                        i,
                    )
                    .await?;
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
                if is_cl_pool_stale(hop.update_block, block) {
                    let (b_sqrt, b_tick, _b_pf, _b_lp, b_liq) =
                        fetch_v4_slot0_liquidity(provider, &state_view, &pool_id, Some(block))
                            .await
                            .map_err(|e| {
                                mismatch(i, &format!("V4 solve-block eth_call at {block}: {e}"))
                            })?;
                    if !cl_state_matches(s_sqrt, s_liq, s_tick, b_sqrt, b_liq, b_tick) {
                        return Err(mismatch(
                            i,
                            &format!(
                                "V4 pool {pm} (id {:02x}…) STALE at solve block {block} (solver \
                                 update_block={ub}, behind by {} blocks): solver snapshot \
                                 (sqrt={s_sqrt}, liq={s_liq}, tick={s_tick}) no longer matches \
                                 on-chain at {block} (sqrt={b_sqrt}, liq={b_liq}, tick={b_tick})",
                                pool_id[0],
                                block.saturating_sub(hop.update_block),
                                ub = hop.update_block,
                            ),
                        ));
                    }
                }
                // Solve-time tick-map fidelity probe (ADR-021 D3) — see the V3
                // arm above.
                if let Some(cl) = hop.cl_tick_map.as_ref() {
                    probe_cl_tick_map_fidelity(
                        provider,
                        hop,
                        cl,
                        Address::ZERO,
                        state_view,
                        pool_id,
                        i,
                    )
                    .await?;
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

/// A lightweight `TickMap` carrier over a CL hop's extracted snapshot, so the
/// solve-time tick-map fidelity probe can reuse `verify_v3_pool` /
/// `verify_v4_pool` (which take `&impl degenbot_pools::tick_map::TickMap`) on
/// the post-guard snapshot without cloning the tick map a second time.
struct SolverTickMapCarrier<'a> {
    address: Address,
    tick_spacing: i32,
    active_tick: i32,
    tick_data: &'a std::collections::HashMap<i32, degenbot_pools::TickInfo>,
}

impl degenbot_pools::tick_map::TickMap for SolverTickMapCarrier<'_> {
    fn address(&self) -> Address {
        self.address
    }

    fn tick_spacing(&self) -> i32 {
        self.tick_spacing
    }

    fn active_tick(&self) -> i32 {
        self.active_tick
    }

    fn tick_data(&self) -> &std::collections::HashMap<i32, degenbot_pools::TickInfo> {
        self.tick_data
    }
}

/// The solve-time CL **tick-map fidelity probe** (ADR-021 D3 — one state-
/// accuracy tripwire converging the solver-state scalar diff with the
/// liquidity-verifier tick-map machinery).
///
/// Verifies the solver's extracted live tick map against on-chain at the
/// hop's tick-data anchor (`tick_data_block`) via `verify_v3_pool` /
/// `verify_v4_pool`, which scan the bitmap and fire on "on-chain tick NOT in
/// engine" — the partial-snapshot class the scalar gate (active-liquidity
/// diff only) cannot see. A `Mismatch` is a hard trip (a genuine desync → the
/// gate aborts). An `Rpc` transport failure is NOT a divergence — it is logged
/// and skipped (the probe must not kill the bot on a transient read hiccup; a
/// retry/backoff candidate, per the liquidity-verifier `Rpc` contract).
///
/// The anchor is the hop's tick-data clock (`tick_data_block`), NOT the price
/// clock `update_block`: the map reflects the liquidity clock (OB7UNY), so
/// verifying at the map's own claimed anchor lets a 1-2 block price lag or a
/// never-advanced liquidity clock pass legitimately and only trips on a truly
/// desynced map.
#[allow(clippy::too_many_arguments)]
async fn probe_cl_tick_map_fidelity(
    provider: &AlloyProvider,
    hop: &SolverHopScalarState,
    cl: &ClTickMapSnapshot,
    v3_addr: Address,
    v4_state_view: Address,
    v4_pool_id: [u8; 32],
    i: usize,
) -> Result<(), SolverStateMismatch> {
    let tick_anchor = if hop.tick_data_block > 0 {
        hop.tick_data_block
    } else {
        hop.update_block
    };
    let carrier = SolverTickMapCarrier {
        address: match hop.hop_type {
            HopType::V3 => v3_addr,
            _ => v4_state_view,
        },
        tick_spacing: cl.tick_spacing,
        active_tick: cl.active_tick,
        tick_data: &cl.tick_data,
    };
    let result = match hop.hop_type {
        HopType::V3 => verify_v3_pool(provider, &carrier, Some(tick_anchor)).await,
        HopType::V4 => {
            verify_v4_pool(
                provider,
                v4_state_view,
                v4_pool_id,
                &carrier,
                Some(tick_anchor),
            )
            .await
        }
        _ => return Ok(()),
    };
    match result {
        Ok(()) => Ok(()),
        Err(LiquidityVerifyError::Mismatch(m)) => Err(mismatch(
            i,
            &format!(
                "tick-map fidelity probe at tick-data anchor {tick_anchor}: {} (the scalar gate \
                 diffs active liquidity only and can miss off-range missing positions — a thin/\
                 partial tick map over-predicts output once a solve crosses into the missing \
                 region; UO3JM4 class, ADR-021)",
                m.message,
            ),
        )),
        Err(LiquidityVerifyError::Rpc { message }) => {
            // A transport failure is NOT a divergence — do not abort the bot on
            // a transient read hiccup; log + continue (the `Mismatch` branch is
            // the hard trip).
            tracing::warn!(
                %message,
                tick_anchor,
                hop_index = i,
                "tick-map fidelity probe RPC skipped (transient, not a divergence)"
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{I256, U256};
    use alloy::transports::mock::Asserter;
    use std::sync::Arc;

    #[test]
    fn anchor_block_uses_update_block_when_set() {
        // A pool with a real update_block is diffed at ITS anchor.
        assert_eq!(solver_anchor_block(100, 102), 100);
        // A never-updated pool (update_block == 0) falls back to the solve block.
        assert_eq!(solver_anchor_block(0, 102), 102);
    }

    #[test]
    fn in_progress_hop_is_skipped_completed_hop_is_not() {
        // A hop touched IN the solve block (mid-block capture) is skipped.
        assert!(skip_in_progress_hop(25_658_682, 25_658_682));
        // A hop equal to or ahead of the solve block is skipped too.
        assert!(skip_in_progress_hop(25_658_683, 25_658_682));
        // A hop one block behind (normal latency) reflects a COMPLETED block
        // and is diffed, NOT skipped.
        assert!(!skip_in_progress_hop(25_658_682, 25_658_683));
        // A never-updated pool (0) is diffed at the solve block, not skipped.
        assert!(!skip_in_progress_hop(0, 100));
        // A genuinely frozen pool (far behind) is NOT skipped — the gate
        // verifies it and would catch the desync.
        assert!(!skip_in_progress_hop(25_658_442, 25_658_682));
    }

    #[test]
    fn fresh_pool_is_not_stale() {
        // A hop 1-2 blocks behind (normal pump latency) is NOT stale.
        assert!(!is_cl_pool_stale(100, 101));
        assert!(!is_cl_pool_stale(100, 102));
        // A hop at/beyond the solve block is not stale (handled by skip).
        assert!(!is_cl_pool_stale(102, 102));
        assert!(!is_cl_pool_stale(102, 101));
        // A never-updated pool (update_block == 0) is not flagged here; the
        // anchor path verifies it at the solve block instead.
        assert!(!is_cl_pool_stale(0, 100));
    }

    #[test]
    fn stale_pool_is_flagged_after_threshold() {
        // Exactly at threshold is not stale.
        assert!(!is_cl_pool_stale(100, 103));
        // One past the threshold -> stale.
        assert!(is_cl_pool_stale(100, 104));
        // The observed failure: a pool ~100 blocks behind IS stale.
        assert!(is_cl_pool_stale(25_664_550, 25_664_704));
    }

    #[test]
    fn future_price_is_detected() {
        // The observed failure: solved 25677777 with a 25677789 price clock.
        assert!(is_future_price(25_677_789, 25_677_777));
        // Strictly ahead (even +1) is never legitimate.
        assert!(is_future_price(101, 100));
        // Equal to the solve block is a mid-block capture, NOT future.
        assert!(!is_future_price(100, 100));
        // Behind (normal latency) is NOT future.
        assert!(!is_future_price(99, 100));
        assert!(!is_future_price(0, 100));
    }

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

    // -----------------------------------------------------------------
    // Solve-time tick-map fidelity probe (ADR-021 D3)
    //
    // RED: a CL hop whose in-memory tick map is MISSING an on-chain tick
    // must trip the gate EVEN WHEN the scalar diff passes. The scalar gate
    // compares (sqrt, liquidity=ACTIVE γ, tick); a missing OFF-range position
    // leaves γ unchanged so the scalar passes, yet a solve that crosses into
    // the missing region under-counts liquidity → over-predicts output (the
    // UO3JM4 thin-tick-map class).
    // -----------------------------------------------------------------

    /// Build a mock `AlloyProvider` whose `Asserter` queue is preloaded with
    /// `responses` (one JSON `result` per expected `eth_call`, FIFO). Mirrors
    /// the `liquidity_verifier` mock-transport harness.
    fn mock_provider(responses: Vec<String>) -> AlloyProvider {
        use alloy::network::Ethereum as NetEth;
        use alloy::providers::{Provider, ProviderBuilder};
        use alloy::rpc::client::ClientBuilder;
        use alloy::transports::mock::MockTransport;
        let asserter = Asserter::new();
        for resp in responses {
            asserter.push_success(&resp);
        }
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<NetEth>>
        )
    }

    /// Encode a Multicall3 `aggregate3` return payload — `(bool,bytes)[]` —
    /// as a `0x`-prefixed hex string (one `(success, return_data)` tuple per
    /// sub-call, in batch order).
    fn encode_mc3_return(results: &[(bool, Vec<u8>)]) -> String {
        let arr = alloy::dyn_abi::DynSolValue::Array(
            results
                .iter()
                .map(|(ok, data)| {
                    alloy::dyn_abi::DynSolValue::Tuple(vec![
                        alloy::dyn_abi::DynSolValue::Bool(*ok),
                        alloy::dyn_abi::DynSolValue::Bytes(data.clone()),
                    ])
                })
                .collect(),
        );
        format!("0x{}", alloy::primitives::hex::encode(arr.abi_encode()))
    }

    /// 32-byte ABI word for a u256 bitmap value.
    fn u256_word(val: u64) -> Vec<u8> {
        U256::from(val).to_be_bytes::<32>().to_vec()
    }

    /// 64-byte `ticks()` return: `(uint128 liquidityGross, int128 liquidityNet)`.
    fn ticks_return(gross: u128, net: i128) -> Vec<u8> {
        let mut buf = vec![0u8; 64];
        buf[16..32].copy_from_slice(&gross.to_be_bytes());
        let sign = if net < 0 { 0xff } else { 0x00 };
        for b in &mut buf[32..48] {
            *b = sign;
        }
        buf[48..64].copy_from_slice(&net.to_be_bytes());
        buf
    }

    fn tick_info(gross: u128, net: i128) -> degenbot_pools::TickInfo {
        degenbot_pools::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(gross),
            liquidity_net: I256::unchecked_from(net),
            block: 0,
        }
    }

    /// The RED case: a V3 hop whose in-memory tick map (tick 0 only) is
    /// missing on-chain tick 60. The scalar reads (slot0 + active liquidity)
    /// MATCH so the scalar gate would pass; only the tick-map fidelity probe
    /// catches the missing position → `verify_solver_hop_states` must return
    /// `Err` with the missing-tick class.
    #[tokio::test]
    async fn tickmap_fidelity_probe_catches_missing_onchain_tick() {
        let sqrt: U256 = U256::from(1u128) << 96;
        let liq = 1_000_000u128;
        let tick = 0i32;
        let mut tick_data = std::collections::HashMap::new();
        tick_data.insert(0, tick_info(100, 50)); // engine holds ONLY tick 0

        // Scalar + probe responses (FIFO):
        // 1. slot0 (7 words: sqrt, tick=0, five zero tail fields)
        let mut slot0 = vec![0u8; 7 * 32];
        slot0[0..32].copy_from_slice(&sqrt.to_be_bytes::<32>());
        // word 1 = tick 0, words 2-6 = zero tail
        // 2. liquidity (1 word = 1_000_000) — matches solver γ
        let mut liq_word = vec![0u8; 32];
        liq_word[16..32].copy_from_slice(&liq.to_be_bytes());
        // 3. bitmap batch over sorted words [-2,-1,0,1,2]; word 0 = bits {0,1}
        //    (on-chain ticks 0 AND 60 @ spacing 60) → value 3.
        let bitmap = encode_mc3_return(&[
            (true, u256_word(0)), // word -2
            (true, u256_word(0)), // word -1
            (true, u256_word(3)), // word  0 → ticks 0, 60
            (true, u256_word(0)), // word  1
            (true, u256_word(0)), // word  2
        ]);
        // 4. ticks batch over sorted [0, 60]: (100,50) for 0, (200,-30) for 60.
        let ticks = encode_mc3_return(&[
            (true, ticks_return(100, 50)),
            (true, ticks_return(200, -30)),
        ]);
        let provider = mock_provider(vec![
            format!("0x{}", alloy::primitives::hex::encode(&slot0)),
            format!("0x{}", alloy::primitives::hex::encode(&liq_word)),
            bitmap,
            ticks,
        ]);

        let hop = SolverHopScalarState {
            hop_type: HopType::V3,
            update_block: 100,
            tick_data_block: 100,
            cl_meta: None,
            v2: None,
            v3: Some((Address::from([0xaau8; 20]), sqrt, liq, tick)),
            v4: None,
            cl_tick_map: Some(ClTickMapSnapshot {
                tick_spacing: 60,
                active_tick: 0,
                tick_data,
            }),
        };

        // Scalar reads match on-chain (sqrt/γ/tick all equal at anchor 100),
        // so a scalar-only gate returns Ok; the tick-map probe must be what
        // turns this into an Err (the RED assertion).
        let err = verify_solver_hop_states(&provider, &[hop], 102)
            .await
            .expect_err("gate must trip: on-chain tick 60 is missing from the solver's map");
        assert!(
            err.message.contains("tick-map fidelity probe")
                && err.message.contains("NOT in engine"),
            "expected the missing-tick class in the trip message, got: {}",
            err.message
        );
    }

    /// V4 twin of `tickmap_fidelity_probe_catches_missing_onchain_tick`: a V4
    /// hop whose in-memory tick map is missing an on-chain tick must trip the
    /// gate even though its scalar reads (getSlot0 sqrt/tick + getLiquidity γ)
    /// match on-chain. V4 RPC goes through `StateView.getTickBitmap` /
    /// `getTickLiquidity` (the `state_view` param), but the mock transport
    /// returns queued responses FIFO regardless of target/calldata — so the
    /// bitmap/ticks batch encoding is identical to the V3 case.
    #[tokio::test]
    async fn v4_tickmap_fidelity_probe_catches_missing_onchain_tick() {
        let sqrt: U256 = U256::from(1u128) << 96;
        let liq = 1_000_000u128;
        let tick = 0i32;
        let mut tick_data = std::collections::HashMap::new();
        tick_data.insert(0, tick_info(100, 50)); // engine holds ONLY tick 0

        let pool_manager = Address::from([0xbbu8; 20]);
        let state_view = Address::from([0xccu8; 20]);
        let pool_id = [0x11u8; 32];

        // 1. getSlot0 (4 words: sqrt, tick=0, protocolFee=0, lpFee=0)
        let mut slot0 = vec![0u8; 4 * 32];
        slot0[0..32].copy_from_slice(&sqrt.to_be_bytes::<32>());
        // word 1 = tick 0, words 2-3 = fees 0
        // 2. getLiquidity (1 word = 1_000_000) — matches solver γ
        let mut liq_word = vec![0u8; 32];
        liq_word[16..32].copy_from_slice(&liq.to_be_bytes());
        // 3. bitmap batch over sorted words [-2,-1,0,1,2]; word 0 = bits {0,1}
        //    (on-chain ticks 0 AND 60 @ spacing 60) → value 3.
        let bitmap = encode_mc3_return(&[
            (true, u256_word(0)), // word -2
            (true, u256_word(0)), // word -1
            (true, u256_word(3)), // word  0 → ticks 0, 60
            (true, u256_word(0)), // word  1
            (true, u256_word(0)), // word  2
        ]);
        // 4. ticks batch over sorted [0, 60]: (100,50) for 0, (200,-30) for 60.
        let ticks = encode_mc3_return(&[
            (true, ticks_return(100, 50)),
            (true, ticks_return(200, -30)),
        ]);
        let provider = mock_provider(vec![
            format!("0x{}", alloy::primitives::hex::encode(&slot0)),
            format!("0x{}", alloy::primitives::hex::encode(&liq_word)),
            bitmap,
            ticks,
        ]);

        let hop = SolverHopScalarState {
            hop_type: HopType::V4,
            update_block: 100,
            tick_data_block: 100,
            cl_meta: None,
            v2: None,
            v3: None,
            v4: Some((pool_manager, pool_id, state_view, sqrt, liq, tick)),
            cl_tick_map: Some(ClTickMapSnapshot {
                tick_spacing: 60,
                active_tick: 0,
                tick_data,
            }),
        };

        let err = verify_solver_hop_states(&provider, &[hop], 102)
            .await
            .expect_err("gate must trip: V4 on-chain tick 60 missing from the solver's map");
        assert!(
            err.message.contains("tick-map fidelity probe")
                && err.message.contains("NOT in engine"),
            "expected the missing-tick class in the trip message, got: {}",
            err.message
        );
    }
}

//! Path resolution, solver dispatch, and rebuild logic.

use alloy::primitives::U256;
use rayon::prelude::*;

use super::{ArbitrageEngine, BlockMetadata, HashMap, HashSet};

// There is deliberately NO solve-time "staleness" pre-gate here (ergo YXHHKR,
// resolved QNFYR5). The former TQ43TU `hop_is_too_stale` gate deferred a whole
// path on any co-hop whose price-clock `update_block` trailed > 10 blocks — but
// `update_block` is a last-activity clock, so a pool that swapped once and then
// went quiet (state byte-identical to on-chain) was falsely deferred: QNFYR5's
// instrumented live run showed 3,550 such defers (V2/V3/V4, gap 11-16, 0 genuine)
// with a healthy engine solve→sim path underneath. A static age check cannot
// distinguish "quiet but current" from "genuinely moved but only moderately
// behind" (AV42C7 — the zero-tolerance retread was already REVERTED for the same
// over-deferral). The accurate discriminator requires a fresh on-chain read, which
// the ADR-021 verifier (`verify_solver_state_against_chain`) already performs at
// the publish point, diffing each hop at its OWN `update_block` anchor and
// `std::process::abort`ing on the first real desync before simulation. That
// tripwire — not an age heuristic — is the sole chain/solver-mismatch guard; on a
// genuine stale/desync pool it fails HARD and LOUDLY, which is the preferred
// behavior (develop on loud failures).

/// Whether a hop's price clock runs AHEAD of the solve block (U6RNHH T1).
/// Mirror of the verifier's `is_future_price`; after the B2 re-anchor
/// (`solve_block = max(block_number, pool_state_head)`) this is impossible for
/// any pool (head is the max across all pools), so this is a belt-and-suspenders
/// invariant assertion, not a normal-path rejection.
#[must_use]
fn hop_is_future(update_block: u64, solve_block: u64) -> bool {
    update_block > solve_block
}
use ::degenbot_solvers::mixed::{
    BalancerStableHopState, BalancerWeightedHopState, CurveStableswapHopState, HopType,
    MixedPoolRef, ResolvedHop, ResolvedMixedPath, SolidlyHopState, SolvePathResult,
};

impl ArbitrageEngine {
    /// Re-resolve and re-solve only paths that contain updated pools.
    ///
    /// Uses the `pool_to_paths` reverse index to identify `affected_path_ids`,
    /// then re-resolves and re-solves only those. Unaffected paths carry
    /// their previous results forward.
    #[allow(clippy::too_many_lines)]
    pub fn rebuild_and_solve_affected(
        &mut self,
        v2_affected: &HashSet<u64>,
        v3_affected: &HashSet<u64>,
        v4_affected: &HashSet<u64>,
        block_number: u64,
        _metadata: &BlockMetadata,
    ) {
        tracing::info!(
            "[solver-dbg] rebuild_and_solve_affected called block={block_number} dirty v2={} v3={} v4={}",
            v2_affected.len(),
            v3_affected.len(),
            v4_affected.len()
        );
        // Collect affected path IDs from the reverse index
        let mut affected_path_ids: HashSet<u64> = HashSet::new();

        for pool_key in v2_affected {
            if let Some(path_ids) = self.pool_to_paths.get(&(HopType::V2, *pool_key)) {
                affected_path_ids.extend(path_ids);
            }
        }
        for pool_key in v3_affected {
            if let Some(path_ids) = self.pool_to_paths.get(&(HopType::V3, *pool_key)) {
                affected_path_ids.extend(path_ids);
            }
        }
        for pool_key in v4_affected {
            if let Some(path_ids) = self.pool_to_paths.get(&(HopType::V4, *pool_key)) {
                affected_path_ids.extend(path_ids);
            }
        }

        // Also re-solve any paths registered via register_and_solve_path that
        // haven't been through rebuild_and_solve_affected yet. These paths were
        // eagerly solved at registration time, but the pump's process_block
        // replaces self.results entirely — so we must include them to avoid
        // dropping their results.
        affected_path_ids.extend(&self.pending_new_paths);
        self.pending_new_paths.clear();

        // ADR-021 change-set accumulation (pump-freeze fix): record every path
        // re-solved this solve cycle so the solver-state verifier can scope its
        // per-block on-chain diff to ONLY the paths actually solved (consumed
        // + cleared at the publish point via `take_solver_path_pool_refs_change_set`)
        // instead of the whole registered set — the verified root of the
        // confirmed O(registered × hops × RPC) pump freeze. Union/accumulate
        // (not overwrite) so a multi-solve-before-publish batch is fully
        // covered rather than only its final solve.
        self.last_solved_path_ids.extend(&affected_path_ids);

        // Solve-block anchor: the batch's `solve_block` (= `results_block`)
        // is the block the pool state actually reflects — the pool-state
        // head, NOT the (possibly-lagging) drain `block_number`. Since
        // BO5FBS the pump pre-promotes `active_block = max(current_block,
        // pool_state_head())` before calling `on_drain`, so `block_number`
        // here is already >= `pool_state_head` and this `max` is a defensive
        // no-op that preserves the pre-promotion invariant if any caller
        // bypasses the pump (e.g. tests driving `solve_dirty` directly). Keep
        // it — it is now the guard, not the re-anchor.
        let solve_block = block_number.max(self.core.read().pool_state_head());
        // If no paths are affected, just update the block number
        if affected_path_ids.is_empty() {
            self.results_block = solve_block;
            return;
        }

        // Re-resolve and solve only affected paths — update results in-place
        // without cloning unchanged entries.

        // Re-derive resolved hop states under the core lock — a single
        // consistent snapshot of BotState for the whole re-derive (ADR-003
        // Option A: one core-lock window per `solve_dirty`). V3/V4 state still
        // reads from the per-family block engines here; Slices 2/3 migrate
        // those into BotState too. The guard drops before `solve_path` runs,
        // which is pure `&self`.
        //
        // AV42C7 lesson: a per-path `update_block`-MIX freshness gate was
        // attempted here and REVERTED — it deferred every legitimate
        // single-pool-update arb (Sync pool A, solve with a stable reference
        // pool B at an older `update_block`). A zero-tolerance `update_block`
        // check cannot distinguish "this pool had no block-N event" (normal)
        // from "this pool is genuinely far behind" (missed swap events), so
        // its false-positive rate is catastrophic.
        //
        // YXHHKR (resolved QNFYR5): NO solve-time staleness gate here. The former
        // TQ43TU bounded-window gate deferred a whole path on any co-hop trailing
        // >10 blocks, but `update_block` is a last-activity clock, so a quiet-but-
        // current pool was falsely deferred (QNFYR5 proved 3,550 of them live).
        // `deferred_paths` is now reserved for the genuinely illegitimate future-
        // price case below; genuine chain/solver divergence is left to the ADR-021
        // verifier, which fatal-aborts loudly (the preferred failure, esp. in dev).
        let mut deferred_paths: HashSet<u64> = HashSet::new();
        {
            let core = self.core.read();
            for &path_id in &affected_path_ids {
                let Some(path) = self.path_pools.get(&path_id) else {
                    continue;
                };
                // U6RNHH T1 solve-stage future-price tripwire (belt + suspenders):
                // a hop whose PRICE clock runs AHEAD of the solve block is never
                // legitimate and must be rejected loudly, not solved (a future-price
                // solve reports a misleading downstream IIA). Note the B2 re-anchor
                // above sets `solve_block = max(block_number, pool_state_head)`,
                // so the only way a hop beats it is `update_block > pool_state_head`
                // — impossible by definition (head is the max across all pools).
                // This guard therefore normally never fires: it is an explicit
                // invariant assertion for the truly-future case, and it does NOT
                // regress B2 (which concerns `update_block > block_number` — a
                // legitimate live-head path that the re-anchor folds into
                // `solve_block`).
                let future = path.pools.iter().any(|pool_ref| {
                    hop_is_future(core.pool_update_block(pool_ref.pool_key), solve_block)
                });
                if future {
                    deferred_paths.insert(path_id);
                    tracing::error!(
                        "[future-price] path_id={path_id} rejected at solve block {solve_block}: \
                         a hop price clock runs AHEAD of the solve block (update_block > \
                         solve_block) — never legitimate"
                    );
                    continue;
                }
                let mut resolved = ResolvedMixedPath::default();
                Self::resolve_path(&core, &path.pools, &mut resolved);
                self.path_resolved.insert(path_id, resolved);
            }
        }

        // Remove old results for affected paths (they'll be re-solved below).
        // A deferred path's result is dropped too: it is excluded from this
        // live solve (its pool is stale, so its prior result is stale as well).
        for &path_id in &affected_path_ids {
            self.results.remove(&path_id);
        }

        // Solve only the non-deferred affected set.
        let solve_path_ids: HashSet<u64> = affected_path_ids
            .iter()
            .filter(|p| !deferred_paths.contains(p))
            .copied()
            .collect();

        // Solve affected paths and insert new results.
        //
        // ADR-005 slice 15b-1: rayon `par_iter` parallelizes the solve across
        // the affected-path set. `Self::solve_path` is a free-standing dispatch
        // (no `&self` read); each work item takes the `path_id` + a CLONED
        // `ResolvedMixedPath` (the `Clone` derive is cheap; the V3-V4 path
        // math reads only immutable statics), then writes — under the parallel
        // closure — into the engine-level result-set via a `Mutex`-free
        // pattern: collect `(path_id, SolvePathResult)` pairs into a Vec, then
        // merge sequentially into `self.results`. The parallel workers touch
        // NO engine state and NO core.lock — engine-then-core lock ordering is
        // preserved unchanged (rayon's internal thread pool never re-enters the
        // engine `Mutex`). For tiny batches the par_iter dispatch overhead is
        // bounded by rayon's lazy split (see `par_iter` docs); the sequential
        // cost dominates below the rayon internal cutoff.
        //
        // Pre-collect the work items (path_id + resolved-snapshot). The clone
        // drops the immutable borrow on `self.path_resolved` that would block
        // parallel dispatch.
        let to_solve: Vec<(u64, ResolvedMixedPath)> = solve_path_ids
            .iter()
            .filter_map(|&pid| {
                let resolved = self.path_resolved.get(&pid)?;
                if !resolved.valid {
                    return None;
                }
                // A path whose `max_update_block` is AHEAD of the drain
                // `block_number` is LIVE head state (the pools advanced by
                // backfill), not poison — it is correctly re-anchored at
                // `solve_block` above (B2); skipping it would DROP a capturable
                // opportunity. The genuinely-future case (`update_block >
                // solve_block`) is already rejected by the U6RNHH T1 belt-and-
                // suspenders guard in the gate loop above, which removes the
                // path from `solve_path_ids` entirely.
                Some((pid, resolved.clone()))
            })
            .collect();

        // Filter out empty/profitless results in the same pass that produces
        // them — the contract is identical to the prior serial loop.
        let solved: Vec<(u64, SolvePathResult)> = to_solve
            .par_iter()
            .filter_map(|(pid, resolved)| {
                ::degenbot_solvers::mixed::solve_path(resolved).map(|r| (*pid, r))
            })
            // Log solver pool state for every solved path (including
            // unprofitable) — diagnostic cross-referencing against sim
            // captured swaps.
            .inspect(|(pid, r)| {
                if !r.solver_pool_states.is_empty() {
                    tracing::info!(
                        "[solver-st] path_id={pid} hops=[{}]",
                        r.solver_pool_states.join(";")
                    );
                }
            })
            .filter(|(_, r)| !r.optimal_input.is_zero() && !r.profit.is_zero())
            .collect();

        // Sequential merge — no lock acquisition; workers above owned their
        // clones.
        for (pid, solve_result) in solved {
            self.results.insert(pid, solve_result);
        }

        self.results_block = solve_block;
        // Note: no compute_diff_and_send here — the pump controls when
        // batches are dispatched (debounce timer or block boundary).
    }

    /// Solve all registered paths using `solve_path`.
    ///
    /// `solve_all` is not currently used live — the pump calls
    /// `solve_all_paths` which calls this only at cold start; subsequent
    /// re-solves go through `rebuild_and_solve_affected`.
    ///
    /// ADR-005 slice 15b-1: the solve loop runs under rayon `par_iter` over
    /// the registered `path_resolved` map. `Self::solve_path` is receiver-free
    /// (slice 15b-1: pure dispatch to the freestanding math helpers), so the
    /// parallel closure borrows only the `path_resolved` entry — no `&self`
    /// mutation under the workers; they collect pairs that the outer loop
    /// inserts into the fresh result map sequentially. The engine-then-core
    /// lock ordering is unchanged: this method is `&self` (no core.lock taken
    /// here; the caller already resolved the paths under `core.read()` at the
    /// `solve_all_paths` entry).
    #[must_use]
    pub fn solve_all(&self) -> HashMap<u64, SolvePathResult> {
        tracing::info!(
            "[solver-dbg] solve_all called, resolved_paths={}",
            self.path_resolved.len()
        );
        self.path_resolved
            .par_iter()
            .filter_map(|(&path_id, resolved)| {
                if !resolved.valid {
                    return None;
                }
                ::degenbot_solvers::mixed::solve_path(resolved)
                    // Log solver pool state for diagnostic cross-referencing
                    // (path_id -> pool state at solve time).
                    .inspect(|r| {
                        if !r.solver_pool_states.is_empty() {
                            tracing::info!(
                                "[solver-st] path_id={path_id} hops=[{}]",
                                r.solver_pool_states.join(";")
                            );
                        }
                    })
                    .filter(|r| !r.optimal_input.is_zero() && !r.profit.is_zero())
                    .map(|r| (path_id, r))
            })
            .collect()
    }

    ///
    /// `core` is the locked [`BotState`] snapshot to read V2 state from
    /// (ADR-003). V3/V4 hops still read the per-family block engines; their
    /// state migrates into `core` in Slices 2/3.
    #[allow(clippy::too_many_lines)]
    pub fn resolve_path(
        core: &crate::bot_core::BotState,
        pool_refs: &[MixedPoolRef],
        resolved: &mut ResolvedMixedPath,
    ) {
        resolved.hops.clear();
        resolved.valid = false;
        resolved.state_nonces.clear();

        if pool_refs.len() < 2 {
            return;
        }

        resolved.hops.reserve(pool_refs.len());
        resolved.state_nonces.reserve(pool_refs.len());

        for pool_ref in pool_refs {
            // Capture the max price-clock `update_block` across all hops.
            resolved.max_update_block = resolved
                .max_update_block
                .max(core.pool_update_block(pool_ref.pool_key));
            match pool_ref.hop_type {
                HopType::V2 => {
                    // Read V2 state from BotState and build the orientation-specific
                    // `IntHopState` at resolve time from `zero_for_one` (ADR-003
                    // "Swap Orientation": single PoolEntry per address, orientation
                    // derived at solve — the engine never mutates this state).
                    let Some(state) = core.get_v2_pool_state(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };
                    let Some(identity) = core.get_v2_identity(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };
                    let (reserve_in, reserve_out, gamma_numer, fee_denom) = if pool_ref.zero_for_one
                    {
                        (
                            state.reserve0.to::<U256>(),
                            state.reserve1.to::<U256>(),
                            identity.fee_token0.0,
                            identity.fee_token0.1,
                        )
                    } else {
                        (
                            state.reserve1.to::<U256>(),
                            state.reserve0.to::<U256>(),
                            identity.fee_token1.0,
                            identity.fee_token1.1,
                        )
                    };
                    let hop_state = degenbot_v2_math::IntHopState::new(
                        reserve_in,
                        reserve_out,
                        gamma_numer,
                        fee_denom,
                    );
                    resolved.hops.push(ResolvedHop::V2 { state: hop_state });
                    resolved.state_nonces.push(state.state_nonce);
                }
                HopType::V3 => {
                    // Look up V3 pool state (now owned by BotState — ADR-003) and
                    // build the integer tick-range sequence used by the CL solver.
                    let Some(pool_state) = core.get_v3_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };
                    let Some(identity) = core.get_v3_identity(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };
                    let Some(int_seq) = pool_state.build_int_v3_sequence(
                        identity.tick_spacing,
                        identity.fee,
                        pool_ref.zero_for_one,
                        // T47PPB: 24 = the active-set walk feed depth. The
                        // enumeration-era value was 10 (tuple cap); the walk
                        // has no tuple cap, so depth is bounded by data
                        // availability (the range cache stores 24).
                        24,
                    ) else {
                        return; // No integer sequence → invalid
                    };

                    resolved.hops.push(ResolvedHop::V3 { int_seq });
                    resolved.state_nonces.push(pool_state.state_nonce);
                }
                HopType::V4 => {
                    // V4 pools use identical CL math as V3 (BotState-owned, ADR-003).
                    let Some(pool_state) = core.get_v4_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };
                    let Some(identity) = core.get_v4_identity(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };
                    let Some(int_seq) = pool_state.build_int_v4_sequence(
                        identity.pool_key.tick_spacing,
                        identity.pool_key.fee,
                        pool_ref.zero_for_one,
                        // T47PPB: 24 = the active-set walk feed depth (twin of
                        // the V3 site above).
                        24,
                    ) else {
                        return; // No integer sequence → invalid
                    };

                    // AV42C7-debug: dump V4 solver intermediates for the
                    // closed-form vs on-chain divergence hunt. Conservative
                    // default ON (`crate::bot_core::bot_env_flag_default_on`);
                    // set `DEGENBOT_DEBUG_V4_SOLVE=0` to disable. grep the log
                    // for the failing pool_id (from the [sim-fixture] dump) to
                    // localize the over-prediction to drain/coverage/range.
                    if crate::bot_core::bot_env_flag_default_on("DEGENBOT_DEBUG_V4_SOLVE") {
                        let pid_hex = alloy::hex::encode(identity.pool_id);
                        let drain: i128 = if pool_ref.zero_for_one {
                            pool_state
                                .tick_data
                                .get(&pool_state.tick)
                                .map_or(0, |info| {
                                    let bytes = info.liquidity_net.to_be_bytes::<32>();
                                    let low: [u8; 16] =
                                        bytes[16..32].try_into().unwrap_or([0u8; 16]);
                                    i128::from_be_bytes(low)
                                })
                        } else {
                            0
                        };
                        tracing::info!(
                            pool_manager = ?identity.pool_manager,
                            pool_id = %pid_hex,
                            zero_for_one = %pool_ref.zero_for_one,
                            tick = pool_state.tick,
                            liquidity = pool_state.liquidity,
                            sqrt_price_x96 = %pool_state.sqrt_price_x96,
                            protocol_fee = pool_state.protocol_fee,
                            coverage = ?pool_state.coverage,
                            n_ranges = int_seq.ranges.len(),
                            drain = %drain,
                            "[debug-v4-solve] pool details"
                        );
                    }

                    resolved.hops.push(ResolvedHop::V4 { int_seq });
                    resolved.state_nonces.push(pool_state.state_nonce);
                }
                // Solidly-stable (Aerodrome stable / Camelot stable_swap) resolve. Reads
                // reserves + identity off the per-family `PoolEntry` arm, then
                // fetches token decimals via the token registry (never stored
                // on the identity — ADR-003 single source of truth).
                HopType::SolidlyStable => {
                    if let Some(id) = core.get_aerodrome_identity(pool_ref.pool_key) {
                        let Some(state) = core.get_aerodrome_pool(pool_ref.pool_key) else {
                            return; // Missing pool → invalid
                        };
                        let (decimals_0, decimals_1) =
                            match (core.token_entry(&id.token0), core.token_entry(&id.token1)) {
                                (Some(t0), Some(t1)) => (
                                    U256::from(10u64).pow(U256::from(t0.decimals)),
                                    U256::from(10u64).pow(U256::from(t1.decimals)),
                                ),
                                _ => return, // Missing token entry → invalid
                            };
                        // Aerodrome fee is stored as the fee fraction directly
                        // (cf. Camelot below).
                        resolved.hops.push(ResolvedHop::SolidlyStable {
                            state: SolidlyHopState {
                                reserves_0: state.reserve0.to::<U256>(),
                                reserves_1: state.reserve1.to::<U256>(),
                                decimals_0,
                                decimals_1,
                                token_in: u8::from(!pool_ref.zero_for_one),
                                fee_numer: U256::from(id.fee.0),
                                fee_denom: U256::from(id.fee.1),
                                stable: id.stable,
                                variant: id.variant,
                            },
                        });
                        resolved.state_nonces.push(state.state_nonce);
                    } else if let Some(id) = core.get_v2_identity(pool_ref.pool_key) {
                        // Camelot stable_swap path (V2PoolIdentity with
                        // `stable_swap=true`).
                        let Some(state) = core.get_v2_pool_state(pool_ref.pool_key) else {
                            return; // Missing pool → invalid
                        };
                        let (decimals_0, decimals_1) =
                            match (core.token_entry(&id.token0), core.token_entry(&id.token1)) {
                                (Some(t0), Some(t1)) => (
                                    U256::from(10u64).pow(U256::from(t0.decimals)),
                                    U256::from(10u64).pow(U256::from(t1.decimals)),
                                ),
                                _ => return, // Missing token entry → invalid
                            };
                        // Camelot stores the per-direction RETAINED fraction
                        // `(gamma_numer, fee_denom)`; the solidly math takes the
                        // FEE fraction, so invert: `fee_numer = denom - gamma`,
                        // `fee_denom = denom`. Selected by `zero_for_one`
                        // (token0 in → fee_token0; token1 in → fee_token1).
                        let (gamma, denom) = if pool_ref.zero_for_one {
                            id.fee_token0
                        } else {
                            id.fee_token1
                        };
                        resolved.hops.push(ResolvedHop::SolidlyStable {
                            state: SolidlyHopState {
                                reserves_0: state.reserve0.to::<U256>(),
                                reserves_1: state.reserve1.to::<U256>(),
                                decimals_0,
                                decimals_1,
                                token_in: u8::from(!pool_ref.zero_for_one),
                                fee_numer: U256::from(denom.saturating_sub(gamma)),
                                fee_denom: U256::from(denom),
                                stable: id.stable_swap,
                                variant: id.variant,
                            },
                        });
                        resolved.state_nonces.push(state.state_nonce);
                    } else {
                        return; // Not an Aerodrome/Camelot pool → invalid
                    }
                }
                HopType::BalancerWeighted => {
                    let Some(id) = core.get_balancer_weighted_identity(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };
                    let Some(state) = core.get_balancer_weighted_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };
                    // N-token pool: zero_for_one selects token[0]→token[1]
                    // (i=0, j=1) or token[1]→token[0] (i=1, j=0). The engine
                    // only handles the pairwise (0/1) case; N>2 pair selection
                    // is a Python-side concern (BalancerPairView) that fixes
                    // the pair before registration.
                    if id.n_tokens() < 2 {
                        return; // Can't form a pairwise hop
                    }
                    // Upscale balances to 18-decimal fixed-point (Balancer
                    // convention: the math leaf operates at ONE = 1e18 scale).
                    // scaling_factors[i] = 10^(18 - token_decimals_i).
                    let (balance_in, balance_out, weight_in, weight_out, sf_in, sf_out) =
                        if pool_ref.zero_for_one {
                            (
                                state.balances[0].saturating_mul(id.scaling_factors[0]),
                                state.balances[1].saturating_mul(id.scaling_factors[1]),
                                id.weights[0],
                                id.weights[1],
                                id.scaling_factors[0],
                                id.scaling_factors[1],
                            )
                        } else {
                            (
                                state.balances[1].saturating_mul(id.scaling_factors[1]),
                                state.balances[0].saturating_mul(id.scaling_factors[0]),
                                id.weights[1],
                                id.weights[0],
                                id.scaling_factors[1],
                                id.scaling_factors[0],
                            )
                        };
                    let Some(pow_version) =
                        degenbot_balancer_math::PowVersion::from_u8(id.pow_version)
                    else {
                        return; // Unknown pow_version → invalid
                    };
                    resolved.hops.push(ResolvedHop::BalancerWeighted {
                        state: BalancerWeightedHopState {
                            balance_in,
                            balance_out,
                            weight_in,
                            weight_out,
                            swap_fee: U256::from(id.swap_fee),
                            pow_version,
                            scaling_factor_in: sf_in,
                            scaling_factor_out: sf_out,
                        },
                    });
                    resolved.state_nonces.push(state.state_nonce);
                }
                HopType::BalancerStable => {
                    let Some(id) = core.get_balancer_stable_identity(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };
                    let Some(state) = core.get_balancer_stable_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };
                    if id.n_tokens() < 2 {
                        return; // Can't form a pairwise hop
                    }
                    let (raw_idx_in, raw_idx_out) = if pool_ref.zero_for_one {
                        (0, 1)
                    } else {
                        (1, 0)
                    };
                    let skip_bpt = |idx: usize| -> usize {
                        match id.bpt_idx {
                            Some(bpt) if idx >= bpt => idx - 1,
                            _ => idx,
                        }
                    };
                    let token_index_in = skip_bpt(raw_idx_in);
                    let token_index_out = skip_bpt(raw_idx_out);
                    let upscaled_balances: Vec<U256> = {
                        let mut ub = Vec::with_capacity(id.n_tokens());
                        for (i, &bal) in state.balances.iter().enumerate() {
                            if id.bpt_idx.is_some_and(|bpt| bpt == i) {
                                continue;
                            }
                            ub.push(bal.saturating_mul(id.scaling_factors[i]));
                        }
                        ub
                    };
                    if token_index_in >= upscaled_balances.len()
                        || token_index_out >= upscaled_balances.len()
                    {
                        return;
                    }
                    let amp_u256 = U256::from(id.amp);
                    let invariant = if id.invariant_version == 1 {
                        degenbot_balancer_math::stable_math::calculate_invariant(
                            amp_u256,
                            &upscaled_balances,
                        )
                    } else {
                        degenbot_balancer_math::stable_math::calculate_invariant_deployed(
                            amp_u256,
                            &upscaled_balances,
                            true,
                        )
                    };
                    let Ok(invariant) = invariant else {
                        return;
                    };
                    resolved.hops.push(ResolvedHop::BalancerStable {
                        state: BalancerStableHopState {
                            amp: amp_u256,
                            balances: upscaled_balances,
                            token_index_in,
                            token_index_out,
                            invariant,
                            swap_fee: U256::from(id.swap_fee),
                            scaling_factor_in: id.scaling_factors[raw_idx_in],
                            scaling_factor_out: id.scaling_factors[raw_idx_out],
                        },
                    });
                    resolved.state_nonces.push(state.state_nonce);
                }
                HopType::CurveStableswap => {
                    let Some(id) = core.get_curve_identity(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };
                    let Some(state) = core.get_curve_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };
                    if id.tokens.len() < 2 {
                        return; // Can't form a pairwise hop
                    }
                    let (raw_idx_in, raw_idx_out) = if pool_ref.zero_for_one {
                        (0, 1)
                    } else {
                        (1, 0)
                    };
                    // Curve constants
                    let precision = U256::from(10u64).pow(U256::from(18u64));
                    let fee_denom = U256::from(10u64).pow(U256::from(10u64));
                    let a_precision = U256::from(100u64);
                    let amp = U256::from(id.a_coefficient).saturating_mul(a_precision);
                    let n_coins = U256::from(id.tokens.len() as u64);
                    // Build rate-adjusted XP: xp[i] = balances[i] * rate_multipliers[i] / PRECISION
                    let xp: Vec<U256> = state
                        .balances
                        .iter()
                        .zip(id.rate_multipliers.iter())
                        .map(|(b, rm)| b.saturating_mul(*rm) / precision)
                        .collect();
                    if raw_idx_in >= xp.len() || raw_idx_out >= xp.len() {
                        return;
                    }
                    let Some(y_variant) =
                        degenbot_curve_math::stableswap::YVariant::try_from_u8(id.y_variant)
                    else {
                        return;
                    };
                    let Some(d_variant) =
                        degenbot_curve_math::stableswap::DVariant::try_from_u8(id.d_variant)
                    else {
                        return;
                    };
                    resolved.hops.push(ResolvedHop::CurveStableswap {
                        state: CurveStableswapHopState {
                            amp,
                            a_precision,
                            xp,
                            token_index_in: raw_idx_in,
                            token_index_out: raw_idx_out,
                            n_coins,
                            fee: U256::from(id.fee),
                            fee_denom,
                            precision,
                            rate_multiplier_in: id.rate_multipliers[raw_idx_in],
                            rate_multiplier_out: id.rate_multipliers[raw_idx_out],
                            y_variant,
                            d_variant,
                        },
                    });
                    resolved.state_nonces.push(state.state_nonce);
                }
            }
        }

        resolved.valid = true;
    }
}

impl Default for ArbitrageEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod staleness_gate_tests {
    use super::hop_is_future;

    #[test]
    fn ahead_is_future_never_legitimate() {
        // Any magnitude ahead is future (U6RNHH T1 / TVJF6K T2).
        assert!(hop_is_future(101, 100));
        assert!(hop_is_future(25_677_789, 25_677_777));
        // Equal to the solve block is a mid-block capture, NOT future.
        assert!(!hop_is_future(100, 100));
        // Behind (normal latency) is NOT future.
        assert!(!hop_is_future(99, 100));
        assert!(!hop_is_future(0, 100));
    }
}

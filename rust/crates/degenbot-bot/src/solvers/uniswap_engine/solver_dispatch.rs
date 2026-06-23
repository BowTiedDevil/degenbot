//! Path resolution, solver dispatch, and rebuild logic.

use alloy::primitives::U256;
use rayon::prelude::*;

use super::{
    BlockMetadata, HashMap, HashSet, HopType, MixedPoolRef, ResolvedHop, ResolvedMixedPath,
    SolvePathResult, UniswapEngine, INT128_MAX,
};

impl UniswapEngine {
    /// Re-resolve and re-solve only paths that contain updated pools.
    ///
    /// Uses the `pool_to_paths` reverse index to identify `affected_path_ids`,
    /// then re-resolves and re-solves only those. Unaffected paths carry
    /// their previous results forward.
    pub fn rebuild_and_solve_affected(
        &mut self,
        v2_affected: &HashSet<u64>,
        v3_affected: &HashSet<u64>,
        v4_affected: &HashSet<u64>,
        block_number: u64,
        _metadata: &BlockMetadata,
    ) {
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

        // If no paths are affected, just update the block number
        if affected_path_ids.is_empty() {
            self.results_block = block_number;
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
        {
            let core = self.core.read();
            for &path_id in &affected_path_ids {
                let Some(path) = self.path_pools.get(&path_id) else {
                    continue;
                };
                let mut resolved = ResolvedMixedPath::default();
                Self::resolve_path(&core, &path.pools, &mut resolved);
                self.path_resolved.insert(path_id, resolved);
            }
        }

        // Remove old results for affected paths (they'll be re-solved below)
        for &path_id in &affected_path_ids {
            self.results.remove(&path_id);
        }

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
        let to_solve: Vec<(u64, ResolvedMixedPath)> = affected_path_ids
            .iter()
            .filter_map(|&pid| {
                let resolved = self.path_resolved.get(&pid)?;
                if !resolved.valid {
                    return None;
                }
                Some((pid, resolved.clone()))
            })
            .collect();

        // Filter out empty/profitless results in the same pass that produces
        // them — the contract is identical to the prior serial loop.
        let solved: Vec<(u64, SolvePathResult)> = to_solve
            .par_iter()
            .filter_map(|(pid, resolved)| Self::solve_path(resolved).map(|r| (*pid, r)))
            .filter(|(_, r)| !r.optimal_input.is_zero() && !r.profit.is_zero())
            .collect();

        // Sequential merge — no lock acquisition; workers above owned their
        // clones.
        for (pid, solve_result) in solved {
            self.results.insert(pid, solve_result);
        }

        self.results_block = block_number;
        // Note: no compute_diff_and_send here — the pump controls when
        // batches are dispatched (debounce timer or block boundary).
    }

    /// Dispatches based on path composition:
    /// - V2-V2: integer-exact Möbius solver (closed-form U512 isqrt)
    /// - V3-V3 / V4-V4 / V3-V4 / V4-V3: integer piecewise-Möbius (CL × CL)
    /// - V2-V3 / V3-V2 / V2-V4 / V4-V2: mixed integer-exact solver
    ///
    /// ADR-005 slice 15b-1: signature dropped the `&self` receiver (it was
    /// `#[allow(clippy::unused_self)]` — body is pure dispatch to freestanding
    /// math helpers). The static form lets `rebuild_and_solve_affected` and
    /// `solve_all` invoke `solve_path` from a rayon `par_iter` closure without
    /// borrowing `self` (which would conflict with the `&mut self` write to
    /// `self.results` that follows the solve).
    #[allow(clippy::unused_self)]
    pub fn solve_path(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
        let all_v2 = resolved
            .hops
            .iter()
            .all(|h| matches!(h, ResolvedHop::V2 { .. }));
        let all_cl = resolved.hops.iter().all(|h| h.as_int_sequence().is_some());

        let result = if all_v2 {
            let int_hops: Vec<_> = resolved
                .hops
                .iter()
                .filter_map(ResolvedHop::as_v2_state)
                .cloned()
                .collect();
            if int_hops.len() == resolved.hops.len() {
                crate::solvers::mobius_int_exact::exact_mobius_solve(&int_hops)
                    .ok()
                    .and_then(|r| {
                        if r.is_profitable && !r.optimal_input.is_zero() && !r.profit.is_zero() {
                            // V2 constant-product pools: each hop's consumed input
                            // is the previous hop's output (hop_outputs[i-1]),
                            // with hop 0 consuming optimal_input.
                            let mut consumed_inputs = Vec::with_capacity(r.hop_outputs.len());
                            consumed_inputs.push(r.optimal_input);
                            for i in 1..r.hop_outputs.len() {
                                consumed_inputs.push(r.hop_outputs[i - 1]);
                            }
                            Some(SolvePathResult {
                                optimal_input: r.optimal_input,
                                profit: r.profit,
                                hop_outputs: r.hop_outputs,
                                consumed_inputs,
                            })
                        } else {
                            None
                        }
                    })
            } else {
                None
            }
        } else if all_cl {
            // V3-V3, V4-V4, V3-V4, V4-V3, V3-V3-V3, etc: all concentrated-liquidity
            let int_sequences: Vec<_> = resolved
                .hops
                .iter()
                .filter_map(ResolvedHop::as_int_sequence)
                .collect();
            if int_sequences.len() >= 2 {
                crate::solvers::mobius_v3_int::int_solve_cl_path(&int_sequences).map(
                    |(optimal_input, _profit, hop_outputs)| {
                        // consumed_inputs[0] = optimal_input (first hop always consumes
                        // its full input for single-range paths; no partial fill).
                        // consumed_inputs[i>0] = hop_outputs[i-1] (the previous hop's
                        // output becomes this hop's input — matching the pipeline:
                        // V3 output flows into V4 as amountSpecified).
                        let mut consumed_inputs = Vec::with_capacity(hop_outputs.len());
                        consumed_inputs.push(optimal_input);
                        for i in 1..hop_outputs.len() {
                            consumed_inputs.push(hop_outputs[i - 1]);
                        }
                        let profit = hop_outputs
                            .last()
                            .copied()
                            .unwrap_or(U256::ZERO)
                            .saturating_sub(consumed_inputs[0]);
                        SolvePathResult {
                            optimal_input,
                            profit,
                            hop_outputs,
                            consumed_inputs,
                        }
                    },
                )
            } else {
                None
            }
        } else {
            // Mixed V2 + CL (V3 or V4)
            Self::solve_mixed_path_int(resolved)
        };

        // V4 int128 guard: reject paths where any V4 hop's consumed input or
        // output exceeds int128_max. V4's toBalanceDelta() calls toInt128() on
        // swap amounts — if either doesn't fit, V4 reverts with SafeCastOverflow.
        if let Some(ref r) = result {
            for (i, hop) in resolved.hops.iter().enumerate() {
                if matches!(hop, ResolvedHop::V4 { .. }) {
                    let consumed = r.consumed_inputs.get(i).copied().unwrap_or(U256::ZERO);
                    let output = r.hop_outputs.get(i).copied().unwrap_or(U256::ZERO);
                    if consumed > INT128_MAX || output > INT128_MAX {
                        return None;
                    }
                }
            }
        }

        result
    }

    /// Solve all registered paths using `solve_path`.
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
        self.path_resolved
            .par_iter()
            .filter_map(|(&path_id, resolved)| {
                if !resolved.valid {
                    return None;
                }
                Self::solve_path(resolved)
                    .filter(|r| !r.optimal_input.is_zero() && !r.profit.is_zero())
                    .map(|r| (path_id, r))
            })
            .collect()
    }

    /// Solve a mixed V2 + CL (V3 or V4) path using integer-exact Möbius solver.
    ///
    /// Uses the pre-built `IntV3TickRangeSequence` from `resolve_path`,
    /// which was constructed directly from U256 values (no f64 conversion).
    /// V3 and V4 hops produce the same type — `IntV3TickRangeSequence`.
    ///
    /// The sequence-based solver enumerates CL ending ranges and computes
    /// the optimal input for each piece, validating with crossing-aware
    /// simulation. This eliminates false positives from single-range
    /// approximation when swaps exceed the current tick range capacity.
    fn solve_mixed_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
        if resolved.hops.len() < 2 {
            return None;
        }

        // Check that this is actually a mixed path (both V2 and CL hops)
        let has_v2 = resolved
            .hops
            .iter()
            .any(|h| matches!(h, ResolvedHop::V2 { .. }));
        let has_cl = resolved.hops.iter().any(|h| h.as_int_sequence().is_some());
        if !has_v2 || !has_cl {
            return None; // not a mixed path — should be handled by other dispatches
        }

        // Build hop_order and adapter arrays from the enum
        let hop_order: Vec<bool> = resolved
            .hops
            .iter()
            .map(|h| matches!(h, ResolvedHop::V2 { .. }))
            .collect();
        let v2_hops: Vec<Option<crate::solvers::mobius_int::IntHopState>> = resolved
            .hops
            .iter()
            .map(|h| h.as_v2_state().cloned())
            .collect();
        let int_v3_sequences: Vec<Option<crate::solvers::mobius_v3_int::IntV3TickRangeSequence>> =
            resolved
                .hops
                .iter()
                .map(|h| h.as_int_sequence().cloned())
                .collect();

        crate::solvers::mobius_v3_int::exact_solve_mixed_path_n(
            &v2_hops,
            &int_v3_sequences,
            &hop_order,
        )
        .map(|(optimal_input, profit, hop_outputs)| {
            // consumed_inputs[0] = optimal_input (first hop consumes full input).
            // consumed_inputs[i>0] = hop_outputs[i-1] (previous hop's output
            // becomes this hop's input).
            let mut consumed_inputs = Vec::with_capacity(hop_outputs.len());
            consumed_inputs.push(optimal_input);
            for i in 1..hop_outputs.len() {
                consumed_inputs.push(hop_outputs[i - 1]);
            }
            SolvePathResult {
                optimal_input,
                profit,
                hop_outputs,
                consumed_inputs,
            }
        })
    }

    /// Resolve a path's pool refs into hop states and tick-range sequences.
    ///
    /// `core` is the locked [`BotState`] snapshot to read V2 state from
    /// (ADR-003). V3/V4 hops still read the per-family block engines; their
    /// state migrates into `core` in Slices 2/3.
    pub fn resolve_path(
        core: &crate::bot_core::BotState,
        pool_refs: &[MixedPoolRef],
        resolved: &mut ResolvedMixedPath,
    ) {
        resolved.hops.clear();
        resolved.valid = false;

        if pool_refs.len() < 2 {
            return;
        }

        resolved.hops.reserve(pool_refs.len());

        for pool_ref in pool_refs {
            match pool_ref.hop_type {
                HopType::V2 => {
                    // Read V2 state from BotState and build the orientation-specific
                    // `IntHopState` at resolve time from `zero_for_one` (ADR-003
                    // "Swap Orientation": single PoolEntry per address, orientation
                    // derived at solve — the engine never mutates this state).
                    let Some(state) = core.get_v2_pool_state(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };
                    let (reserve_in, reserve_out, gamma_numer, fee_denom) = if pool_ref.zero_for_one
                    {
                        (
                            state.reserve0,
                            state.reserve1,
                            state.fee_token0.0,
                            state.fee_token0.1,
                        )
                    } else {
                        (
                            state.reserve1,
                            state.reserve0,
                            state.fee_token1.0,
                            state.fee_token1.1,
                        )
                    };
                    let hop_state = crate::solvers::mobius_int::IntHopState::new(
                        reserve_in,
                        reserve_out,
                        gamma_numer,
                        fee_denom,
                    );
                    resolved.hops.push(ResolvedHop::V2 { state: hop_state });
                }
                HopType::V3 => {
                    // Look up V3 pool state (now owned by BotState — ADR-003) and
                    // build the integer tick-range sequence used by the CL solver.
                    let Some(pool_state) = core.get_v3_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };

                    let Some(int_seq) = pool_state.build_int_v3_sequence(pool_ref.zero_for_one, 10)
                    else {
                        return; // No integer sequence → invalid
                    };

                    resolved.hops.push(ResolvedHop::V3 { int_seq });
                }
                HopType::V4 => {
                    // V4 pools use identical CL math as V3 (BotState-owned, ADR-003).
                    let Some(pool_state) = core.get_v4_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };

                    let Some(int_seq) = pool_state.build_int_v4_sequence(pool_ref.zero_for_one, 10)
                    else {
                        return; // No integer sequence → invalid
                    };

                    resolved.hops.push(ResolvedHop::V4 { int_seq });
                }
            }
        }

        resolved.valid = true;
    }
}

impl Default for UniswapEngine {
    fn default() -> Self {
        Self::new()
    }
}

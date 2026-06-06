//! Path resolution, solver dispatch, and rebuild logic.

use alloy::primitives::U256;

use crate::optimizers::mobius_int::u256_to_f64;

use super::{UniswapEngine, HashSet, BlockMetadata, HopType, ResolvedHop, ResolvedMixedPath, SolvePathResult, INT128_MAX, HashMap, MixedPoolRef};

impl UniswapEngine {
    /// Re-resolve and re-solve only paths that contain updated pools.
    ///
    /// Uses the `pool_to_paths` reverse index to identify `affected_path_ids`,
    /// then re-resolves and re-solves only those. Unaffected paths carry
    /// their previous results forward.
    pub(super) fn rebuild_and_solve_affected(
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

        // Re-resolve affected paths directly (no clone needed — path_pools is
        // immutable, path_resolved is mutable, no borrow conflict)
        for &path_id in &affected_path_ids {
            let Some(path) = self.path_pools.get(&path_id) else {
                continue;
            };
            let mut resolved = ResolvedMixedPath::default();
            self.resolve_path(&path.pools, &mut resolved);
            self.path_resolved.insert(path_id, resolved);
        }

        // Remove old results for affected paths (they'll be re-solved below)
        for &path_id in &affected_path_ids {
            self.results.remove(&path_id);
        }

        // Solve affected paths and insert new results
        for &path_id in &affected_path_ids {
            let Some(resolved) = self.path_resolved.get(&path_id) else {
                continue;
            };
            if !resolved.valid {
                continue;
            }

            if let Some(solve_result) = self.solve_path(resolved) {
                if !solve_result.optimal_input.is_zero() && !solve_result.profit.is_zero() {
                    self.results.insert(path_id, solve_result);
                }
            }
        }

        self.results_block = block_number;
        // Note: no compute_diff_and_send here — the pump controls when
        // batches are dispatched (debounce timer or block boundary).
    }

    /// Dispatches based on path composition:
    /// - V2-V2: integer-exact Möbius solver (closed-form U512 isqrt)
    /// - V3-V3 / V4-V4 / V3-V4 / V4-V3: integer piecewise-Möbius (CL × CL)
    /// - V2-V3 / V3-V2 / V2-V4 / V4-V2: mixed integer-exact solver
    #[allow(clippy::unused_self)]
    pub(super) fn solve_path(&self, resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
        let all_v2 = resolved.hops.iter().all(|h| matches!(h, ResolvedHop::V2 { .. }));
        let all_cl = resolved.hops.iter().all(|h| h.as_int_sequence().is_some());

        let result = if all_v2 {
            let int_hops: Vec<_> = resolved.hops.iter()
                .filter_map(ResolvedHop::as_v2_state)
                .cloned()
                .collect();
            if int_hops.len() == resolved.hops.len() {
                crate::optimizers::mobius_int_exact::exact_mobius_solve(&int_hops)
                    .ok()
                    .and_then(|r| {
                        if r.is_profitable
                            && !r.optimal_input.is_zero()
                            && !r.profit.is_zero()
                        {
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
            let int_sequences: Vec<_> = resolved.hops.iter()
                .filter_map(ResolvedHop::as_int_sequence)
                .collect();
            if int_sequences.len() >= 2 {
                crate::optimizers::mobius_v3_int::int_solve_cl_path(&int_sequences)
                    .map(|(optimal_input, _profit, hop_outputs)| {
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
                        let profit = hop_outputs.last().copied().unwrap_or(U256::ZERO)
                            .saturating_sub(consumed_inputs[0]);
                        SolvePathResult {
                            optimal_input,
                            profit,
                            hop_outputs,
                            consumed_inputs,
                        }
                    })
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
    #[must_use]
    pub(super) fn solve_all(&self) -> HashMap<u64, SolvePathResult> {
        let mut results = HashMap::with_capacity(self.path_resolved.len());

        for (&path_id, resolved) in &self.path_resolved {
            if !resolved.valid {
                continue;
            }

            if let Some(solve_result) = self.solve_path(resolved) {
                if !solve_result.optimal_input.is_zero() && !solve_result.profit.is_zero() {
                    results.insert(path_id, solve_result);
                }
            }
        }

        results
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
    fn solve_mixed_path_int(
        resolved: &ResolvedMixedPath,
    ) -> Option<SolvePathResult> {
        if resolved.hops.len() < 2 {
            return None;
        }

        // Check that this is actually a mixed path (both V2 and CL hops)
        let has_v2 = resolved.hops.iter().any(|h| matches!(h, ResolvedHop::V2 { .. }));
        let has_cl = resolved.hops.iter().any(|h| h.as_int_sequence().is_some());
        if !has_v2 || !has_cl {
            return None; // not a mixed path — should be handled by other dispatches
        }

        // Build hop_order and adapter arrays from the enum
        let hop_order: Vec<bool> = resolved.hops.iter()
            .map(|h| matches!(h, ResolvedHop::V2 { .. }))
            .collect();
        let v2_hops: Vec<Option<crate::optimizers::mobius_int::IntHopState>> = resolved.hops.iter()
            .map(|h| h.as_v2_state().cloned())
            .collect();
        let int_v3_sequences: Vec<Option<crate::optimizers::mobius_v3_int::IntV3TickRangeSequence>> = resolved.hops.iter()
            .map(|h| h.as_int_sequence().cloned())
            .collect();

        crate::optimizers::mobius_v3_int::exact_solve_mixed_path_n(
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
    pub(super) fn resolve_path(&self, pool_refs: &[MixedPoolRef], resolved: &mut ResolvedMixedPath) {
        resolved.hops.clear();
        resolved.valid = false;

        if pool_refs.len() < 2 {
            return;
        }

        resolved.hops.reserve(pool_refs.len());

        for pool_ref in pool_refs {
            match pool_ref.hop_type {
                HopType::V2 => {
                    // Look up the V2 pool state
                    let Some(hop_state) = self.v2_engine.get_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };

                    let base = hop_state.to_base_hop();
                    resolved.hops.push(ResolvedHop::V2 { state: hop_state.clone(), base });
                }
                HopType::V3 => {
                    // Look up V3 pool state and build tick-range sequence
                    let Some(pool_state) = self.v3_engine.get_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };

                    let Some(sequence) = pool_state.build_sequence(pool_ref.zero_for_one, 3) else {
                        return; // No sequence → invalid
                    };
                    let Some(first_range) = sequence.ranges.first() else {
                        return; // Empty sequence → invalid
                    };
                    let base = first_range.to_hop_state();

                    // Build integer V3 hop from original U256 values (exact, no f64 conversion)
                    let Some(int_hop) = pool_state.build_int_v3_hop(pool_ref.zero_for_one) else {
                        return; // No integer hop → invalid
                    };
                    // Build integer V3 sequence for V3-V3 paths
                    let Some(int_seq) = pool_state.build_int_v3_sequence(pool_ref.zero_for_one, 10) else {
                        return; // No integer sequence → invalid
                    };

                    resolved.hops.push(ResolvedHop::V3 {
                        seq: sequence,
                        int_hop,
                        int_seq,
                        base,
                    });
                }
                HopType::V4 => {
                    // V4 pools use identical concentrated-liquidity math as V3.
                    // They produce the same `IntV3TickRangeSequence` type.
                    let Some(pool_state) = self.v4_engine.get_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };

                    // Build integer V4 sequence (same type as V3)
                    let Some(int_seq) = pool_state.build_int_v4_sequence(pool_ref.zero_for_one, 10) else {
                        return; // No integer sequence → invalid
                    };

                    // V4 doesn't use f64-based tick-range sequences
                    // (the integer solver is sufficient).
                    resolved.hops.push(ResolvedHop::V4 {
                        int_seq,
                        base: crate::optimizers::mobius::HopState::new(0.0, 0.0, 0.0),
                    });
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

// ---------------------------------------------------------------------------
// IntHopState extension for base hop conversion
// ---------------------------------------------------------------------------

/// Extension trait for converting `IntHopState` to base f64 `HopState`.
trait IntHopStateExt {
    /// Convert to a f64 `HopState` for Mobius initial estimates.
    fn to_base_hop(&self) -> crate::optimizers::mobius::HopState;
}

impl IntHopStateExt for crate::optimizers::mobius_int::IntHopState {
    #[allow(clippy::cast_precision_loss)]
    fn to_base_hop(&self) -> crate::optimizers::mobius::HopState {
        let fee = 1.0 - (self.gamma_numer as f64 / self.fee_denom as f64);
        let r_in = u256_to_f64(self.reserve_in);
        let r_out = u256_to_f64(self.reserve_out);
        crate::optimizers::mobius::HopState::new(r_in, r_out, fee)
    }
}

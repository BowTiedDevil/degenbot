//! Path resolution, solver dispatch, and rebuild logic.

use alloy::primitives::{U256, U512};
use rayon::prelude::*;

use super::{
    ArbitrageEngine, BalancerStableHopState, BalancerWeightedHopState, BlockMetadata,
    CurveStableswapHopState, HashMap, HashSet, HopType, MixedPoolRef, ResolvedHop,
    ResolvedMixedPath, SolidlyHopState, SolvePathResult, INT128_MAX,
};

impl ArbitrageEngine {
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
    #[allow(clippy::too_many_lines)]
    pub fn solve_path(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
        // An invalid (partially-resolved) path has hops missing — don't solve.
        if !resolved.valid {
            return None;
        }
        let all_v2 = resolved
            .hops
            .iter()
            .all(|h| matches!(h, ResolvedHop::V2 { .. }));
        let all_cl = resolved.hops.iter().all(|h| h.as_int_sequence().is_some());
        let has_solidly = resolved
            .hops
            .iter()
            .any(|h| matches!(h, ResolvedHop::SolidlyStable { .. }));
        let all_v2_or_solidly = resolved.hops.iter().all(|h| {
            matches!(
                h,
                ResolvedHop::V2 { .. } | ResolvedHop::SolidlyStable { .. }
            )
        });
        let has_balancer_weighted = resolved
            .hops
            .iter()
            .any(|h| matches!(h, ResolvedHop::BalancerWeighted { .. }));
        let all_v2_or_weighted = resolved.hops.iter().all(|h| {
            matches!(
                h,
                ResolvedHop::V2 { .. } | ResolvedHop::BalancerWeighted { .. }
            )
        });
        let has_balancer_stable = resolved
            .hops
            .iter()
            .any(|h| matches!(h, ResolvedHop::BalancerStable { .. }));
        let all_v2_or_stable = resolved.hops.iter().all(|h| {
            matches!(
                h,
                ResolvedHop::V2 { .. } | ResolvedHop::BalancerStable { .. }
            )
        });
        let has_curve = resolved
            .hops
            .iter()
            .any(|h| matches!(h, ResolvedHop::CurveStableswap { .. }));
        let all_v2_or_curve = resolved.hops.iter().all(|h| {
            matches!(
                h,
                ResolvedHop::V2 { .. } | ResolvedHop::CurveStableswap { .. }
            )
        });

        let result = if all_v2 {
            let int_hops: Vec<_> = resolved
                .hops
                .iter()
                .filter_map(ResolvedHop::as_v2_state)
                .cloned()
                .collect();
            if int_hops.len() == resolved.hops.len() {
                ::degenbot_solvers::mobius_int_exact::exact_mobius_solve(&int_hops)
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
                ::degenbot_solvers::mobius_v3_int::int_solve_cl_path(&int_sequences).map(
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
        } else if all_v2_or_solidly && has_solidly {
            // All-V2-or-Solidly with ≥1 Solidly hop — the two-stage Möbius
            // precheck + golden-section solve (task DMPSNG). Scope (p):
            // Solidly mixed with CL is rejected below.
            Self::solve_solidly_path_int(resolved)
        } else if has_solidly {
            // A Solidly hop alongside a CL hop — out of scope (p). The
            // Solidly solve is a per-hop `swap_fn` walk incompatible with
            // CL tick-range enumeration; Python rejects these too.
            None
        } else if all_v2_or_weighted && has_balancer_weighted {
            // All-V2-or-Balancer-weighted with ≥1 weighted hop — the
            // Möbius precheck + golden-section solve over the Balancer
            // weighted math leaf. Scope: weighted mixed with CL is
            // rejected below (same scope rule as Solidly+CL).
            Self::solve_balancer_weighted_path_int(resolved)
        } else if has_balancer_weighted {
            // A weighted hop alongside a CL hop — out of scope (same
            // rationale as Solidly+CL).
            None
        } else if all_v2_or_stable && has_balancer_stable {
            // All-V2-or-Balancer-stable with ≥1 stable hop — the
            // Möbius precheck + golden-section solve over the Balancer
            // stable math leaf. Scope: stable mixed with CL or weighted is
            // rejected below.
            Self::solve_balancer_stable_path_int(resolved)
        } else if has_balancer_stable {
            // A stable hop alongside a CL or weighted hop — out of scope.
            None
        } else if all_v2_or_curve && has_curve {
            // All-V2-or-Curve with ≥1 Curve hop — the Möbius precheck +
            // golden-section solve over the Curve stableswap math leaf.
            Self::solve_curve_path_int(resolved)
        } else if has_curve {
            // A Curve hop alongside a CL or Balancer hop — out of scope.
            None
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
        let v2_hops: Vec<Option<degenbot_v2_math::IntHopState>> = resolved
            .hops
            .iter()
            .map(|h| h.as_v2_state().cloned())
            .collect();
        let int_v3_sequences: Vec<
            Option<::degenbot_solvers::mobius_v3_int::IntV3TickRangeSequence>,
        > = resolved
            .hops
            .iter()
            .map(|h| h.as_int_sequence().cloned())
            .collect();

        ::degenbot_solvers::mobius_v3_int::exact_solve_mixed_path_n(
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

    /// Solve an all-V2-or-Solidly (no CL hops, ≥1 Solidly) path using the
    /// two-stage Solidly solve: a V2-equivalent Möbius precheck narrows the
    /// golden-section bracket around the Möbius optimum (±5x), then 25
    /// golden-section iterations refine the optimum, then integer
    /// verification scans ±3 candidates and picks the max-profit input.
    ///
    /// Faithful port of `arbitrage.solvers.solidly_stable.SolidlyStableSolver`
    /// (the `_solve_golden_section` branch with `swap_fn` set). The Möbius
    /// precheck early-outs unprofitable paths before the expensive search.
    fn solve_solidly_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
        use ::degenbot_solvers::mobius_int::compute_int_mobius_coefficients;
        use ::degenbot_solvers::mobius_int_exact::compute_mobius_model_optimal_input;
        use degenbot_v2_math::IntHopState;

        let hops = &resolved.hops;
        if hops.len() < 2 {
            return None;
        }

        // V2-equivalent IntHopState per hop: Solidly hops orient reserves by
        // `token_in` and convert the fee (SolidlyHopState stores the fee
        // fraction `fee_numer/fee_denom`; IntHopState wants the retained
        // fraction `gamma_numer/fee_denom`, so `gamma_numer = fee_denom -
        // fee_numer`). V2 hops pass through unchanged.
        let v2_equiv: Vec<IntHopState> = hops
            .iter()
            .map(Self::solidly_hop_v2_equiv)
            .collect::<Option<Vec<_>>>()?;

        // --- Profitability precheck (V2-equivalent Möbius) ---
        let coeffs = compute_int_mobius_coefficients(&v2_equiv).ok()?;
        if !coeffs.is_profitable {
            return None;
        }
        let x_mobius = compute_mobius_model_optimal_input(&coeffs);

        // --- Bracket: [1, max_reserve], narrowed around the Möbius optimum ---
        let one = U256::from(1u64);
        let five = U256::from(5u64);
        let max_reserve = v2_equiv
            .iter()
            .map(|h| h.reserve_in)
            .max()
            .unwrap_or(U256::ZERO);
        if max_reserve.is_zero() {
            return None;
        }
        let mut x_low = one;
        let mut x_high = max_reserve;
        if !x_mobius.is_zero() {
            let x_center = x_mobius.min(x_high);
            x_low = x_low.max(x_center / five);
            x_high = x_high.min(x_center.saturating_mul(five));
        }
        if x_high <= x_low {
            // Degenerate bracket — fall back to a single-point verification.
            return Self::solidly_brute_force_best(hops, x_low);
        }

        // --- Golden-section search (25 iterations) ---
        // `phi = (sqrt(5) - 1) / 2` ≈ 0.6180339887498949, approximated as the
        // U256 fraction `phi_num / phi_den` and applied to the bracket span in
        // U512 (the span may approach U256 magnitude; the product does not fit
        // in U256 so the multiply is done in U512 then truncated back).
        let phi_num = U256::from(6_180_339_887_498_949u64); // phi * 1e16
        let phi_den = U256::from(10_000_000_000_000_000u64); // 1e16
        let phi_span = |lo: U256, hi: U256| -> U256 {
            let d = U512::from(hi - lo);
            let scaled = d * U512::from(phi_num) / U512::from(phi_den);
            ::degenbot_solvers::mobius_int_exact::u512_to_u256_internal(scaled)
        };
        let mut x1 = x_high - phi_span(x_low, x_high);
        let mut x2 = x_low + phi_span(x_low, x_high);
        let mut p1 = Self::simulate_solidly_path(x1, hops).saturating_sub(x1);
        let mut p2 = Self::simulate_solidly_path(x2, hops).saturating_sub(x2);

        for _ in 0..Self::SOLIDLY_GOLDEN_SECTION_ITERATIONS {
            if p1 < p2 {
                x_low = x1;
                x1 = x2;
                p1 = p2;
                x2 = x_low + phi_span(x_low, x_high);
                p2 = Self::simulate_solidly_path(x2, hops).saturating_sub(x2);
            } else {
                x_high = x2;
                x2 = x1;
                p2 = p1;
                x1 = x_high - phi_span(x_low, x_high);
                p1 = Self::simulate_solidly_path(x1, hops).saturating_sub(x1);
            }
        }

        let x_opt = (x_low + x_high) / U256::from(2u64);
        Self::solidly_brute_force_best(hops, x_opt)
    }

    /// Number of golden-section refinement iterations (mirrors Python's
    /// `SolidlyStableSolver.GOLDEN_SECTION_ITERATIONS`).
    const SOLIDLY_GOLDEN_SECTION_ITERATIONS: usize = 25;

    /// Integer verification: scan `±SOLIDLY_INTEGER_SEARCH_RADIUS` candidates
    /// around `center` (plus one past the upper edge) and pick the max-profit
    /// input. Mirrors Python's `search_radius = 3` sweep. Returns `None` when
    /// no candidate is profitable.
    fn solidly_brute_force_best(hops: &[ResolvedHop], center: U256) -> Option<SolvePathResult> {
        const SEARCH_RADIUS: u64 = 3;
        let one = U256::from(1u64);
        let start = center.saturating_sub(U256::from(SEARCH_RADIUS)).max(one);
        let end = center + U256::from(SEARCH_RADIUS) + one;

        let mut best_input = U256::ZERO;
        let mut best_profit = U256::ZERO;
        let mut cand = start;
        while cand <= end {
            let output = Self::simulate_solidly_path(cand, hops);
            let profit = output.saturating_sub(cand);
            if profit > best_profit {
                best_profit = profit;
                best_input = cand;
            }
            if cand == U256::MAX {
                break;
            }
            cand += one;
        }

        if best_profit.is_zero() {
            return None;
        }

        // Re-simulate at best_input to record per-hop outputs.
        let hop_outputs = Self::simulate_solidly_path_outputs(best_input, hops);
        let mut consumed_inputs = Vec::with_capacity(hop_outputs.len());
        consumed_inputs.push(best_input);
        for i in 1..hop_outputs.len() {
            consumed_inputs.push(hop_outputs[i - 1]);
        }
        Some(SolvePathResult {
            optimal_input: best_input,
            profit: best_profit,
            hop_outputs,
            consumed_inputs,
        })
    }

    /// Build the V2-equivalent [`IntHopState`] for a hop in a Solidly solve
    /// path. Solidly hops orient reserves by `token_in` and convert the fee
    /// (fee fraction → retained fraction); V2 hops pass through unchanged.
    /// Returns `None` for any non-V2/non-Solidly hop or when the Solidly fee
    /// pair overflows `u64` (the path is then unsolvable here).
    fn solidly_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_v2_math::IntHopState> {
        use degenbot_v2_math::IntHopState;
        match hop {
            ResolvedHop::V2 { state } => Some(state.clone()),
            ResolvedHop::SolidlyStable { state } => {
                let (reserve_in, reserve_out) = if state.token_in == 0 {
                    (state.reserves_0, state.reserves_1)
                } else {
                    (state.reserves_1, state.reserves_0)
                };
                // SolidlyHopState fee = fee fraction (fee_numer/fee_denom).
                // IntHopState fee = retained fraction (gamma_numer/fee_denom).
                let gamma_numer = state.fee_denom.saturating_sub(state.fee_numer);
                let gn: u64 = gamma_numer.try_into().ok()?;
                let fd: u64 = state.fee_denom.try_into().ok()?;
                Some(IntHopState::new(reserve_in, reserve_out, gn, fd))
            }
            _ => None,
        }
    }

    /// Simulate a Solidly-or-V2 path, returning only the final output.
    /// Per-hop dispatch: Solidly hops call the `degenbot-solidly-math` leaf
    /// selected by `(variant, stable)`; V2 hops reuse `IntHopState::swap`.
    #[must_use]
    pub(crate) fn simulate_solidly_path(x: U256, hops: &[ResolvedHop]) -> U256 {
        Self::simulate_solidly_path_outputs(x, hops)
            .last()
            .copied()
            .unwrap_or(U256::ZERO)
    }

    /// Simulate a Solidly-or-V2 path, returning per-hop outputs.
    /// `hop_outputs[i]` = output of hop `i` given `hop_outputs[i-1]` as input
    /// (or `x` for `i==0`). Returns `U256::ZERO` per hop once the chain breaks
    /// at a zero amount (matching the Python `_simulate_mixed_path_int`'s
    /// early-out).
    #[must_use]
    fn simulate_solidly_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
        let mut amount = x;
        let mut outputs = Vec::with_capacity(hops.len());
        for hop in hops {
            if amount.is_zero() {
                outputs.push(U256::ZERO);
                continue;
            }
            let out = match hop {
                ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
                ResolvedHop::SolidlyStable { state } => Self::simulate_solidly_hop(amount, state),
                // Non-V2/Solidly hops can't reach here — the solve dispatch
                // rejects Solidly+CL paths before calling this.
                _ => U256::ZERO,
            };
            outputs.push(out);
            amount = out;
        }
        outputs
    }

    /// Evaluate a single Solidly hop's output via the `degenbot-solidly-math`
    /// leaf, selected by `(variant, stable)`. Returns `U256::ZERO` on
    /// overflow / invalid `token_in` (the leaf's `Err` variants), matching the
    /// Python `_simulate_mixed_path_int`'s defensive fallback.
    fn simulate_solidly_hop(amount_in: U256, hop: &SolidlyHopState) -> U256 {
        use degenbot_solidly_math::{
            calc_exact_in_stable_camelot, calc_exact_in_stable_solidly, calc_exact_in_volatile,
        };
        use degenbot_uniswap::dex_identity::DexVariant;
        if amount_in.is_zero() {
            return U256::ZERO;
        }
        let out = match (hop.variant, hop.stable) {
            (DexVariant::AerodromeV2Stable | DexVariant::AerodromeV2Volatile, true) => {
                calc_exact_in_stable_solidly(
                    amount_in,
                    hop.token_in,
                    hop.reserves_0,
                    hop.reserves_1,
                    hop.decimals_0,
                    hop.decimals_1,
                    hop.fee_numer,
                    hop.fee_denom,
                )
            }
            (DexVariant::CamelotV2Stable | DexVariant::CamelotV2Volatile, true) => {
                calc_exact_in_stable_camelot(
                    amount_in,
                    hop.token_in,
                    hop.reserves_0,
                    hop.reserves_1,
                    hop.decimals_0,
                    hop.decimals_1,
                    hop.fee_numer,
                    hop.fee_denom,
                )
            }
            (_, false) => calc_exact_in_volatile(
                amount_in,
                hop.token_in,
                hop.reserves_0,
                hop.reserves_1,
                hop.fee_numer,
                hop.fee_denom,
            ),
            // A non-Solidly variant with `stable=true` is impossible — these
            // DEXes have no stable pool family. Falls back to zero output.
            _ => return U256::ZERO,
        };
        out.unwrap_or(U256::ZERO)
    }

    /// Resolve a path's pool refs into hop states and tick-range sequences.
    /// Solve an all-V2-or-Balancer-weighted (no CL hops, ≥1 weighted) path
    /// using the same two-stage template as the Solidly solve: a V2-equivalent
    /// Möbius precheck narrows the golden-section bracket around the Möbius
    /// optimum (±5x), then 25 golden-section iterations refine the optimum,
    /// then integer verification scans ±3 candidates and picks the max-profit
    /// input.
    ///
    /// Per-hop simulation calls
    /// `degenbot_balancer_math::weighted_math::calc_out_given_in`. The
    /// V2-equivalent precheck treats each weighted hop as a constant-product
    /// hop — exact for 50/50 pools, approximate for other weights (good enough
    /// for bracket narrowing).
    fn solve_balancer_weighted_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
        use ::degenbot_solvers::mobius_int::compute_int_mobius_coefficients;
        use ::degenbot_solvers::mobius_int_exact::compute_mobius_model_optimal_input;
        use degenbot_v2_math::IntHopState;

        let hops = &resolved.hops;
        if hops.len() < 2 {
            return None;
        }

        // V2-equivalent IntHopState per hop: weighted hops use
        // balance_in/balance_out + the retained fee fraction; V2 hops pass
        // through unchanged.
        let v2_equiv: Vec<IntHopState> = hops
            .iter()
            .map(Self::balancer_weighted_hop_v2_equiv)
            .collect::<Option<Vec<_>>>()?;

        // --- Profitability precheck (V2-equivalent Möbius) ---
        let coeffs = compute_int_mobius_coefficients(&v2_equiv).ok()?;
        if !coeffs.is_profitable {
            return None;
        }
        let x_mobius = compute_mobius_model_optimal_input(&coeffs);

        // --- Bracket: [1, max_reserve], narrowed around the Möbius optimum ---
        let one = U256::from(1u64);
        let five = U256::from(5u64);
        let max_reserve = v2_equiv
            .iter()
            .map(|h| h.reserve_in)
            .max()
            .unwrap_or(U256::ZERO);
        if max_reserve.is_zero() {
            return None;
        }
        let mut x_low = one;
        let mut x_high = max_reserve;
        if !x_mobius.is_zero() {
            let x_center = x_mobius.min(x_high);
            x_low = x_low.max(x_center / five);
            x_high = x_high.min(x_center.saturating_mul(five));
        }
        if x_high <= x_low {
            // Degenerate bracket — fall back to a single-point verification.
            return Self::balancer_weighted_brute_force_best(hops, x_low);
        }

        // --- Golden-section search (25 iterations) ---
        let phi_num = U256::from(6_180_339_887_498_949u64); // phi * 1e16
        let phi_den = U256::from(10_000_000_000_000_000u64); // 1e16
        let phi_span = |lo: U256, hi: U256| -> U256 {
            let d = U512::from(hi - lo);
            let scaled = d * U512::from(phi_num) / U512::from(phi_den);
            ::degenbot_solvers::mobius_int_exact::u512_to_u256_internal(scaled)
        };
        let mut x1 = x_high - phi_span(x_low, x_high);
        let mut x2 = x_low + phi_span(x_low, x_high);
        let mut p1 = Self::simulate_balancer_weighted_path(x1, hops).saturating_sub(x1);
        let mut p2 = Self::simulate_balancer_weighted_path(x2, hops).saturating_sub(x2);

        for _ in 0..Self::SOLIDLY_GOLDEN_SECTION_ITERATIONS {
            if p1 < p2 {
                x_low = x1;
                x1 = x2;
                p1 = p2;
                x2 = x_low + phi_span(x_low, x_high);
                p2 = Self::simulate_balancer_weighted_path(x2, hops).saturating_sub(x2);
            } else {
                x_high = x2;
                x2 = x1;
                p2 = p1;
                x1 = x_high - phi_span(x_low, x_high);
                p1 = Self::simulate_balancer_weighted_path(x1, hops).saturating_sub(x1);
            }
        }

        let x_opt = (x_low + x_high) / U256::from(2u64);
        Self::balancer_weighted_brute_force_best(hops, x_opt)
    }

    /// Build the V2-equivalent [`IntHopState`] for a hop in a Balancer
    /// weighted solve path. Weighted hops use the raw balances as reserves
    /// and convert the fee (`swap_fee` as fraction of 1e18 → retained fraction
    /// `gamma_numer/fee_denom`). V2 hops pass through unchanged. Returns `None`
    /// for any non-V2/non-weighted hop.
    fn balancer_weighted_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_v2_math::IntHopState> {
        use degenbot_v2_math::IntHopState;
        match hop {
            ResolvedHop::V2 { state } => Some(state.clone()),
            ResolvedHop::BalancerWeighted { state } => {
                // swap_fee is the fee fraction as parts of 1e18.
                // gamma = 1e18 - swap_fee; IntHopState wants gamma_numer/fee_denom.
                let one_e18 = U256::from(10u64).pow(U256::from(18u64));
                let gamma = one_e18.saturating_sub(state.swap_fee);
                let gn: u64 = gamma.try_into().ok()?;
                let fd: u64 = one_e18.try_into().ok()?;
                Some(IntHopState::new(
                    state.balance_in,
                    state.balance_out,
                    gn,
                    fd,
                ))
            }
            _ => None,
        }
    }

    /// V2-equivalent [`IntHopState`] for a Balancer stable hop. Used by the
    /// Möbius precheck to narrow the golden-section bracket. `StableSwap` ≠
    /// constant product, but at high amplification it's locally V2-like.
    fn balancer_stable_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_v2_math::IntHopState> {
        use degenbot_v2_math::IntHopState;
        match hop {
            ResolvedHop::V2 { state } => Some(state.clone()),
            ResolvedHop::BalancerStable { state } => {
                let one_e18 = U256::from(10u64).pow(U256::from(18u64));
                let gamma = one_e18.saturating_sub(state.swap_fee);
                let gn: u64 = gamma.try_into().ok()?;
                let fd: u64 = one_e18.try_into().ok()?;
                Some(IntHopState::new(
                    state.balances[state.token_index_in],
                    state.balances[state.token_index_out],
                    gn,
                    fd,
                ))
            }
            _ => None,
        }
    }

    /// Integer verification: scan `±SOLIDLY_INTEGER_SEARCH_RADIUS` candidates
    /// around `center` and pick the max-profit input. Mirrors
    /// `solidly_brute_force_best`'s sweep.
    fn balancer_weighted_brute_force_best(
        hops: &[ResolvedHop],
        center: U256,
    ) -> Option<SolvePathResult> {
        const SEARCH_RADIUS: u64 = 3;
        let one = U256::from(1u64);
        let start = center.saturating_sub(U256::from(SEARCH_RADIUS)).max(one);
        let end = center + U256::from(SEARCH_RADIUS) + one;

        let mut best_input = U256::ZERO;
        let mut best_profit = U256::ZERO;
        let mut cand = start;
        while cand <= end {
            let output = Self::simulate_balancer_weighted_path(cand, hops);
            let profit = output.saturating_sub(cand);
            if profit > best_profit {
                best_profit = profit;
                best_input = cand;
            }
            if cand == U256::MAX {
                break;
            }
            cand += one;
        }

        if best_profit.is_zero() {
            return None;
        }

        let hop_outputs = Self::simulate_balancer_weighted_path_outputs(best_input, hops);
        let mut consumed_inputs = Vec::with_capacity(hop_outputs.len());
        consumed_inputs.push(best_input);
        for i in 1..hop_outputs.len() {
            consumed_inputs.push(hop_outputs[i - 1]);
        }
        Some(SolvePathResult {
            optimal_input: best_input,
            profit: best_profit,
            hop_outputs,
            consumed_inputs,
        })
    }

    /// Simulate a Balancer-weighted-or-V2 path, returning only the final output.
    #[must_use]
    pub(crate) fn simulate_balancer_weighted_path(x: U256, hops: &[ResolvedHop]) -> U256 {
        Self::simulate_balancer_weighted_path_outputs(x, hops)
            .last()
            .copied()
            .unwrap_or(U256::ZERO)
    }

    /// Simulate a Balancer-weighted-or-V2 path, returning per-hop outputs.
    fn simulate_balancer_weighted_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
        let mut amount = x;
        let mut outputs = Vec::with_capacity(hops.len());
        for hop in hops {
            if amount.is_zero() {
                outputs.push(U256::ZERO);
                continue;
            }
            let out = match hop {
                ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
                ResolvedHop::BalancerWeighted { state } => {
                    Self::simulate_balancer_weighted_hop(amount, state)
                }
                _ => U256::ZERO,
            };
            outputs.push(out);
            amount = out;
        }
        outputs
    }

    /// Evaluate a single Balancer weighted hop's output via
    /// `degenbot_balancer_math::weighted_math::calc_out_given_in`. The swap
    /// fee is subtracted from the input first (Balancer convention:
    /// `amount_in_after_fee = amount_in - mul_up(amount_in, swap_fee)`),
    /// then the invariant math runs on the after-fee amount. Returns
    /// `U256::ZERO` on overflow / error (matching the defensive fallback in
    /// `simulate_solidly_hop`).
    fn simulate_balancer_weighted_hop(amount_in: U256, hop: &BalancerWeightedHopState) -> U256 {
        use degenbot_balancer_math::fixed_point::mul_up;
        use degenbot_balancer_math::weighted_math::calc_out_given_in;
        if amount_in.is_zero() {
            return U256::ZERO;
        }
        // Upscale input to 18-decimal fixed-point (plain integer multiply,
        // NOT mul_down — scaling factors are integer multipliers, not FXP).
        let amount_in_upscaled = amount_in.saturating_mul(hop.scaling_factor_in);
        // Subtract swap fee: amount_in_after_fee = amount_in - mul_up(amount_in, swap_fee).
        let Ok(fee_amount) = mul_up(amount_in_upscaled, hop.swap_fee) else {
            return U256::ZERO;
        };
        let amount_in_after_fee = amount_in_upscaled.saturating_sub(fee_amount);
        let Ok(amount_out_upscaled) = calc_out_given_in(
            hop.balance_in,
            hop.weight_in,
            hop.balance_out,
            hop.weight_out,
            amount_in_after_fee,
            hop.pow_version,
        ) else {
            return U256::ZERO;
        };
        // Downscale output from 18-decimal to native decimals (plain integer
        // division, NOT div_down — matches Python's `amount // scaling_factor`).
        if hop.scaling_factor_out.is_zero() {
            return U256::ZERO;
        }
        amount_out_upscaled / hop.scaling_factor_out
    }

    /// Balancer stable solve — Möbius precheck + golden-section search,
    /// mirroring `solve_balancer_weighted_path_int` with the stable math
    /// leaf (`calc_out_given_in` with pre-computed invariant).
    #[allow(clippy::too_many_lines)]
    fn solve_balancer_stable_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
        use ::degenbot_solvers::mobius_int::compute_int_mobius_coefficients;
        use ::degenbot_solvers::mobius_int_exact::compute_mobius_model_optimal_input;
        use degenbot_v2_math::IntHopState;

        let hops = &resolved.hops;

        // V2-equivalent IntHopState per hop: stable hops use
        // balances[idx_in]/balances[idx_out] + the retained fee fraction;
        // V2 hops pass through unchanged.
        let v2_equiv: Vec<IntHopState> = hops
            .iter()
            .map(Self::balancer_stable_hop_v2_equiv)
            .collect::<Option<Vec<_>>>()?;

        let coeffs = compute_int_mobius_coefficients(&v2_equiv).ok()?;
        if !coeffs.is_profitable {
            return None;
        }
        let x_mobius = compute_mobius_model_optimal_input(&coeffs);

        let one = U256::from(1u64);
        let five = U256::from(5u64);
        let max_reserve = v2_equiv
            .iter()
            .map(|h| h.reserve_in)
            .max()
            .unwrap_or(U256::ZERO);
        if max_reserve.is_zero() {
            return None;
        }
        let mut x_low = one;
        let mut x_high = max_reserve;
        if !x_mobius.is_zero() {
            let x_center = x_mobius.min(x_high);
            x_low = x_low.max(x_center / five);
            x_high = x_high.min(x_center.saturating_mul(five));
        }
        if x_high <= x_low {
            // Degenerate bracket — fall back to a single-point verification.
            let best_input = Self::balancer_stable_brute_force_best(hops, x_low)?;
            let best_profit =
                Self::simulate_balancer_stable_path(best_input, hops).saturating_sub(best_input);
            if best_profit.is_zero() {
                return None;
            }
            let hop_outputs = Self::simulate_balancer_stable_path_outputs(best_input, hops);
            let consumed_inputs: Vec<U256> = std::iter::once(best_input)
                .chain(
                    hop_outputs
                        .iter()
                        .take(hop_outputs.len().saturating_sub(1))
                        .copied(),
                )
                .collect();
            return Some(SolvePathResult {
                optimal_input: best_input,
                profit: best_profit,
                hop_outputs,
                consumed_inputs,
            });
        }

        let phi_num = U256::from(6_180_339_887_498_949u64);
        let phi_den = U256::from(10_000_000_000_000_000u64);
        let phi_span = |lo: U256, hi: U256| -> U256 {
            let d = U512::from(hi - lo);
            let scaled = d * U512::from(phi_num) / U512::from(phi_den);
            ::degenbot_solvers::mobius_int_exact::u512_to_u256_internal(scaled)
        };
        let mut x1 = x_high - phi_span(x_low, x_high);
        let mut x2 = x_low + phi_span(x_low, x_high);
        let mut p1 = Self::simulate_balancer_stable_path(x1, hops).saturating_sub(x1);
        let mut p2 = Self::simulate_balancer_stable_path(x2, hops).saturating_sub(x2);

        for _ in 0..Self::SOLIDLY_GOLDEN_SECTION_ITERATIONS {
            if p1 < p2 {
                x_low = x1;
                x1 = x2;
                p1 = p2;
                x2 = x_low + phi_span(x_low, x_high);
                p2 = Self::simulate_balancer_stable_path(x2, hops).saturating_sub(x2);
            } else {
                x_high = x2;
                x2 = x1;
                p2 = p1;
                x1 = x_high - phi_span(x_low, x_high);
                p1 = Self::simulate_balancer_stable_path(x1, hops).saturating_sub(x1);
            }
        }

        let x_opt = (x_low + x_high) / U256::from(2u64);
        let best_input = Self::balancer_stable_brute_force_best(hops, x_opt)?;
        let best_profit =
            Self::simulate_balancer_stable_path(best_input, hops).saturating_sub(best_input);
        if best_profit.is_zero() {
            return None;
        }
        let hop_outputs = Self::simulate_balancer_stable_path_outputs(best_input, hops);
        let consumed_inputs: Vec<U256> = std::iter::once(best_input)
            .chain(
                hop_outputs
                    .iter()
                    .take(hop_outputs.len().saturating_sub(1))
                    .copied(),
            )
            .collect();
        Some(SolvePathResult {
            optimal_input: best_input,
            profit: best_profit,
            hop_outputs,
            consumed_inputs,
        })
    }

    /// Simulate the full path and return the final output.
    fn simulate_balancer_stable_path(x: U256, hops: &[ResolvedHop]) -> U256 {
        let outputs = Self::simulate_balancer_stable_path_outputs(x, hops);
        outputs.last().copied().unwrap_or(U256::ZERO)
    }

    /// Simulate the full path and return each hop's output.
    fn simulate_balancer_stable_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
        let mut amount = x;
        let mut outputs = Vec::with_capacity(hops.len());
        for hop in hops {
            if amount.is_zero() {
                outputs.push(U256::ZERO);
                continue;
            }
            let out = match hop {
                ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
                ResolvedHop::BalancerStable { state } => {
                    Self::simulate_balancer_stable_hop(amount, state)
                }
                _ => U256::ZERO,
            };
            outputs.push(out);
            amount = out;
        }
        outputs
    }

    /// Simulate a single Balancer stable hop (upscaling, fee, math,
    /// downscaling).
    fn simulate_balancer_stable_hop(amount_in: U256, hop: &BalancerStableHopState) -> U256 {
        use degenbot_balancer_math::fixed_point::mul_up;
        use degenbot_balancer_math::stable_math::calc_out_given_in;
        if amount_in.is_zero() {
            return U256::ZERO;
        }
        let amount_in_upscaled = amount_in.saturating_mul(hop.scaling_factor_in);
        let Ok(fee_amount) = mul_up(amount_in_upscaled, hop.swap_fee) else {
            return U256::ZERO;
        };
        let amount_in_after_fee = amount_in_upscaled.saturating_sub(fee_amount);
        let Ok(amount_out_upscaled) = calc_out_given_in(
            hop.amp,
            &hop.balances,
            hop.token_index_in,
            hop.token_index_out,
            amount_in_after_fee,
            hop.invariant,
        ) else {
            return U256::ZERO;
        };
        if hop.scaling_factor_out.is_zero() {
            return U256::ZERO;
        }
        amount_out_upscaled / hop.scaling_factor_out
    }

    /// ±3 integer brute-force verify sweep (matches the Solidly/weighted
    /// template — ensures the golden-section result is a local max).
    /// Returns `Some(input)` only if profit > 0.
    fn balancer_stable_brute_force_best(hops: &[ResolvedHop], start: U256) -> Option<U256> {
        let mut best_profit =
            Self::simulate_balancer_stable_path(start, hops).saturating_sub(start);
        let mut best_input = start;
        for delta in 1u64..=3 {
            let cand_up = best_input.saturating_add(U256::from(delta));
            let profit_up =
                Self::simulate_balancer_stable_path(cand_up, hops).saturating_sub(cand_up);
            if profit_up > best_profit {
                best_profit = profit_up;
                best_input = cand_up;
            }
            if best_input >= U256::from(delta) {
                let cand_dn = best_input - U256::from(delta);
                let profit_dn =
                    Self::simulate_balancer_stable_path(cand_dn, hops).saturating_sub(cand_dn);
                if profit_dn > best_profit {
                    best_profit = profit_dn;
                    best_input = cand_dn;
                }
            }
        }
        if best_profit.is_zero() {
            None
        } else {
            Some(best_input)
        }
    }

    /// Curve stableswap solve — Möbius precheck + golden-section search,
    /// mirroring the Balancer stable solve with the Curve stableswap math
    /// leaf (`stableswap_get_y` + fee/rate conversion).
    #[allow(clippy::too_many_lines)]
    fn solve_curve_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
        use ::degenbot_solvers::mobius_int::compute_int_mobius_coefficients;
        use ::degenbot_solvers::mobius_int_exact::compute_mobius_model_optimal_input;
        use degenbot_v2_math::IntHopState;

        let hops = &resolved.hops;

        // V2-equivalent IntHopState per hop: Curve hops use xp[idx_in]/xp[idx_out]
        // + the retained fee fraction; V2 hops pass through unchanged.
        let v2_equiv: Vec<IntHopState> = hops
            .iter()
            .map(Self::curve_hop_v2_equiv)
            .collect::<Option<Vec<_>>>()?;

        let coeffs = compute_int_mobius_coefficients(&v2_equiv).ok()?;
        if !coeffs.is_profitable {
            return None;
        }
        let x_mobius = compute_mobius_model_optimal_input(&coeffs);

        let one = U256::from(1u64);
        let five = U256::from(5u64);
        let max_reserve = v2_equiv
            .iter()
            .map(|h| h.reserve_in)
            .max()
            .unwrap_or(U256::ZERO);
        if max_reserve.is_zero() {
            return None;
        }
        let mut x_low = one;
        let mut x_high = max_reserve;
        if !x_mobius.is_zero() {
            let x_center = x_mobius.min(x_high);
            x_low = x_low.max(x_center / five);
            x_high = x_high.min(x_center.saturating_mul(five));
        }
        if x_high <= x_low {
            let best_input = Self::curve_brute_force_best(hops, x_low)?;
            return Self::curve_build_result(hops, best_input);
        }

        let phi_num = U256::from(6_180_339_887_498_949u64);
        let phi_den = U256::from(10_000_000_000_000_000u64);
        let phi_span = |lo: U256, hi: U256| -> U256 {
            let d = U512::from(hi - lo);
            let scaled = d * U512::from(phi_num) / U512::from(phi_den);
            ::degenbot_solvers::mobius_int_exact::u512_to_u256_internal(scaled)
        };
        let mut x1 = x_high - phi_span(x_low, x_high);
        let mut x2 = x_low + phi_span(x_low, x_high);
        let mut p1 = Self::simulate_curve_path(x1, hops).saturating_sub(x1);
        let mut p2 = Self::simulate_curve_path(x2, hops).saturating_sub(x2);

        for _ in 0..Self::SOLIDLY_GOLDEN_SECTION_ITERATIONS {
            if p1 < p2 {
                x_low = x1;
                x1 = x2;
                p1 = p2;
                x2 = x_low + phi_span(x_low, x_high);
                p2 = Self::simulate_curve_path(x2, hops).saturating_sub(x2);
            } else {
                x_high = x2;
                x2 = x1;
                p2 = p1;
                x1 = x_high - phi_span(x_low, x_high);
                p1 = Self::simulate_curve_path(x1, hops).saturating_sub(x1);
            }
        }

        let x_opt = (x_low + x_high) / U256::from(2u64);
        let best_input = Self::curve_brute_force_best(hops, x_opt)?;
        Self::curve_build_result(hops, best_input)
    }

    /// Build the full `SolvePathResult` from the best input.
    fn curve_build_result(hops: &[ResolvedHop], best_input: U256) -> Option<SolvePathResult> {
        let best_profit = Self::simulate_curve_path(best_input, hops).saturating_sub(best_input);
        if best_profit.is_zero() {
            return None;
        }
        let hop_outputs = Self::simulate_curve_path_outputs(best_input, hops);
        let consumed_inputs: Vec<U256> = std::iter::once(best_input)
            .chain(
                hop_outputs
                    .iter()
                    .take(hop_outputs.len().saturating_sub(1))
                    .copied(),
            )
            .collect();
        Some(SolvePathResult {
            optimal_input: best_input,
            profit: best_profit,
            hop_outputs,
            consumed_inputs,
        })
    }

    /// V2-equivalent [`IntHopState`] for a Curve stableswap hop.
    fn curve_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_v2_math::IntHopState> {
        use degenbot_v2_math::IntHopState;
        match hop {
            ResolvedHop::V2 { state } => Some(state.clone()),
            ResolvedHop::CurveStableswap { state } => {
                // Curve fee is fee/fee_denom; gamma = 1 - fee/fee_denom.
                // IntHopState wants gamma_numer/fee_denom.
                let gn: u64 = state.fee_denom.saturating_sub(state.fee).try_into().ok()?;
                let fd: u64 = state.fee_denom.try_into().ok()?;
                Some(IntHopState::new(
                    state.xp[state.token_index_in],
                    state.xp[state.token_index_out],
                    gn,
                    fd,
                ))
            }
            _ => None,
        }
    }

    /// Simulate the full path and return the final output.
    fn simulate_curve_path(x: U256, hops: &[ResolvedHop]) -> U256 {
        let outputs = Self::simulate_curve_path_outputs(x, hops);
        outputs.last().copied().unwrap_or(U256::ZERO)
    }

    /// Simulate the full path and return each hop's output.
    fn simulate_curve_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
        let mut amount = x;
        let mut outputs = Vec::with_capacity(hops.len());
        for hop in hops {
            if amount.is_zero() {
                outputs.push(U256::ZERO);
                continue;
            }
            let out = match hop {
                ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
                ResolvedHop::CurveStableswap { state } => Self::simulate_curve_hop(amount, state),
                _ => U256::ZERO,
            };
            outputs.push(out);
            amount = out;
        }
        outputs
    }

    /// Simulate a single Curve stableswap hop (rate-adjusted XP, `get_y`,
    /// fee+rate conversion).
    fn simulate_curve_hop(amount_in: U256, hop: &CurveStableswapHopState) -> U256 {
        use degenbot_curve_math::stableswap::stableswap_get_y;
        if amount_in.is_zero() {
            return U256::ZERO;
        }
        let i = hop.token_index_in;
        let j = hop.token_index_out;
        // Rate-adjust the input: x = xp[i] + dx * rate_in / PRECISION
        let x = hop.xp[i]
            .saturating_add(amount_in.saturating_mul(hop.rate_multiplier_in) / hop.precision);
        let Ok(y) = stableswap_get_y(
            i,
            j,
            x,
            &hop.xp,
            hop.amp,
            hop.n_coins,
            hop.a_precision,
            hop.y_variant,
            hop.d_variant,
        ) else {
            return U256::ZERO;
        };
        // dy = xp[j] - y - 1
        let dy_raw = hop.xp[j].saturating_sub(y).saturating_sub(U256::from(1u64));
        // Fee on dy, then rate convert: (dy - fee) * PRECISION / rate_out
        let fee = dy_raw.saturating_mul(hop.fee) / hop.fee_denom;
        let dy_after_fee = dy_raw.saturating_sub(fee);
        // Rate-convert from XP-space to native token space
        dy_after_fee.saturating_mul(hop.precision) / hop.rate_multiplier_out
    }

    /// ±3 integer brute-force verify sweep.
    fn curve_brute_force_best(hops: &[ResolvedHop], start: U256) -> Option<U256> {
        let mut best_profit = Self::simulate_curve_path(start, hops).saturating_sub(start);
        let mut best_input = start;
        for delta in 1u64..=3 {
            let cand_up = best_input.saturating_add(U256::from(delta));
            let profit_up = Self::simulate_curve_path(cand_up, hops).saturating_sub(cand_up);
            if profit_up > best_profit {
                best_profit = profit_up;
                best_input = cand_up;
            }
            if best_input >= U256::from(delta) {
                let cand_dn = best_input - U256::from(delta);
                let profit_dn = Self::simulate_curve_path(cand_dn, hops).saturating_sub(cand_dn);
                if profit_dn > best_profit {
                    best_profit = profit_dn;
                    best_input = cand_dn;
                }
            }
        }
        if best_profit.is_zero() {
            None
        } else {
            Some(best_input)
        }
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
                        10,
                    ) else {
                        return; // No integer sequence → invalid
                    };

                    resolved.hops.push(ResolvedHop::V3 { int_seq });
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
                        10,
                    ) else {
                        return; // No integer sequence → invalid
                    };

                    resolved.hops.push(ResolvedHop::V4 { int_seq });
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

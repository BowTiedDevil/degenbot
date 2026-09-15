//! Pure workload-analysis cluster (from the retired grab-file dissolution,
//!): the solve-bin sizing seam (`solve_bin_count`,
//! P6YXA6), the LPT pre-balanced partition (`lpt_partition`, RAYPAR T3), the
//! named cordon-fallback seat plan (`plan_bins` + `SeatPlan` +
//! `CordonFallbackDecision`, LW-T7), and the resolve-time cost proxies
//! (`path_cost_proxy`, `sims_aware_cost`).
//!
//! Extracted from the retired grab file so the pure binning/costing seams and their invariants live at their
//! own interface, independently of the lane walk.
use ::degenbot_solvers::mixed::ResolvedMixedPath;
use degenbot_core::op_info;
// ---------------------------------------------------------------------------
// RAYPAR T3: LPT-pre-balanced scoped-thread partition
// ---------------------------------------------------------------------------
/// The ONE solve-bin sizing seam (P6YXA6): fleet-hosted cycles bin at the
/// fleet's structural Solver seat count (pins == bins, so every bin owns a
/// warm keyed seat); every other arm bins at the machine-derived solve
/// worker count. One funnel so the arms can never re-derive the count and
/// drift apart (a hermetic fleet under machine-derived bins aborts at the
/// T2 grant — the host-only `just test-rust` failure).
pub(crate) fn solve_bin_count() -> usize {
    crate::arb_engine::executor::global_executor().bin_count()
}
#[expect(clippy::doc_markdown)]
/// RAYPAR T3: LPT (longest-processing-time) bin-packing. Sorts items by
/// descending cost and greedily assigns each to the least-loaded bin. Returns
/// indices into the original items slice, one Vec per bin.
///
/// The RAYPAR lab (docs/rayon-parallelism-lab.md) showed rayon work-stealing
/// par_iter achieves only 4.91/8 efficiency on the heavy-CL capture corpus
/// because the workload has extreme cost skew (top 8 of 80 paths = 60% of CPU).
/// LPT pre-balances so no thread gets stuck with an unsplittable giant while
/// others idle — achieving 7.80/8 (35% wall reduction). Same solver, same
/// threads, same memory bandwidth.
pub(crate) fn lpt_partition(
    n_items: usize,
    n_bins: usize,
    cost: impl Fn(usize) -> usize,
) -> Vec<Vec<usize>> {
    if n_bins == 0 {
        return Vec::new();
    }
    if n_items == 0 {
        return vec![Vec::new(); n_bins];
    }
    let mut idx: Vec<usize> = (0..n_items).collect();
    idx.sort_unstable_by_key(|&i| std::cmp::Reverse(cost(i)));
    let mut loads = vec![0usize; n_bins];
    let mut bins: Vec<Vec<usize>> = vec![Vec::new(); n_bins];
    for i in idx {
        let mi = loads
            .iter()
            .enumerate()
            .min_by_key(|&(_, l)| l)
            .map_or(0, |(i, _)| i);
        bins[mi].push(i);
        // Measured per-path costs are unbounded (the stance fixtures pin
        // placement with near-u64::MAX walks, and the serial tier's ONE
        // solve seat accumulates EVERY item's cost into bin 0): the
        // accumulation must not overflow — saturate instead of panicking.
        loads[mi] = loads[mi].saturating_add(cost(i));
    }
    bins
}
/// The RUNTIME lane-capability decision (LW-T7, Seam F): the cycle either
/// runs at full structural width or takes the NAMED narrower fallback
/// (reth `state_root_task_timeout => sequential` lesson: the fallback is a
/// NAMED-AND-LOGGED decision, never a silent narrower bin mid-drain) — the
/// runtime twin of LW-T4's boot-time capacity floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CordonFallbackDecision {
    /// Structural seats cover the intended bins.
    FullCapacity,
    /// The hosting capability dropped (T9 resize under cordon): the cycle
    /// runs `running` bins (< `intended`) this block — logged at INFO.
    Narrower {
        /// The bins the workload intended.
        intended: usize,
        /// The bins the current capability hosts.
        running: usize,
    },
}
/// One cycle's planned bin fan-out (LW-T7, Seam F).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeatPlan {
    /// The bin count the cycle fans out over.
    pub bins: usize,
    /// The typed fallback decision this plan took.
    pub decision: CordonFallbackDecision,
}
/// Plan this cycle's bin fan-out: a capability narrower than the intended
/// fan-out takes the NAMED narrower fallback (typed + logged at INFO).
#[must_use]
pub(crate) fn plan_bins(intended_bins: usize, structural_seats: usize) -> SeatPlan {
    if structural_seats < intended_bins {
        op_info!(
            domain = solver,
            intended = intended_bins,
            running = structural_seats,
            "capability drop under cordon — running the NAMED \
             narrower fallback (LW-T7; the serial arm remains a downstream decision)"
        );
        SeatPlan {
            bins: structural_seats,
            decision: CordonFallbackDecision::Narrower {
                intended: intended_bins,
                running: structural_seats,
            },
        }
    } else {
        SeatPlan {
            bins: intended_bins,
            decision: CordonFallbackDecision::FullCapacity,
        }
    }
}
#[expect(clippy::doc_markdown)]
/// Resolve-time cost proxy for LPT binning: the total number of word-boundary
/// prices across all CL hops. Correlates with walk combinatorics without
/// requiring a solve, so it is available at to_solve collection time.
pub(crate) fn path_cost_proxy(resolved: &ResolvedMixedPath) -> usize {
    resolved
        .hops
        .iter()
        .filter_map(|h| h.as_int_sequence())
        .flat_map(|seq| seq.ranges.iter())
        .map(|r| r.word_boundary_prices.len())
        .sum()
}
/// LPT cost used at binning: max(structural word-boundary proxy, previous
/// block's measured walk sims + measured gate µs). The measured counts
/// predict the current block's combinatorics better for stable pool shapes;
/// the proxy floors it for freshly dirty pools. (loop-12 BY7BLS KUKHMX;
/// loop-18 adds the gate-µs term — gate-heavy paths carry sims≈0 and were
/// bin-packed cheap while dominating wall time.) The sims and gate terms add
/// (same µs-scale: a walk sim ≈0.7-0.8µs, so `sims` ≈ walk µs).
pub(crate) fn sims_aware_cost(
    proxy: usize,
    last_sims: Option<u64>,
    last_gate_us: Option<u64>,
) -> usize {
    let measured = match last_sims {
        Some(v) => usize::try_from(v).unwrap_or(usize::MAX),
        None => 0,
    };
    let measured_gate = match last_gate_us {
        Some(v) => usize::try_from(v).unwrap_or(usize::MAX),
        None => 0,
    };
    proxy.max(measured.saturating_add(measured_gate))
}
#[cfg(test)]
mod lpt_partition_tests {
    use super::*;
    // ---- LW-T7 (Seam F): determinism, runtime fallback, promotion gate ----
    /// LW-T7 (Seam F): LPT is bit-stable — the same input & cost fn yields
    /// IDENTICAL bins across 50 invocations at widely varying shape, and
    /// equal-cost ties resolve by the FIXED rule (original index order) —
    /// the expected bins are computed by an independent reading of the
    /// documented rule (stable sort desc by cost with index-order ties,
    /// then item-by-item onto the first minimal-load bin).
    #[test]
    fn lpt_partition_is_bit_stable_across_invocations_and_ties_are_index_ordered() {
        let costs = vec![40, 40, 40, 70, 70, 30, 30, 30, 30, 55];
        let n = costs.len();
        // Independent reading of the documented rule (order-stable, ties by
        // ascending original index; min-load bin tie by lowest bin index).
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_by_key(|&i| (std::cmp::Reverse(costs[i]), i));
        let mut loads = [0usize; 3];
        let expected: Vec<Vec<usize>> = {
            let mut bins: Vec<Vec<usize>> = vec![Vec::new(); 3];
            for i in idx {
                // Closed 3-bin range: the Option is statically Some.
                #[expect(
                    clippy::expect_used,
                    reason = "the 3-bin range is statically non-empty"
                )]
                let mi = (0..3)
                    .min_by_key(|&bi| (loads[bi], bi))
                    .expect("closed 3-bin range is non-empty");
                bins[mi].push(i);
                loads[mi] += costs[i];
            }
            bins
        };
        for invocation in 0..50 {
            let bins = lpt_partition(n, 3, |i| costs[i]);
            assert_eq!(
                bins, expected,
                "invocation {invocation}: bins deviate from the documented rule"
            );
        }
    }
    /// LW-T7 (Seam F): a seat-capacity drop under a cordon drives a NAMED
    /// typed runtime fallback decision (typed enum, logged at INFO) —
    /// never silent narrower bins mid-drain (the runtime twin of LW-T4's
    /// boot-time capacity floor).
    #[test]
    fn seat_drop_under_cordon_drives_a_named_typed_runtime_fallback() {
        // Narrower capability: the plan NAMES the drop (intended → running).
        let plan = plan_bins(6, 4);
        assert_eq!(plan.bins, 4);
        assert_eq!(
            plan.decision,
            CordonFallbackDecision::Narrower {
                intended: 6,
                running: 4,
            },
            "the fallback must be a NAMED typed decision"
        );
        // Full capability: no fallback, full width.
        let full = plan_bins(6, 6);
        assert_eq!(full.decision, CordonFallbackDecision::FullCapacity);
        assert_eq!(full.bins, 6);
    }
    #[test]
    fn lpt_distributes_heavy_items_across_bins() {
        // Costs: [100, 100, 100, 1, 1, 1, 1, 1, 1, 1] — three heavy items
        // must go to three different bins (not clustered on one).
        let costs = [100, 100, 100, 1, 1, 1, 1, 1, 1, 1];
        let bins = lpt_partition(costs.len(), 3, |i| costs[i]);
        assert_eq!(bins.len(), 3);
        // Each bin should have exactly one heavy item.
        for bin in &bins {
            let heavy_count = bin.iter().filter(|&&i| costs[i] == 100).count();
            assert!(
                heavy_count <= 1,
                "bin has {heavy_count} heavy items, expected <= 1"
            );
        }
        // Total items preserved.
        let total: usize = bins.iter().map(Vec::len).sum();
        assert_eq!(total, costs.len());
    }
    #[test]
    fn lpt_empty_items_produces_empty_bins() {
        let bins = lpt_partition(0, 4, |_| 0);
        assert_eq!(bins.len(), 4);
        assert!(bins.iter().all(Vec::is_empty));
    }
    #[test]
    fn lpt_fewer_items_than_bins() {
        // 2 items, 8 bins — each item gets its own bin.
        let costs = [50, 30];
        let bins = lpt_partition(costs.len(), 8, |i| costs[i]);
        assert_eq!(bins.len(), 8);
        let non_empty: usize = bins.iter().filter(|b| !b.is_empty()).count();
        assert_eq!(non_empty, 2);
    }
    #[test]
    #[expect(clippy::unwrap_used)]
    fn lpt_balances_load() {
        // Costs: [10, 9, 8, 7, 6, 5, 4, 3, 2, 1] on 3 bins.
        // LPT assignment: 10→bin0(10), 9→bin1(9), 8→bin2(8), 7→bin1(16),
        // 6→bin2(14), 5→bin0(15), 4→bin2(18), 3→bin1(19), 2→bin0(17),
        // 1→bin0(18). Max load = 19, min load = 18. Well-balanced.
        let costs = [10, 9, 8, 7, 6, 5, 4, 3, 2, 1];
        let bins = lpt_partition(costs.len(), 3, |i| costs[i]);
        let loads: Vec<usize> = bins
            .iter()
            .map(|b| b.iter().map(|&i| costs[i]).sum())
            .collect();
        let max_load = *loads.iter().max().unwrap();
        let min_load = *loads.iter().min().unwrap();
        // LPT guarantees max_load - min_load <= max_item_cost.
        assert!(
            max_load - min_load <= 10,
            "load spread {max_load}-{min_load}={spread} exceeds max_item",
            spread = max_load - min_load
        );
    }
    #[test]
    fn sims_aware_cost_prefers_measured_last_block_walk() {
        // No measured value → structural proxy governs.
        assert_eq!(sims_aware_cost(500, None, None), 500);
        // Measured below the proxy → proxy still governs (fresh pool state
        // can always cost at least the structural floor).
        assert_eq!(sims_aware_cost(500, Some(300), None), 500);
        // Measured above the proxy → measured wins (the last block's sims
        // predict the current block's cost better than structure alone).
        assert_eq!(sims_aware_cost(300, Some(900), None), 900);
        // Oversized measured values saturate to usize::MAX rather than wrap.
        assert_eq!(sims_aware_cost(1, Some(u64::MAX), None), usize::MAX);
        // Loop-18: gate-heavy paths (sims≈0, gate 14ms) now register real cost.
        assert_eq!(sims_aware_cost(1, Some(0), Some(14_000)), 14_000);
        // Sims + gate terms ADD (both µs-scale) before the proxy comparison.
        assert_eq!(sims_aware_cost(500, Some(300), Some(14_000)), 14_300);
    }
    #[test]
    fn lpt_zero_bins_returns_empty_vec() {
        let bins = lpt_partition(5, 0, |_| 1);
        assert!(bins.is_empty());
    }
}
#[cfg(test)]
mod dispatch_binning_properties {
    //! JXCAR4 : solver dispatch binning properties over
    //! ARBITRARY item counts, cost shapes and seat shapes. The spawn-seam
    //! invariant (`FleetSolveExecutor::spawn` aborts on a bin >= the
    //! structural seat count, commit `ccc148275`) must never be the
    //! discovery mechanism again: a future binning regression shows up here
    //! as a shrunk counterexample, not a host-only SIGABRT.
    use super::lpt_partition;
    use crate::arb_engine::executor_ab_probe::prod_lpt_bins;
    use degenbot_solvers::mixed::ResolvedMixedPath;
    use proptest::prelude::*;
    use std::sync::Arc;
    /// Synthesized fixture paths (empty hops = zero structural cost; the
    /// properties exercise the BINDER, not the solver).
    fn synth_items(n: usize) -> Vec<Arc<ResolvedMixedPath>> {
        (0..n)
            .map(|_| {
                Arc::new(ResolvedMixedPath {
                    hops: Vec::new(),
                    valid: true,
                    state_nonces: Vec::new(),
                    max_update_block: 0,
                })
            })
            .collect()
    }
    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(256))]
        #[test]
        fn lpt_partition_preserves_items_and_stays_enclosed(
            n_items in 0usize..=2048usize,
            n_bins in 1usize..=64usize,
            costs in proptest::collection::vec(0usize..=97usize, 0..=2048),
        ) {
            let cost = |i: usize| costs.get(i).copied().unwrap_or(0);
            let bins = lpt_partition(n_items, n_bins, cost);
            // Enclosure: exactly the requested bin shape, indices inside.
            prop_assert_eq!(bins.len(), n_bins);
            for bin in &bins {
                for &i in bin {
                    prop_assert!(i < n_items);
                }
            }
            // Preservation: every item index appears exactly once.
            let mut seen: Vec<usize> = bins.iter().flatten().copied().collect();
            seen.sort_unstable();
            prop_assert_eq!(seen.len(), n_items);
            for (want, got) in seen.iter().enumerate() {
                prop_assert_eq!(*got, want);
            }
        }
        #[test]
        #[expect(
            clippy::cast_precision_loss,
            reason = "quota units (1e6 scale, <= 6.4e7) are exact in f64"
        )]
        fn prod_bins_never_exceed_the_structural_seats(
            quota_units in 6_000_000u64..=64_000_000u64,
            n_items in 0usize..=1024usize,
        ) {
            // Hostable quotas only (floor >= the pinned-role floor of 6);
            // the seat count comes from the REAL budget table keyed to the
            // quota property, never from this machine's shape.
            let q = (quota_units as f64) / 1_000_000.0;
            let seats = match degenbot_workers::budget::FleetBudget::derive(
                q,
                &degenbot_workers::budget::BudgetOverrides::default(),
            ) {
                Ok(b) => b.solver_pin_count,
                Err(err) => {
                    return Err(TestCaseError::fail(format!(
                        "hostable quota refused: {err:?}"
                    )));
                }
            };
            let items = synth_items(n_items);
            let bins = prod_lpt_bins(&items, seats);
            // Binding at the fleet's own seat count: the spawn normalize
            // (validate_bin_index) can never fire.
            prop_assert_eq!(bins.len(), seats);
            for (bin_idx, bin) in bins.iter().enumerate() {
                prop_assert!(bin_idx < seats);
                for &i in bin {
                    prop_assert!(i < n_items);
                }
            }
            // Item preservation across the seats.
            let total: usize = bins.iter().map(Vec::len).sum();
            prop_assert_eq!(total, n_items);
        }
    }
}
// ---------------------------------------------------------------------------
// 5WCRWZ T2 test-upgrade: direct coverage for the moved items the relocated
// islands did not pin at their own interface (`solve_bin_count` was pinned
// only through the engine's binning; `path_cost_proxy` only through
// `prod_lpt_bins`). The moved islands above pin `lpt_partition`,
// `plan_bins`/`SeatPlan`/`CordonFallbackDecision`, and `sims_aware_cost`.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod direct_seam_tests {
    #![expect(clippy::expect_used)] // known-good tick-range fixture
    use super::{path_cost_proxy, solve_bin_count};
    use crate::arb_engine::ArbitrageEngine;
    use ::degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
    use ::degenbot_solvers::mixed::{ResolvedHop, ResolvedMixedPath};
    use alloy::primitives::U256;
    use std::sync::Arc;
    /// `solve_bin_count` is the ONE solve-bin sizing seam (P6YXA6): it answers
    /// the executor's structural seat count (>= 1) rather than a re-derived
    /// number. Constructing an engine first satisfies `global_executor`'s
    /// construction-stamped boot precondition.
    #[test]
    fn solve_bin_count_delegates_to_the_executor_seat_count() {
        let _engine = ArbitrageEngine::new();
        let n = solve_bin_count();
        assert!(n >= 1, "the solve arm always owns at least one bin");
        assert_eq!(
            n,
            crate::arb_engine::executor::global_executor().bin_count(),
            "the sizing seam must not re-derive the bin count"
        );
    }
    fn cl_hop(word_boundaries: usize) -> ResolvedHop {
        let seq = IntV3TickRangeSequence::new(vec![IntV3TickRangeHop {
            liquidity: 1_000_000,
            sqrt_price_x96: U256::from(1u64 << 40),
            sqrt_price_lower_x96: U256::from(1u64 << 39),
            sqrt_price_upper_x96: U256::from(1u64 << 41),
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: true,
            word_boundary_prices: vec![U256::from(1u64); word_boundaries],
        }])
        .expect("valid tick range sequence");
        ResolvedHop::V3 {
            int_seq: Arc::new(seq),
            word_profiles: Arc::new(Vec::new()),
            crossing_table: Arc::new(Vec::new()),
        }
    }
    /// `path_cost_proxy` is the structural LPT cost: the total count of
    /// interior word-boundary prices across the CL hops (0 for an empty path).
    #[test]
    fn path_cost_proxy_sums_cl_word_boundaries() {
        let empty = ResolvedMixedPath {
            hops: Vec::new(),
            valid: true,
            ..Default::default()
        };
        assert_eq!(path_cost_proxy(&empty), 0);
        let path = ResolvedMixedPath {
            hops: vec![cl_hop(3), cl_hop(2)],
            valid: true,
            ..Default::default()
        };
        assert_eq!(path_cost_proxy(&path), 5);
    }
}

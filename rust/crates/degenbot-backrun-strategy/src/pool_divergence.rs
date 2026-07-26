//! Per-pool solver-divergence tracking (ergo epic GAXXNJ, task GMWYIU).
//!
//! A pool whose state the solver read wrong will flag `SolverCalc` across
//! every path routing through it in the same block. Today each such path
//! fails N times independently before per-path suppression kicks in. This
//! module tracks the per-pool divergence memo so the dispatch leaf can skip
//! paths through recently-divergent pools after the FIRST path flags the
//! pool — stopping the "every path through stale pool X fails, each
//! independently, each burning an N-count" waste.
//!
//! # Location decision (spike `docs/spikes/pool-divergence-memo-location.md`)
//!
//! Rust-core (option b), not Python-side. Stands on the precedent of
//! `PathSuppression` (`degenbot-submission::PathSuppression`) — a stateful
//! per-key counter consumed by the dispatch leaf, kept standalone so the
//! simulation seam can lock it directly without locking the `Dispatcher`.
//! `PoolDivergence` mirrors that: a `HashMap<(HopType, u64), u64>` of
//! `pool_key → last-flagged-block`, decayed by a block window.
//!
//! # Decay window
//!
//! `POOL_DIVERGENCE_DECAY_BLOCKS = 100` — mirrors
//! `PATH_SUPPRESS_RETRY_INTERVAL = 100` (the existing path-suppress retry
//! interval). A pool flagged `SolverCalc` stays divergent for 100 blocks of
//! clean history before paths route through it again. The suppression
//! threshold (10 consecutive failures, `PATH_SUPPRESS_THRESHOLD`) is a
//! separate concept (per-path fail-count suppression, not per-pool divergence
//! decay).
//!
//! # Granularity (V4 caveat)
//!
//! The pool key is `(HopType, u64)` where the `u64` is the registered pool's
//! handle, NOT the `CapturedSwap.emitter` address. For V2/V3 these coincide
//! (the emitter IS the pool contract); for V4 the emitter is the PoolManager
//! address AND every V4 hop's `HopType` is the same, so all V4 pools would
//! collapse to the same key. The v1 path-skip therefore maps the captured
//! swap's emitter back to the engine's registered pool via the
//! per-path hop→pool index (the same `pool_to_paths` reverse index the
//! engine already holds). Per-pool-id V4 granularity is a later refinement.

use std::collections::HashMap;

use alloy::primitives::U256;

use crate::simulator::SimFailure;
use degenbot_simulation::CapturedSwap;
use degenbot_solvers::mixed::HopType;

/// The decay window for the divergent-pool memo — mirrors
/// `PATH_SUPPRESS_RETRY_INTERVAL` (the existing path-suppress retry
/// interval). A pool flagged `SolverCalc` stays divergent for this many
/// blocks of clean history, then clears.
pub const POOL_DIVERGENCE_DECAY_BLOCKS: u64 = 100;

/// Per-pool solver-divergence memo. A pool flagged `SolverCalc` (the solver's
/// reported `hop_outputs[i]` disagreed with the inspector-captured actual
/// swap output) stays divergent for `POOL_DIVERGENCE_DECAY_BLOCKS` blocks;
/// paths routing through a divergent pool are skipped pre-sim (counted in
/// `DispatchOutcome::divergent_dropped`).
///
/// Parallels `PathSuppression` (a stateful per-key counter consumed by the
/// dispatch leaf) — kept standalone + `Default` so the simulation seam can
/// lock it directly without locking the `Dispatcher`.
#[derive(Debug, Default, Clone)]
pub struct PoolDivergence {
    /// `pool_key` → last block the pool flagged `SolverCalc`.
    /// Keyed by `(HopType, u64)` — matches the engine's `pool_to_paths`
    /// reverse index (NOT the `CapturedSwap.emitter` address; the V4
    /// caveat above explains the emitter→pool_key mapping).
    last_flagged: HashMap<(HopType, u64), u64>,
    /// Total paths skipped via the divergence memo (for logging parity with
    /// `PathSuppression::total_suppressed`).
    total_divergent_dropped: u64,
}

impl PoolDivergence {
    /// Construct a fresh, empty divergence tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `pool_key` flagged `SolverCalc` at `current_block`.
    /// Subsequent `is_divergent` calls within the decay window return `true`.
    pub fn record_divergence(&mut self, pool_key: (HopType, u64), current_block: u64) {
        self.last_flagged.insert(pool_key, current_block);
    }

    /// Is `pool_key` divergent as of `current_block`? Returns `true` iff the
    /// pool was flagged `SolverCalc` within the last
    /// `POOL_DIVERGENCE_DECAY_BLOCKS` blocks (clean history clears it).
    #[must_use]
    pub fn is_divergent(&self, pool_key: (HopType, u64), current_block: u64) -> bool {
        let Some(&last) = self.last_flagged.get(&pool_key) else {
            return false;
        };
        current_block.saturating_sub(last) < POOL_DIVERGENCE_DECAY_BLOCKS
    }

    /// Total paths skipped via the divergence memo (mirrors
    /// `PathSuppression::total_suppressed`).
    #[must_use]
    pub fn total_divergent_dropped(&self) -> u64 {
        self.total_divergent_dropped
    }

    /// Increment the dropped-path tally (called by the dispatch leaf when a
    /// candidate is dropped because it routes through a divergent pool).
    pub fn record_dropped(&mut self) {
        self.total_divergent_dropped += 1;
    }

    /// The current divergent-pool set (for the FFI getter +
    /// `[pool-divergence]` rendering). One `(pool_key, last_flagged_block)`
    /// per divergent pool. Clears entries past the decay window.
    #[must_use]
    pub fn divergent_pools(&self, current_block: u64) -> Vec<((HopType, u64), u64)> {
        self.last_flagged
            .iter()
            .filter(|(_, &last)| current_block.saturating_sub(last) < POOL_DIVERGENCE_DECAY_BLOCKS)
            .map(|(&k, &v)| (k, v))
            .collect()
    }
}

/// Did `failure` classify as `SolverCalc`? The Rust-core port of the Python
/// `logs/permutation_analyzer.py::classify_candidate` SolverCalc verdict —
/// the one dispatch policy cares about (the other verdicts, `Encoding`/
/// `Unknown`/`Drift`, are log-line taxonomy, not dispatch policy).
///
/// `SolverCalc` ⟺ the failure has a non-empty `captured_swaps` list AND at
/// least one captured swap's output (`max(amount0, amount1)` — the positive
/// amount is the token RECEIVED by the swapper) differs from the solver's
/// reported `hop_outputs[i]`.
///
/// Mirrors the Python classifier's amount-direction convention: the output is
/// the positive amount (received); for an exact-input swap exactly one of
/// `amount0`/`amount1` is positive. A count mismatch (captured_swaps.len()
/// != hop_outputs.len()) is NOT `SolverCalc` (defensive — classify as
/// non-divergent, mirroring the Python `Unknown` fallback).
#[must_use]
pub fn is_solver_calc_failure(failure: &SimFailure) -> bool {
    if failure.captured_swaps.is_empty() {
        return false;
    }
    if failure.captured_swaps.len() != failure.hop_outputs.len() {
        return false;
    }
    failure
        .captured_swaps
        .iter()
        .zip(failure.hop_outputs.iter())
        .any(|(swap, expected)| captured_swap_output(swap) != U256::from(*expected))
}

/// The output amount of a captured swap — the POSITIVE one (token received by
/// the swapper). For an exact-input swap exactly one of `amount0`/`amount1` is
/// positive; `max` picks the output. Mirrors the Python classifier's
/// `actual_output = max(amount0, amount1)` convention.
fn captured_swap_output(swap: &CapturedSwap) -> U256 {
    // amount0/amount1 are I256 (signed deltas — positive = received,
    // negative = paid in). The output is the positive one; clamp negatives
    // to 0 so `max` picks the received amount (and a swap with both negative
    // — adversarial / malformed — yields 0, which won't match any
    // non-zero expected).
    let a0 = swap.amount0.max(alloy::primitives::I256::ZERO);
    let a1 = swap.amount1.max(alloy::primitives::I256::ZERO);
    // Safe: both are non-negative I256; a non-negative I256 fits in U256.
    U256::try_from(a0)
        .unwrap_or(U256::ZERO)
        .max(U256::try_from(a1).unwrap_or(U256::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulator::SimFailure;
    use alloy::primitives::{Address, I256};
    use degenbot_simulation::SwapFamily;

    fn swap(family: SwapFamily, amount0: i128, amount1: i128) -> CapturedSwap {
        CapturedSwap {
            emitter: Address::repeat_byte(0x42),
            family,
            amount0: I256::try_from(amount0).unwrap(),
            amount1: I256::try_from(amount1).unwrap(),
            sqrt_price_x96: U256::ZERO,
            liquidity: U256::ZERO,
            tick: 0,
        }
    }

    fn failure(captured_swaps: Vec<CapturedSwap>, hop_outputs: Vec<u128>) -> SimFailure {
        SimFailure {
            path_id: 1,
            bucket: "0x CurrencyNotSettled".to_string(),
            fail_index: Some(3),
            revert_data: alloy::primitives::Bytes::default(),
            reverting_frame: None,
            captured_swaps,
            optimal_input: 1000,
            hop_outputs,
        }
    }

    // =====================================================================
    // is_solver_calc_failure
    // =====================================================================

    #[test]
    fn solvercalc_when_captured_output_differs_from_hop_output() {
        // captured swap output = 3000 (amount1=+3000), hop_outputs[0] = 2900.
        let f = failure(vec![swap(SwapFamily::V2, -1000, 3000)], vec![2900]);
        assert!(is_solver_calc_failure(&f));
    }

    #[test]
    fn not_solvercalc_when_captured_output_matches_hop_output() {
        // amounts match → Encoding (not SolverCalc) → no divergence.
        let f = failure(vec![swap(SwapFamily::V2, -1000, 3000)], vec![3000]);
        assert!(!is_solver_calc_failure(&f));
    }

    #[test]
    fn not_solvercalc_when_no_captured_swaps() {
        // orchestration-only bucket — no pool to attribute divergence to.
        let f = failure(vec![], vec![]);
        assert!(!is_solver_calc_failure(&f));
    }

    #[test]
    fn not_solvercalc_when_count_mismatch() {
        // Defensive: captured vs hop_outputs count mismatch → Unknown (not
        // SolverCalc), so don't flag the pool.
        let f = failure(
            vec![
                swap(SwapFamily::V2, -1000, 3000),
                swap(SwapFamily::V3, -500, 1500),
            ],
            vec![2900], // one hop_output, two captured swaps
        );
        assert!(!is_solver_calc_failure(&f));
    }

    #[test]
    fn solvercalc_when_reverse_direction_amount0_is_output() {
        // amount0=+2 (received), amount1=-500 (paid in) → output is amount0.
        let f = failure(vec![swap(SwapFamily::V2, 2, -500)], vec![1]);
        assert!(is_solver_calc_failure(&f));
    }

    #[test]
    fn solvercalc_when_v4_captured_amount_differs() {
        // V4 classifies the same as V2/V3 (the 7a129a3d re-point).
        let f = failure(vec![swap(SwapFamily::V4, -1000, 3000)], vec![2900]);
        assert!(is_solver_calc_failure(&f));
    }

    #[test]
    fn solvercalc_when_any_one_of_many_hops_differs() {
        // two hops, the second diverges → SolverCalc (any_mismatch).
        let f = failure(
            vec![
                swap(SwapFamily::V2, -1000, 3000),
                swap(SwapFamily::V3, -500, 1500),
            ],
            vec![3000, 1450],
        );
        assert!(is_solver_calc_failure(&f));
    }

    // =====================================================================
    // PoolDivergence decay + is_divergent
    // =====================================================================

    #[test]
    fn pool_is_divergent_within_decay_window() {
        let mut pd = PoolDivergence::new();
        let key = (HopType::V2, 1);
        pd.record_divergence(key, 1000);
        assert!(pd.is_divergent(key, 1000));
        assert!(pd.is_divergent(key, 1050));
        assert!(pd.is_divergent(key, 1099)); // 99 blocks later — still divergent
    }

    #[test]
    fn pool_clears_after_decay_window() {
        let mut pd = PoolDivergence::new();
        let key = (HopType::V3, 7);
        pd.record_divergence(key, 1000);
        assert!(pd.is_divergent(key, 1099));
        assert!(!pd.is_divergent(key, 1100)); // exactly 100 — decayed
        assert!(!pd.is_divergent(key, 2000));
    }

    #[test]
    fn unflagged_pool_is_not_divergent() {
        let pd = PoolDivergence::new();
        assert!(!pd.is_divergent((HopType::V2, 42), 1000));
    }

    #[test]
    fn pool_age_of_empires_record_overwrites_last_flagged() {
        // a fresh SolverCalc flag resets the decay window.
        let mut pd = PoolDivergence::new();
        let key = (HopType::V4, 9);
        pd.record_divergence(key, 1000);
        pd.record_divergence(key, 1050); // re-flagged 50 blocks later
        assert!(pd.is_divergent(key, 1149)); // 99 blocks after the re-flag
        assert!(!pd.is_divergent(key, 1150));
    }

    #[test]
    fn divergent_pools_returns_only_in_window() {
        let mut pd = PoolDivergence::new();
        pd.record_divergence((HopType::V2, 1), 1000);
        pd.record_divergence((HopType::V3, 2), 1050);
        pd.record_divergence((HopType::V4, 3), 900); // past decay at block 1100
        let live = pd.divergent_pools(1050);
        // V2 (flagged 1000, age 50) + V3 (flagged 1050, age 0) live;
        // V4 (flagged 900, age 150) decayed.
        assert_eq!(live.len(), 2);
        assert!(live.iter().any(|(k, _)| *k == (HopType::V2, 1)));
        assert!(live.iter().any(|(k, _)| *k == (HopType::V3, 2)));
    }

    #[test]
    fn record_dropped_increments_tally() {
        let mut pd = PoolDivergence::new();
        assert_eq!(pd.total_divergent_dropped(), 0);
        pd.record_dropped();
        pd.record_dropped();
        assert_eq!(pd.total_divergent_dropped(), 2);
    }
}

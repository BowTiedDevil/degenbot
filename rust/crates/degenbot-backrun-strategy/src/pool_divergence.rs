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
//! `PathSuppression` (`degenbot_submission::PathSuppression`) — a stateful
//! per-key counter consumed by the dispatch leaf, kept standalone so the
//! simulation seam can lock it directly without locking the `Dispatcher`.
//! `PoolDivergence` mirrors that: a `HashMap<PoolDivergenceKey, u64>` of
//! `pool_key → last-flagged-block`, decayed by a block window.
//!
//! # Identity keying (not engine pool handles)
//!
//! `PoolDivergenceKey` wraps the pool's chain identity directly:
//! `V2(Address)` / `V3(Address)` (the pool contract address — also the
//! swap-event emitter) + `V4(B256)` (the V4 `poolId` bytes32 — NOT the
//! PoolManager address, which is shared by every V4 pool). Both are
//! derivable from a path's [`HopInfo`], with NO engine pool-registry
//! lookup — so the skip path (pre-sim, per-hop) + the feedback path
//! (post-sim, per diverging hop) both derive the key from the candidate's
//! `path_info.hops` data they already hold. A standalone Rust consumer
//! (`cargo add degenbot`) uses `PoolDivergence` without an engine instance.
//!
//! # V4 attribution (why keys come from `HopInfo`, not `CapturedSwap`)
//!
//! The V4 `Swap` event carries `poolId` in `topic[1]`, but the
//! [`CapturedSwap`](degenbot_simulation::CapturedSwap) surface (built by the
//! swap-event inspector) stores only the `emitter` address — which for V4 is
//! the shared `PoolManager`, useless for per-pool attribution. The feedback
//! path therefore zips `captured_swaps[i]` ↔ `hops[i]` (index correspondence
//! is the `[is_solver_calc_failure]` count-guard contract — a count mismatch
//! short-circuits to non-`SolverCalc`) and derives the V4 key from
//! `hops[i].pool_id_hex`. Symmetric with the skip path (which derives from
//! `hops[i]` too); a follow-up may widen `CapturedSwap` to carry the V4
//! `poolId` for emitter-only attribution.
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

use std::collections::HashMap;

use alloy::primitives::{Address, B256, U256};
use degenbot_executor::composers::HopInfo;

use crate::simulator::SimFailure;
use degenbot_simulation::CapturedSwap;

/// The decay window for the divergent-pool memo — mirrors
/// `PATH_SUPPRESS_RETRY_INTERVAL` (the existing path-suppress retry
/// interval). A pool flagged `SolverCalc` stays divergent for this many
/// blocks of clean history, then clears.
pub const POOL_DIVERGENCE_DECAY_BLOCKS: u64 = 100;

/// The chain-identity key a pool's divergence is tracked under. Derivable
/// from a path's [`HopInfo`] (V2/V3 pool address or V4 `poolId` bytes32) —
/// NO engine pool-registry lookup, so the dispatch skip + feedback paths
/// are standalone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PoolDivergenceKey {
    /// A Uniswap-V2 (or V2-compatible) pool — the pool contract address
    /// (also the V2 `Swap` event emitter).
    V2(Address),
    /// A Uniswap-V3 pool — the pool contract address (also the V3 `Swap`
    /// event emitter).
    V3(Address),
    /// A Uniswap-V4 pool — the `poolId` bytes32 (NOT the PoolManager address,
    /// which is shared by every V4 pool + useless for per-pool attribution).
    V4(B256),
}

/// Derive the [`PoolDivergenceKey`] for a hop — the pool's chain identity
/// (V2/V3 address, V4 `poolId` bytes32). Returns `None` only for a malformed
/// V4 `pool_id_hex` (not a valid 0x-prefixed 32-byte hex), which is a
/// candidate-construction bug (the executor parses V4 pool ids from the same
/// hex at encode time); the caller treats `None` as "skip the hop for
/// divergence purposes" (never flags a pool it can't identify).
#[must_use]
pub fn hop_pool_key(hop: &HopInfo) -> Option<PoolDivergenceKey> {
    match hop {
        HopInfo::V2(v2) => Some(PoolDivergenceKey::V2(v2.pool_address)),
        HopInfo::V3(v3) => Some(PoolDivergenceKey::V3(v3.pool_address)),
        HopInfo::V4(v4) => parse_v4_pool_id(&v4.pool_id_hex).map(PoolDivergenceKey::V4),
    }
}

/// Parse a V4 `poolId` bytes32 from its `0x`-prefixed hex string. Returns
/// `None` for a malformed hex (not 64 hex chars / not a 32-byte value) —
/// the caller treats `None` as "skip the hop" (never flags a pool it can't
/// identify). Mirrors `parity_v4_swap.rs::parse_pool_id` (the standalone
/// dual-driver test) so the two parse the same way.
fn parse_v4_pool_id(hex: &str) -> Option<B256> {
    let stripped = hex.strip_prefix("0x").unwrap_or(hex);
    let bytes = alloy::hex::decode(stripped).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok().map(B256::from)
}

/// Per-pool solver-divergence memo. A pool flagged `SolverCalc` (the solver's
/// reported `hop_outputs[i]` disagreed with the inspector-captured actual
/// swap output) stays divergent for `POOL_DIVERGENCE_DECAY_BLOCKS` blocks;
/// paths routing through a divergent pool are skipped pre-sim (counted in
/// `DispatchOutcome::divergent_dropped`).
///
/// The feedback path (post-sim) records divergence via
/// [`diverging_pool_keys`] — the keys of the hops whose captured swap output
/// mismatched `hop_outputs`. A pool flagged this block clears after
/// `POOL_DIVERGENCE_DECAY_BLOCKS` clean blocks.
///
/// Parallels `PathSuppression` (a stateful per-key counter consumed by the
/// dispatch leaf) — kept standalone + `Default` so the simulation seam can
/// lock it directly without locking the `Dispatcher`.
#[derive(Debug, Default, Clone)]
pub struct PoolDivergence {
    /// `pool_key` → last block the pool flagged `SolverCalc`. Keyed by
    /// chain identity (V2/V3 address, V4 `poolId`) — derivable from a path's
    /// `HopInfo` without an engine lookup.
    last_flagged: HashMap<PoolDivergenceKey, u64>,
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
    pub fn record_divergence(&mut self, pool_key: PoolDivergenceKey, current_block: u64) {
        self.last_flagged.insert(pool_key, current_block);
    }

    /// Is `pool_key` divergent as of `current_block`? Returns `true` iff the
    /// pool was flagged `SolverCalc` within the last
    /// `POOL_DIVERGENCE_DECAY_BLOCKS` blocks (clean history clears it).
    #[must_use]
    pub fn is_divergent(&self, pool_key: PoolDivergenceKey, current_block: u64) -> bool {
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

    /// The current divergent-pool set (for the FFI getter + the
    /// `[pool-divergence]` rendering). One `(pool_key, last_flagged_block)`
    /// per divergent pool. Clears entries past the decay window.
    #[must_use]
    pub fn divergent_pools(&self, current_block: u64) -> Vec<(PoolDivergenceKey, u64)> {
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
/// `SolverCalc` ⟺ the failure has a non-empty `captured_swaps` list AND
/// at least one captured swap's output (`max(amount0, amount1)` — the positive
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

/// The diverging pools in a `SolverCalc` failure — the keys of the hops whose
/// captured swap output differed from `hop_outputs[i]`. The dispatch feedback
/// path records divergence for each.
///
/// Zips `captured_swaps[i]` ↔ `hop_outputs[i]` ↔ `hops[i]` (index
/// correspondence is the [`is_solver_calc_failure`] count-guard contract).
/// Returns empty when the failure is not `SolverCalc` (no captured swaps,
/// count mismatch, or `hops.len() != hop_outputs.len()` — a defensive guard
/// against a malformed candidate where the hop list + the solver's reported
/// outputs disagree; the skip treats such a path as non-attributeable).
/// `hops[i]` keys that fail to derive (a malformed V4 `pool_id_hex`) are
/// skipped — never flags a pool it can't identify.
#[must_use]
pub fn diverging_pool_keys(failure: &SimFailure, hops: &[HopInfo]) -> Vec<PoolDivergenceKey> {
    if failure.captured_swaps.is_empty() {
        return Vec::new();
    }
    // The classifier's count guard — but this fn is called for the feedback
    // (which only runs when is_solver_calc_failure returned true), so the
    // captured_swaps.len() == hop_outputs.len() invariant holds. The hops
    // length check is the additional triple-length guard.
    if failure.captured_swaps.len() != failure.hop_outputs.len()
        || failure.hop_outputs.len() != hops.len()
    {
        return Vec::new();
    }
    failure
        .captured_swaps
        .iter()
        .zip(failure.hop_outputs.iter())
        .zip(hops.iter())
        .filter_map(|((swap, expected), hop)| {
            if captured_swap_output(swap) == U256::from(*expected) {
                None
            } else {
                hop_pool_key(hop)
            }
        })
        .collect()
}

/// The output amount of a captured swap — the POSITIVE one (token received by
/// the swapper). For an exact-input swap exactly one of `amount0`/`amount1` is
/// positive; `max` picks the output. Mirrors the Python classifier's
/// `actual_output = max(amount0, amount1)` convention.
pub(crate) fn captured_swap_output(swap: &CapturedSwap) -> U256 {
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
    use alloy::primitives::{address, Address, I256};
    use degenbot_executor::composers::{HopInfo, V2HopInfo, V3HopInfo, V4HopInfo};
    use degenbot_simulation::SwapFamily;

    const V2_POOL: Address = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    const V3_POOL: Address = address!("cccccccccccccccccccccccccccccccccccccccc");

    fn v4_pool_id(hex: &str) -> B256 {
        parse_v4_pool_id(hex).expect("valid 0x hex")
    }

    fn v2_hop(pool: Address) -> HopInfo {
        HopInfo::V2(V2HopInfo {
            pool_address: pool,
            token0_address: Address::ZERO,
            token1_address: Address::ZERO,
            fee: 30,
            zfo: true,
        })
    }

    fn v3_hop(pool: Address) -> HopInfo {
        HopInfo::V3(V3HopInfo {
            pool_address: pool,
            token0_address: Address::ZERO,
            token1_address: Address::ZERO,
            fee: 3000,
            zfo: true,
        })
    }

    fn v4_hop(pool_id_hex: &str) -> HopInfo {
        HopInfo::V4(V4HopInfo {
            pool_manager_address: Address::ZERO,
            pool_id_hex: pool_id_hex.to_string(),
            currency0_address: Address::ZERO,
            currency1_address: Address::ZERO,
            fee: 3000,
            tick_spacing: 60,
            hook_address: Address::ZERO,
            zfo: true,
        })
    }

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
            reverted_swaps: Vec::new(),
            optimal_input: 1000,
            hop_outputs,
            call_trace: Vec::new(),
            weth_before: 0,
            weth_after: 0,
        }
    }

    // =====================================================================
    // hop_pool_key — identity derivation from HopInfo
    // =====================================================================

    #[test]
    fn hop_pool_key_v2_is_pool_address() {
        assert_eq!(
            hop_pool_key(&v2_hop(V2_POOL)),
            Some(PoolDivergenceKey::V2(V2_POOL))
        );
    }

    #[test]
    fn hop_pool_key_v3_is_pool_address() {
        assert_eq!(
            hop_pool_key(&v3_hop(V3_POOL)),
            Some(PoolDivergenceKey::V3(V3_POOL))
        );
    }

    #[test]
    fn hop_pool_key_v4_is_pool_id_bytes32() {
        let hex = "0xabcd000000000000000000000000000000000000000000000000000000000001";
        let key = hop_pool_key(&v4_hop(hex)).expect("valid V4 pool id hex");
        assert_eq!(key, PoolDivergenceKey::V4(v4_pool_id(hex)));
    }

    #[test]
    fn hop_pool_key_v4_malformed_hex_returns_none() {
        // A malformed pool_id_hex (missing 0x prefix / wrong length) → None
        // (the skip treats None as "skip the hop", never flags it).
        assert_eq!(hop_pool_key(&v4_hop("not-a-hex")), None);
    }

    // =====================================================================
    // diverging_pool_keys — the feedback attribution
    // =====================================================================

    #[test]
    fn diverging_pool_keys_returns_only_mismatched_hop_keys() {
        // Two V2 hops: hop0 matches (3000 == 3000), hop1 mismatches (1500 vs 1450).
        let hops = vec![v2_hop(V2_POOL), v2_hop(V3_POOL)];
        let f = failure(
            vec![
                swap(SwapFamily::V2, -1000, 3000),
                swap(SwapFamily::V2, -500, 1500), // captured 1500, expected 1450
            ],
            vec![3000, 1450],
        );
        let keys = diverging_pool_keys(&f, &hops);
        assert_eq!(keys, vec![PoolDivergenceKey::V2(V3_POOL)]);
    }

    #[test]
    fn diverging_pool_keys_empty_when_all_match() {
        let hops = vec![v2_hop(V2_POOL)];
        let f = failure(vec![swap(SwapFamily::V2, -1000, 3000)], vec![3000]);
        assert!(diverging_pool_keys(&f, &hops).is_empty());
    }

    #[test]
    fn diverging_pool_keys_empty_when_no_captured_swaps() {
        let hops = vec![v2_hop(V2_POOL)];
        let f = failure(vec![], vec![]);
        assert!(diverging_pool_keys(&f, &hops).is_empty());
    }

    #[test]
    fn diverging_pool_keys_empty_when_captured_hops_length_mismatch() {
        // captured_swaps.len() (2) != hop_outputs.len() (1) → not attributeable.
        let hops = vec![v2_hop(V2_POOL)];
        let f = failure(
            vec![
                swap(SwapFamily::V2, -1000, 3000),
                swap(SwapFamily::V2, -500, 1500),
            ],
            vec![1450],
        );
        assert!(diverging_pool_keys(&f, &hops).is_empty());
    }

    #[test]
    fn diverging_pool_keys_empty_when_hops_length_mismatch() {
        // hop_outputs.len() (1) == captured_swaps.len() (1), but hops.len()
        // (2) != 1 → the defensive triple-length guard fires.
        let hops = vec![v2_hop(V2_POOL), v2_hop(V3_POOL)];
        let f = failure(vec![swap(SwapFamily::V2, -1000, 3000)], vec![1450]);
        assert!(diverging_pool_keys(&f, &hops).is_empty());
    }

    #[test]
    fn diverging_pool_keys_v4_uses_hop_pool_id_not_emitter() {
        // V4 captured swap emitter is the PoolManager (0x42) — useless. The
        // key comes from the hop's pool_id_hex.
        let hex = "0xabcd000000000000000000000000000000000000000000000000000000000001";
        let hops = vec![v4_hop(hex)];
        let f = failure(vec![swap(SwapFamily::V4, -1000, 3000)], vec![2900]);
        let keys = diverging_pool_keys(&f, &hops);
        assert_eq!(keys, vec![PoolDivergenceKey::V4(v4_pool_id(hex))]);
    }

    #[test]
    fn diverging_pool_keys_multiple_mismatches_returns_all() {
        // Both hops diverge → both keys.
        let pool_a = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let pool_b = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let hops = vec![v2_hop(pool_a), v3_hop(pool_b)];
        let f = failure(
            vec![
                swap(SwapFamily::V2, -1000, 3000),
                swap(SwapFamily::V3, -500, 1500),
            ],
            vec![2900, 1450],
        );
        let keys = diverging_pool_keys(&f, &hops);
        assert_eq!(
            keys,
            vec![PoolDivergenceKey::V2(pool_a), PoolDivergenceKey::V3(pool_b)]
        );
    }

    // =====================================================================
    // is_solver_calc_failure (unchanged from foundation)
    // =====================================================================

    #[test]
    fn solvercalc_when_captured_output_differs_from_hop_output() {
        let f = failure(vec![swap(SwapFamily::V2, -1000, 3000)], vec![2900]);
        assert!(is_solver_calc_failure(&f));
    }

    #[test]
    fn not_solvercalc_when_captured_output_matches_hop_output() {
        let f = failure(vec![swap(SwapFamily::V2, -1000, 3000)], vec![3000]);
        assert!(!is_solver_calc_failure(&f));
    }

    #[test]
    fn not_solvercalc_when_no_captured_swaps() {
        let f = failure(vec![], vec![]);
        assert!(!is_solver_calc_failure(&f));
    }

    #[test]
    fn not_solvercalc_when_count_mismatch() {
        let f = failure(
            vec![
                swap(SwapFamily::V2, -1000, 3000),
                swap(SwapFamily::V3, -500, 1500),
            ],
            vec![2900],
        );
        assert!(!is_solver_calc_failure(&f));
    }

    #[test]
    fn solvercalc_when_reverse_direction_amount0_is_output() {
        let f = failure(vec![swap(SwapFamily::V2, 2, -500)], vec![1]);
        assert!(is_solver_calc_failure(&f));
    }

    #[test]
    fn solvercalc_when_v4_captured_amount_differs() {
        let f = failure(vec![swap(SwapFamily::V4, -1000, 3000)], vec![2900]);
        assert!(is_solver_calc_failure(&f));
    }

    #[test]
    fn solvercalc_when_any_one_of_many_hops_differs() {
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
    // PoolDivergence decay + is_divergent (re-keyed to PoolDivergenceKey)
    // =====================================================================

    #[test]
    fn pool_is_divergent_within_decay_window() {
        let mut pd = PoolDivergence::new();
        let key = PoolDivergenceKey::V2(V2_POOL);
        pd.record_divergence(key, 1000);
        assert!(pd.is_divergent(key, 1000));
        assert!(pd.is_divergent(key, 1050));
        assert!(pd.is_divergent(key, 1099));
    }

    #[test]
    fn pool_clears_after_decay_window() {
        let mut pd = PoolDivergence::new();
        let key = PoolDivergenceKey::V3(V3_POOL);
        pd.record_divergence(key, 1000);
        assert!(pd.is_divergent(key, 1099));
        assert!(!pd.is_divergent(key, 1100));
        assert!(!pd.is_divergent(key, 2000));
    }

    #[test]
    fn unflagged_pool_is_not_divergent() {
        let pd = PoolDivergence::new();
        assert!(!pd.is_divergent(PoolDivergenceKey::V2(V2_POOL), 1000));
    }

    #[test]
    fn fresh_solvercalc_flag_resets_decay_window() {
        let mut pd = PoolDivergence::new();
        let key = PoolDivergenceKey::V4(v4_pool_id(
            "0xabcd000000000000000000000000000000000000000000000000000000000001",
        ));
        pd.record_divergence(key, 1000);
        pd.record_divergence(key, 1050);
        assert!(pd.is_divergent(key, 1149));
        assert!(!pd.is_divergent(key, 1150));
    }

    #[test]
    fn divergent_pools_returns_only_in_window() {
        let mut pd = PoolDivergence::new();
        pd.record_divergence(PoolDivergenceKey::V2(V2_POOL), 1000);
        pd.record_divergence(PoolDivergenceKey::V3(V3_POOL), 1050);
        pd.record_divergence(
            PoolDivergenceKey::V4(v4_pool_id(
                "0xabcd000000000000000000000000000000000000000000000000000000000001",
            )),
            900,
        );
        let live = pd.divergent_pools(1050);
        assert_eq!(live.len(), 2);
        assert!(live
            .iter()
            .any(|(k, _)| *k == PoolDivergenceKey::V2(V2_POOL)));
        assert!(live
            .iter()
            .any(|(k, _)| *k == PoolDivergenceKey::V3(V3_POOL)));
    }

    #[test]
    fn record_dropped_increments_tally() {
        let mut pd = PoolDivergence::new();
        assert_eq!(pd.total_divergent_dropped(), 0);
        pd.record_dropped();
        pd.record_dropped();
        assert_eq!(pd.total_divergent_dropped(), 2);
    }

    #[test]
    fn v2_and_v3_at_same_address_are_distinct_keys() {
        // A V2 + V3 pool can share an address (the engine keys by (HopType,
        // u64) for the same reason). The identity key keeps them distinct.
        let mut pd = PoolDivergence::new();
        pd.record_divergence(PoolDivergenceKey::V2(V2_POOL), 1000);
        assert!(pd.is_divergent(PoolDivergenceKey::V2(V2_POOL), 1000));
        assert!(!pd.is_divergent(PoolDivergenceKey::V3(V2_POOL), 1000));
    }
}

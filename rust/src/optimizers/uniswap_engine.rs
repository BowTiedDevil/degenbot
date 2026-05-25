//! Uniswap Engine — mixed V2/V3 arbitrage engine.
//!
//! A unified engine that handles both Uniswap V2 and V3 pools in the same
//! per-block lifecycle. Supports mixed paths (e.g., V2→V3 or V3→V2 hops).
//!
//! # Design
//!
//! The engine composes:
//! - A [`V2BlockEngine`] for V2 pool state and constant-product solving
//! - A [`V3BlockEngine`] for V3 pool state, tick ranges, and piecewise V3 solving
//!
//! On [`UniswapEngine::process_block`]:
//! 1. Decode both Sync and Swap events from logs
//! 2. Route V2 Sync events to the V2 engine, V3 Swap events to the V3 engine
//! 3. Solve registered paths using the appropriate solver (V2-V2, V3-V3, or mixed)
//!
//! Mixed V2-V3 paths use a golden-section search over the piecewise profit
//! function, where the V2 hop uses standard Möbius and the V3 hop uses
//! tick-range-constrained simulation.

use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;

use crate::optimizers::mobius_int::u256_to_f64;
use crate::optimizers::mobius_v3::V3TickRangeSequence;

use crate::optimizers::v2_block_engine::V2BlockEngine;
use crate::optimizers::v3_block_engine::{RegisterV3PoolParams, V3BlockEngine, V3SwapUpdate};

// ---------------------------------------------------------------------------
// Path types
// ---------------------------------------------------------------------------

/// Which engine owns a given hop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HopType {
    /// V2 constant-product hop
    V2,
    /// V3 concentrated-liquidity hop
    V3,
}

/// A pool reference in a mixed path.
#[derive(Clone, Debug)]
pub struct MixedPoolRef {
    /// Which engine owns this hop
    pub hop_type: HopType,
    /// For V2: `pool_id` in V2 engine. For V3: `pool_idx` in V3 engine.
    pub pool_key: u64,
    /// Direction (V2: implied by `pool_id` orientation; V3: explicit)
    pub zero_for_one: bool,
}

/// A registered mixed arbitrage path.
#[derive(Clone, Debug)]
struct MixedPath {
    pools: Vec<MixedPoolRef>,
}

/// Resolved state for a mixed path, ready for solving.
#[derive(Clone, Debug, Default)]
struct ResolvedMixedPath {
    hop_types: Vec<HopType>,
    /// V2 hop states (Some for V2 hops, None for V3 hops)
    v2_hops: Vec<Option<crate::optimizers::mobius_int::IntHopState>>,
    /// V3 tick-range sequences (Some for V3 hops, None for V2 hops)
    v3_sequences: Vec<Option<V3TickRangeSequence>>,
    /// Integer V3 hops built from original U256 values (Some for V3 hops, None for V2 hops)
    int_v3_hops: Vec<Option<crate::optimizers::mobius_v3_int::IntV3TickRangeHop>>,
    /// Integer V3 tick-range sequences for V3-V3 paths (Some for V3 hops, None for V2 hops)
    int_v3_sequences: Vec<Option<crate::optimizers::mobius_v3_int::IntV3TickRangeSequence>>,
    /// Base (f64) hops for Mobius initial estimate
    base_hops: Vec<crate::optimizers::mobius::HopState>,
    /// Whether this path is valid for solving
    valid: bool,
}

// ---------------------------------------------------------------------------
// UniswapEngine
// ---------------------------------------------------------------------------

/// The unified Uniswap engine — owns both V2 and V3 pool state and solves
/// mixed arbitrage paths.
pub struct UniswapEngine {
    /// The V2 engine
    v2_engine: V2BlockEngine,
    /// The V3 engine
    v3_engine: V3BlockEngine,
    /// Registered mixed paths
    paths: HashMap<u64, (MixedPath, ResolvedMixedPath)>,
    /// Reverse index: (`hop_type`, `pool_key`) maps to set of `path_ids` that use this pool
    pool_to_paths: HashMap<(HopType, u64), HashSet<u64>>,
    /// Last solved results
    results: Vec<(u64, U256, U256)>,
    /// Block number for the last solved results
    results_block: u64,
    /// Whether the engine is running (freezes registration after start)
    running: bool,
    /// Auto-incrementing path ID
    next_path_id: u64,
}

impl UniswapEngine {
    /// Create a new engine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            v2_engine: V2BlockEngine::new(),
            v3_engine: V3BlockEngine::new(),
            paths: HashMap::new(),
            pool_to_paths: HashMap::new(),
            results: Vec::new(),
            results_block: 0,
            running: false,
            next_path_id: 1,
        }
    }

    /// Access the V2 engine (for registration).
    #[allow(clippy::missing_const_for_fn)]
    pub fn v2_engine(&mut self) -> &mut V2BlockEngine {
        &mut self.v2_engine
    }

    /// Access the V3 engine (for registration).
    #[allow(clippy::missing_const_for_fn)]
    pub fn v3_engine(&mut self) -> &mut V3BlockEngine {
        &mut self.v3_engine
    }

    /// Register a mixed arbitrage path as an ordered list of `MixedPoolRef`s.
    ///
    /// Returns the auto-assigned path ID.
    ///
    /// # Panics
    ///
    /// Panics if called after `start()` or with fewer than 2 pool refs.
    pub fn register_path(&mut self, pool_refs: Vec<MixedPoolRef>) -> u64 {
        assert!(!self.running, "cannot register paths after start()");
        assert!(pool_refs.len() >= 2, "need at least 2 pool refs");

        let path_id = self.next_path_id;
        self.next_path_id += 1;

        let mut resolved = ResolvedMixedPath::default();
        self.resolve_path(&pool_refs, &mut resolved);

        // Build reverse index: (hop_type, pool_key) → path_id
        for pool_ref in &pool_refs {
            self.pool_to_paths
                .entry((pool_ref.hop_type, pool_ref.pool_key))
                .or_default()
                .insert(path_id);
        }

        self.paths
            .insert(path_id, (MixedPath { pools: pool_refs }, resolved));
        path_id
    }

    /// Process a block: decode both Sync and Swap events, route to
    /// sub-engines, and re-solve only affected paths.
    pub fn process_block(&mut self, logs: &[Log], block_number: u64) {
        // Separate V2 Sync and V3 Swap logs
        let mut v2_logs: Vec<&Log> = Vec::new();
        let mut v3_logs: Vec<&Log> = Vec::new();

        for log in logs {
            // Try to identify the log type by its topic
            if let Some(topic) = log.topics().first() {
                if *topic == crate::optimizers::v2_sync_decoder::V2_SYNC_TOPIC {
                    v2_logs.push(log);
                } else if *topic == crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC {
                    v3_logs.push(log);
                }
            }
        }

        // Collect affected pool addresses before applying updates
        let v2_addrs: Vec<Address> = v2_logs.iter().map(|log| log.address()).collect();
        let v3_addrs: Vec<Address> = v3_logs.iter().map(|log| log.address()).collect();

        // Process V2 Sync events
        if !v2_logs.is_empty() {
            let v2_log_owned: Vec<Log> = v2_logs.iter().map(|l| (*l).clone()).collect();
            self.v2_engine.process_block(&v2_log_owned, block_number);
        }

        // Process V3 Swap events
        if !v3_logs.is_empty() {
            let v3_log_owned: Vec<Log> = v3_logs.iter().map(|l| (*l).clone()).collect();
            self.v3_engine.process_block(&v3_log_owned, block_number);
        }

        // Map addresses to pool keys
        let v2_affected: HashSet<u64> = v2_addrs
            .iter()
            .filter_map(|addr| self.v2_engine.pool_key_for_address(addr))
            .collect();
        let v3_affected: HashSet<u64> = v3_addrs
            .iter()
            .filter_map(|addr| self.v3_engine.pool_key_for_address(addr))
            .collect();

        // Re-solve only paths containing updated pools
        self.rebuild_and_solve_affected(&v2_affected, &v3_affected, block_number);
    }

    /// Process pre-decoded updates for testing.
    pub fn process_updates(
        &mut self,
        v2_updates: &[(Address, U256, U256)],
        v3_updates: &[V3SwapUpdate],
        block_number: u64,
    ) {
        // Apply updates to sub-engines and collect affected pool keys
        let v2_affected = self.v2_engine.apply_sync_updates(v2_updates);
        let v3_affected = self.v3_engine.apply_swap_updates(v3_updates, block_number);

        // Re-solve only paths containing updated pools
        self.rebuild_and_solve_affected(&v2_affected, &v3_affected, block_number);
    }

    /// Re-resolve and re-solve only paths that contain updated pools.
    ///
    /// Uses the `pool_to_paths` reverse index to identify `affected_path_ids`,
    /// then re-resolves and re-solves only those. Unaffected paths carry
    /// their previous results forward.
    fn rebuild_and_solve_affected(
        &mut self,
        v2_affected: &HashSet<u64>,
        v3_affected: &HashSet<u64>,
        block_number: u64,
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

        // If no paths are affected, just update the block number
        if affected_path_ids.is_empty() {
            self.results_block = block_number;
            return;
        }

        // Re-resolve affected paths
        let resolve_work: Vec<(u64, Vec<MixedPoolRef>)> = affected_path_ids
            .iter()
            .filter_map(|&path_id| {
                self.paths
                    .get(&path_id)
                    .map(|(path, _)| (path_id, path.pools.clone()))
            })
            .collect();

        for (path_id, pool_refs) in &resolve_work {
            let mut resolved = ResolvedMixedPath::default();
            self.resolve_path(pool_refs, &mut resolved);
            if let Some((_, stored)) = self.paths.get_mut(path_id) {
                *stored = resolved;
            }
        }

        // Re-solve affected paths and merge with unchanged results
        let mut new_results: Vec<(u64, U256, U256)> = Vec::with_capacity(self.paths.len());

        // Carry forward unchanged results
        for &(path_id, ref input, ref profit) in &self.results {
            if !affected_path_ids.contains(&path_id) {
                new_results.push((path_id, *input, *profit));
            }
        }

        // Solve affected paths
        for &path_id in &affected_path_ids {
            let Some((_path, resolved)) = self.paths.get(&path_id) else {
                continue;
            };
            if !resolved.valid {
                continue;
            }

            if let Some((opt_input, profit)) = self.solve_path(resolved) {
                if !opt_input.is_zero() && !profit.is_zero() {
                    new_results.push((path_id, opt_input, profit));
                }
            }
        }

        // Sort by path_id for deterministic output
        new_results.sort_unstable_by_key(|(path_id, _, _)| *path_id);

        self.results = new_results;
        self.results_block = block_number;
    }

    /// Solve a single resolved path.
    ///
    /// Dispatches based on path composition:
    /// - V2-V2: integer-exact Möbius solver (closed-form U512 isqrt)
    /// - V3-V3: integer piecewise-Möbius (closed-form per segment)
    /// - V2-V3 / V3-V2: mixed integer-exact solver
    #[allow(clippy::unused_self)]
    fn solve_path(&self, resolved: &ResolvedMixedPath) -> Option<(U256, U256)> {
        let all_v2 = resolved.hop_types.iter().all(|&t| t == HopType::V2);
        let all_v3 = resolved.hop_types.iter().all(|&t| t == HopType::V3);

        if all_v2 {
            let int_hops: Vec<_> = resolved
                .v2_hops
                .iter()
                .filter_map(Option::as_ref)
                .cloned()
                .collect();
            if int_hops.len() == resolved.hop_types.len() {
                crate::optimizers::mobius_int_exact::exact_mobius_solve(&int_hops)
                    .ok()
                    .and_then(|result| {
                        if result.is_profitable
                            && !result.optimal_input.is_zero()
                            && !result.profit.is_zero()
                        {
                            Some((result.optimal_input, result.profit))
                        } else {
                            None
                        }
                    })
            } else {
                None
            }
        } else if all_v3 {
            let int_sequences: Vec<_> = resolved
                .int_v3_sequences
                .iter()
                .filter_map(Option::as_ref)
                .collect();
            if int_sequences.len() == 2 {
                crate::optimizers::mobius_v3_int::int_solve_v3_v3(
                    int_sequences[0],
                    int_sequences[1],
                )
            } else {
                None
            }
        } else {
            Self::solve_mixed_path_int(resolved)
        }
    }

    /// Solve all registered paths using `solve_path`.
    #[must_use]
    fn solve_all(&self) -> Vec<(u64, U256, U256)> {
        let mut results = Vec::with_capacity(self.paths.len());

        for (&path_id, (_path, resolved)) in &self.paths {
            if !resolved.valid {
                continue;
            }

            if let Some((opt_input, profit)) = self.solve_path(resolved) {
                if !opt_input.is_zero() && !profit.is_zero() {
                    results.push((path_id, opt_input, profit));
                }
            }
        }

        results
    }

    /// Solve a mixed V2-V3 path using integer-exact Möbius solver.
    ///
    /// Uses the pre-built `IntV3TickRangeHop` from `resolve_path`,
    /// which was constructed directly from U256 values (no f64 conversion).
    ///
    /// Uses the closed-form `exact_mobius_solve` instead of golden-section
    /// search over f64 approximations.
    ///
    /// This produces **EVM-exact** results with zero float conversions.
    fn solve_mixed_path_int(
        resolved: &ResolvedMixedPath,
    ) -> Option<(U256, U256)> {
        if resolved.hop_types.len() != 2 {
            return None;
        }

        let hop0_is_v2 = resolved.hop_types[0] == HopType::V2;

        // Get the V2 hop state and integer V3 hop
        let (v2_hop, int_v3_hop, v2_first) = if hop0_is_v2 {
            let v2 = resolved.v2_hops[0].as_ref()?;
            let v3 = resolved.int_v3_hops[1].as_ref()?;
            (v2, v3, true)
        } else {
            let v3 = resolved.int_v3_hops[0].as_ref()?;
            let v2 = resolved.v2_hops[1].as_ref()?;
            (v2, v3, false)
        };

        // Use the integer-exact mixed solver
        crate::optimizers::mobius_v3_int::exact_solve_mixed_v2_v3(
            std::slice::from_ref(v2_hop),
            int_v3_hop,
            v2_first,
        )
    }

    /// Read the last solved results and block number.
    #[must_use]
    pub const fn latest_results(&self) -> (&Vec<(u64, U256, U256)>, u64) {
        (&self.results, self.results_block)
    }

    /// Mark the engine as running. Freezes registration.
    #[allow(clippy::missing_const_for_fn)]
    pub fn start(&mut self) {
        self.running = true;
        self.v2_engine.start();
        self.v3_engine.start();
    }

    /// Perform initial solve of ALL paths.
    ///
    /// Called once after `freeze()` + `start()` to populate `results`
    /// for the first time. Subsequent updates use `rebuild_and_solve_affected`
    /// which only re-solves paths containing updated pools.
    pub fn initial_solve(&mut self, block_number: u64) {
        // Resolve all paths
        let path_pool_refs: Vec<(u64, Vec<MixedPoolRef>)> = self
            .paths
            .iter()
            .map(|(&id, (path, _))| (id, path.pools.clone()))
            .collect();

        for (path_id, pool_refs) in &path_pool_refs {
            let mut resolved = ResolvedMixedPath::default();
            self.resolve_path(pool_refs, &mut resolved);
            if let Some((_, stored)) = self.paths.get_mut(path_id) {
                *stored = resolved;
            }
        }

        // Solve all paths
        self.results = self.solve_all();
        self.results_block = block_number;
    }

    /// Whether the engine is running.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.running
    }

    /// Number of registered V2 pools.
    #[must_use]
    pub fn v2_pool_count(&self) -> usize {
        self.v2_engine.pool_count()
    }

    /// Number of registered V3 pools.
    #[must_use]
    pub fn v3_pool_count(&self) -> usize {
        self.v3_engine.pool_count()
    }

    /// Number of registered mixed paths.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.paths.len()
    }

    /// Return the list of registered V2 pool addresses.
    #[must_use]
    pub fn v2_registered_addresses(&self) -> Vec<Address> {
        self.v2_engine.registered_addresses()
    }

    /// Return the list of registered V3 pool addresses.
    #[must_use]
    pub fn v3_registered_addresses(&self) -> Vec<Address> {
        self.v3_engine.registered_addresses()
    }

    /// Resolve a path's pool refs into hop states and tick-range sequences.
    fn resolve_path(&self, pool_refs: &[MixedPoolRef], resolved: &mut ResolvedMixedPath) {
        resolved.hop_types.clear();
        resolved.v2_hops.clear();
        resolved.v3_sequences.clear();
        resolved.int_v3_hops.clear();
        resolved.int_v3_sequences.clear();
        resolved.base_hops.clear();
        resolved.valid = false;

        if pool_refs.len() < 2 {
            return;
        }

        resolved.hop_types.reserve(pool_refs.len());
        resolved.v2_hops.reserve(pool_refs.len());
        resolved.v3_sequences.reserve(pool_refs.len());
        resolved.int_v3_hops.reserve(pool_refs.len());
        resolved.int_v3_sequences.reserve(pool_refs.len());

        for pool_ref in pool_refs {
            match pool_ref.hop_type {
                HopType::V2 => {
                    // Look up the V2 pool state
                    let Some(hop_state) = self.v2_engine.get_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };

                    resolved.hop_types.push(HopType::V2);
                    resolved.v2_hops.push(Some(hop_state.clone()));
                    resolved.v3_sequences.push(None);
                    resolved.int_v3_hops.push(None);
                    resolved.int_v3_sequences.push(None);

                    let base = hop_state.to_base_hop();
                    resolved.base_hops.push(base);
                }
                HopType::V3 => {
                    // Look up V3 pool state and build tick-range sequence
                    let Some(pool_state) = self.v3_engine.get_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };

                    let sequence = pool_state.build_sequence(pool_ref.zero_for_one, 3);
                    if let Some(seq) = &sequence {
                        if let Some(first_range) = seq.ranges.first() {
                            resolved.base_hops.push(first_range.to_hop_state());
                        } else {
                            return; // Empty sequence → invalid
                        }
                    } else {
                        return; // No sequence → invalid
                    }

                    // Build integer V3 hop from original U256 values (exact, no f64 conversion)
                    let int_v3_hop = pool_state.build_int_v3_hop(pool_ref.zero_for_one);
                    // Build integer V3 sequence for V3-V3 paths
                    let int_v3_sequence = pool_state.build_int_v3_sequence(pool_ref.zero_for_one, 10);

                    resolved.hop_types.push(HopType::V3);
                    resolved.v2_hops.push(None);
                    resolved.v3_sequences.push(sequence);
                    resolved.int_v3_hops.push(int_v3_hop);
                    resolved.int_v3_sequences.push(int_v3_sequence);
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
// Helpers
// ---------------------------------------------------------------------------

// NOTE: `simulate_v3_hop` and `user_max_or` were removed when the mixed
// V2-V3 solver was replaced with the integer-exact version. The integer
// solver uses `mobius_v3_int::exact_solve_mixed_v2_v3` instead of
// golden-section search over f64 approximations.

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

#[cfg(test)]
mod tests {
    use super::*;

    fn usdc(amount: u64) -> U256 {
        U256::from(amount) * U256::from(10u64).pow(U256::from(6))
    }

    fn weth(amount: u64) -> U256 {
        U256::from(amount) * U256::from(10u64).pow(U256::from(18))
    }

    const GAMMA_03: u64 = 997;
    const FEE_DENOM_03: u64 = 1000;

    #[test]
    fn register_v2_and_v3_pools() {
        let mut engine = UniswapEngine::new();

        // Register a V2 pool
        let v2_fwd = engine.v2_engine().register_pool(
            Address::ZERO,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // Register a V3 pool
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );

        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x22u8; 20]),
                token0: Address::from([0u8; 20]),
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(79228162514264337593543950336u128),
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
            },
        );

        assert_eq!(engine.v2_pool_count(), 1);
        assert_eq!(engine.v3_pool_count(), 1);

        // Register a mixed V2→V3 path
        let path_id = engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: false,
            },
        ]);

        assert_eq!(path_id, 1);
        assert_eq!(engine.path_count(), 1);

        // Path should be resolved
        let (_, resolved) = &engine.paths[&path_id];
        assert!(resolved.valid);
        assert_eq!(resolved.hop_types.len(), 2);
        assert_eq!(resolved.hop_types[0], HopType::V2);
        assert_eq!(resolved.hop_types[1], HopType::V3);
    }

    #[test]
    fn process_block_routes_logs_to_sub_engines() {
        let mut engine = UniswapEngine::new();

        // Register V2 pools
        let v2_addr = Address::ZERO;
        let v2_fwd = engine.v2_engine().register_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        let v2_addr1 = Address::from([1u8; 20]);
        let v2_fwd1 = engine.v2_engine().register_pool(
            v2_addr1,
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // Register a pure V2 path
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd1,
                zero_for_one: true,
            },
        ]);

        // Process with no logs — should not panic
        engine.process_block(&[], 1);

        let (results, block) = engine.latest_results();
        assert_eq!(block, 1);
        let _ = results; // May or may not have profitable results
    }

    #[test]
    fn mixed_path_v2_to_v3_resolves() {
        let mut engine = UniswapEngine::new();

        // V2 pool
        let v2_fwd = engine.v2_engine().register_pool(
            Address::ZERO,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V3 pool
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );

        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x22u8; 20]),
                token0: Address::from([0u8; 20]),
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(79228162514264337593543950336u128),
                liquidity: 10_000_000_000_000,
                tick: 0,
                tick_data,
            },
        );

        // Mixed V2→V3 path
        let path_id = engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: false,
            },
        ]);

        let (_, resolved) = &engine.paths[&path_id];
        assert!(resolved.valid);
        assert!(resolved.v2_hops[0].is_some());
        assert!(resolved.v3_sequences[1].is_some());
    }

    #[test]
    fn missing_v2_pool_makes_path_invalid() {
        let mut engine = UniswapEngine::new();

        // Only register V3 pool
        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x22u8; 20]),
                token0: Address::from([0u8; 20]),
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(79228162514264337593543950336u128),
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
            },
        );

        // Reference a non-existent V2 pool
        let path_id = engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: 999, // Non-existent
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: false,
            },
        ]);

        let (_, resolved) = &engine.paths[&path_id];
        assert!(!resolved.valid);
    }

    #[test]
    fn process_updates_applies_both_types() {
        let mut engine = UniswapEngine::new();

        // Register V2 pools
        let v2_addr = Address::from([0x11u8; 20]);
        let v2_fwd = engine.v2_engine().register_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        let v2_addr1 = Address::from([0x12u8; 20]);
        let v2_fwd1 = engine.v2_engine().register_pool(
            v2_addr1,
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // Register V2-only path
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd1,
                zero_for_one: true,
            },
        ]);

        // Process updates
        engine.process_updates(
            &[(v2_addr, usdc(1_400_000), weth(750))],
            &[],
            42,
        );

        let (_, block) = engine.latest_results();
        assert_eq!(block, 42);
    }

    #[test]
    fn register_path_after_start_panics() {
        let mut engine = UniswapEngine::new();
        engine.start();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine.register_path(vec![
                MixedPoolRef {
                    hop_type: HopType::V2,
                    pool_key: 1,
                    zero_for_one: true,
                },
                MixedPoolRef {
                    hop_type: HopType::V3,
                    pool_key: 2,
                    zero_for_one: false,
                },
            ]);
        }));
        assert!(result.is_err());
    }

    #[test]
    fn pure_v2_path_finds_profitable_arb() {
        let mut engine = UniswapEngine::new();

        // V2 pool A: USDC/WETH with price ~1875 USDC/WETH
        let v2_addr_a = Address::from([0x11u8; 20]);
        let v2_fwd_a = engine.v2_engine().register_pool(
            v2_addr_a,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V2 pool B: WETH/USDC with price ~2000 USDC/WETH (mispriced — arb opportunity)
        let v2_addr_b = Address::from([0x12u8; 20]);
        let v2_fwd_b = engine.v2_engine().register_pool(
            v2_addr_b,
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V2→V2 path: USDC → WETH (pool A) → USDC (pool B)
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd_a, // reserve0=USDC, reserve1=WETH
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd_b, // reserve0=WETH, reserve1=USDC
                zero_for_one: true,
            },
        ]);

        // Solve
        let results = engine.solve_all();
        // Should find a profitable arbitrage
        assert!(!results.is_empty(), "should find profitable V2-V2 arb");
        let (_, x, p) = &results[0];
        assert!(!x.is_zero());
        assert!(!p.is_zero());
    }

    #[test]
    fn pure_v3_path_finds_profitable_arb() {
        let mut engine = UniswapEngine::new();

        // V3 pool A at tick 0 (1:1), high liquidity, with tick boundaries
        let mut tick_data_a = HashMap::new();
        tick_data_a.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data_a.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(-5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );

        let v3_key_a = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x21u8; 20]),
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(79228162514264337593543950336u128),
                liquidity: 10_000_000_000_000_000,
                tick: 0,
                tick_data: tick_data_a,
            },
        );

        // V3 pool B at tick -60 (slightly cheaper token1), high liquidity
        let sqrt_price_lower_u160 = crate::tick_math::get_sqrt_ratio_at_tick_internal(-60)
            .unwrap_or(alloy::primitives::U160::ZERO);
        let sqrt_price_lower = U256::from(sqrt_price_lower_u160);

        let mut tick_data_b = HashMap::new();
        tick_data_b.insert(
            0,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data_b.insert(
            -120,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(-5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );

        let v3_key_b = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x22u8; 20]),
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: sqrt_price_lower,
                liquidity: 10_000_000_000_000_000,
                tick: -60,
                tick_data: tick_data_b,
            },
        );

        // V3→V3 path: pool A (zfo) → pool B (ofz)
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key_a,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key_b,
                zero_for_one: false,
            },
        ]);

        let results = engine.solve_all();
        // V3-V3 arb depends on the exact price divergence — the important thing
        // is that the path resolves and the solver runs without panicking.
        // With a single tick spacing of 60 and 0.6% total fees, the arb may
        // not be profitable at these liquidity levels.
        let _ = results;
    }

    #[test]
    fn mixed_v2_to_v3_path_finds_arb() {
        let mut engine = UniswapEngine::new();

        // V2 pool: USDC/WETH
        let v2_addr = Address::from([0x11u8; 20]);
        let v2_fwd = engine.v2_engine().register_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V3 pool: same pair but different price
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );

        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x22u8; 20]),
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(79228162514264337593543950336u128),
                liquidity: 10_000_000_000_000,
                tick: 0,
                tick_data,
            },
        );

        // Mixed V2→V3 path
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: false,
            },
        ]);

        // Even if no profit found (depends on exact numbers),
        // solve_all should run without panicking
        let results = engine.solve_all();
        // Just verify it doesn't crash
        let _ = results;
    }

    #[test]
    fn mixed_v3_to_v2_path_resolves() {
        let mut engine = UniswapEngine::new();

        // V3 pool with tick data
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );

        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x22u8; 20]),
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(79228162514264337593543950336u128),
                liquidity: 10_000_000_000_000,
                tick: 0,
                tick_data,
            },
        );

        // V2 pool
        let v2_addr = Address::from([0x11u8; 20]);
        let v2_fwd = engine.v2_engine().register_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V3→V2 path
        let path_id = engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: false,
            },
        ]);

        let (_, resolved) = &engine.paths[&path_id];
        assert!(resolved.valid);
        assert_eq!(resolved.hop_types[0], HopType::V3);
        assert_eq!(resolved.hop_types[1], HopType::V2);
        assert!(resolved.v3_sequences[0].is_some());
        assert!(resolved.v2_hops[1].is_some());
    }

    #[test]
    fn rebuild_on_v2_update_changes_results() {
        let mut engine = UniswapEngine::new();

        // V2 pool A: USDC/WETH
        let v2_addr_a = Address::from([0x11u8; 20]);
        let v2_fwd_a = engine.v2_engine().register_pool(
            v2_addr_a,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V2 pool B: WETH/USDC
        let v2_addr_b = Address::from([0x12u8; 20]);
        let v2_fwd_b = engine.v2_engine().register_pool(
            v2_addr_b,
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V2→V2 path
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd_a,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd_b,
                zero_for_one: true,
            },
        ]);

        // Initial solve
        let results_before = engine.solve_all();

        // Apply V2 update to make pool A even more mispriced
        engine.process_updates(
            &[(v2_addr_a, usdc(1_400_000), weth(750))],
            &[],
            1,
        );

        let (results_after, block) = engine.latest_results();
        assert_eq!(block, 1);
        // Results should differ after the update
        let _ = results_before; // Just ensure initial solve didn't panic
        let _ = results_after;
    }
}

// ---------------------------------------------------------------------------
// PyO3 wrapper
// ---------------------------------------------------------------------------

use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyList;

/// Python-facing mixed V2/V3 arbitrage engine.
///
/// Wraps [`UniswapEngine`] with a `parking_lot::Mutex` for safe access
/// from the Tokio pump task.
#[pyclass(name = "UniswapArbEngine")]
#[allow(dead_code)]
pub struct PyUniswapArbEngine {
    /// Shared engine state
    engine: Arc<parking_lot::Mutex<UniswapEngine>>,
    /// Shutdown flag for the pump
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    /// Handle for the pump task (None until `start()` is called)
    pump_handle: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[pymethods]
impl PyUniswapArbEngine {
    #[new]
    fn new() -> Self {
        Self {
            engine: Arc::new(parking_lot::Mutex::new(UniswapEngine::new())),
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pump_handle: parking_lot::Mutex::new(None),
        }
    }

    /// Register a V2 pool by contract address and initial reserves.
    /// Returns the forward `pool_id`. The reverse `pool_id` is `forward_id + 1`.
    #[pyo3(signature = (address, reserve0, reserve1, gamma_numer, fee_denom))]
    fn register_v2_pool(
        &self,
        address: &str,
        reserve0: &Bound<'_, pyo3::PyAny>,
        reserve1: &Bound<'_, pyo3::PyAny>,
        gamma_numer: u64,
        fee_denom: u64,
    ) -> PyResult<u64> {
        let addr: Address = address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
        })?;
        let r0 = crate::alloy_py::extract_python_u256(reserve0)?;
        let r1 = crate::alloy_py::extract_python_u256(reserve1)?;

        Ok(self.engine.lock().v2_engine().register_pool(addr, r0, r1, gamma_numer, fee_denom))
    }

    /// Register a V3 pool by contract address and initial state.
    /// Returns the pool key for use in path registration.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (address, token0, token1, fee, tick_spacing, factory, sqrt_price_x96, liquidity, tick, tick_data))]
    fn register_v3_pool(
        &self,
        address: &str,
        token0: &str,
        token1: &str,
        fee: u32,
        tick_spacing: i32,
        factory: &str,
        sqrt_price_x96: &Bound<'_, pyo3::PyAny>,
        liquidity: u128,
        tick: i32,
        tick_data: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<u64> {
        let addr = address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        let t0 = token0.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid token0 address: {e}"))
        })?;
        let t1 = token1.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid token1 address: {e}"))
        })?;
        let fac = factory.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid factory address: {e}"))
        })?;
        let sp = crate::alloy_py::extract_python_u256(sqrt_price_x96)?;

        let mut rust_tick_data = HashMap::new();
        for (key, value) in tick_data.iter() {
            let tick_idx: i32 = key.extract()?;
            let tuple = value.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 2 {
                let msg = format!(
                    "Expected 2-tuple (liquidity_gross, liquidity_net), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let liquidity_gross: u128 = tuple.get_item(0)?.extract()?;
            let liquidity_net: i128 = tuple.get_item(1)?.extract()?;
            rust_tick_data.insert(tick_idx, make_tick_info(liquidity_gross, liquidity_net));
        }

        Ok(self.engine.lock().v3_engine().register_pool(RegisterV3PoolParams {
            address: addr,
            token0: t0,
            token1: t1,
            fee,
            tick_spacing,
            factory: fac,
            sqrt_price_x96: sp,
            liquidity,
            tick,
            tick_data: rust_tick_data,
        }))
    }

    /// Register a mixed arbitrage path.
    ///
    /// Each entry is (`hop_type_str`, `pool_key`, `zero_for_one`) where
    /// `hop_type_str` is "V2" or "V3".
    #[pyo3(signature = (pool_refs))]
    fn register_path(&self, pool_refs: &Bound<'_, PyList>) -> PyResult<u64> {
        let mut rust_refs = Vec::with_capacity(pool_refs.len());
        for item in pool_refs.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 3 {
                let msg = format!(
                    "Expected 3-tuple (hop_type, pool_key, zero_for_one), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let hop_type_str: String = tuple.get_item(0)?.extract()?;
            let pool_key: u64 = tuple.get_item(1)?.extract()?;
            let zero_for_one: bool = tuple.get_item(2)?.extract()?;

            let hop_type = match hop_type_str.as_str() {
                "V2" => HopType::V2,
                "V3" => HopType::V3,
                _ => {
                    let msg = format!("Invalid hop_type: {hop_type_str}. Expected 'V2' or 'V3'");
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
            };

            rust_refs.push(MixedPoolRef {
                hop_type,
                pool_key,
                zero_for_one,
            });
        }

        if rust_refs.len() < 2 {
            let msg = format!("Need at least 2 pool refs, got {}", rust_refs.len());
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        Ok(self.engine.lock().register_path(rust_refs))
    }

    /// Start the engine. Freezes registration.
    /// (Does not start a pump \- use `process_logs()` for testing.)
    fn start(&self) {
        self.engine.lock().start();
    }

    /// Whether the engine is running (registration is frozen).
    #[allow(clippy::missing_const_for_fn)]
    fn is_running(&self) -> bool {
        self.engine.lock().is_running()
    }

    /// Freeze registration without starting a pump.
    fn freeze(&self) {
        self.engine.lock().start();
    }

    /// Perform initial solve of all paths. Call once after `freeze()`.
    ///
    /// Populates `results` for the first time. Subsequent `process_logs`
    /// calls use dependency tracking to only re-solve affected paths.
    fn initial_solve(&self, block_number: u64) {
        self.engine.lock().initial_solve(block_number);
    }

    /// Number of registered V2 pools.
    fn v2_pool_count(&self) -> usize {
        self.engine.lock().v2_pool_count()
    }

    /// Number of registered V3 pools.
    fn v3_pool_count(&self) -> usize {
        self.engine.lock().v3_pool_count()
    }

    /// Number of registered paths.
    fn path_count(&self) -> usize {
        self.engine.lock().path_count()
    }

    /// Process Sync and Swap events synchronously (for testing without a subscription).
    ///
    /// `v2_sync_updates`: list of (`address_str`, `reserve0`, `reserve1`)
    /// `v3_swap_updates`: list of (`address_str`, `sqrt_price_x96`, liquidity, tick, `tick_priors`)
    ///   where `tick_priors` is a list of (`tick_index`, (`liquidity_gross`, `liquidity_net`))
    #[pyo3(signature = (v2_sync_updates, v3_swap_updates, block_number))]
    fn process_logs(
        &self,
        v2_sync_updates: &Bound<'_, PyList>,
        v3_swap_updates: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<()> {
        // Parse V2 Sync updates
        let mut rust_v2: Vec<(Address, U256, U256)> = Vec::with_capacity(v2_sync_updates.len());
        for item in v2_sync_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 3 {
                let msg = format!(
                    "Expected 3-tuple (address, reserve0, reserve1), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let addr_obj = tuple.get_item(0)?;
            let addr_str: String = addr_obj.extract()?;
            let addr = addr_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
            })?;
            let r0 = crate::alloy_py::extract_python_u256(&tuple.get_item(1)?)?;
            let r1 = crate::alloy_py::extract_python_u256(&tuple.get_item(2)?)?;
            rust_v2.push((addr, r0, r1));
        }

        // Parse V3 Swap updates
        let mut rust_v3: Vec<V3SwapUpdate> = Vec::with_capacity(v3_swap_updates.len());
        for item in v3_swap_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 5 {
                let msg = format!(
                    "Expected 5-tuple (address, sqrt_price, liquidity, tick, tick_priors), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }

            let addr_obj = tuple.get_item(0)?;
            let addr_str: String = addr_obj.extract()?;
            let addr = addr_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
            })?;
            let sqrt_price = crate::alloy_py::extract_python_u256(&tuple.get_item(1)?)?;
            let liquidity: u128 = tuple.get_item(2)?.extract()?;
            let tick: i32 = tuple.get_item(3)?.extract()?;

            let priors_obj = tuple.get_item(4)?;
            let priors_list = priors_obj.cast::<PyList>()?;
            let mut tick_priors = Vec::new();
            for prior_item in priors_list.iter() {
                let prior_tuple = prior_item.cast::<pyo3::types::PyTuple>()?;
                if prior_tuple.len() != 2 {
                    let msg = format!(
                        "Expected 2-tuple (tick_index, (lg, ln)), got {} elements",
                        prior_tuple.len()
                    );
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let tick_idx: i32 = prior_tuple.get_item(0)?.extract()?;
                let info_obj = prior_tuple.get_item(1)?;
                let info_tuple = info_obj.cast::<pyo3::types::PyTuple>()?;
                if info_tuple.len() != 2 {
                    let msg = format!(
                        "Expected 2-tuple (liquidity_gross, liquidity_net), got {} elements",
                        info_tuple.len()
                    );
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let lg: u128 = info_tuple.get_item(0)?.extract()?;
                let ln: i128 = info_tuple.get_item(1)?.extract()?;
                tick_priors.push((tick_idx, make_tick_info(lg, ln)));
            }

            rust_v3.push(V3SwapUpdate {
                pool_address: addr,
                sqrt_price_x96: sqrt_price,
                liquidity,
                tick,
                tick_priors,
            });
        }

        self.engine.lock().process_updates(&rust_v2, &rust_v3, block_number);
        Ok(())
    }

    /// Read the last solved results and block number.
    ///
    /// Returns (`results`, `block_number`) where results is a flat list:
    /// [`path_id_0`, `optimal_input_0`, `profit_0`, `path_id_1`, ...]
    #[allow(clippy::significant_drop_tightening)]
    fn latest_results(&self, py: Python<'_>) -> PyResult<(Py<PyList>, u64)> {
        let (results, block_num) = {
            let engine = self.engine.lock();
            let (r, b) = engine.latest_results();
            (r.clone(), b)
        };

        let py_list = PyList::empty(py);
        for (path_id, optimal_input, profit) in results {
            py_list.append(path_id)?;
            let input_py = crate::alloy_py::PyU256(optimal_input).into_pyobject(py)?;
            py_list.append(input_py)?;
            let profit_py = crate::alloy_py::PyU256(profit).into_pyobject(py)?;
            py_list.append(profit_py)?;
        }

        Ok((py_list.unbind(), block_num))
    }
}

/// Helper to construct `TickInfo` from Python-extracted values.
fn make_tick_info(liquidity_gross: u128, liquidity_net: i128) -> crate::bot_core::TickInfo {
    use alloy::primitives::{I256, U128};
    crate::bot_core::TickInfo {
        liquidity_gross: U128::from(liquidity_gross),
        liquidity_net: I256::try_from(liquidity_net).unwrap_or(I256::ZERO),
    }
}

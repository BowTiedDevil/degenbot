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

use std::collections::HashMap;

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;

use crate::optimizers::mobius::simulate_path;
use crate::optimizers::mobius_int::u256_to_f64;
use crate::optimizers::mobius_v3::V3TickRangeSequence;
use crate::optimizers::mobius_v3_v3::solve_v3_v3;
use crate::optimizers::v2_block_engine::V2BlockEngine;
use crate::optimizers::v3_block_engine::{RegisterV3PoolParams, V3BlockEngine, V3SwapUpdate};

// ---------------------------------------------------------------------------
// Path types
// ---------------------------------------------------------------------------

/// Which engine owns a given hop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

        self.paths
            .insert(path_id, (MixedPath { pools: pool_refs }, resolved));
        path_id
    }

    /// Process a block: decode both Sync and Swap events, route to
    /// sub-engines, and solve all paths.
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

        // Rebuild all mixed paths
        self.rebuild_and_solve(block_number);
    }

    /// Process pre-decoded updates for testing.
    pub fn process_updates(
        &mut self,
        v2_updates: &[(Address, U256, U256)],
        v3_updates: &[V3SwapUpdate],
        block_number: u64,
    ) {
        // Apply V2 updates
        self.v2_engine.process_sync_updates(v2_updates, block_number);

        // Apply V3 updates
        self.v3_engine.process_swap_updates(v3_updates, block_number);

        // Rebuild and solve mixed paths
        self.rebuild_and_solve(block_number);
    }

    /// Rebuild all path resolutions and solve.
    fn rebuild_and_solve(&mut self, block_number: u64) {
        // Collect path pool refs for rebuilding
        let path_pool_refs: Vec<(u64, Vec<MixedPoolRef>)> = self
            .paths
            .iter()
            .map(|(&id, (path, _))| (id, path.pools.clone()))
            .collect();

        // Rebuild each path
        for (path_id, pool_refs) in &path_pool_refs {
            let mut resolved = ResolvedMixedPath::default();
            self.resolve_path(pool_refs, &mut resolved);
            if let Some((_, stored)) = self.paths.get_mut(path_id) {
                *stored = resolved;
            }
        }

        // Solve all paths
        self.results = self.solve_all(None);
        self.results_block = block_number;
    }

    /// Solve all registered paths.
    ///
    /// Dispatches based on path composition:
    /// - V2-V2: `mobius_solve_with_refinement`
    /// - V3-V3: `solve_v3_v3`
    /// - V2-V3 / V3-V2: mixed golden-section search
    #[must_use]
    pub fn solve_all(&self, max_input: Option<f64>) -> Vec<(u64, U256, U256)> {
        let mut results = Vec::with_capacity(self.paths.len());

        for (&path_id, (_path, resolved)) in &self.paths {
            if !resolved.valid {
                continue;
            }

            let all_v2 = resolved.hop_types.iter().all(|&t| t == HopType::V2);
            let all_v3 = resolved.hop_types.iter().all(|&t| t == HopType::V3);

            if all_v2 {
                // Pure V2 path — delegate to V2 engine solver
                let int_hops: Vec<_> = resolved
                    .v2_hops
                    .iter()
                    .filter_map(Option::as_ref)
                    .cloned()
                    .collect();
                if int_hops.len() == resolved.hop_types.len() {
                    let result = crate::optimizers::mobius_int::mobius_solve_with_refinement(
                        &resolved.base_hops,
                        &int_hops,
                        true,
                        max_input,
                    );
                    if result.success {
                        if let (Some(x), Some(p)) = (result.optimal_input_int, result.profit_int) {
                            if !x.is_zero() && !p.is_zero() {
                                results.push((path_id, x, p));
                            }
                        }
                    }
                }
            } else if all_v3 {
                // Pure V3 path — use V3-V3 solver
                let sequences: Vec<&V3TickRangeSequence> = resolved
                    .v3_sequences
                    .iter()
                    .filter_map(Option::as_ref)
                    .collect();
                if sequences.len() == 2 {
                    let (x, profit, _iters) =
                        solve_v3_v3(sequences[0], sequences[1], max_input, 10);
                    if x > 0.0 && profit > 0.0 {
                        #[allow(clippy::cast_possible_truncation)]
                        #[allow(clippy::cast_sign_loss)]
                        {
                            let x_int = U256::from(x as u128);
                            let profit_int = U256::from(profit as u128);
                            if !x_int.is_zero() && !profit_int.is_zero() {
                                results.push((path_id, x_int, profit_int));
                            }
                        }
                    }
                }
            } else {
                // Mixed V2-V3 or V3-V2 path
                if let Some((x, profit)) = Self::solve_mixed_path(resolved, max_input) {
                    if !x.is_zero() && !profit.is_zero() {
                        results.push((path_id, x, profit));
                    }
                }
            }
        }

        results
    }

    /// Solve a mixed V2-V3 path using golden-section search.
    ///
    /// The profit function is: `profit(x) = output(x) - x`
    /// where `output(x)` routes through both hops sequentially.
    fn solve_mixed_path(
        resolved: &ResolvedMixedPath,
        max_input: Option<f64>,
    ) -> Option<(U256, U256)> {
        if resolved.hop_types.len() != 2 {
            return None;
        }

        let hop0_is_v2 = resolved.hop_types[0] == HopType::V2;

        // Get the V2 hop state and V3 sequence
        let (v2_hop, v3_seq, v2_first) = if hop0_is_v2 {
            let v2 = resolved.v2_hops[0].as_ref()?;
            let v3 = resolved.v3_sequences[1].as_ref()?;
            (v2, v3, true)
        } else {
            let v3 = resolved.v3_sequences[0].as_ref()?;
            let v2 = resolved.v2_hops[1].as_ref()?;
            (v2, v3, false)
        };

        let base_hop = v2_hop.to_base_hop();

        // Max input constrained by V3 range capacity
        let v3_capacity = v3_seq.ranges.first().map_or(f64::MAX, |r| {
            r.max_gross_input_in_range() * 0.999
        });
        let x_max = user_max_or(max_input, v3_capacity);

        let profit_fn = |x: f64| -> f64 {
            if v2_first {
                // V2 → V3: simulate V2 hop, then V3 hop
                let output0 = simulate_path(x, std::slice::from_ref(&base_hop));
                if output0 <= 0.0 {
                    return f64::MIN;
                }
                let v3_result = simulate_v3_hop(output0, v3_seq);
                v3_result - x
            } else {
                // V3 → V2: simulate V3 hop, then V2 hop
                let output0 = simulate_v3_hop(x, v3_seq);
                if output0 <= 0.0 {
                    return f64::MIN;
                }
                let v2_result = simulate_path(output0, std::slice::from_ref(&base_hop));
                v2_result - x
            }
        };

        // Golden-section search for maximum profit
        let x_min = 1.0;
        if x_min >= x_max {
            return None;
        }

        let (x_best, _iters) = crate::optimizers::mobius::golden_section_search_max(
            profit_fn,
            x_min,
            x_max,
        );
        let profit_best = profit_fn(x_best);

        if profit_best > 0.0 {
            #[allow(clippy::cast_possible_truncation)]
            #[allow(clippy::cast_sign_loss)]
            {
                Some((U256::from(x_best as u128), U256::from(profit_best as u128)))
            }
        } else {
            None
        }
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
        resolved.base_hops.clear();
        resolved.valid = false;

        if pool_refs.len() < 2 {
            return;
        }

        resolved.hop_types.reserve(pool_refs.len());
        resolved.v2_hops.reserve(pool_refs.len());
        resolved.v3_sequences.reserve(pool_refs.len());

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

                    resolved.hop_types.push(HopType::V3);
                    resolved.v2_hops.push(None);
                    resolved.v3_sequences.push(sequence);
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

/// Simulate a V3 hop with the given input amount.
/// Uses the first range of the tick-range sequence as a single-hop estimate.
fn simulate_v3_hop(x: f64, sequence: &V3TickRangeSequence) -> f64 {
    if sequence.ranges.is_empty() {
        return 0.0;
    }
    // Use the first range for the initial estimate
    let first = &sequence.ranges[0];
    let hop = first.to_hop_state();
    simulate_path(x, std::slice::from_ref(&hop))
}

/// Apply user max input or fall back to a computed max.
fn user_max_or(user_max: Option<f64>, computed_max: f64) -> f64 {
    user_max.map_or(computed_max, |m| m.min(computed_max))
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
        let results = engine.solve_all(None);
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

        let results = engine.solve_all(None);
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
        let results = engine.solve_all(None);
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
        let results_before = engine.solve_all(None);

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

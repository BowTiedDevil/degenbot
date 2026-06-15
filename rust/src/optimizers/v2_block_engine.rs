//! V2 Block Engine — Rust-centric arbitrage engine for Uniswap V2 paths.
//!
//! Owns the full per-block lifecycle: Sync event decoding, pool state updates,
//! path resolution, and Mobius solver dispatch. Python participates only in
//! initial construction and reading results.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;
use pyo3::prelude::*;
use pyo3::types::PyList;

use crate::optimizers::mobius::HopState;
use crate::optimizers::mobius_int::{mobius_solve_with_refinement, u256_to_f64, IntHopState};
use crate::optimizers::v2_sync_decoder::{decode_sync_log, SyncEvent};

// ---------------------------------------------------------------------------
// Path types (carried over from prototype)
// ---------------------------------------------------------------------------

/// A registered arbitrage path: list of pool IDs.
#[derive(Clone, Debug)]
struct V2Path {
    pool_ids: Vec<u64>,
}

/// Pre-resolved hop state pairs for a registered path.
/// Updated from pool state on each `rebuild_hops()` call.
#[derive(Clone, Debug, Default)]
struct ResolvedPath {
    base_hops: Vec<HopState>,
    int_hops: Vec<IntHopState>,
    valid: bool,
}

// ---------------------------------------------------------------------------
// V2BlockEngine — pure Rust, no PyO3
// ---------------------------------------------------------------------------

/// The core engine — owns pool state and registered paths.
/// All solve logic runs in pure Rust.
pub struct V2BlockEngine {
    /// Pool state: pool_id → IntHopState (both forward and reverse orientations)
    pools: HashMap<u64, IntHopState>,
    /// Pool contract address → (forward_pool_id, reverse_pool_id)
    pool_addresses: HashMap<Address, (u64, u64)>,
    /// Registered paths: path_id → (V2Path, ResolvedPath)
    paths: HashMap<u64, (V2Path, ResolvedPath)>,
    /// Last solved results: (path_id, optimal_input, profit)
    results: Vec<(u64, U256, U256)>,
    /// Block number for the last solved results
    results_block: u64,
    /// Auto-incrementing path ID
    next_path_id: u64,
    /// Auto-incrementing pool ID (forward_id; reverse_id = forward_id + 1)
    next_pool_id: u64,
}

impl V2BlockEngine {
    /// Create a new engine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pools: HashMap::new(),
            pool_addresses: HashMap::new(),
            paths: HashMap::new(),
            results: Vec::new(),
            results_block: 0,
            next_path_id: 1,
            next_pool_id: 1,
        }
    }

    /// Register a pool by contract address.
    ///
    /// Creates entries in both reserve orientations:
    /// - Forward (pool_id): reserve0 → reserve1
    /// - Reverse (pool_id + 1): reserve1 → reserve0
    ///
    /// Returns the forward pool_id. The reverse pool_id is `forward_id + 1`.
    ///
    /// # Panics
    pub fn register_pool(
        &mut self,
        address: Address,
        reserve0: U256,
        reserve1: U256,
        gamma_numer: u64,
        fee_denom: u64,
    ) -> u64 {
        assert!(gamma_numer < fee_denom, "gamma_numer must be less than fee_denom");

        let forward_id = self.next_pool_id;
        let reverse_id = self.next_pool_id + 1;
        self.next_pool_id += 2;

        // Forward: reserve0 → reserve1
        self.pools.insert(
            forward_id,
            IntHopState::new(reserve0, reserve1, gamma_numer, fee_denom),
        );

        // Reverse: reserve1 → reserve0
        self.pools.insert(
            reverse_id,
            IntHopState::new(reserve1, reserve0, gamma_numer, fee_denom),
        );

        self.pool_addresses.insert(address, (forward_id, reverse_id));

        forward_id
    }

    /// Register an arbitrage path by ordered pool IDs.
    ///
    /// Returns the auto-assigned path ID.
    ///
    /// # Panics
    ///
    /// Panics with fewer than 2 pool IDs.
    pub fn register_path(&mut self, pool_ids: Vec<u64>) -> u64 {
        assert!(pool_ids.len() >= 2, "need at least 2 pool IDs");

        let path_id = self.next_path_id;
        self.next_path_id += 1;

        let mut resolved = ResolvedPath::default();
        Self::resolve_path(&self.pools, &pool_ids, &mut resolved);

        self.paths
            .insert(path_id, (V2Path { pool_ids }, resolved));
        path_id
    }

    /// Update reserves for a registered pool from a Sync event.
    ///
    /// Sync carries absolute reserves — last-event-wins per pool per block.
    /// Both orientations are updated from the same event.
    pub fn apply_sync(&mut self, pool_address: Address, reserve0: U256, reserve1: U256) -> Option<u64> {
        let Some(&(forward_id, reverse_id)) = self.pool_addresses.get(&pool_address) else {
            return None; // Not a registered pool — skip
        };

        // Get gamma_numer/fee_denom from existing forward entry
        let forward_state = self.pools.get(&forward_id)?;
        let gamma_numer = forward_state.gamma_numer;
        let fee_denom = forward_state.fee_denom;

        // Update forward: reserve0 → reserve1
        self.pools.insert(
            forward_id,
            IntHopState::new(reserve0, reserve1, gamma_numer, fee_denom),
        );

        // Update reverse: reserve1 → reserve0
        self.pools.insert(
            reverse_id,
            IntHopState::new(reserve1, reserve0, gamma_numer, fee_denom),
        );

        Some(forward_id)
    }

    /// Apply Sync updates and return the set of forward pool keys that changed.
    /// Does NOT rebuild paths or solve — caller handles that.
    pub fn apply_sync_updates(&mut self, updates: &[(Address, U256, U256)]) -> HashSet<u64> {
        let mut affected = HashSet::new();
        for &(addr, r0, r1) in updates {
            if let Some(fwd_key) = self.apply_sync(addr, r0, r1) {
                affected.insert(fwd_key);
                // Insert the reverse key too — both orientations may be in paths
                affected.insert(fwd_key + 1);
            }
        }
        affected
    }

    /// Look up both pool keys (forward + reverse) for a registered address.
    /// Returns `None` if the address is not registered.
    ///
    /// Needed because paths may use either orientation (forward for zfo=True,
    /// reverse for zfo=False), and both must be tracked for dependency resolution.
    #[must_use]
    pub fn pool_keys_for_address(&self, address: &Address) -> Option<(u64, u64)> {
        self.pool_addresses.get(address).copied()
    }

    /// Look up the forward pool key for a registered address.
    /// Returns `None` if the address is not registered.
    #[must_use]
    pub fn pool_key_for_address(&self, address: &Address) -> Option<u64> {
        self.pool_addresses.get(address).map(|(fwd, _)| *fwd)
    }

    /// Process a block: decode Sync events, apply updates, rebuild paths,
    /// solve all, and store results.
    pub fn process_block(&mut self, logs: &[Log], block_number: u64) {
        // Decode Sync events and apply updates (last Sync wins per pool)
        let mut sync_events: Vec<SyncEvent> = Vec::new();
        for log in logs {
            if let Some(event) = decode_sync_log(log) {
                sync_events.push(event);
            }
        }

        // Apply Sync events — for pools with multiple Sync events, last one wins
        // (we just apply all in order; the last one overwrites previous updates)
        for event in sync_events {
            self.apply_sync(event.pool_address, event.reserve0, event.reserve1);
        }

        // Rebuild all paths from updated pool state
        for (path, resolved) in self.paths.values_mut() {
            Self::resolve_path(&self.pools, &path.pool_ids, resolved);
        }

        // Solve all paths
        self.results = self.solve_all(None);
        self.results_block = block_number;
    }

    /// Process pre-decoded Sync updates, rebuild paths, solve all, and store results.
    /// Convenience method for testing without log decode.
    pub fn process_sync_updates(&mut self, updates: &[(Address, U256, U256)], block_number: u64) {
        for &(addr, r0, r1) in updates {
            self.apply_sync(addr, r0, r1);
        }

        // Rebuild all paths from updated pool state
        for (path, resolved) in self.paths.values_mut() {
            Self::resolve_path(&self.pools, &path.pool_ids, resolved);
        }

        // Solve all paths
        self.results = self.solve_all(None);
        self.results_block = block_number;
    }

    /// Solve all registered paths. Returns profitable (path_id, input, profit).
    #[must_use]
    pub fn solve_all(&self, max_input: Option<f64>) -> Vec<(u64, U256, U256)> {
        let mut results = Vec::with_capacity(self.paths.len());

        for (&path_id, (_path, resolved)) in &self.paths {
            if !resolved.valid || resolved.int_hops.len() < 2 {
                continue;
            }

            let result =
                mobius_solve_with_refinement(&resolved.base_hops, &resolved.int_hops, true, max_input);

            if result.success {
                if let (Some(optimal_input), Some(profit)) =
                    (result.optimal_input_int, result.profit_int)
                {
                    if !optimal_input.is_zero() && !profit.is_zero() {
                        results.push((path_id, optimal_input, profit));
                    }
                }
            }
        }

        results
    }

    /// Read the last solved results and block number.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn latest_results(&self) -> (&Vec<(u64, U256, U256)>, u64) {
        (&self.results, self.results_block)
    }

    /// Return the list of registered pool addresses.
    /// Called once at `start()` time — the list is frozen thereafter.
    #[must_use]
    pub fn registered_addresses(&self) -> Vec<Address> {
        self.pool_addresses.keys().copied().collect()
    }

    /// Number of registered pools (counting forward orientations only).
    #[must_use]
    pub fn pool_count(&self) -> usize {
        self.pool_addresses.len()
    }

    /// Access the pool address → (forward_id, reverse_id) map.
    /// Used for reverse lookups in inspection/debugging.
    #[must_use]
    pub fn pool_addresses(&self) -> &HashMap<Address, (u64, u64)> {
        &self.pool_addresses
    }

    /// Get a reference to a pool's `IntHopState` by pool ID.
    #[must_use]
    pub fn get_pool(&self, pool_id: u64) -> Option<&IntHopState> {
        self.pools.get(&pool_id)
    }

    /// Number of registered paths.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.paths.len()
    }

    /// Resolve a path's pool IDs into HopState/IntHopState lists.
    fn resolve_path(
        pools: &HashMap<u64, IntHopState>,
        pool_ids: &[u64],
        resolved: &mut ResolvedPath,
    ) {
        resolved.base_hops.clear();
        resolved.int_hops.clear();
        resolved.valid = false;

        if pool_ids.len() < 2 {
            return;
        }

        resolved.base_hops.reserve(pool_ids.len());
        resolved.int_hops.reserve(pool_ids.len());

        for &pool_id in pool_ids {
            let Some(hop_state) = pools.get(&pool_id) else {
                return; // Missing pool → invalid
            };

            resolved.int_hops.push(hop_state.clone());
            #[allow(clippy::cast_precision_loss)]
            let r_in_f64 = u256_to_f64(hop_state.reserve_in);
            #[allow(clippy::cast_precision_loss)]
            let r_out_f64 = u256_to_f64(hop_state.reserve_out);
            #[allow(clippy::cast_precision_loss)]
            let fee_f64 = 1.0 - (hop_state.gamma_numer as f64 / hop_state.fee_denom as f64);
            resolved.base_hops.push(HopState::new(r_in_f64, r_out_f64, fee_f64));
        }

        resolved.valid = true;
    }
}

impl Default for V2BlockEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// PyO3 wrapper — minimal Python interface
// ---------------------------------------------------------------------------

/// V2 block engine — Rust-centric arbitrage engine for Uniswap V2 paths.
///
/// Python constructs the engine (registers pools and paths), then starts
/// a Rust-side pump that drives the full per-block lifecycle. Python reads
/// results via `latest_results()`.
#[pyclass(name = "V2ArbEngine", skip_from_py_object)]
pub struct PyV2ArbEngine {
    /// Shared engine state — Arc allows the pump to hold a reference too
    engine: Arc<parking_lot::Mutex<V2BlockEngine>>,
    /// Shutdown flag for the pump
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    /// Handle for the pump task (None until start() is called)
    pump_handle: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[pymethods]
impl PyV2ArbEngine {
    #[new]
    fn new() -> Self {
        Self {
            engine: Arc::new(parking_lot::Mutex::new(V2BlockEngine::new())),
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pump_handle: parking_lot::Mutex::new(None),
        }
    }

    /// Register a pool by contract address.
    /// Returns the forward pool_id. The reverse pool_id is forward_id + 1.
    #[pyo3(signature = (address, reserve0, reserve1, gamma_numer, fee_denom))]
    fn register_pool(
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

        Ok(self.engine.lock().register_pool(addr, r0, r1, gamma_numer, fee_denom))
    }

    /// Register an arbitrage path by pool IDs. Returns the path ID.
    #[pyo3(signature = (pool_ids))]
    fn register_path(&self, pool_ids: Vec<u64>) -> PyResult<u64> {
        if pool_ids.len() < 2 {
            let msg = format!("Need at least 2 pool IDs, got {}", pool_ids.len());
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }
        Ok(self.engine.lock().register_path(pool_ids))
    }

    /// Start the engine pump with the given RPC URL.
    /// Spawns the V2EnginePump on the Tokio runtime.
    /// The pump subscribes to block headers and drives process_block() automatically.
    #[pyo3(signature = (rpc_url))]
    fn start(&self, rpc_url: String) -> PyResult<()> {
        // Don't start twice
        if self.shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            let msg = "Engine already running";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        // Reset shutdown flag (in case stop() was called before)
        self.shutdown.store(false, std::sync::atomic::Ordering::Relaxed);

        // Spawn the pump
        let handle = crate::optimizers::v2_engine_pump::V2EnginePump::spawn(
            rpc_url,
            Arc::clone(&self.engine),
            &self.shutdown,
        ).map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        *self.pump_handle.lock() = Some(handle);

        Ok(())
    }

    /// Stop the engine pump.
    fn stop(&self) {
        self.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);

        // Abort the pump task
        let handle = self.pump_handle.lock().take();
        if let Some(handle) = handle {
            handle.abort();
        }
    }

    /// Number of registered pools.
    fn pool_count(&self) -> usize {
        self.engine.lock().pool_count()
    }

    /// Number of registered paths.
    fn path_count(&self) -> usize {
        self.engine.lock().path_count()
    }

    /// Process Sync events synchronously (for testing without a subscription).
    ///
    /// Each entry is (address_str, reserve0, reserve1) where reserve0/reserve1
    /// are Python ints. The engine applies these as Sync updates, rebuilds
    /// paths, solves all, and stores results.
    #[pyo3(signature = (sync_updates, block_number))]
    fn process_logs(&self, sync_updates: &Bound<'_, PyList>, block_number: u64) -> PyResult<()> {
        let mut rust_updates: Vec<(Address, U256, U256)> = Vec::with_capacity(sync_updates.len());

        for item in sync_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 3 {
                let msg = format!("Expected 3-tuple (address, reserve0, reserve1), got {} elements", tuple.len());
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let addr_obj = tuple.get_item(0)?;
            let addr_str: &str = addr_obj.extract()?;
            let addr: Address = addr_str.parse().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
            })?;
            let r0 = crate::alloy_py::extract_python_u256(&tuple.get_item(1)?)?;
            let r1 = crate::alloy_py::extract_python_u256(&tuple.get_item(2)?)?;
            rust_updates.push((addr, r0, r1));
        }

        self.engine.lock().process_sync_updates(&rust_updates, block_number);
        Ok(())
    }

    /// Read the last solved results and block number.
    /// Returns (results, block_number) where results is a flat list:
    /// [path_id_0, optimal_input_0, profit_0, path_id_1, ...]
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    fn usdc(amount: u64) -> U256 {
        U256::from(amount) * U256::from(10u64).pow(U256::from(6))
    }

    fn weth(amount: u64) -> U256 {
        U256::from(amount) * U256::from(10u64).pow(U256::from(18))
    }

    const GAMMA_03: u64 = 997;
    const FEE_DENOM_03: u64 = 1000;

    fn make_sync_log(
        pool_address: Address,
        reserve0: U256,
        reserve1: U256,
    ) -> Log {
        let data = {
            let r0_bytes = reserve0.to_be_bytes::<32>();
            let r1_bytes = reserve1.to_be_bytes::<32>();
            let mut data = Vec::with_capacity(64);
            data.extend_from_slice(&r0_bytes);
            data.extend_from_slice(&r1_bytes);
            data
        };

        let inner = alloy::primitives::Log::new_unchecked(
            pool_address,
            vec![crate::optimizers::v2_sync_decoder::V2_SYNC_TOPIC],
            alloy::primitives::Bytes::from(data),
        );
        Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        }
    }

    #[test]
    fn register_pool_creates_both_orientations() {
        let mut engine = V2BlockEngine::new();
        let addr = Address::ZERO;
        let fwd_id = engine.register_pool(addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);

        // Forward and reverse pool IDs
        assert_eq!(fwd_id, 1);
        let rev_id = fwd_id + 1;
        assert_eq!(rev_id, 2);

        // Both entries exist in pools
        assert!(engine.pools.contains_key(&fwd_id));
        assert!(engine.pools.contains_key(&rev_id));

        // Forward: reserve0 → reserve1
        let fwd_state = engine.pools.get(&fwd_id).unwrap();
        assert_eq!(fwd_state.reserve_in, usdc(1_500_000));
        assert_eq!(fwd_state.reserve_out, weth(800));

        // Reverse: reserve1 → reserve0
        let rev_state = engine.pools.get(&rev_id).unwrap();
        assert_eq!(rev_state.reserve_in, weth(800));
        assert_eq!(rev_state.reserve_out, usdc(1_500_000));
    }

    #[test]
    fn register_pool_stores_address_mapping() {
        let mut engine = V2BlockEngine::new();
        let addr = Address::ZERO;
        let fwd_id = engine.register_pool(addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);

        let mapping = engine.pool_addresses.get(&addr).unwrap();
        assert_eq!(*mapping, (fwd_id, fwd_id + 1));
    }

    #[test]
    fn apply_sync_updates_both_orientations() {
        let mut engine = V2BlockEngine::new();
        let addr = Address::ZERO;
        let fwd_id = engine.register_pool(addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);

        // Apply a Sync event with new reserves
        engine.apply_sync(addr, usdc(2_000_000), weth(1000));

        // Forward updated
        let fwd = engine.pools.get(&fwd_id).unwrap();
        assert_eq!(fwd.reserve_in, usdc(2_000_000));
        assert_eq!(fwd.reserve_out, weth(1000));

        // Reverse updated
        let rev = engine.pools.get(&(fwd_id + 1)).unwrap();
        assert_eq!(rev.reserve_in, weth(1000));
        assert_eq!(rev.reserve_out, usdc(2_000_000));
    }

    #[test]
    fn process_block_decodes_sync_events() {
        let mut engine = V2BlockEngine::new();
        let addr = Address::ZERO;
        let fwd_id = engine.register_pool(addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);

        // Register a second pool and a path
        let addr1 = Address::from([1u8; 20]);
        let fwd_id1 = engine.register_pool(addr1, weth(1000), usdc(2_000_000), GAMMA_03, FEE_DENOM_03);
        let path_id = engine.register_path(vec![fwd_id, fwd_id1]);

        // Process a block with a Sync event that creates an arbitrage opportunity:
        // Pool 0: 1.5M USDC / 800 WETH → Pool 1: 1000 WETH / 2M USDC
        // Pool 0 price: 1 WETH = 1875 USDC, Pool 1 price: 1 WETH = 2000 USDC
        // → buy WETH on pool 0, sell on pool 1
        let logs = vec![make_sync_log(addr, usdc(1_500_000), weth(800))];
        engine.process_block(&logs, 42);

        // Results should be populated
        let (results, block_num) = engine.latest_results();
        assert_eq!(block_num, 42);
        // The path should be profitable with these reserves
        assert!(!results.is_empty(), "expected profitable path but got no results");
        assert_eq!(results[0].0, path_id);
    }

    #[test]
    fn process_block_ignores_unregistered_pools() {
        let mut engine = V2BlockEngine::new();
        let addr = Address::ZERO;
        engine.register_pool(addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);

        // Sync for an unregistered address
        let unregistered = Address::from([0xaa; 20]);
        let logs = vec![make_sync_log(unregistered, usdc(999), weth(999))];
        engine.process_block(&logs, 1);

        // Original pool should be unchanged
        let fwd = engine.pools.get(&1u64).unwrap();
        assert_eq!(fwd.reserve_in, usdc(1_500_000));
        assert_eq!(fwd.reserve_out, weth(800));
    }

    #[test]
    fn process_block_last_sync_wins() {
        let mut engine = V2BlockEngine::new();
        let addr = Address::ZERO;
        let fwd_id = engine.register_pool(addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);

        // Two Sync events for the same pool in one block
        let logs = vec![
            make_sync_log(addr, usdc(1_600_000), weth(850)),
            make_sync_log(addr, usdc(2_000_000), weth(1000)),
        ];
        engine.process_block(&logs, 1);

        // Last Sync wins
        let fwd = engine.pools.get(&fwd_id).unwrap();
        assert_eq!(fwd.reserve_in, usdc(2_000_000));
        assert_eq!(fwd.reserve_out, weth(1000));
    }

    #[test]
    fn latest_results_returns_last_solved() {
        let engine = V2BlockEngine::new();

        // Before any solve — empty results
        let (results, block_num) = engine.latest_results();
        assert!(results.is_empty());
        assert_eq!(block_num, 0);
    }

    #[test]
    fn register_pool_after_start_succeeds() {
        let mut engine = V2BlockEngine::new();
        engine.register_pool(Address::ZERO, U256::ONE, U256::ONE, 997, 1000);
        // Registration is always-on; this should not panic
        engine.register_pool(Address::from([1u8; 20]), U256::ONE, U256::ONE, 997, 1000);
    }

    #[test]
    fn register_path_after_start_succeeds() {
        let mut engine = V2BlockEngine::new();
        let fwd = engine.register_pool(Address::ZERO, U256::ONE, U256::ONE, 997, 1000);
        let fwd2 = engine.register_pool(Address::from([1u8; 20]), U256::ONE, U256::ONE, 997, 1000);
        engine.register_path(vec![fwd, fwd2]);
        // Registration is always-on; this should not panic
        engine.register_path(vec![fwd, fwd2]);
    }
}

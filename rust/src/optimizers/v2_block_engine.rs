//! V2 Block Engine — V2 pool state owned by [`crate::optimizers::uniswap_engine::UniswapEngine`].
//!
//! This struct owns V2 pool state (reserves, fees, address mapping) for the
//! unified engine. It no longer owns a path/solve subsystem: `UniswapEngine`
//! resolves paths against this state and solves them through the gen-3
//! integer-exact Möbius solver directly (`exact_mobius_solve`), never through
//! a stand-alone per-block solve on this engine. The previous f64-based
//! stand-alone solve (`solve_all` / `resolve_path` / `process_block`) has been
//! retired — see `rust/CONTEXT.md` ruling "f64 vs U512 Möbius solver stack".
//!
//! All methods here are pure state accessors/mutators; solving lives in
//! [`crate::optimizers::uniswap_engine::solver_dispatch`].

use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};

use crate::optimizers::affected_keys::AffectedKeys;
use crate::optimizers::mobius_int::IntHopState;

/// V2 pool state owner. Held as sub-state by [`UniswapEngine`].
///
/// Each registered pool creates two [`IntHopState`] entries — forward
/// (reserve0 → reserve1) and reverse (reserve1 → reserve0) — the dual-orientation
/// registration that lets paths reference either direction by pool ID.
pub struct V2BlockEngine {
    /// Pool state: `pool_id` → `IntHopState` (both forward and reverse orientations).
    pools: HashMap<u64, IntHopState>,
    /// Pool contract address → (`forward_pool_id`, `reverse_pool_id`).
    pool_addresses: HashMap<Address, (u64, u64)>,
    /// Auto-incrementing pool ID (`forward_id`; `reverse_id` = `forward_id` + 1).
    next_pool_id: u64,
}

impl V2BlockEngine {
    /// Create a new engine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pools: HashMap::new(),
            pool_addresses: HashMap::new(),
            next_pool_id: 1,
        }
    }

    /// Register a pool by contract address.
    ///
    /// Creates entries in both reserve orientations:
    /// - Forward (`pool_id)`: reserve0 → reserve1
    /// - Reverse (`pool_id` + 1): reserve1 → reserve0
    ///
    /// Returns the forward `pool_id`. The reverse `pool_id` is `forward_id + 1`.
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

    /// Update reserves for a registered pool from a Sync event.
    ///
    /// Sync carries absolute reserves — last-event-wins per pool per block.
    /// Both orientations are updated from the same event.
    ///
    /// Returns the affected pool keys (forward + reverse orientations), or
    /// an empty [`AffectedKeys`] if the pool is not registered.
    ///
    /// # Panics
    ///
    /// Panics if the address is registered but the forward orientation is
    /// missing from `self.pools` (indicates an internal inconsistency).
    pub fn apply_sync(&mut self, pool_address: Address, reserve0: U256, reserve1: U256) -> AffectedKeys {
        let Some(&(forward_id, reverse_id)) = self.pool_addresses.get(&pool_address) else {
            return AffectedKeys::empty(); // Not a registered pool — skip
        };

        // Get gamma_numer/fee_denom from existing forward entry
        let forward_state = self.pools.get(&forward_id)
            .expect("forward pool entry must exist when address is registered");
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

        AffectedKeys::pair(forward_id, reverse_id)
    }

    /// Apply Sync updates and return the set of pool keys that changed.
    /// Does NOT rebuild paths or solve — caller handles that.
    pub fn apply_sync_updates(&mut self, updates: &[(Address, U256, U256)]) -> HashSet<u64> {
        let mut affected = HashSet::new();
        for &(addr, r0, r1) in updates {
            for key in self.apply_sync(addr, r0, r1).iter() {
                affected.insert(key);
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

    /// Return the list of registered pool addresses.
    #[must_use]
    pub fn registered_addresses(&self) -> Vec<Address> {
        self.pool_addresses.keys().copied().collect()
    }

    /// Number of registered pools (counting forward orientations only).
    #[must_use]
    pub fn pool_count(&self) -> usize {
        self.pool_addresses.len()
    }

    /// Access the pool address → (`forward_id`, `reverse_id`) map.
    #[must_use]
    pub const fn pool_addresses(&self) -> &HashMap<Address, (u64, u64)> {
        &self.pool_addresses
    }

    /// Get a reference to a pool's `IntHopState` by pool ID.
    #[must_use]
    pub fn get_pool(&self, pool_id: u64) -> Option<&IntHopState> {
        self.pools.get(&pool_id)
    }
}

impl Default for V2BlockEngine {
    fn default() -> Self {
        Self::new()
    }
}

//! Balance-vector structural family — `impl BotState` orchestration (`Curve` + `Balancer`).
//!
//! Carved out of `bot_core/mod.rs` (the `BotState` god-file). This module owns the balance-vector
//! capability's LIVE-STATE surfaces — `Curve` and `Balancer` (weighted + stable) registration,
//! the unified balance-apply row, and the identity/state getters. State ownership is NOT family
//! behavior (GLOSSARY "concentrated liquidity orchestration capability" pattern): the registration entries and the
//! family-neutral apply/get rows stay here (ADR-073 D3 keeps the named seams), while the family
//! behavior — the swap reads, the resolve projections, and the get/calc math — folds through the
//! family read modules' intent doors (`degenbot_pools::families::{curve_stableswap,
//! balancer_weighted, balancer_stable}`, ADR-073 D1). The family state types live in
//! `degenbot-pools` (I/O-free, ADR-001).
//!
//! Child-module impl blocks reach `BotState`'s private fields directly (same
//! pattern as `divergence_probe.rs`); the public surface is unchanged because
//! these are inherent methods on `BotState`, and `bot_core/mod.rs` remains the
//! assembly + re-export hub.

use alloy::primitives::U256;

use degenbot_math::curve::calculate_dy_underlying;
use degenbot_pools::balancer_stable_state::{
    BalancerStablePoolState, RegisterBalancerStablePoolParams,
};
use degenbot_pools::balancer_weighted_state::{
    BalancerWeightedPoolState, RegisterBalancerWeightedPoolParams,
};
use degenbot_pools::curve_state::{CurvePoolState, RegisterCurvePoolParams};
use degenbot_pools::families::curve_stableswap as curve_read;
use degenbot_pools::CurveInputsError;

use super::{
    check_metapool_liquidity, resolve_dy_inputs, BotCurveBasePoolPort, BotState, PoolEntry,
};

impl BotState {
    /// Register a Curve `StableSwap` pool by contract address.
    ///
    /// Curve is the third `PoolEntry` family. Carries
    /// immutable config (tokens, A, fee, variant strategy enums, base-pool
    /// reference) + the registration-time mutable state (`balances`,
    /// `update_block`). Seeds the reorg journal with a genesis anchor (mirror
    /// of V2's discipline) so the balance-vector trait dispatcher
    /// (`restore_balance_vector_before_block`, ADR-016) can land on the
    /// registration state.
    ///
    /// Returns the auto-assigned pool ID.
    ///
    /// # Panics
    ///
    /// Panics if the pool address is already registered. The caller pre-checks
    /// hook / dynamic-fee rejection at the Python seam for V4; Curve has no
    /// analogous admission floor in this sub-slice (the stableswap math stays
    /// Python-side at calc time, so there's no Rust correctness floor to
    /// enforce yet).
    pub fn register_curve_pool(&mut self, params: &RegisterCurvePoolParams) -> u64 {
        assert!(
            !self.registry.pool_addresses.contains_key(&params.address),
            "pool already registered: {}",
            params.address
        );

        assert!(
            params.balances.len() == params.tokens.len()
                && params.balances.len() == params.rate_multipliers.len(),
            "Curve params mismatch: tokens={}, balances={}, rate_multipliers={} (must all be N)",
            params.tokens.len(),
            params.balances.len(),
            params.rate_multipliers.len(),
        );

        let pool_id = self.registry.next_pool_id;
        self.registry.next_pool_id += 1;

        let (identity, state) =
            CurvePoolState::from_params(params.clone(), self.registry.journal_depth);
        self.registry
            .pools
            .insert(pool_id, PoolEntry::Curve(Box::new((identity, state))));
        self.registry.pool_addresses.insert(params.address, pool_id);

        pool_id
    }

    /// Apply a Curve `external_update` (new balances from an `Exchange` event)
    /// by `pool_id` — the `PyLiquidityPool.apply_curve_balance_update` backing.
    ///
    /// Journals the prior balances (genesis-anchor V2-style discipline), then
    /// lands the new balances + `update_block`. Returns the affected `pool_id`,
    /// or `None` if not registered / not a Curve pool (silent no-op — don't
    /// corrupt a V2/V3/V4 pool).
    ///
    /// # Panics
    ///
    /// Panics if `balances.len()` doesn't match the registered pool's coin
    /// count — a wiring/programming error (the builder always passes an
    /// `Exchange`-decoded balance tuple of the right arity).
    #[must_use]
    /// Apply a balance-vector update (a Curve `Exchange` event or a Balancer
    /// Vault `PoolBalanceChanged` event) keyed by the handle's `pool_id`,
    /// dispatching through `BalanceVectorPoolState::apply_balance_update`
    /// (ADR-017 D1 — replaces the three per-family
    /// `apply_curve_balance_update_by_pool_id` /
    /// `apply_balancer_weighted_balance_update_by_pool_id` /
    /// `apply_balancer_stable_balance_update_by_pool_id` methods, whose bodies
    /// were byte-identical modulo the arity `assert!` message).
    ///
    /// Returns `Some(pool_id)` if the pool is a balance-vector family
    /// (Curve / `BalancerWeighted` / `BalancerStable`); `None` otherwise (silent
    /// no-op — mirrors the per-family silent-no-op contract on a non-matching
    /// family, e.g. a V2 `pool_id`).
    pub fn apply_balance_update_by_pool_id(
        &mut self,
        pool_id: u64,
        balances: Vec<U256>,
        block_number: u64,
    ) -> Option<u64> {
        let entry = self.registry.pools.get_mut(&pool_id)?;
        entry
            .as_balance_vector_mut()?
            .apply_balance_update(balances, block_number);
        Some(pool_id)
    }

    /// Rust-owned stableswap `get_dy` — the family-neutral calc row. The
    /// counterpart of the companion's `get_dy`: resolves the pool entry, then
    /// the family read module's [`curve_read::get_dy`] door runs the
    /// liquidity check, the dy-input resolution, and the pure swap math over
    /// the slice (no Python provider / cache / calculator on the path).
    ///
    /// `i`/`j` are coin indices, `dx` the input amount. `override_balances`
    /// swaps the balance source (the companion `override_state.balances`).
    ///
    /// # Errors
    ///
    /// Returns [`CurveInputsError::UnknownPool`] for a non-Curve / unknown
    /// `pool_id`; otherwise the family door's errors propagate unchanged.
    pub fn get_dy(
        &self,
        pool_id: u64,
        i: usize,
        j: usize,
        dx: U256,
        block_number: u64,
        override_balances: Option<&[U256]>,
    ) -> Result<U256, CurveInputsError> {
        let (identity, state) = self
            .registry
            .pools
            .get(&pool_id)
            .and_then(PoolEntry::curve)
            .ok_or(CurveInputsError::UnknownPool(pool_id))?;
        curve_read::get_dy(state, identity, i, j, dx, block_number, override_balances)
    }

    /// Rust-owned metapool `get_dy_underlying(i, j, dx)`. The registry + base
    /// pool resolution stay at this orchestration row (the base-pool port
    /// reaches back into this same `BotState`); the snapshot resolution
    /// delegates to [`degenbot_pools::resolve_dy_inputs`] and the math to
    /// [`degenbot_math::curve::calculate_dy_underlying`] — the Rust twin of
    /// the Python `_LazyBasePool` delegate (retires that go-between for the
    /// swap path).
    ///
    /// # Errors
    ///
    /// Returns [`CurveInputsError::UnknownPool`] if the pool or its base pool
    /// isn't a registered Curve pool, [`CurveInputsError::NotMetapool`] for a
    /// plain pool, otherwise the orchestration / swap-math error.
    pub fn get_dy_underlying(
        &self,
        pool_id: u64,
        i: usize,
        j: usize,
        dx: U256,
        block_number: u64,
        override_balances: Option<&[U256]>,
    ) -> Result<U256, CurveInputsError> {
        let (identity, state) = self
            .registry
            .pools
            .get(&pool_id)
            .and_then(PoolEntry::curve)
            .ok_or(CurveInputsError::UnknownPool(pool_id))?;
        let base_addr = identity.base_pool.ok_or(CurveInputsError::NotMetapool)?;
        let base_id = self
            .pool_id_by_address(&base_addr)
            .ok_or(CurveInputsError::UnknownPool(u64::MAX))?;
        // The contract's zero-balance refusal, evaluated where the balance
        // vectors are read (core-side; the companion's pre-validation is gone).
        let base_balances: &[U256] = self
            .registry
            .pools
            .get(&base_id)
            .and_then(PoolEntry::curve)
            .map_or(&[], |(_, base_state)| &base_state.balances[..]);
        check_metapool_liquidity(&state.balances, base_balances)?;
        let provider = state.data_provider.as_deref();
        let inputs = resolve_dy_inputs(
            identity,
            &state.balances,
            provider,
            block_number,
            override_balances,
        )?;
        let port = BotCurveBasePoolPort {
            state: self,
            base_id,
        };
        calculate_dy_underlying(i, j, dx, &inputs, &port).map_err(CurveInputsError::Swap)
    }

    /// Rust twin of the companion `calc_token_amount(amounts, deposit)` — the
    /// family-neutral calc row. The registry lookup stays here; the D-before /
    /// D-after / LP-supply scaling math is the family read module's
    /// [`curve_read::calc_token_amount`] door. Base-pool delegation op for
    /// metapool `get_dy_underlying`; also a standalone calc entry.
    ///
    /// # Errors
    ///
    /// Returns [`CurveInputsError::UnknownPool`] for a non-Curve / unknown
    /// `pool_id`; otherwise the family door's errors propagate unchanged.
    pub fn calc_token_amount(
        &self,
        pool_id: u64,
        amounts: &[U256],
        deposit: bool,
        block_number: u64,
    ) -> Result<U256, CurveInputsError> {
        let (identity, state) = self
            .registry
            .pools
            .get(&pool_id)
            .and_then(PoolEntry::curve)
            .ok_or(CurveInputsError::UnknownPool(pool_id))?;
        curve_read::calc_token_amount(state, identity, amounts, deposit, block_number)
    }

    /// Rust twin of the companion `calc_withdraw_one_coin(token_amount, i)` —
    /// the family-neutral calc row. The registry lookup stays here; the
    /// single-coin output math is the family read module's
    /// [`curve_read::calc_withdraw_one_coin`] door. Base-pool delegation op
    /// for metapool `get_dy_underlying`; also a standalone calc entry.
    ///
    /// # Errors
    ///
    /// Returns [`CurveInputsError::UnknownPool`] for a non-Curve / unknown
    /// `pool_id`; otherwise the family door's errors propagate unchanged.
    pub fn calc_withdraw_one_coin(
        &self,
        pool_id: u64,
        token_amount: U256,
        i: usize,
        block_number: u64,
    ) -> Result<U256, CurveInputsError> {
        let (identity, state) = self
            .registry
            .pools
            .get(&pool_id)
            .and_then(PoolEntry::curve)
            .ok_or(CurveInputsError::UnknownPool(pool_id))?;
        curve_read::calc_withdraw_one_coin(state, identity, token_amount, i, block_number)
    }

    /// Register a Balancer V2 weighted pool. The pool's immutable config
    /// (`pool_id`, vault, tokens, weights, `scaling_factors`, `swap_fee`,
    /// `pow_version`) + the registration `balances`/`update_block` are stored
    /// in a `BalancerWeightedPoolState` and seeded with a genesis reorg
    /// journal delta. The Python `BalancerV2Pool` companion will
    /// be built over a `PyLiquidityPool` handle that reads back through
    /// [`Self::get_balancer_weighted_pool`].
    ///
    /// # Panics
    ///
    /// Panics if the pool's address is already registered, or if
    /// `balances.len()` doesn't match `tokens.len()` / `weights.len()` /
    /// `scaling_factors.len()` (a builder wiring error — the builder always
    /// passes N-token tuples of consistent arity).
    pub fn register_balancer_weighted_pool(
        &mut self,
        params: &RegisterBalancerWeightedPoolParams,
    ) -> u64 {
        assert!(
            !self.registry.pool_addresses.contains_key(&params.address),
            "pool already registered: {}",
            params.address
        );

        assert!(
            params.balances.len() == params.tokens.len()
                && params.balances.len() == params.weights.len()
                && params.balances.len() == params.scaling_factors.len(),
            "Balancer weighted params mismatch: tokens={}, balances={}, weights={}, scaling_factors={} (must all be N)",
            params.tokens.len(),
            params.balances.len(),
            params.weights.len(),
            params.scaling_factors.len(),
        );

        let pool_id = self.registry.next_pool_id;
        self.registry.next_pool_id += 1;

        let (identity, state) =
            BalancerWeightedPoolState::from_params(params.clone(), self.registry.journal_depth);
        self.registry.pools.insert(
            pool_id,
            PoolEntry::BalancerWeighted(Box::new((identity, state))),
        );
        self.registry.pool_addresses.insert(params.address, pool_id);

        pool_id
    }

    /// Register a Balancer V2 stable pool. The pool's immutable config
    /// (`pool_id`, vault, tokens, amp, `scaling_factors`, `swap_fee`,
    /// `bpt_idx`, `invariant_version`) + the registration `balances`/
    /// `update_block` are stored in a `BalancerStablePoolState` and seeded
    /// with a genesis reorg journal delta. The Python `BalancerV2StablePool`
    /// companion will be built over a `PyLiquidityPool` handle
    /// that reads back through [`Self::get_balancer_stable_pool`].
    ///
    /// # Panics
    ///
    /// Panics if the pool's address is already registered, if
    /// `balances.len()` doesn't match `tokens.len()` / `scaling_factors.len()`,
    /// or if `bpt_idx` is `Some(i)` with `i >= tokens.len()` (a builder
    /// wiring error — the builder always passes N-token tuples of
    /// consistent arity and resolves `bpt_idx` via `detect_bpt_index`).
    pub fn register_balancer_stable_pool(
        &mut self,
        params: &RegisterBalancerStablePoolParams,
    ) -> u64 {
        assert!(
            !self.registry.pool_addresses.contains_key(&params.address),
            "pool already registered: {}",
            params.address
        );

        assert!(
            params.balances.len() == params.tokens.len()
                && params.balances.len() == params.scaling_factors.len(),
            "Balancer stable params mismatch: tokens={}, balances={}, scaling_factors={} (must all be N)",
            params.tokens.len(),
            params.balances.len(),
            params.scaling_factors.len(),
        );

        if let Some(idx) = params.bpt_idx {
            assert!(
                idx < params.tokens.len(),
                "Balancer stable bpt_idx {} >= tokens.len() {} (BPT must be in-token-list)",
                idx,
                params.tokens.len(),
            );
        }

        let pool_id = self.registry.next_pool_id;
        self.registry.next_pool_id += 1;

        let (identity, state) =
            BalancerStablePoolState::from_params(params.clone(), self.registry.journal_depth);
        self.registry.pools.insert(
            pool_id,
            PoolEntry::BalancerStable(Box::new((identity, state))),
        );
        self.registry.pool_addresses.insert(params.address, pool_id);

        pool_id
    }
}

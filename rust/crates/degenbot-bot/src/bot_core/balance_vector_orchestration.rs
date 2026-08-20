//! Balance-vector structural family — `impl BotState` orchestration (`Curve` + `Balancer`).
//!
//! Carved out of `bot_core/mod.rs` (the `BotState` god-file). This module owns the balance-vector
//! `BotState` method set — `Curve` and `Balancer` (weighted + stable) registration,
//! balance-apply, calc, and identity/state getters. Pure `impl BotState`
//! orchestration: the family state types live in `degenbot-pools` (I/O-free,
//! ADR-001).
//!
//! Child-module impl blocks reach `BotState`'s private fields directly (same
//! pattern as `divergence_probe.rs`); the public surface is unchanged because
//! these are inherent methods on `BotState`, and `bot_core/mod.rs` remains the
//! assembly + re-export hub.

use alloy::primitives::U256;

use degenbot_math::curve::{
    calculate_dy, calculate_dy_underlying, resolve_ramping_a, stableswap_get_d, stableswap_get_y_d,
    ARampingParams, CurveSwapError, DVariant, YDVariant,
};
use degenbot_pools::balancer_stable_state::{
    BalancerStablePoolIdentity, BalancerStablePoolState, RegisterBalancerStablePoolParams,
};
use degenbot_pools::balancer_weighted_state::{
    BalancerWeightedPoolIdentity, BalancerWeightedPoolState, RegisterBalancerWeightedPoolParams,
};
use degenbot_pools::curve_state::{CurvePoolIdentity, CurvePoolState, RegisterCurvePoolParams};

use super::{resolve_dy_inputs, BotCurveBasePoolPort, BotState, CurveInputsError, PoolEntry};

/// The Curve swap-fee denominator (1e10).
const CURVE_FEE_DENOMINATOR: U256 = U256::from_limbs([10_000_000_000, 0, 0, 0]);
/// The Curve `PRECISION` (18 decimals).
const CURVE_PRECISION: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);

/// Compute the base pool's `xp = rate * balance // PRECISION` from its
/// immutable `rate_multipliers` + a balance vector (twin of the companion
/// `_xp`).
fn curve_base_xp(
    identity: &degenbot_pools::curve_state::CurvePoolIdentity,
    balances: &[U256],
) -> Result<Vec<U256>, CurveInputsError> {
    if identity.rate_multipliers.len() != balances.len() {
        return Err(CurveInputsError::LengthMismatch("rates/balances"));
    }
    Ok(identity
        .rate_multipliers
        .iter()
        .zip(balances)
        .map(|(r, b)| *r * *b / CURVE_PRECISION)
        .collect())
}

/// Build the `ARampingParams` the `resolve_ramping_a` twin reads, from a
/// pool's immutable A-ramping identity fields.
fn curve_ramping_params(
    identity: &degenbot_pools::curve_state::CurvePoolIdentity,
) -> ARampingParams {
    ARampingParams {
        a_coefficient: identity.a_coefficient,
        initial_a_coefficient: identity.initial_a_coefficient,
        future_a_coefficient: identity.future_a_coefficient,
        initial_a_coefficient_time: identity.initial_a_coefficient_time,
        future_a_coefficient_time: identity.future_a_coefficient_time,
        create_timestamp: identity.create_timestamp,
        a_precision: u32::try_from(identity.a_precision).unwrap_or(0),
    }
}

/// Resolve the block timestamp for A-ramping. Only a pool that actually ramps
/// (has end-time set) needs a provider block-timestamp fetch; plain pools
/// return `0` and `resolve_ramping_a` ignores it.
fn curve_block_timestamp(
    identity: &degenbot_pools::curve_state::CurvePoolIdentity,
    provider: Option<&dyn degenbot_pools::curve_data_provider::CurveDataProvider>,
    block_number: u64,
) -> Result<u64, CurveInputsError> {
    if identity.future_a_coefficient_time.is_none() {
        return Ok(0);
    }
    let p = provider.ok_or(CurveInputsError::NoProvider("block_timestamp"))?;
    p.block_timestamp(block_number)
        .map_err(CurveInputsError::Provider)
}

/// Fetch a pool's LP-token total supply via its stored provider (`lp_token`
/// falling back to the pool token when the LP IS the pool token).
fn curve_total_supply(
    identity: &degenbot_pools::curve_state::CurvePoolIdentity,
    provider: Option<&dyn degenbot_pools::curve_data_provider::CurveDataProvider>,
    block_number: u64,
) -> Result<U256, CurveInputsError> {
    let p = provider.ok_or(CurveInputsError::NoProvider("token_total_supply"))?;
    let lp = identity.lp_token.unwrap_or(identity.tokens[0]);
    p.token_total_supply(lp, block_number)
        .map_err(CurveInputsError::Provider)
}

impl BotState {
    /// Register a Curve `StableSwap` pool by contract address.
    ///
    /// ADR-005 slice 11a (state port) — the third `PoolEntry` family. Carries
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
            !self.pool_addresses.contains_key(&params.address),
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

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        let (identity, state) = CurvePoolState::from_params(params.clone(), self.journal_depth);
        self.pools
            .insert(pool_id, PoolEntry::Curve(identity, state));
        self.pool_addresses.insert(params.address, pool_id);

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
        let entry = self.pools.get_mut(&pool_id)?;
        entry
            .as_balance_vector_mut()?
            .apply_balance_update(balances, block_number);
        Some(pool_id)
    }

    /// Read a registered Curve pool's state by `pool_id`.
    ///
    /// The Python companion (slice 11b) reads `balances` / `update_block`
    /// through this accessor via `PyLiquidityPool.balances` getter. Returns
    /// `None` for non-Curve pools (silent no-op).
    #[must_use]
    pub fn get_curve_pool(&self, pool_id: u64) -> Option<&CurvePoolState> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::curve)
            .map(|(_, state)| state)
    }

    /// Look up a Curve pool's immutable registration identity (address,
    /// tokens, fee, `admin_fee`, `rate_multipliers`, variant enums, `base_pool`).
    /// Returns `None` if the pool is not registered or isn't a Curve pool.
    #[must_use]
    pub fn get_curve_identity(&self, pool_id: u64) -> Option<&CurvePoolIdentity> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::curve)
            .map(|(identity, _)| identity)
    }

    /// Rust-owned Curve stableswap `get_dy(i, j, dx)` (task `45QBUG`, epic
    /// `TV72EG`). The counterpart of the companion's `get_dy` — resolves the
    /// dy-calculation snapshot from the pool's identity + balances + stored
    /// provider via [`degenbot_pools::resolve_dy_inputs`], then runs the pure
    /// [`degenbot_math::curve::calculate_dy`]. No Python provider / cache /
    /// calculator is on the path.
    ///
    /// `i`/`j` are coin indices, `dx` the input amount. `override_balances`
    /// swaps the balance source (the companion `override_state.balances`).
    ///
    /// # Errors
    ///
    /// Returns [`CurveInputsError::UnknownPool`] for a non-Curve / unknown
    /// `pool_id`; otherwise the orchestration or swap-math errors propagate.
    pub fn curve_get_dy(
        &self,
        pool_id: u64,
        i: usize,
        j: usize,
        dx: U256,
        block_number: u64,
        override_balances: Option<&[U256]>,
    ) -> Result<U256, CurveInputsError> {
        let (identity, state) = self
            .pools
            .get(&pool_id)
            .and_then(PoolEntry::curve)
            .ok_or(CurveInputsError::UnknownPool(pool_id))?;
        let provider = state.data_provider.as_deref();
        let inputs = resolve_dy_inputs(
            identity,
            &state.balances,
            provider,
            block_number,
            override_balances,
        )?;
        calculate_dy(i, j, dx, &inputs).map_err(CurveInputsError::Swap)
    }

    /// Rust-owned Curve metapool `get_dy_underlying(i, j, dx)` (task
    /// `V5X2YP`, epic `TV72EG`). Resolves the metapool snapshot via
    /// [`degenbot_pools::resolve_dy_inputs`], then delegates the base-pool ops
    /// (`calc_token_amount` / `get_dy` / `calc_withdraw_one_coin`) through a
    /// [`BotCurveBasePoolPort`] over the registered base `CurvePoolState` in
    /// this same `BotState` — the Rust twin of the Python `_LazyBasePool`
    /// delegate (retires that go-between for the swap path).
    ///
    /// # Errors
    ///
    /// Returns [`CurveInputsError::UnknownPool`] if the pool or its base pool
    /// isn't a registered Curve pool, [`CurveInputsError::NotMetapool`] for a
    /// plain pool, otherwise the orchestration / swap-math error.
    pub fn curve_get_dy_underlying(
        &self,
        pool_id: u64,
        i: usize,
        j: usize,
        dx: U256,
        block_number: u64,
        override_balances: Option<&[U256]>,
    ) -> Result<U256, CurveInputsError> {
        let (identity, state) = self
            .pools
            .get(&pool_id)
            .and_then(PoolEntry::curve)
            .ok_or(CurveInputsError::UnknownPool(pool_id))?;
        let base_addr = identity.base_pool.ok_or(CurveInputsError::NotMetapool)?;
        let base_id = self
            .pool_id_by_address(&base_addr)
            .ok_or(CurveInputsError::UnknownPool(u64::MAX))?;
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

    /// Rust twin of the companion `calc_token_amount(amounts, deposit)` on a
    /// Curve pool (task `V5X2YP`). Base-pool delegation op for metapool
    /// `get_dy_underlying`; also a standalone calc entry. Computes `D` before
    /// and after the balance change and scales by the LP total supply
    /// (fetched via the stored provider).
    ///
    /// # Errors
    ///
    /// Returns [`CurveInputsError::UnknownPool`] / [`CurveInputsError::Swap`] /
    /// [`CurveInputsError::LengthMismatch`] / [`CurveInputsError::NoProvider`]
    /// (`"token_total_supply"`).
    pub fn curve_calc_token_amount(
        &self,
        pool_id: u64,
        amounts: &[U256],
        deposit: bool,
        block_number: u64,
    ) -> Result<U256, CurveInputsError> {
        let (identity, state) = self
            .pools
            .get(&pool_id)
            .and_then(PoolEntry::curve)
            .ok_or(CurveInputsError::UnknownPool(pool_id))?;
        let n = identity.n_coins();
        if amounts.len() != n {
            return Err(CurveInputsError::LengthMismatch("amounts/coins"));
        }
        let provider = state.data_provider.as_deref();
        let timestamp = curve_block_timestamp(identity, provider, block_number)?;
        let amp = resolve_ramping_a(curve_ramping_params(identity), timestamp)?;
        let a_precision = U256::from(identity.a_precision);
        let d_variant =
            DVariant::try_from_u8(identity.d_variant).ok_or(CurveSwapError::UnknownStyle(99))?;
        let n_u = U256::from(n);

        let d_0 = stableswap_get_d(
            &curve_base_xp(identity, &state.balances)?,
            amp,
            n_u,
            a_precision,
            d_variant,
        )?;

        let mut pool_balances = state.balances.clone();
        for (b, a) in pool_balances.iter_mut().zip(amounts) {
            *b = if deposit { *b + *a } else { *b - *a };
        }
        let d_1 = stableswap_get_d(
            &curve_base_xp(identity, &pool_balances)?,
            amp,
            n_u,
            a_precision,
            d_variant,
        )?;

        let token_amount = curve_total_supply(identity, provider, block_number)?;
        let diff = if deposit { d_1 - d_0 } else { d_0 - d_1 };
        Ok(diff * token_amount / d_0)
    }

    /// Rust twin of the companion `calc_withdraw_one_coin(token_amount, i)`
    /// (task `V5X2YP`). Base-pool delegation op for metapool
    /// `get_dy_underlying`; also a standalone calc entry. Returns the single
    /// coin-`i` output `dy` (the port only needs `dy`; the companion's extra
    /// tuple fields `dy_0 - dy` / `total_supply` aren't consumed).
    ///
    /// # Errors
    ///
    /// Returns [`CurveInputsError::UnknownPool`] / [`CurveInputsError::Swap`] /
    /// [`CurveInputsError::NoProvider`] (`"token_total_supply"`).
    pub fn curve_calc_withdraw_one_coin(
        &self,
        pool_id: u64,
        token_amount: U256,
        i: usize,
        block_number: u64,
    ) -> Result<U256, CurveInputsError> {
        let (identity, state) = self
            .pools
            .get(&pool_id)
            .and_then(PoolEntry::curve)
            .ok_or(CurveInputsError::UnknownPool(pool_id))?;
        let n = identity.n_coins();
        if i >= n {
            return Err(CurveInputsError::Swap(CurveSwapError::IndexOutOfBounds));
        }
        let provider = state.data_provider.as_deref();
        let timestamp = curve_block_timestamp(identity, provider, block_number)?;
        let amp = resolve_ramping_a(curve_ramping_params(identity), timestamp)?;
        let a_precision = U256::from(identity.a_precision);
        let d_variant =
            DVariant::try_from_u8(identity.d_variant).ok_or(CurveSwapError::UnknownStyle(99))?;
        let yd_variant =
            YDVariant::try_from_u8(identity.yd_variant).ok_or(CurveSwapError::UnknownStyle(99))?;
        let n_u = U256::from(n);
        let precisions = &identity.precision_multipliers;

        let xp = curve_base_xp(identity, &state.balances)?;
        let d_0 = stableswap_get_d(&xp, amp, n_u, a_precision, d_variant)?;
        let total_supply = curve_total_supply(identity, provider, block_number)?;
        let d_1 = d_0 - token_amount * d_0 / total_supply;
        let new_y = stableswap_get_y_d(amp, i, &xp, d_1, n_u, a_precision, yd_variant)?;
        let raw_dy_0 = (xp[i] - new_y) / precisions[i];

        let n_u64 = n as u64;
        let mut xp_reduced = xp.clone();
        let fee = identity.fee * n_u64 / (4 * (n_u64 - 1));
        let fee_u = U256::from(fee);
        for (j, x) in xp_reduced.iter_mut().enumerate() {
            let dx_expected = if j == i {
                xp[j] * d_1 / d_0 - new_y
            } else {
                xp[j] - xp[j] * d_1 / d_0
            };
            *x -= fee_u * dx_expected / CURVE_FEE_DENOMINATOR;
        }

        let dy = xp_reduced[i]
            - stableswap_get_y_d(amp, i, &xp_reduced, d_1, n_u, a_precision, yd_variant)?;
        let _ = raw_dy_0; // kept only to mirror the companion's tuple shape; not returned
        Ok((dy - U256::from(1u8)) / precisions[i])
    }

    /// Register a Balancer V2 weighted pool. The pool's immutable config
    /// (`pool_id`, vault, tokens, weights, `scaling_factors`, `swap_fee`,
    /// `pow_version`) + the registration `balances`/`update_block` are stored
    /// in a `BalancerWeightedPoolState` and seeded with a genesis reorg
    /// journal delta. The Python `BalancerV2Pool` companion (slice 12b) will
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
            !self.pool_addresses.contains_key(&params.address),
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

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        let (identity, state) =
            BalancerWeightedPoolState::from_params(params.clone(), self.journal_depth);
        self.pools
            .insert(pool_id, PoolEntry::BalancerWeighted(identity, state));
        self.pool_addresses.insert(params.address, pool_id);

        pool_id
    }

    /// Read a registered Balancer weighted pool's state by `pool_id`.
    ///
    /// The Python companion (slice 12b) reads `balances` / `update_block`
    /// through this accessor via `PyLiquidityPool` getters. Returns `None`
    /// for non-Balancer-weighted pools (silent no-op).
    #[must_use]
    pub fn get_balancer_weighted_pool(&self, pool_id: u64) -> Option<&BalancerWeightedPoolState> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::balancer_weighted)
            .map(|(_, state)| state)
    }

    /// Look up a Balancer weighted pool's immutable registration identity
    /// (address, vault, `pool_id`, tokens, weights, `scaling_factors`, `swap_fee`,
    /// `pow_version`). Returns `None` if not registered or not a weighted pool.
    #[must_use]
    pub fn get_balancer_weighted_identity(
        &self,
        pool_id: u64,
    ) -> Option<&BalancerWeightedPoolIdentity> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::balancer_weighted)
            .map(|(identity, _)| identity)
    }

    /// Register a Balancer V2 stable pool. The pool's immutable config
    /// (`pool_id`, vault, tokens, amp, `scaling_factors`, `swap_fee`,
    /// `bpt_idx`, `invariant_version`) + the registration `balances`/
    /// `update_block` are stored in a `BalancerStablePoolState` and seeded
    /// with a genesis reorg journal delta. The Python `BalancerV2StablePool`
    /// companion (slice 12d) will be built over a `PyLiquidityPool` handle
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
            !self.pool_addresses.contains_key(&params.address),
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

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        let (identity, state) =
            BalancerStablePoolState::from_params(params.clone(), self.journal_depth);
        self.pools
            .insert(pool_id, PoolEntry::BalancerStable(identity, state));
        self.pool_addresses.insert(params.address, pool_id);

        pool_id
    }

    /// Read a registered Balancer stable pool's state by `pool_id`.
    ///
    /// The Python companion (slice 12d) reads `balances` / `update_block` /
    /// `bpt_idx` / `invariant_version` / `amp` through this accessor via
    /// `PyLiquidityPool` getters. Returns `None` for non-Balancer-stable
    /// pools (silent no-op).
    #[must_use]
    pub fn get_balancer_stable_pool(&self, pool_id: u64) -> Option<&BalancerStablePoolState> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::balancer_stable)
            .map(|(_, state)| state)
    }

    /// Look up a Balancer stable pool's immutable registration identity
    /// (address, vault, `pool_id`, tokens, amp, `scaling_factors`, `swap_fee`,
    /// `bpt_idx`, `invariant_version`). Returns `None` if not registered or not
    /// a stable pool.
    #[must_use]
    pub fn get_balancer_stable_identity(
        &self,
        pool_id: u64,
    ) -> Option<&BalancerStablePoolIdentity> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::balancer_stable)
            .map(|(identity, _)| identity)
    }
}

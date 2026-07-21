//! Curve `StableSwap` pool state — the single `BotState`-owned home for Curve
//! V1 (`StableSwap`) pool data (ADR-003 "third family").
//!
//! This is the **state-port sub-slice** of ADR-005 slice 11 (the plan's
//! "state port" sub-slice, mirroring what slice 8a did for V3 before the V3
//! companion rewrite). It adds a third family to `PoolEntry` — the
//! ADR-003-deferred "third family ports, now's the moment" decision point —
//! and confirms the inline-`PoolEntry` + `BotState::apply_*` pattern survives
//! a third variant without extracting a `LiquidityMap` generic (the ADR-003
//! ruling holds: a sample-of-one is not a pattern).
//!
//! Scope of THIS sub-slice: the Rust state struct + `register_curve_pool` +
//! `apply_balance_update_by_pool_id` + journal restore/discard +
//! `PyBot.register_curve_pool` + Curve read getters on `PyLiquidityPool`
//! (`balances`, `n_coins`, `update_block`, `snapshot_curve`). The Python
//! `CurveStableswapPool` companion rewrite (delegating `self._state.balances`
//! to the handle) + the pure-math Rust port (`stableswap_get_y`/
//! `stableswap_newton_y`) + the `CurveDataProvider` Rust port are deferred to
//! follow-on sub-slices (11b companion, 11c math, 11d provider) per the plan's
//! sub-slicing note.
//!
//! ## State shape
//!
//! Curve `StableSwap` pools differ structurally from V2/V3/V4:
//! - **N tokens** (not 2) — `balances: Vec<U256>`.
//! - An **amplification coefficient** `A` with on-chain ramping.
//! - **Variant strategy enums** (`d_variant`/`y_variant`/`yd_variant`,
//!   `swap_style`, `lending_rate_style`) — resolved by the builder from the
//!   pool address; carried immutably on the state so the (future) Rust `get_dy`
//!   can dispatch.
//! - A `CurveDataProvider` 13-method seam for per-block on-chain data (D,
//!   gamma, `virtual_price`, lending rates, etc.) — NOT ported in this
//!   sub-slice; stays Python-side at calculation time.
//!
//! The mutable slot is just `balances` + `update_block` — the only fields a
//! Curve `external_update` (new balances from an `Exchange` event) mutates.
//! Everything else is immutable config set at registration.

use alloy::primitives::{Address, U256};
use std::sync::Arc;

use crate::curve_data_provider::CurveDataProvider;
use crate::state_history::{
    BalanceVectorPoolState, BalancesBlockDelta, JournalError, ReorgJournal, ReorgPoolState,
};

// ---------------------------------------------------------------------------
// Block delta
// ---------------------------------------------------------------------------
//
// ADR-014 D3a: the Curve-specific `CurveBlockDelta` was a byte-identical
// full-state delta to the Balancer family's; it's unified into the shared
// `BalancesBlockDelta` in `state_history.rs`. The Curve journal is now
// `ReorgJournal<BalancesBlockDelta>`.

// ---------------------------------------------------------------------------
// Registration params + state struct
// ---------------------------------------------------------------------------

/// Parameters for registering a Curve `StableSwap` pool with `BotState`.
///
/// Carries immutable pool config (address, tokens, A, fee, `admin_fee`, rate
/// multipliers, variant strategy enums, base-pool reference) + the
/// registration-time mutable state (`balances`, `update_block`).
///
/// `balances.len()` MUST equal `tokens.len()` and `rate_multipliers.len()`.
#[derive(Clone, Debug)]
pub struct RegisterCurvePoolParams {
    /// Pool contract address.
    pub address: Address,
    /// The pool's ERC-20 tokens (2 or more), in canonical Curve coin order.
    pub tokens: Vec<Address>,
    /// Amplification coefficient `A` (raw; ramping is resolved Python-side at
    /// calculation time via the `CurveDataProvider` + per-block timestamp —
    /// not ported in this sub-slice).
    pub a_coefficient: u128,
    /// Swap fee (in `FEE_DENOMINATOR = 1e10` units — Curve's denominator).
    pub fee: u64,
    /// Admin fee share of the swap fee (in `FEE_DENOMINATOR` units).
    pub admin_fee: u64,
    /// Rate multipliers (precision-adjusted) — `10**(2*PRECISION - decimals)`
    /// per token, or `precision_multipliers * 10**PRECISION` when overridden.
    pub rate_multipliers: Vec<U256>,
    /// Initial balances (one per token), captured at `update_block`.
    pub balances: Vec<U256>,
    /// Block number of the registration state — seeds the genesis reorg
    /// journal delta (ADR-005 slice 4).
    pub update_block: u64,

    // --- Variant strategy enums (opaque to Rust; carried so the future
    //     Rust get_dy can dispatch on them) ---
    /// `swap_style` discriminator (standard / `live_admin` / crypto / ...).
    /// Stored as a raw u8 to keep Rust free of the Python enum surface.
    pub swap_style: u8,
    /// `lending_rate_style` discriminator (`NONE` / ...).
    pub lending_rate_style: u8,
    /// `d_variant` discriminator (stableswap D-iteration variant).
    pub d_variant: u8,
    /// `y_variant` discriminator (stableswap y-iteration variant).
    pub y_variant: u8,
    /// `yd_variant` discriminator (stableswap yd-iteration variant).
    pub yd_variant: u8,

    // --- Optional metapool wiring ---
    /// Base pool address for metapools (`None` for plain pools).
    pub base_pool: Option<Address>,

    // --- A-ramping (immutable on-chain config; resolves Python-side at
    //     calc time via per-block timestamp). `None` for non-ramping pools. ---
    /// Initial amplification coefficient (A at `initial_a_coefficient_time`).
    pub initial_a_coefficient: Option<u128>,
    /// Target amplification coefficient (A at `future_a_coefficient_time`).
    pub future_a_coefficient: Option<u128>,
    /// Block timestamp at which ramping starts (`u64`).
    pub initial_a_coefficient_time: Option<u64>,
    /// Block timestamp at which ramping ends (`u64`).
    pub future_a_coefficient_time: Option<u64>,
    /// Pool deployment timestamp (the ramp anchor; `u64`).
    pub create_timestamp: Option<u64>,

    // --- Crypto-pool fees (immutable on-chain config; `None` / 0 for
    //     standard stableswap pools that don't use them). ---
    /// Crypto-pool `fee_gamma` (`u64`).
    pub fee_gamma: Option<u64>,
    /// Crypto-pool `mid_fee` (`u64`).
    pub mid_fee: Option<u64>,
    /// Crypto-pool `offpeg_fee_multiplier` (`u64`).
    pub offpeg_fee_multiplier: Option<u64>,
    /// Crypto-pool `out_fee` (`u64`).
    pub out_fee: Option<u64>,
    /// Crypto-pool `gamma` (`u64`).
    pub gamma: Option<u64>,

    // --- LP token + lending flags + precision multipliers (immutable
    //     on-chain config). ---
    /// Dedicated LP token address (`None` ⇔ the pool token itself IS the
    /// LP, the common Curve V1 plain-pool case).
    pub lp_token: Option<Address>,
    /// Per-token `use_lending` flags (true for lending-backed coins).
    pub use_lending: Vec<bool>,
    /// Per-token `precision_multipliers` (distinct from `rate_multipliers`:
    /// `rate_multipliers` is the precision × rate product consumed by `xp`;
    /// `precision_multipliers` is `10**(2*PRECISION - decimals)`, the
    /// pre-rate adjustment). One per token.
    pub precision_multipliers: Vec<U256>,

    // --- Metapool underlying-token addresses (immutable on-chain config). ---
    /// Underlying ERC20 coin addresses for a metapool (the coins beneath the
    /// base-pool intermediary coins; `None` for non-metapools). One entry per
    /// underlying coin. The companion resolves these to `PyErc20Token`
    /// handles for swap routing — they are NOT the base pool's tokens.
    pub tokens_underlying: Option<Vec<Address>>,

    // --- Metapool strategy discriminants (immutable; the two
    //     `PoolStrategies` enums the BOMDRK extension did not carry). ---
    /// `metapool_rate_style` discriminant (auto()-based `MetapoolRateStyle`).
    pub metapool_rate_style: u8,
    /// `metapool_underlying_style` discriminant (`MetapoolUnderlyingStyle`).
    pub metapool_underlying_style: u8,

    // --- I/O trait object (layer-2 design; the on-chain-state reader). ---
    /// Off-chain data provider (ADR-005 JFGCHJ). `None` ⇔ no I/O path — a
    /// pure-Rust fixture test or a pool whose calc doesn't need per-block
    /// on-chain lookups. When `Some`, the (future, seam task) Python
    /// companion delegates reads here instead of holding a Python
    /// `CurveDataProvider`.
    pub data_provider: Option<Arc<dyn CurveDataProvider>>,
}

/// Immutable Curve registration identity (ADR-005 identity slice).
///
/// Pure registration data — the pool's permanent identity, captured at
/// `register_curve_pool` and never mutated. Mirrors
/// `V2PoolIdentity`/`V3PoolIdentity`/`V4PoolIdentity`/`TokenEntry`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurvePoolIdentity {
    /// Pool contract address.
    pub address: Address,
    /// The pool's ERC-20 tokens (2 or more), in canonical Curve coin order.
    pub tokens: Vec<Address>,
    /// Amplification coefficient `A` (raw).
    pub a_coefficient: u128,
    /// Swap fee (in `FEE_DENOMINATOR = 1e10` units).
    pub fee: u64,
    /// Admin fee share (in `FEE_DENOMINATOR` units).
    pub admin_fee: u64,
    /// Rate multipliers (precision-adjusted), one per token.
    pub rate_multipliers: Vec<U256>,
    // --- Variant strategy enums (opaque to Rust; carried so the future
    //     Rust get_dy can dispatch on them) ---
    /// `swap_style` discriminator (standard / `live_admin` / crypto / ...).
    /// Stored as a raw u8 to keep Rust free of the Python enum surface.
    pub swap_style: u8,
    /// `lending_rate_style` discriminator (`NONE` / ...).
    pub lending_rate_style: u8,
    /// `d_variant` discriminator (stableswap D-iteration variant).
    pub d_variant: u8,
    /// `y_variant` discriminator (stableswap y-iteration variant).
    pub y_variant: u8,
    /// `yd_variant` discriminator (stableswap yd-iteration variant).
    pub yd_variant: u8,
    // --- Optional metapool wiring ---
    /// Base pool address for metapools (`None` for plain pools).
    pub base_pool: Option<Address>,
    // --- A-ramping (immutable; see [`RegisterCurvePoolParams`]). ---
    /// Initial amplification coefficient.
    pub initial_a_coefficient: Option<u128>,
    /// Target amplification coefficient.
    pub future_a_coefficient: Option<u128>,
    /// Ramping start timestamp.
    pub initial_a_coefficient_time: Option<u64>,
    /// Ramping end timestamp.
    pub future_a_coefficient_time: Option<u64>,
    /// Pool deployment timestamp.
    pub create_timestamp: Option<u64>,
    // --- Crypto-pool fees (immutable). ---
    /// Crypto-pool `fee_gamma`.
    pub fee_gamma: Option<u64>,
    /// Crypto-pool `mid_fee`.
    pub mid_fee: Option<u64>,
    /// Crypto-pool `offpeg_fee_multiplier`.
    pub offpeg_fee_multiplier: Option<u64>,
    /// Crypto-pool `out_fee`.
    pub out_fee: Option<u64>,
    /// Crypto-pool `gamma`.
    pub gamma: Option<u64>,
    // --- LP token + lending flags + precision multipliers (immutable). ---
    /// Dedicated LP token address (`None` ⇔ the pool token IS the LP).
    pub lp_token: Option<Address>,
    /// Per-token `use_lending` flags.
    pub use_lending: Vec<bool>,
    /// Per-token `precision_multipliers`.
    pub precision_multipliers: Vec<U256>,
    // --- Metapool underlying-token addresses (immutable). ---
    /// Underlying ERC20 coin addresses for a metapool (`None` for plain).
    pub tokens_underlying: Option<Vec<Address>>,
    // --- Metapool strategy discriminants (immutable). ---
    /// `metapool_rate_style` discriminant.
    pub metapool_rate_style: u8,
    /// `metapool_underlying_style` discriminant.
    pub metapool_underlying_style: u8,
}

/// Curve `StableSwap` pool state owned by [`crate::BotState`].
///
/// Carries the mutable `balances`/`update_block` slot + a per-pool reorg
/// journal. Immutable identity lives on [`CurvePoolIdentity`] (look it up via
/// [`crate::BotState::get_curve_identity`]). The state-port
/// sub-slice (ADR-005 slice 11a): the Python `CurveStableswapPool` companion
/// (11b) reads `balances`/`update_block` from this struct via `PyLiquidityPool`
/// getters and delegates `external_update` to
/// `apply_balance_update_by_pool_id`.
#[derive(Clone, Debug)]
pub struct CurvePoolState {
    // --- Mutable state (authoritative) ---
    /// Current balances (one per token).
    pub balances: Vec<U256>,
    /// Block number of the last balance update.
    pub update_block: u64,

    /// Reorg journal — balance priors for rollback.
    pub journal: ReorgJournal<BalancesBlockDelta>,

    /// Off-chain data provider (ADR-005 JFGCHJ). `None` ⇔ no I/O path.
    /// Stored on state (not immutable identity) because the provider is an
    /// I/O shim, not pool identity; the companion reads through the handle
    /// at calc time.
    pub data_provider: Option<Arc<dyn CurveDataProvider>>,
}

impl CurvePoolIdentity {
    /// Number of tokens (== number of balances).
    #[must_use]
    pub fn n_coins(&self) -> usize {
        self.tokens.len()
    }
}

impl CurvePoolState {
    /// Construct the (immutable identity, mutable state) pair from
    /// registration params, with a journal of the given depth.
    /// Pushes a genesis anchor delta (mirror of V2's discipline) so
    /// `restore_before_block` can land on the registration state.
    #[must_use]
    pub fn from_params(
        params: RegisterCurvePoolParams,
        journal_depth: usize,
    ) -> (CurvePoolIdentity, CurvePoolState) {
        let mut journal = ReorgJournal::<BalancesBlockDelta>::new(journal_depth);
        // Genesis anchor: before == after == registration balances at
        // update_block. The "landed-at" registration point.
        journal.push_delta(BalancesBlockDelta {
            block: params.update_block,
            balances_before: params.balances.clone(),
            balances_after: params.balances.clone(),
        });
        let identity = CurvePoolIdentity {
            address: params.address,
            tokens: params.tokens,
            a_coefficient: params.a_coefficient,
            fee: params.fee,
            admin_fee: params.admin_fee,
            rate_multipliers: params.rate_multipliers,
            swap_style: params.swap_style,
            lending_rate_style: params.lending_rate_style,
            d_variant: params.d_variant,
            y_variant: params.y_variant,
            yd_variant: params.yd_variant,
            base_pool: params.base_pool,
            initial_a_coefficient: params.initial_a_coefficient,
            future_a_coefficient: params.future_a_coefficient,
            initial_a_coefficient_time: params.initial_a_coefficient_time,
            future_a_coefficient_time: params.future_a_coefficient_time,
            create_timestamp: params.create_timestamp,
            fee_gamma: params.fee_gamma,
            mid_fee: params.mid_fee,
            offpeg_fee_multiplier: params.offpeg_fee_multiplier,
            out_fee: params.out_fee,
            gamma: params.gamma,
            lp_token: params.lp_token,
            use_lending: params.use_lending,
            precision_multipliers: params.precision_multipliers,
            tokens_underlying: params.tokens_underlying,
            metapool_rate_style: params.metapool_rate_style,
            metapool_underlying_style: params.metapool_underlying_style,
        };
        let state = CurvePoolState {
            balances: params.balances,
            update_block: params.update_block,
            journal,
            data_provider: params.data_provider,
        };
        (identity, state)
    }
}

// ADR-014 D3 refinement — pool-owned reorg rollback for the balance-vector
// family. The field-write previously duplicated across the three
// `BotState::*_restore_before_block` dispatchers (Curve + Balancer weighted +
// stable) is absorbed into the state struct itself; restore returns `()` so a
// single non-generic trait covers the family with no no-op stubs. Byte-identical
// to the BalancerStable / BalancerWeighted impls modulo the struct name.
impl ReorgPoolState for CurvePoolState {
    fn restore_before_block(&mut self, block: u64) -> Result<(), JournalError> {
        let (balances, landed_block) = self.journal.restore_before_block(block)?;
        self.balances.clone_from(&balances);
        self.update_block = landed_block;
        Ok(())
    }

    fn discard_before_block(&mut self, block: u64) -> Result<(), JournalError> {
        self.journal.discard_before_block(block)
    }

    fn journal_len(&self) -> usize {
        self.journal.len()
    }

    fn newest_block(&self) -> Option<u64> {
        self.journal.newest_block()
    }
}

// ADR-017 D1 — forward-apply twin of `ReorgPoolState`. The field-write
// previously inlined in `BotState::apply_balance_update_by_pool_id` is
// absorbed into the state struct; the trait returns `()` (the `Option<u64>`
// is a `BotState` variant-dispatch concern). Byte-identical to the
// BalancerWeighted / BalancerStable impls modulo the struct name + assert
// message.
impl BalanceVectorPoolState for CurvePoolState {
    fn apply_balance_update(&mut self, balances: Vec<U256>, block_number: u64) {
        assert!(
            balances.len() == self.balances.len(),
            "Curve balance length mismatch: pool has {} tokens, update has {}",
            self.balances.len(),
            balances.len(),
        );
        self.journal.push_delta(BalancesBlockDelta {
            block: block_number,
            balances_before: self.balances.clone(),
            balances_after: balances.clone(),
        });
        self.balances = balances;
        self.update_block = block_number;
    }
}

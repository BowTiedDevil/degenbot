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
//! `apply_curve_balance_update_by_pool_id` + journal restore/discard +
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

use crate::bot_core::state_history::{BlockDelta, ReorgJournal};

// ---------------------------------------------------------------------------
// Block delta
// ---------------------------------------------------------------------------

/// Per-block delta for a Curve `StableSwap` pool.
///
/// Stores the **before** balances captured at the moment a block's
/// `external_update` was applied — used for reorg rollback. Mirrors
/// [`V2BlockDelta`](crate::bot_core::state_history::V2BlockDelta): a full-state
/// delta (N balances + block), with `balances_before` redundant with the
/// preceding delta's `balances_after` (retained for a self-describing record).
///
/// A genesis delta is pushed at registration (`before == after == registration
/// balances`, at `block = update_block`) so `restore_before_block` can land on
/// the registration state — the same anchor discipline as V2 (the Curve journal
/// carries a genesis anchor; V3/V4 do not, because their first forward event's
/// "before" IS the registration state).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurveBlockDelta {
    /// Block number of this delta.
    pub block: u64,
    /// Balances *before* this block's update (redundant with the preceding
    /// delta's `balances_after`; retained for a self-describing transition
    /// record — matches `V2BlockDelta`'s `*_before` fields).
    pub balances_before: Vec<U256>,
    /// Balances *after* this block's update — the landed-at state for this
    /// block, returned by `restore_before_block` when it lands here.
    pub balances_after: Vec<U256>,
}

impl BlockDelta for CurveBlockDelta {
    fn block(&self) -> u64 {
        self.block
    }

    // Curve is a **full-state** delta (balances_before/after, mirroring V2);
    // the default no-op coalesce is correct — `restore_before_block` reads
    // the surviving delta's `balances_after`, so collapsing to one entry per
    // block loses nothing (same reasoning as `V2BlockDelta`).
}

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
}

/// Curve `StableSwap` pool state owned by [`crate::bot_core::BotState`].
///
/// Carries the immutable config captured at registration + the mutable
/// `balances`/`update_block` slot + a per-pool reorg journal. The state-port
/// sub-slice (ADR-005 slice 11a): the Python `CurveStableswapPool` companion
/// (11b) will read `balances`/`update_block` from this struct via
/// `PyLiquidityPool` getters and delegate `external_update` to
/// `apply_curve_balance_update_by_pool_id`.
#[derive(Clone, Debug)]
pub struct CurvePoolState {
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

    // --- Mutable state (authoritative) ---
    /// Current balances (one per token).
    pub balances: Vec<U256>,
    /// Block number of the last balance update.
    pub update_block: u64,

    // --- Variant strategy enums (opaque to Rust) ---
    pub swap_style: u8,
    pub lending_rate_style: u8,
    pub d_variant: u8,
    pub y_variant: u8,
    pub yd_variant: u8,

    // --- Optional metapool wiring ---
    pub base_pool: Option<Address>,

    /// Reorg journal — balance priors for rollback.
    pub journal: ReorgJournal<CurveBlockDelta>,
}

impl CurvePoolState {
    /// Construct from registration params with a journal of the given depth.
    /// Pushes a genesis anchor delta (mirror of V2's discipline) so
    /// `restore_before_block` can land on the registration state.
    #[must_use]
    pub fn from_params(params: RegisterCurvePoolParams, journal_depth: usize) -> Self {
        let mut journal = ReorgJournal::<CurveBlockDelta>::new(journal_depth);
        // Genesis anchor: before == after == registration balances at
        // update_block. The "landed-at" registration point.
        journal.push_delta(CurveBlockDelta {
            block: params.update_block,
            balances_before: params.balances.clone(),
            balances_after: params.balances.clone(),
        });
        Self {
            address: params.address,
            tokens: params.tokens,
            a_coefficient: params.a_coefficient,
            fee: params.fee,
            admin_fee: params.admin_fee,
            rate_multipliers: params.rate_multipliers,
            balances: params.balances,
            update_block: params.update_block,
            swap_style: params.swap_style,
            lending_rate_style: params.lending_rate_style,
            d_variant: params.d_variant,
            y_variant: params.y_variant,
            yd_variant: params.yd_variant,
            base_pool: params.base_pool,
            journal,
        }
    }

    /// Number of tokens (== number of balances).
    #[must_use]
    pub fn n_coins(&self) -> usize {
        self.tokens.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot_core::{BotState, RegisterCurvePoolParams};

    fn three_coin_params(block: u64, balances: &[u64]) -> RegisterCurvePoolParams {
        RegisterCurvePoolParams {
            address: Address::repeat_byte(0xc1),
            tokens: vec![
                Address::repeat_byte(0x01),
                Address::repeat_byte(0x02),
                Address::repeat_byte(0x03),
            ],
            a_coefficient: 2000,
            fee: 4_000_000,
            admin_fee: 5_000_000_000,
            rate_multipliers: vec![U256::from(10u64).pow(U256::from(18u64)); 3],
            balances: balances.iter().map(|&b| U256::from(b)).collect(),
            update_block: block,
            swap_style: 0,
            lending_rate_style: 0,
            d_variant: 0,
            y_variant: 0,
            yd_variant: 0,
            base_pool: None,
        }
    }

    #[test]
    fn register_and_read_back_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_curve_pool(&three_coin_params(10, &[1_000, 2_000, 3_000]));
        let s = core.get_curve_pool(pool_id).expect("curve pool registered");
        assert_eq!(s.n_coins(), 3);
        assert_eq!(
            s.balances,
            vec![U256::from(1_000), U256::from(2_000), U256::from(3_000)]
        );
        assert_eq!(s.update_block, 10);
        // Genesis anchor pushed.
        assert_eq!(core.curve_journal_len(pool_id), 1);
    }

    #[test]
    fn apply_balance_update_journals_and_lands_new_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_curve_pool(&three_coin_params(10, &[1_000, 2_000, 3_000]));
        let affected = core.apply_curve_balance_update_by_pool_id(
            pool_id,
            vec![U256::from(1_500), U256::from(2_500), U256::from(3_500)],
            12,
        );
        assert_eq!(affected, Some(pool_id));
        let s = core.get_curve_pool(pool_id).expect("curve pool registered");
        assert_eq!(
            s.balances,
            vec![U256::from(1_500), U256::from(2_500), U256::from(3_500)]
        );
        assert_eq!(s.update_block, 12);
        // Genesis + the new transition delta.
        assert_eq!(core.curve_journal_len(pool_id), 2);
    }

    #[test]
    fn apply_balance_update_is_silent_noop_on_v2_pool() {
        let mut core = BotState::new();
        // Register a V2 pool at pool_id 1, then try the Curve apply path.
        let v2 = core.register_v2_pool(&crate::bot_core::RegisterV2PoolParams {
            address: Address::repeat_byte(0x22),
            token0: Address::repeat_byte(0x01),
            token1: Address::repeat_byte(0x02),
            reserve0: U256::from(1_000),
            reserve1: U256::from(2_000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::repeat_byte(0xff),
            update_block: 0,
        });
        let affected = core.apply_curve_balance_update_by_pool_id(v2, vec![U256::from(1_500)], 5);
        assert!(
            affected.is_none(),
            "Curve apply on a V2 pool must be a silent no-op"
        );
    }

    #[test]
    fn restore_before_block_lands_at_prior_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_curve_pool(&three_coin_params(10, &[1_000, 2_000, 3_000]));
        // Block 12: balances change.
        let _ = core.apply_curve_balance_update_by_pool_id(
            pool_id,
            vec![U256::from(1_500), U256::from(2_500), U256::from(3_500)],
            12,
        );
        // Restore to before block 12 → landed-at = the genesis registration
        // balances (the largest delta strictly below 12).
        let restored = core
            .curve_restore_before_block(pool_id, 12)
            .expect("Some(Ok) on a registered Curve pool")
            .expect("Ok (target > genesis block)");
        assert_eq!(
            restored.0,
            vec![U256::from(1_000), U256::from(2_000), U256::from(3_000)]
        );
        assert_eq!(restored.1, 10);
        // Current mutable state was written back.
        let s = core.get_curve_pool(pool_id).expect("curve pool registered");
        assert_eq!(
            s.balances,
            vec![U256::from(1_000), U256::from(2_000), U256::from(3_000)]
        );
        assert_eq!(s.update_block, 10);
    }

    #[test]
    fn restore_to_before_registration_is_an_error() {
        let mut core = BotState::new();
        let pool_id = core.register_curve_pool(&three_coin_params(10, &[1_000, 2_000, 3_000]));
        // Target at the registration block → rolling back past registration.
        let res = core
            .curve_restore_before_block(pool_id, 10)
            .expect("Some on registered pool");
        assert!(
            res.is_err(),
            "restoring to the registration block must error"
        );
    }
}

//! Balancer V2 **stable** pool state — the `BotState`-owned home for a
//! Balancer V2 stable pool (`BalancerV2StablePool`, `StableSwap` invariant).
//!
//! ADR-005 slice 12c — the **stable-state-port** sub-slice of slice 12
//! (the plan's "state port" sub-slice for the stable variant, mirroring
//! what slice 11a did for Curve + slice 12a did for Balancer weighted).
//! Adds the Balancer stable family as a **fifth** `PoolEntry` variant —
//! alongside V2/V3/V4/Curve/BalancerWeighted (which landed in 12a).
//!
//! Scope of THIS sub-slice: the Rust state struct +
//! `register_balancer_stable_pool` + `apply_balancer_stable_balance_update_by_pool_id`
//! + journal restore/discard + `PyBot.register_balancer_stable_pool` +
//! Balancer stable read getters on `PyLiquidityPool`
//! (`balancer_stable_balances`, `balancer_stable_update_block`,
//! `snapshot_balancer_stable`, `n_balancer_stable_tokens`, `balancer_amp`,
//! `balancer_bpt_index`, `balancer_invariant_version`,
//! `balancer_stable_scaling_factors`, `balancer_stable_swap_fee`). The
//! Python `BalancerV2StablePool` companion
//! rewrite (delegating `self._state` to the handle) +
//! `make_balancer_stable_pool` factory + `BalancerBuilder` stable-branch
//! migration + `CacheAwareRateProvider`-in-Rust + `StaleRateResult` are
//! deferred to follow-on sub-slice 12d. The pure-math Rust port
//! (`StableMath` + `FixedPoint` / `LogExpMath`) is deferred to 12e.
//!
//! ## State shape
//!
//! Balancer V2 stable pools (a.k.a. `ComposableStablePools` / MetaStablePools):
//! - **N tokens** (2 or more). For `ComposableStablePools` the BPT token is
//!   itself in the token list — `bpt_idx` marks its position so the swap
//!   math can drop it before invariant/swap calculations. `None` for
//!   `MetaStablePools` (no BPT in the list). Carried immutably on the
//!   state so the (future, 12e) Rust `StableMath` can drop it. The ADR-003
//!   per-family index seam: V2/V3/V4 have nothing to drop; Curve has
//!   coins (the `rate_multipliers` project); Balancer stable has the
//!   BPT-index.
//! - An **amplification coefficient** `amp` — treated as **immutable after
//!   construction** in this plan (the `external_update` carries no amp field;
//!   A ramping is a future, non-epic concern; the builder resolves the
//!   ramped-at-this-block value and forwards it here).
//! - **Scaling factors** (`scaling_factors: Vec<U256>`, rate-multiplied)
//!   — precomputed at registration from `base_scaling_factors * rate_at_registration
//!   // ONE` per token; the builder resolves these so Rust never touches
//!   ERC20 decimals or rate providers. The rate-provider caching
//!   (`CacheAwareRateProvider`) is 12d — this sub-slice carries the
//!   registration-time `scaling_factors` snapshot the builder provides, the
//!   same way the state-port sub-slice 12a carried the weighted-side
//!   snapshot.
//! - A **swap fee** as a fraction of `FEE_DENOMINATOR = 1e18`. Stored as
//!   the Python-computed `int(fee * FEE_DENOMINATOR)`.
//! - **Invariant version** (`invariant_version: u8`, V1=1 / V2=2) — the
//!   systematic-1-wei-error guard. V1 (`INVARIANT_V1`) is the always-
//!   roundDown `D_P` accumulation path used by most deployed
//!   `ComposableStablePools`; V2 (`INVARIANT_V2`) is the roundUp-param
//!   `P_D` accumulation path used by `MetaStablePools`. Carried as opaque
//!   `u8` for the (future, 12e) Rust `StableMath`.
//!
//! The mutable slot is just `balances` + `update_block` — the only fields
//! a Balancer stable `external_update` (new balances from a Vault
//! `PoolBalanceChanged` event) mutates. Everything else is immutable
//! config set at registration.
//!
//! Matches `CurvePoolState` / `BalancerWeightedPoolState` discipline: a
//! full-state `balances_before/after` delta + a V2-style genesis anchor at
//! registration.

use alloy::primitives::{Address, U256};

use crate::bot_core::state_history::{BlockDelta, ReorgJournal};

// ---------------------------------------------------------------------------
// Block delta
// ---------------------------------------------------------------------------

/// Per-block delta for a Balancer V2 stable pool.
///
/// Stores the **before** balances captured at the moment a block's
/// `external_update` was applied — used for reorg rollback. Mirrors
/// [`crate::bot_core::state_history::V2BlockDelta`] and
/// [`crate::bot_core::CurveBlockDelta`] /
/// [`crate::bot_core::BalancerWeightedBlockDelta`]: a full-state delta (N
/// balances + block), with `balances_before` redundant with the preceding
/// delta's `balances_after` (retained for a self-describing record).
///
/// A genesis delta is pushed at registration (`before == after == registration
/// balances`, at `block = update_block`) so `restore_before_block` can land
/// on the registration state — the same anchor discipline as V2/Curve/
/// `BalancerWeighted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalancerStableBlockDelta {
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

impl BlockDelta for BalancerStableBlockDelta {
    fn block(&self) -> u64 {
        self.block
    }

    // Same full-state delta shape as V2/Curve/BalancerWeighted; the default
    // no-op coalesce is correct (`restore_before_block` reads the surviving
    // delta's `balances_after`).
}

// ---------------------------------------------------------------------------
// Registration params + state struct
// ---------------------------------------------------------------------------

/// Parameters for registering a Balancer V2 stable pool with `BotState`.
///
/// Carries immutable pool config (address, vault, 32-byte pool ID, tokens,
/// amp, scaling factors, swap fee, BPT index, `invariant_version`) + the
/// registration-time mutable state (`balances`, `update_block`).
///
/// `balances.len()` MUST equal `tokens.len()` and `scaling_factors.len()`.
/// `bpt_idx`, when `Some(i)`, MUST be `< tokens.len()` (the BPT token's
/// position in the list).
#[derive(Clone, Debug)]
pub struct RegisterBalancerStablePoolParams {
    /// Pool contract address (the pool contract that stores the invariant logic).
    pub address: Address,
    /// The Balancer V2 singleton Vault contract address.
    pub vault: Address,
    /// 32-byte Balancer V2 pool identifier (encodes pool address +
    /// specialization + nonce). Carried as a fixed array since the bytes are
    /// a Balancer invariant, not an address.
    pub pool_id: [u8; 32],
    /// The pool's ERC-20 tokens (2 or more), in canonical Vault order. For
    /// `ComposableStablePools` this list INCLUDES the BPT token at
    /// `bpt_idx`.
    pub tokens: Vec<Address>,
    /// Amplification coefficient `amp` at the registration block.
    /// **Immutable after construction** in this plan — A ramping is a
    /// future, non-epic concern; the builder resolves the ramped-at-block
    /// value and forwards it here.
    pub amp: u128,
    /// Scaling factors (rate-multiplied: `base_sf * rate // ONE` per token)
    /// — precomputed by the builder at registration so Rust never touches
    /// ERC20 decimals or rate providers. The rate-cache-aware
    /// `CacheAwareRateProvider` (per-block rate refresh) is 12d — for
    /// 12c, this snapshot is what the Python companion + its
    /// `to_hop_state` `swap_fn` consume.
    pub scaling_factors: Vec<U256>,
    /// Swap fee as a fraction of `FEE_DENOMINATOR = 1e18`
    /// (`int(fee_fraction * 1e18)`). Applied by the (future, 12e) Rust
    /// `StableMath` swap; opaque to this sub-slice.
    pub swap_fee: u128,
    /// BPT token index for `ComposableStablePools` (`Some(i)` where `i <
    /// tokens.len()`); `None` for `MetaStablePools`. Carried so the
    /// (future, 12e) Rust `StableMath` can drop the BPT before invariant
    /// and swap calculations — the CONTEXT.md "BPT Index" ruling.
    pub bpt_idx: Option<usize>,
    /// `invariant_version` discriminator (1 = `INVARIANT_V1` always-
    /// roundDown `D_P` accumulation, used by most `ComposableStablePools`;
    /// 2 = `INVARIANT_V2` roundUp-param `P_D` accumulation, used by
    /// `MetaStablePools`). The systematic-1-wei-error guard — using the
    /// wrong version gives a ±1 wei output diff. Opaque `u8` for the
    /// future Rust math port; this sub-slice doesn't dispatch on it.
    pub invariant_version: u8,

    // --- Mutable state at registration ---
    /// Initial balances (one per token, INCLUDING BPT for Composable
    /// pools), captured at `update_block`.
    pub balances: Vec<U256>,
    /// Block number of the registration state — seeds the genesis reorg
    /// journal delta (ADR-005 slice 4).
    pub update_block: u64,
}

/// Balancer V2 stable pool state owned by [`crate::bot_core::BotState`].
///
/// Carries the immutable config captured at registration + the mutable
/// `balances`/`update_block` slot + a per-pool reorg journal. The state-port
/// sub-slice (ADR-005 slice 12c): the Python `BalancerV2StablePool` companion
/// (12d) will read `balances`/`update_block` from this struct via
/// `PyLiquidityPool` getters and delegate `external_update` to
/// `apply_balancer_stable_balance_update_by_pool_id`.
#[derive(Clone, Debug)]
pub struct BalancerStablePoolState {
    /// Pool contract address.
    pub address: Address,
    /// The Balancer V2 singleton Vault address.
    pub vault: Address,
    /// 32-byte pool identifier (Balancer invariant; unique per pool).
    pub pool_id: [u8; 32],
    /// The pool's ERC-20 tokens (2 or more), in canonical Vault order.
    /// Includes BPT at `bpt_idx` for `ComposableStablePools`.
    pub tokens: Vec<Address>,
    /// Amplification coefficient `amp` (immutable after registration in
    /// this plan).
    pub amp: u128,
    /// Scaling factors (rate-multiplied), one per token.
    pub scaling_factors: Vec<U256>,
    /// Swap fee as a fraction of `FEE_DENOMINATOR = 1e18`.
    pub swap_fee: u128,
    /// BPT token index (`Some(i)` for Composable, `None` for `MetaStable`).
    pub bpt_idx: Option<usize>,
    /// `invariant_version` discriminator (V1=1 / V2=2; opaque to this
    /// sub-slice).
    pub invariant_version: u8,

    // --- Mutable state (authoritative) ---
    /// Current balances (one per token, including BPT for Composable).
    pub balances: Vec<U256>,
    /// Block number of the last balance update.
    pub update_block: u64,

    /// Reorg journal — balance priors for rollback.
    pub journal: ReorgJournal<BalancerStableBlockDelta>,
}

impl BalancerStablePoolState {
    /// Construct from registration params with a journal of the given depth.
    /// Pushes a genesis anchor delta (mirror of V2/Curve/BalancerWeighted
    /// discipline) so `restore_before_block` can land on the registration
    /// state.
    #[must_use]
    pub fn from_params(params: RegisterBalancerStablePoolParams, journal_depth: usize) -> Self {
        let mut journal = ReorgJournal::<BalancerStableBlockDelta>::new(journal_depth);
        // Genesis anchor: before == after == registration balances at
        // update_block. The "landed-at" registration point.
        journal.push_delta(BalancerStableBlockDelta {
            block: params.update_block,
            balances_before: params.balances.clone(),
            balances_after: params.balances.clone(),
        });
        Self {
            address: params.address,
            vault: params.vault,
            pool_id: params.pool_id,
            tokens: params.tokens,
            amp: params.amp,
            scaling_factors: params.scaling_factors,
            swap_fee: params.swap_fee,
            bpt_idx: params.bpt_idx,
            invariant_version: params.invariant_version,
            balances: params.balances,
            update_block: params.update_block,
            journal,
        }
    }

    /// Number of tokens (== number of balances/scaling factors; includes
    /// BPT for Composable pools).
    #[must_use]
    pub fn n_tokens(&self) -> usize {
        self.tokens.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot_core::{BotState, RegisterBalancerStablePoolParams};

    /// 3-token `ComposableStablePool` fixture (BPT at index 2).
    fn composable_params(
        block: u64,
        balances: &[u64],
        bpt_idx: Option<usize>,
        invariant_version: u8,
    ) -> RegisterBalancerStablePoolParams {
        RegisterBalancerStablePoolParams {
            address: Address::repeat_byte(0xb5),
            vault: Address::repeat_byte(0xba),
            pool_id: [0u8; 32],
            tokens: vec![
                Address::repeat_byte(0x01),
                Address::repeat_byte(0x02),
                Address::repeat_byte(0x03),
            ],
            amp: 200,
            scaling_factors: vec![U256::from(10u64).pow(U256::from(18u64)); 3],
            swap_fee: 1_000_000_000_000_000u128, // 0.1% of 1e18
            bpt_idx,
            invariant_version,
            balances: balances.iter().map(|&b| U256::from(b)).collect(),
            update_block: block,
        }
    }

    #[test]
    fn register_and_read_back_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_stable_pool(&composable_params(
            10,
            &[1_000, 2_000, 3_000],
            Some(2),
            1,
        ));
        let s = core
            .get_balancer_stable_pool(pool_id)
            .expect("balancer stable pool registered");
        assert_eq!(s.n_tokens(), 3);
        assert_eq!(
            s.balances,
            vec![U256::from(1_000), U256::from(2_000), U256::from(3_000)]
        );
        assert_eq!(s.update_block, 10);
        // BPT-index round-trip — Some for Composable.
        assert_eq!(s.bpt_idx, Some(2));
        // invariant_version round-trip — V1.
        assert_eq!(s.invariant_version, 1);
        // Genesis anchor pushed.
        assert_eq!(core.balancer_stable_journal_len(pool_id), 1);
    }

    #[test]
    fn bpt_index_none_round_trip_for_meta_stable() {
        // MetaStablePool — bpt_idx None, invariant_version V2 (the
        // MetaStable path).
        let mut core = BotState::new();
        let pool_id = core.register_balancer_stable_pool(&composable_params(
            10,
            &[1_000, 2_000, 3_000],
            None,
            2,
        ));
        let s = core
            .get_balancer_stable_pool(pool_id)
            .expect("balancer stable pool registered");
        assert_eq!(s.bpt_idx, None);
        assert_eq!(s.invariant_version, 2);
    }

    #[test]
    fn apply_balance_update_journals_and_lands_new_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_stable_pool(&composable_params(
            10,
            &[1_000, 2_000, 3_000],
            Some(2),
            1,
        ));
        let affected = core.apply_balancer_stable_balance_update_by_pool_id(
            pool_id,
            vec![U256::from(1_500), U256::from(2_500), U256::from(3_500)],
            12,
        );
        assert_eq!(affected, Some(pool_id));
        let s = core
            .get_balancer_stable_pool(pool_id)
            .expect("balancer stable pool registered");
        assert_eq!(
            s.balances,
            vec![U256::from(1_500), U256::from(2_500), U256::from(3_500)]
        );
        assert_eq!(s.update_block, 12);
        // Genesis + the new transition delta.
        assert_eq!(core.balancer_stable_journal_len(pool_id), 2);
    }

    #[test]
    fn apply_balance_update_is_silent_noop_on_v2_pool() {
        let mut core = BotState::new();
        // Register a V2 pool at pool_id 1, then try the stable apply path.
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
        let affected =
            core.apply_balancer_stable_balance_update_by_pool_id(v2, vec![U256::from(1_500)], 5);
        assert!(
            affected.is_none(),
            "Balancer stable apply on a V2 pool must be a silent no-op"
        );
    }

    #[test]
    fn restore_before_block_lands_at_prior_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_stable_pool(&composable_params(
            10,
            &[1_000, 2_000, 3_000],
            Some(2),
            1,
        ));
        let _ = core.apply_balancer_stable_balance_update_by_pool_id(
            pool_id,
            vec![U256::from(1_500), U256::from(2_500), U256::from(3_500)],
            12,
        );
        // Restore to before block 12 → landed-at = the genesis registration
        // balances (the largest delta strictly below 12).
        let restored = core
            .balancer_stable_restore_before_block(pool_id, 12)
            .expect("Some(Ok) on a registered Balancer stable pool")
            .expect("Ok (target > genesis block)");
        assert_eq!(
            restored.0,
            vec![U256::from(1_000), U256::from(2_000), U256::from(3_000)]
        );
        assert_eq!(restored.1, 10);
        // Current mutable state was written back.
        let s = core
            .get_balancer_stable_pool(pool_id)
            .expect("balancer stable pool registered");
        assert_eq!(
            s.balances,
            vec![U256::from(1_000), U256::from(2_000), U256::from(3_000)]
        );
        assert_eq!(s.update_block, 10);
    }

    #[test]
    fn restore_to_before_registration_is_an_error() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_stable_pool(&composable_params(
            10,
            &[1_000, 2_000, 3_000],
            Some(2),
            1,
        ));
        // Target at the registration block → rolling back past registration.
        let res = core
            .balancer_stable_restore_before_block(pool_id, 10)
            .expect("Some on registered pool");
        assert!(
            res.is_err(),
            "restoring to the registration block must error"
        );
    }
}

//! Balancer V2 **weighted** pool state — the `BotState`-owned home for a
//! Balancer V2 weighted pool (`BalancerV2Pool`, weighted product invariant).
//!
//! ADR-005 slice 12a — the **weighted-state-port** sub-slice of slice 12
//! (the plan's "state port" sub-slice for the weighted variant, mirroring
//! what slice 11a did for Curve before the Curve companion rewrite + the
//! Curve pure-math port). Adds the Balancer weighted family as a fourth
//! `PoolEntry` variant — alongside V2/V3/V4/Curve (ADR-003 "third family"
//! was a Curve-only reservation; Balancer weighted + stable grow that set
//! to a sixth variant in slice 12c).
//!
//! Scope of THIS sub-slice: the Rust state struct +
//! `register_balancer_weighted_pool` +
//! `apply_balancer_weighted_balance_update_by_pool_id` + journal
//! restore/discard + `PyBot.register_balancer_weighted_pool` + Balancer
//! weighted read getters on `PyLiquidityPool`
//! (`balancer_balances`/`balancer_update_block`/`snapshot_balancer_weighted`/
//! `n_balancer_tokens`/`balancer_vault_tokens`/`balancer_weights`/
//! `balancer_scaling_factors`/`balancer_swap_fee`/`balancer_pow_version`).
//! The Python `BalancerV2Pool` companion rewrite (delegating `self._state`
//! to the handle) + `make_balancer_weighted_pool` factory + `BalancerBuilder`
//! weighted-branch migration are deferred to follow-on sub-slice 12b. The
//! pure-math Rust port (`WeightedMath`/`FixedPoint`/`LogExpMath`) + rate-cache-
//! aware providers (`CacheAwareRateProvider`, stable-pool-only) are deferred
//! to 12e and 12d respectively.
//!
//! ## State shape
//!
//! Balancer V2 weighted pools:
//! - **N tokens** (not 2) — the Vault holds the pool's tokens centrally
//!   (`tokens: Vec<Address>`); the pool contract holds the math.
//! - A 32-byte `pool_id` encoding `(address, specialization, nonce)` — used
//!   by the singleton Vault to route swaps.
//! - **Normalized weights** (`weights: Vec<U256>`, 18-decimal) — one per
//!   token, summing to `ONE` (1e18). Drives the weighted product invariant.
//! - **Scaling factors** (`scaling_factors: Vec<U256>`, `10**(18-decimals)`)
//!   — precomputed at registration from token decimals; the builder resolves
//!   these so Rust never touches ERC20 decimals.
//! - A **swap fee** as a fraction of `FEE_DENOMINATOR = 1e18`. Stored as the
//!   Python-computed `int(fee * FEE_DENOMINATOR)`.
//! - **`PowVersion`** (V1/V2) — controls `FixedPoint.pow` fast paths; carried
//!   as opaque `u8` for the (future, 12e) Rust `WeightedMath` port.
//!
//! The mutable slot is just `balances` + `update_block` — the only fields a
//! Balancer `external_update` (new balances from a Vault `PoolBalanceChanged`
//! event) mutates. Everything else is immutable config set at registration.
//!
//! Matches `CurvePoolState`'s discipline (ADR-005 slice 11a): a full-state
//! `balances_before/after` delta + a V2-style genesis anchor at registration.

use alloy::primitives::{Address, U256};

use crate::bot_core::state_history::{BlockDelta, ReorgJournal};

// ---------------------------------------------------------------------------
// Block delta
// ---------------------------------------------------------------------------

/// Per-block delta for a Balancer V2 weighted pool.
///
/// Stores the **before** balances captured at the moment a block's
/// `external_update` was applied — used for reorg rollback. Mirrors
/// [`crate::bot_core::state_history::V2BlockDelta`] and
/// [`crate::bot_core::CurveBlockDelta`]: a full-state delta (N balances +
/// block), with `balances_before` redundant with the preceding delta's
/// `balances_after` (retained for a self-describing record).
///
/// A genesis delta is pushed at registration (`before == after == registration
/// balances`, at `block = update_block`) so `restore_before_block` can land on
/// the registration state — the same anchor discipline as V2/Curve (the
/// Balancer journal carries a genesis anchor; V3/V4 do not, because their
/// first forward event's "before" IS the registration state).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalancerWeightedBlockDelta {
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

impl BlockDelta for BalancerWeightedBlockDelta {
    fn block(&self) -> u64 {
        self.block
    }

    // Same full-state delta shape as V2/Curve; the default no-op coalesce is
    // correct (`restore_before_block` reads the surviving delta's
    // `balances_after`).
}

// ---------------------------------------------------------------------------
// Registration params + state struct
// ---------------------------------------------------------------------------

/// Parameters for registering a Balancer V2 weighted pool with `BotState`.
///
/// Carries immutable pool config (address, vault singleton, 32-byte pool ID,
/// tokens, weights, scaling factors, swap fee, `PowVersion`) + the
/// registration-time mutable state (`balances`, `update_block`).
///
/// `balances.len()` MUST equal `tokens.len()`, `weights.len()`, and
/// `scaling_factors.len()`.
#[derive(Clone, Debug)]
pub struct RegisterBalancerWeightedPoolParams {
    /// Pool contract address (the pool contract that stores the invariant logic).
    pub address: Address,
    /// The Balancer V2 singleton Vault contract address.
    pub vault: Address,
    /// 32-byte Balancer V2 pool identifier (encodes pool address +
    /// specialization + nonce). Carried as a fixed array since the bytes are
    /// a Balancer invariant, not an address.
    pub pool_id: [u8; 32],
    /// The pool's ERC-20 tokens (2 or more), in canonical Vault order.
    pub tokens: Vec<Address>,
    /// Normalized weights (18-decimal; sum to `ONE = 1e18`), one per token.
    pub weights: Vec<U256>,
    /// Scaling factors (`10**(18 - token_decimals)`, 18-decimal), one per
    /// token — precomputed by the builder so Rust never resolves ERC20
    /// decimals.
    pub scaling_factors: Vec<U256>,
    /// Swap fee as a fraction of `FEE_DENOMINATOR = 1e18`
    /// (`int(fee_fraction * 1e18)`). Applied by the (future, 12e) Rust
    /// `WeightedMath` swap; opaque to this sub-slice.
    pub swap_fee: u128,
    /// `PowVersion` discriminator (V1=1 = `WeightedPool2Tokens` general path;
    /// V2=2 = `WeightedPool` fast paths). Opaque `u8` for the future Rust
    /// math port; this sub-slice doesn't dispatch on it.
    pub pow_version: u8,

    // --- Mutable state at registration ---
    /// Initial balances (one per token), captured at `update_block`.
    pub balances: Vec<U256>,
    /// Block number of the registration state — seeds the genesis reorg
    /// journal delta (ADR-005 slice 4).
    pub update_block: u64,
}

/// Balancer V2 weighted pool state owned by [`crate::bot_core::BotState`].
///
/// Carries the immutable config captured at registration + the mutable
/// `balances`/`update_block` slot + a per-pool reorg journal. The state-port
/// sub-slice (ADR-005 slice 12a): the Python `BalancerV2Pool` companion
/// (12b) will read `balances`/`update_block` from this struct via
/// `PyLiquidityPool` getters and delegate `external_update` to
/// `apply_balancer_weighted_balance_update_by_pool_id`.
#[derive(Clone, Debug)]
pub struct BalancerWeightedPoolState {
    /// Pool contract address.
    pub address: Address,
    /// The Balancer V2 singleton Vault address.
    pub vault: Address,
    /// 32-byte pool identifier (Balancer invariant; unique per pool).
    pub pool_id: [u8; 32],
    /// The pool's ERC-20 tokens (2 or more), in canonical Vault order.
    pub tokens: Vec<Address>,
    /// Normalized weights (18-decimal), one per token.
    pub weights: Vec<U256>,
    /// Scaling factors (18-decimal), one per token.
    pub scaling_factors: Vec<U256>,
    /// Swap fee as a fraction of `FEE_DENOMINATOR = 1e18`.
    pub swap_fee: u128,
    /// `PowVersion` discriminator (V1=1 / V2=2; opaque to this sub-slice).
    pub pow_version: u8,

    // --- Mutable state (authoritative) ---
    /// Current balances (one per token).
    pub balances: Vec<U256>,
    /// Block number of the last balance update.
    pub update_block: u64,

    /// Reorg journal — balance priors for rollback.
    pub journal: ReorgJournal<BalancerWeightedBlockDelta>,
}

impl BalancerWeightedPoolState {
    /// Construct from registration params with a journal of the given depth.
    /// Pushes a genesis anchor delta (mirror of V2/Curve's discipline) so
    /// `restore_before_block` can land on the registration state.
    #[must_use]
    pub fn from_params(params: RegisterBalancerWeightedPoolParams, journal_depth: usize) -> Self {
        let mut journal = ReorgJournal::<BalancerWeightedBlockDelta>::new(journal_depth);
        // Genesis anchor: before == after == registration balances at
        // update_block. The "landed-at" registration point.
        journal.push_delta(BalancerWeightedBlockDelta {
            block: params.update_block,
            balances_before: params.balances.clone(),
            balances_after: params.balances.clone(),
        });
        Self {
            address: params.address,
            vault: params.vault,
            pool_id: params.pool_id,
            tokens: params.tokens,
            weights: params.weights,
            scaling_factors: params.scaling_factors,
            swap_fee: params.swap_fee,
            pow_version: params.pow_version,
            balances: params.balances,
            update_block: params.update_block,
            journal,
        }
    }

    /// Number of tokens (== number of balances/weights/scaling factors).
    #[must_use]
    pub fn n_tokens(&self) -> usize {
        self.tokens.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot_core::{BotState, RegisterBalancerWeightedPoolParams};

    /// Two-token weighted pool fixture (mirrors a Curve two-coin helper).
    fn two_token_params(block: u64, balances: &[u64]) -> RegisterBalancerWeightedPoolParams {
        RegisterBalancerWeightedPoolParams {
            address: Address::repeat_byte(0xb1),
            vault: Address::repeat_byte(0xba),
            pool_id: [0u8; 32],
            tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
            weights: vec![
                U256::from(500_000_000_000_000_000u128),
                U256::from(500_000_000_000_000_000u128),
            ],
            scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
            swap_fee: 1_000_000_000_000_000u128, // 0.1% of 1e18
            pow_version: 1,
            balances: balances.iter().map(|&b| U256::from(b)).collect(),
            update_block: block,
        }
    }

    #[test]
    fn register_and_read_back_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_weighted_pool(&two_token_params(10, &[1_000, 2_000]));
        let s = core
            .get_balancer_weighted_pool(pool_id)
            .expect("balancer weighted pool registered");
        assert_eq!(s.n_tokens(), 2);
        assert_eq!(s.balances, vec![U256::from(1_000), U256::from(2_000)]);
        assert_eq!(s.update_block, 10);
        // Genesis anchor pushed.
        assert_eq!(core.balancer_weighted_journal_len(pool_id), 1);
    }

    #[test]
    fn apply_balance_update_journals_and_lands_new_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_weighted_pool(&two_token_params(10, &[1_000, 2_000]));
        let affected = core.apply_balancer_weighted_balance_update_by_pool_id(
            pool_id,
            vec![U256::from(1_500), U256::from(2_500)],
            12,
        );
        assert_eq!(affected, Some(pool_id));
        let s = core
            .get_balancer_weighted_pool(pool_id)
            .expect("balancer weighted pool registered");
        assert_eq!(s.balances, vec![U256::from(1_500), U256::from(2_500)]);
        assert_eq!(s.update_block, 12);
        // Genesis + the new transition delta.
        assert_eq!(core.balancer_weighted_journal_len(pool_id), 2);
    }

    #[test]
    fn apply_balance_update_is_silent_noop_on_v2_pool() {
        let mut core = BotState::new();
        // Register a V2 pool at pool_id 1, then try the Balancer apply path.
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
            core.apply_balancer_weighted_balance_update_by_pool_id(v2, vec![U256::from(1_500)], 5);
        assert!(
            affected.is_none(),
            "Balancer weighted apply on a V2 pool must be a silent no-op"
        );
    }

    #[test]
    fn restore_before_block_lands_at_prior_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_weighted_pool(&two_token_params(10, &[1_000, 2_000]));
        let _ = core.apply_balancer_weighted_balance_update_by_pool_id(
            pool_id,
            vec![U256::from(1_500), U256::from(2_500)],
            12,
        );
        // Restore to before block 12 → landed-at = the genesis registration
        // balances (the largest delta strictly below 12).
        let restored = core
            .balancer_weighted_restore_before_block(pool_id, 12)
            .expect("Some(Ok) on a registered Balancer weighted pool")
            .expect("Ok (target > genesis block)");
        assert_eq!(restored.0, vec![U256::from(1_000), U256::from(2_000)]);
        assert_eq!(restored.1, 10);
        // Current mutable state was written back.
        let s = core
            .get_balancer_weighted_pool(pool_id)
            .expect("balancer weighted pool registered");
        assert_eq!(s.balances, vec![U256::from(1_000), U256::from(2_000)]);
        assert_eq!(s.update_block, 10);
    }

    #[test]
    fn restore_to_before_registration_is_an_error() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_weighted_pool(&two_token_params(10, &[1_000, 2_000]));
        // Target at the registration block → rolling back past registration.
        let res = core
            .balancer_weighted_restore_before_block(pool_id, 10)
            .expect("Some on registered pool");
        assert!(
            res.is_err(),
            "restoring to the registration block must error"
        );
    }
}

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
//!   Balancer stable read getters on `PyLiquidityPool`
//!   (`balancer_stable_balances`, `balancer_stable_update_block`,
//!   `snapshot_balancer_stable`, `n_balancer_stable_tokens`, `balancer_amp`,
//!   `balancer_bpt_index`, `balancer_invariant_version`,
//!   `balancer_stable_scaling_factors`, `balancer_stable_swap_fee`). The
//!   Python `BalancerV2StablePool` companion
//!   rewrite (delegating `self._state` to the handle) +
//!   `make_balancer_stable_pool` factory + `BalancerBuilder` stable-branch
//!   migration + `CacheAwareRateProvider`-in-Rust + `StaleRateResult` are
//!   deferred to follow-on sub-slice 12d. The pure-math Rust port
//!   (`StableMath` + `FixedPoint` / `LogExpMath`) is deferred to 12e.
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

use crate::rate_provider::BalancerRateProvider;
use crate::state_history::{BalancesBlockDelta, JournalError, ReorgJournal, ReorgPoolState};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// Block delta
// ---------------------------------------------------------------------------

// Per-block delta for a Balancer V2 stable pool.
//
// ADR-014 D3a: the family-specific `BalancerStableBlockDelta` was a
// byte-identical full-state delta to the Curve/Balancer-weighted twins;
// it's unified into the shared `BalancesBlockDelta` in `state_history.rs`.
// The stable pool journal is now `ReorgJournal<BalancesBlockDelta>`.

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
    /// and swap calculations.
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

    /// Off-chain rate provider (ADR-005 slice 12c I/O trait object). `None`
    /// is equivalent to a static provider returning `1e18` for every token
    /// (no rate-multiplied scaling). When `Some`, the (future, 12d) Python
    /// companion delegates `scaling_factors` refresh to it; the pure-Rust
    /// `StaticRateProvider` is the no-I/O fallback.
    pub rate_provider: Option<Arc<dyn BalancerRateProvider>>,
}

/// Immutable Balancer V2 stable pool registration identity (ADR-005
/// identity slice).
///
/// Pure registration data — the pool's permanent identity. Mirrors the
/// other `VxPoolIdentity` structs / `TokenEntry`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalancerStablePoolIdentity {
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
    /// sub-slice). The systematic-1-wei-error guard.
    pub invariant_version: u8,
}

/// Balancer V2 stable pool state owned by [`crate::BotState`].
///
/// Carries the mutable `balances`/`update_block` slot + a per-pool reorg
/// journal. Immutable identity lives on [`BalancerStablePoolIdentity`]
/// (look it up via
/// [`crate::BotState::get_balancer_stable_identity`]). The
/// state-port sub-slice (ADR-005 slice 12c): the Python `BalancerV2StablePool`
/// companion (12d) reads `balances`/`update_block` from this struct via
/// `PyLiquidityPool` getters and delegates `external_update` to
/// `apply_balancer_stable_balance_update_by_pool_id`.
#[derive(Clone, Debug)]
pub struct BalancerStablePoolState {
    // --- Mutable state (authoritative) ---
    /// Current balances (one per token, including BPT for Composable).
    pub balances: Vec<U256>,
    /// Block number of the last balance update.
    pub update_block: u64,

    /// Reorg journal — balance priors for rollback.
    pub journal: ReorgJournal<BalancesBlockDelta>,

    /// Off-chain rate provider (ADR-005 slice 12c I/O trait object). `None`
    /// ⇔ static `1e18` rates. Stored so the (future, 12d) companion can
    /// refresh scaling factors at calc time without re-entering Python for
    /// the static fallback.
    pub rate_provider: Option<Arc<dyn BalancerRateProvider>>,
}

impl BalancerStablePoolIdentity {
    /// Number of tokens (== number of balances/scaling factors; includes
    /// BPT for Composable pools).
    #[must_use]
    pub fn n_tokens(&self) -> usize {
        self.tokens.len()
    }
}

impl BalancerStablePoolState {
    /// Construct the (immutable identity, mutable state) pair from
    /// registration params, with a journal of the given depth.
    /// Pushes a genesis anchor delta (mirror of V2/Curve/BalancerWeighted
    /// discipline) so `restore_before_block` can land on the registration
    /// state.
    #[must_use]
    pub fn from_params(
        params: RegisterBalancerStablePoolParams,
        journal_depth: usize,
    ) -> (BalancerStablePoolIdentity, BalancerStablePoolState) {
        let mut journal = ReorgJournal::<BalancesBlockDelta>::new(journal_depth);
        // Genesis anchor: before == after == registration balances at
        // update_block. The "landed-at" registration point.
        journal.push_delta(BalancesBlockDelta {
            block: params.update_block,
            balances_before: params.balances.clone(),
            balances_after: params.balances.clone(),
        });
        let identity = BalancerStablePoolIdentity {
            address: params.address,
            vault: params.vault,
            pool_id: params.pool_id,
            tokens: params.tokens,
            amp: params.amp,
            scaling_factors: params.scaling_factors,
            swap_fee: params.swap_fee,
            bpt_idx: params.bpt_idx,
            invariant_version: params.invariant_version,
        };
        let state = BalancerStablePoolState {
            balances: params.balances,
            update_block: params.update_block,
            journal,
            rate_provider: params.rate_provider,
        };
        (identity, state)
    }
}

// ADR-014 D3 refinement — pool-owned reorg rollback for the balance-vector
// family. The field-write previously duplicated across the three
// `BotState::*_restore_before_block` dispatchers (Curve + Balancer weighted +
// stable) is absorbed into the state struct itself; restore returns `()` so a
// single non-generic trait covers the family with no no-op stubs. The two
// sibling balance-vector structs get byte-identical impls in follow-on slices.
impl ReorgPoolState for BalancerStablePoolState {
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
}

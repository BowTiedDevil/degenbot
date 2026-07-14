//! V2 constant-product pool identity/state + registration DTOs.
//! **Relocated** from `degenbot-bot/src/bot_core/mod.rs`; re-exported there
//! at the historical `bot_core::V2*`/`RegisterV2*` path.

use crate::state_history::{ReorgJournal, V2BlockDelta};
use alloy::primitives::{aliases::U112, Address, B256};
use degenbot_uniswap::dex_identity::DexVariant;

/// Immutable V2 registration identity (ADR-005 identity slice).
///
/// Pure registration data — the pool's permanent identity, set once at
/// `register_v2_pool` and never mutated. Mirrors [`TokenEntry`] (one immutable
/// identity struct per registry entry, no mutable half). Distinct from
/// [`V2PoolState`], which carries only mutable runtime data (reserves +
/// journal + update block).
///
/// This completes the half-done ADR-005 split: the former `V2PoolDescriptor`
/// (`variant/stable_swap/fee_denominator`) is folded in here, alongside the
/// level-2 identity (address/tokens/fees/factory) that previously sat on the
/// mutable `V2PoolState`. The `PyLiquidityPool` handle reads all of this
/// through [`BotState::get_v2_identity`] — the Polars `_from_pydf` end state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V2PoolIdentity {
    /// Pool contract address.
    pub address: Address,
    /// Token0 contract address.
    pub token0: Address,
    /// Token1 contract address.
    pub token1: Address,
    /// Fee parameters for token0→token1 swaps: (`gamma_numer`, `fee_denom`).
    pub fee_token0: (u64, u64),
    /// Fee parameters for token1→token0 swaps: (`gamma_numer`, `fee_denom`).
    pub fee_token1: (u64, u64),
    /// Pool factory address.
    pub factory: Address,
    /// The CREATE2 deployer this pool's address was verified against (Fork A,
    /// NSAZ4X). The JSON row's `deployer` (or `factory` for null), or the
    /// factory itself for non-JSON pools. The `dex` getter merges this per-
    /// (chain,factory) deployer into the protocol preset (replacing the
    /// canonical-mainnet preset deployer).
    pub deployer: Address,
    /// The CREATE2 init code hash (Fork A, NSAZ4X). The JSON row's `init_hash`,
    /// or the V2 mainnet fallback const for non-JSON pools.
    pub init_hash: B256,
    /// The DEX+variant discriminator. Resolves the canonical
    /// [`degenbot_uniswap::dex_identity::DexIdentity`] preset
    /// (factory/deployer/init-hash/default fees/ABI shape) for encoding and
    /// `_verified_address` derivation.
    pub variant: DexVariant,
    /// Camelot solidly-stable strategy flag (false for all non-Camelot V2).
    pub stable_swap: bool,
    /// Camelot's integer fee scaling (used by the solidly-stable math). `None`
    /// for non-Camelot V2 (the volatile calc ignores it).
    pub fee_denominator: Option<u64>,
}

/// Mutable runtime state for a Uniswap V2-style constant-product pool.
///
/// Pure mutable data — the values that change as reserves are pumped and
/// rolled back. Immutable identity lives on [`V2PoolIdentity`]; look it up via
/// [`BotState::get_v2_identity`]. The two are paired in [`PoolEntry::V2`].
///
/// The reserves are typed `U112` to mirror v2-core's on-chain `uint112`
/// storage width (`UniswapV2Pair._update`'s
/// `require(balance0 <= uint112(-1))`). The spec-bound admission contract
/// (ADR-012 / epic `WOYYS2`) enforces this width at `register_v2_pool`;
/// typing the field `U112` makes that contract a *type-level* guarantee —
/// the runtime `validate_v2_reserve` check below it is a tautology against
/// `U112::MAX`, retained for diagnostic-message uniformity with the other
/// `spec_bounds` validators. The deeper fix forward-referenced in commit
/// `19218a2c` ("a multiply-precision dodge") lands here.
#[derive(Clone, Debug)]
pub struct V2PoolState {
    /// Current reserve of token0 (on-chain `uint112`).
    pub reserve0: U112,
    /// Current reserve of token1 (on-chain `uint112`).
    pub reserve1: U112,
    /// Block number of the last update.
    pub update_block: u64,

    /// Reorg journal — "before" values for rollback.
    /// V2 is the degenerate case: delta = full state (two reserves).
    pub journal: ReorgJournal<V2BlockDelta>,
}

/// Parameters for registering a V2 pool.
#[derive(Clone, Debug, Default)]
pub struct RegisterV2PoolParams {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    /// Reserves typed `U112` to match v2-core's on-chain `uint112` storage
    /// width.
    pub reserve0: U112,
    pub reserve1: U112,
    pub fee_token0: (u64, u64),
    pub fee_token1: (u64, u64),
    pub factory: Address,
    /// The CREATE2 deployer the Rust builder verified this pool's address
    /// against (Fork A, NSAZ4X). Equals the JSON row's `deployer`, or
    /// `factory` when the row had `null` (the `None -> factory` convention).
    /// For non-JSON pools, the factory itself. Stored on the identity so the
    /// `dex` getter merges the per-(chain,factory) deployer (no getter-time
    /// `chain_id` lookup).
    pub deployer: Address,
    /// The CREATE2 init code hash the builder verified against (Fork A,
    /// NSAZ4X). The JSON row's `init_hash`; the V2 mainnet fallback const
    /// for non-JSON pools.
    pub init_hash: B256,
    /// Block number of the registration state — seeds the genesis reorg
    /// journal delta (ADR-005 slice 4). The landed-at journal must anchor the
    /// registration state at a real block so `restore_before_block` can land
    /// on it; pre-slice-4 the journal was empty until the first Sync.
    pub update_block: u64,
    /// DEX+variant discriminator (registration metadata — see
    /// [`V2PoolIdentity`]).
    pub variant: DexVariant,
    /// Camelot solidly-stable strategy flag.
    pub stable_swap: bool,
    /// Camelot integer fee scaling (the solidly-stable math's denominator).
    pub fee_denominator: Option<u64>,
}

/// Typed rejection from [`BotState::register_v2_pool`] (the spec-bound +
/// duplicate-address admission contract — see [`spec_bounds`]).
///
/// Mirrors [`RegisterV4PoolError`]: `#[derive(Clone, Debug, PartialEq, Eq)]`,
/// no `Display`/`Error` impl (the `PyO3` mapper pattern-matches the variants
/// directly and constructs Python exceptions via `format!`).
///
/// Variants:
/// - [`AlreadyRegistered`](Self::AlreadyRegistered) — replaces the prior
///   `assert!` duplicate-check panic (`assert!(!pool_addresses.contains_key(..))`).
/// - [`SpecViolation`](Self::SpecViolation) — wraps a
///   [`spec_bounds::SpecViolation`] from the validator helpers
///   (`validate_v2_reserve`); fires on `reserve{0,1} > uint112::MAX` (only
///   reachable for synthetic / corrupt registration — `Sync(uint112,uint112)`
///   events are structurally spec-bound).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegisterV2PoolError {
    /// A pool at this contract address is already registered.
    AlreadyRegistered { address: Address },
    /// An out-of-spec field (e.g. `reserve0 > uint112::MAX`).
    SpecViolation(crate::spec_bounds::SpecViolation),
}

impl From<crate::spec_bounds::SpecViolation> for RegisterV2PoolError {
    fn from(v: crate::spec_bounds::SpecViolation) -> Self {
        Self::SpecViolation(v)
    }
}

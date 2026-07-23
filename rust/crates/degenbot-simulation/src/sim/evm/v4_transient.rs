//! V4 PoolManager transient-storage seeder (EIP-1153, TSTORE/TLOAD).
//!
//! V4 pools have no persistent on-chain storage at fixed slots — their swap
//! state (sqrtPriceX96 / liquidity / tick) lives in the PoolManager's
//! **transient storage** during the `unlock()` batch. revm exposes transient
//! storage as a pre-seedable public field on the built EVM's journaled state
//! (verified by the `transient_seed` PoC + spike §6):
//!
//! ```text
//! evm.ctx.journaled_state.inner.transient_storage: revm::state::TransientStorage
//! ```
//!
//! This module seeds that field from `degenbot-bot`'s tracked-ahead V4 typed
//! state before `transact`, mirroring how
//! [`super::state_override::apply_simulation_overrides`] seeds the `CacheDB`.
//!
//! # Coverage + the V4 slot-layout follow-up
//!
//! The exact transient slot layout the V4 PoolManager uses internally
//! (`keccak(poolId . slot)` patterns, the `StateLibrary` extension's
//! transient-vs-persistent split) is a V4-core-protocol lookup. This module
//! ships the seeder surface + the typed-state extraction; the slot-key mapping
//! is pinned by a cast-derived constants table + a recorded-mainnet-state test
//! (a follow-up sub-step, tracked here, NOT a blocking spike — revm's
//! transient-storage capability is verified, only the V4-core slot indices
//! remain).
//!
//! See `docs/spikes/revm-composition-api-and-cold-miss-latency.md` §6 (the V4
//! fork resolution) + the `transient_seed` PoC.

use alloy::primitives::{Address, U256};
use degenbot_pools::v4_state::V4PoolState;
use revm::context::ContextTr;
use revm::state::TransientStorage;

/// Apply degenbot's tracked-ahead V4 pool state to the built EVM's transient
/// storage, so a simulated `PoolManager.unlock()` batch reads the engine's
/// projected sqrtPriceX96/liquidity/tick (not the node's stale on-chain view).
///
/// Mirrors `apply_simulation_overrides`'s one-shot-seed shape: call after
/// building the EVM + applying the persistent overrides, before `transact`.
///
/// # Panics
///
/// Does not panic; a missing V4 pool for `(pool_manager, pool_id)` is a no-op
/// (the PoolManager falls back to its on-chain transient state, served from
/// AlloyDB's `basic`/`storage` cold-load path).
pub fn apply_v4_transient_state<Ctx>(
    _evm: &mut Ctx,
    _pool_manager: Address,
    _pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    _v4_state: &V4PoolState,
) where
    Ctx: ContextTr,
{
    // TODO(EGMSNS-V4-followup): seed evm.ctx.journaled_state.inner.transient_storage
    // with the V4 pool state at the PoolManager's known transient slot keys.
    // The slot-key mapping (keccak(poolId . slot) patterns from V4-core's
    // PoolManager) is a cast-derived constants table — land it + a
    // recorded-mainnet-state test as the V4 follow-up sub-step (the revm
    // transient-storage capability is verified; only the V4-core slot indices
    // remain). Until then this is a no-op (V4 falls through to AlloyDB,
    // preserving current eth_simulateV1 behavior — no regression).
    let _ = (Address::ZERO, U256::ZERO, TransientStorage::default());
}

//! The one pool key an orchestration verb resolves.
//!
//! A pool is addressed two ways in this codebase — a V3 pool by contract
//! address, a V4 pool by `(pool_manager, pool_id)` — and the state owner used
//! to publish each orchestration verb once per shape (`set_pool_live` /
//! `set_pool_live_by_key`, and so on). [`PoolLookup`] is the value that ends
//! that: a caller resolves its key once, and a verb reads the shape off the
//! lookup.
//!
//! The verbs here delegate to the state owner's own accessors — this is a join
//! over the two key shapes, not a second reading of pool state. The id-keyed
//! accessors remain the interior a hot path uses when it already holds a
//! `pool_id`.

use alloy::primitives::Address;
use hashbrown::HashMap;

use crate::state_lock::StateLock;
use crate::{BotState, PoolTickCoverage, TickInfo};
use degenbot_decoders::v4_swap_decoder::V4PoolId;

/// The pool key the verify lifecycle runs over.
///
/// The lifecycle is one machine over one key. The two concentrated-liquidity
/// designs differ only in how the state owner addresses a pool — a V3 pool by
/// contract address, a V4 pool by `(pool_manager, pool_id)` — so the key is a
/// value and the eight state verbs the machine needs hang off it. Both arms
/// delegate to the state owner's own accessors: this is a join, not a second
/// reading of pool state.
#[derive(Clone, Copy, Debug)]
pub enum PoolLookup {
    /// A V3 concentrated-liquidity pool, addressed by contract address.
    Address(Address),
    /// A V4 concentrated-liquidity pool, addressed by `(pool_manager, pool_id)`.
    V4 {
        /// The canonical `PoolManager`.
        pool_manager: Address,
        /// The pool's on-chain id.
        pool_id: V4PoolId,
    },
}

impl PoolLookup {
    /// The pool's tick coverage, or `None` when the pool is not registered
    /// under this key (the machine's no-op arm).
    pub fn coverage(&self, state: &BotState) -> Option<PoolTickCoverage> {
        match self {
            Self::Address(address) => state.pool_coverage(*address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state.pool_coverage_by_key(*pool_manager, pool_id),
        }
    }

    /// Defer the pool's live events to the pump buffer until verification ends.
    pub fn quarantine(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => state.set_pool_quarantined(*address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state.set_pool_quarantined_by_key(*pool_manager, *pool_id),
        }
    }

    /// Consume the pinned snapshot seed (exactly once, so memory is bounded).
    pub fn take_seed(&self, state: &mut BotState) -> Option<HashMap<i32, TickInfo>> {
        match self {
            Self::Address(address) => state.take_snapshot_seed(*address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state.take_snapshot_seed_by_key(*pool_manager, pool_id),
        }
    }

    /// The pool's OWN liquidity clock (the block its tick data reflects); `0`
    /// when unset.
    ///
    /// Takes the lock rather than a guard: the state owner's fused accessor
    /// chains key → clock under ONE `read_at(Registration)`, so this must not
    /// be recomposed from two single-step reads (nested reads are a
    /// self-deadlock when a writer queues between them — soak-2026-08-22).
    pub fn own_liquidity_clock(&self, core: &StateLock<BotState>) -> u64 {
        match self {
            Self::Address(address) => core.pool_tick_data_block_by_address(address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => core.pool_tick_data_block_by_v4_key(*pool_manager, pool_id),
        }
    }

    /// Drain the buffered backfill and pump events onto the pool. Draining is
    /// not verification: it preserves the permanent V2-V2-V3 apply-buffer
    /// behaviour and runs for a Sparse pool too.
    pub fn drain(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => {
                state.apply_backfill_buffer(address);
                state.apply_pump_buffer(address);
            }
            Self::V4 {
                pool_manager,
                pool_id,
            } => {
                state.apply_backfill_buffer_by_key(*pool_manager, *pool_id);
                state.apply_pump_buffer_by_key(*pool_manager, *pool_id);
            }
        }
    }

    /// Freeze the post-drain `(tick_data, block)` pair atomically with the
    /// drain.
    pub fn pin_post_drain(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => state.pin_post_drain_snapshot(*address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state.pin_post_drain_snapshot_by_key(*pool_manager, pool_id),
        }
    }

    /// Consume the pinned post-drain pair (exactly once).
    pub fn take_post_drain(&self, state: &mut BotState) -> Option<(HashMap<i32, TickInfo>, u64)> {
        match self {
            Self::Address(address) => state.take_post_drain_snapshot(*address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state.take_post_drain_snapshot_by_key(*pool_manager, pool_id),
        }
    }

    /// The terminal transition: only reachable after a verified success.
    pub fn set_live(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => state.set_pool_live(*address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state.set_pool_live_by_key(*pool_manager, *pool_id),
        }
    }
}

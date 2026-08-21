//! `SimAnchorState` — the owned anchor snapshot the sim fan-out reads
//! (ULUWNI, incident 2026-08-20 #1 root fix).
//!
//! Before this type, `BlockSimHandle::build` borrowed `&BotState` for the
//! whole serial sim loop — every cold-miss fetch went over RPC while the
//! read guard was held, parking any writer behind it for the transport
//! delay (minutes on a stalled RPC pre-F2). Now the fan-out takes a SHORT
//! read, snapshots what the sim actually consults, and drops the guard
//! before any provider I/O.
//!
//! What the sim consults through [`super::BotState`] (the complete surface —
//! verified by the ULUWNI audit):
//!
//! 1. `pool_id_by_address` — the `basic_ref` code-less tripwire's tracked-
//!    pool lookup. Snapshotted verbatim (`tracked_pools`).
//! 2. `probe_tracked_storage_slot` — BOTH env-gated diagnostics (divergence
//!    probe + serving seam, default off). Snapshotted for the SCALAR anchor
//!    slots: V2 reserves (slot 8), V3 `slot0`/`liquidity`, V4
//!    `S_state`/`S_state+3`. Per-tick slots are NOT snapshotted (tick data
//!    is unbounded per pool; cloning it per fan-out would defeat the
//!    purpose) — tick-slot probes fall through to RPC in snapshot mode,
//!    i.e. divergence observation for tick slots is suspended. Both
//!    diagnostics are default-off investigation tools whose stale-state
//!    premise was refuted (see `bot_state_db` module docs); scalar coverage
//!    preserves their re-probe value at O(pools) snapshot cost.

use std::collections::HashMap;

use alloy::primitives::{Address, U256};

use super::divergence_probe::{derive_v4_pool_state_base, TrackedSlotProbe};
use super::BotState;

/// Owned snapshot of what the sim fan-out consults on [`BotState`] — see
/// the module docs for the surface audit + the tick-slot degradation note.
#[derive(Debug, Clone, Default)]
pub struct SimAnchorState {
    /// Every tracked pool's address → pool id (the `basic_ref` tripwire).
    tracked_pools: HashMap<Address, u64>,
    /// Scalar anchor words per tracked pool, keyed `(address, slot)`.
    anchor_words: HashMap<(Address, U256), TrackedSlotProbe>,
}

impl SimAnchorState {
    /// Snapshot the anchor under a SHORT read (O(pools); no tick data).
    #[must_use]
    pub fn snapshot(state: &BotState) -> Self {
        let mut anchor = Self {
            tracked_pools: state.pool_addresses.clone(),
            anchor_words: HashMap::new(),
        };
        // Scalar slots for the address-keyed families (V2 reserves slot 8;
        // V3 slot0 = 0 / liquidity = 4). `probe_tracked_storage_slot`
        // answers None for non-matching indexes/variants — insert only hits.
        for &address in state.pool_addresses.keys() {
            for index in [U256::ZERO, U256::from(4u64), U256::from(8u64)] {
                if let Some(probe) = state.probe_tracked_storage_slot(address, index) {
                    anchor.anchor_words.insert((address, index), probe);
                }
            }
        }
        // V4: keyed by (PoolManager, pool-id-derived S_state base) — the
        // address→pool_id map does NOT cover V4.
        for (pm, pool_id_bytes) in state.v4_pool_ids.keys() {
            let s_state = derive_v4_pool_state_base(pool_id_bytes);
            for index in [
                s_state,
                s_state.checked_add(U256::from(3u64)).unwrap_or(U256::MAX),
            ] {
                if let Some(probe) = state.probe_tracked_storage_slot(*pm, index) {
                    anchor.anchor_words.insert((*pm, index), probe);
                }
            }
        }
        anchor
    }

    /// The tracked-pool lookup (the `basic_ref` code-less tripwire).
    #[must_use]
    pub fn pool_id_by_address(&self, address: &Address) -> Option<u64> {
        self.tracked_pools.get(address).copied()
    }

    /// The snapshotted engine word for a tracked slot, if covered. Tick
    /// slots are not snapshotted (see module docs) — they return `None` and
    /// the caller falls through to RPC.
    #[must_use]
    pub fn probe_tracked_storage_slot(
        &self,
        address: Address,
        index: U256,
    ) -> Option<TrackedSlotProbe> {
        self.anchor_words.get(&(address, index)).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot_core::{RegisterV2PoolParams, RegisterV3PoolParams};
    use alloy::primitives::aliases::U112;
    use alloy::primitives::Address;
    use degenbot_uniswap::dex_identity::DexVariant;

    const POOL: Address = Address::new([0xaa; 20]);
    const EOA: Address = Address::new([0xee; 20]);

    #[expect(clippy::expect_used)]
    fn v2_state() -> BotState {
        let mut core = BotState::new();
        core.register_v2_pool(&RegisterV2PoolParams {
            address: POOL,
            token0: Address::new([0xbb; 20]),
            token1: Address::new([0xcc; 20]),
            reserve0: U112::from(1000),
            reserve1: U112::from(2000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::new([0xdd; 20]),
            update_block: 42,
            variant: DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        })
        .expect("test setup: V2 registration");
        core
    }

    /// The snapshot carries the tracked-address set: the tripwire lookup
    /// answers for tracked pools and stays silent for everything else.
    #[test]
    fn snapshot_carries_the_tracked_address_set() {
        let anchor = SimAnchorState::snapshot(&v2_state());
        assert_eq!(anchor.pool_id_by_address(&POOL), Some(1));
        assert_eq!(anchor.pool_id_by_address(&EOA), None);
    }

    /// Scalar anchor words survive the snapshot: V2 reserves (slot 8)
    /// probes with the registered reserves; other slots fall through.
    #[test]
    #[expect(clippy::expect_used)]
    fn snapshot_carries_scalar_anchor_words() {
        let state = v2_state();
        let anchor = SimAnchorState::snapshot(&state);
        let live = state
            .probe_tracked_storage_slot(POOL, U256::from(8u64))
            .expect("live probe answers V2 slot 8");
        let snapped = anchor
            .probe_tracked_storage_slot(POOL, U256::from(8u64))
            .expect("snapshot answers V2 slot 8");
        assert_eq!(live.engine_word, snapped.engine_word);
        assert_eq!(snapped.update_block, 42);
        // Uncovered slot → None (falls through to RPC in the sim DB).
        assert!(anchor
            .probe_tracked_storage_slot(POOL, U256::from(9u64))
            .is_none());
    }

    /// A V3 registration's slot0/liquidity words are snapshotted too
    /// (scalar CL coverage), and the snapshot is an OWNED value — no borrow
    /// of the source state survives the call.
    #[test]
    #[expect(clippy::expect_used)]
    fn snapshot_is_owned_and_covers_cl_scalars() {
        let mut state = BotState::new();
        state
            .register_v3_pool(&RegisterV3PoolParams {
                address: POOL,
                // A valid in-range sqrt price (MIN_SQRT_RATIO bound).
                // 2**96 — a valid mid-range sqrt price.
                sqrt_price_x96: alloy::primitives::U256::from(1u128) << 96,
                tick: 0,
                liquidity: 1_000,
                tick_spacing: 60,
                ..Default::default()
            })
            .expect("test setup: V3 registration");
        let anchor = SimAnchorState::snapshot(&state);
        drop(state);
        assert!(anchor.pool_id_by_address(&POOL).is_some());
        assert!(anchor
            .probe_tracked_storage_slot(POOL, U256::ZERO)
            .is_some());
    }
}

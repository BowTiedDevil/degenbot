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

use hashbrown::HashMap;

use alloy::primitives::{Address, U256};

use super::divergence_probe::TrackedSlotProbe;
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
    /// Snapshot the anchor under a SHORT read: an ENUMERATED per-family
    /// projection (ADR-039) — O(pools) scalar packs, no tick-map iteration,
    /// no V4 reverse-map. The invariant lives in ADR-039: snapshot is an
    /// enumerated surface; adding an anchor slot means an enum entry in the
    /// projection with a test, never an arbitrary-key probe. Callers must
    /// hold the `BotState` read guard for the duration ONLY (`dispatch.rs:638`):
    /// the guard must stay a SHORT read.
    #[must_use]
    pub fn snapshot(state: &BotState) -> Self {
        let mut anchor = Self {
            tracked_pools: state.pool_addresses.clone(),
            anchor_words: HashMap::new(),
        };
        for (key, probe) in state.project_sim_anchor_scalars() {
            anchor.anchor_words.insert(key, probe);
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

    // ---- K4ETHF T5: enumerated projection (ADR-039) ------------------------
    //
    // PARITY: the projection's anchor_words are byte-identical to the
    // query-interface semantics on per-family valid indices, and ABSENT for
    // out-of-family probes (the V3 literal-slot-8 tick descent was precisely
    // an out-of-family probe sneaking in — the negative half guards that).
    // PERF: snapshot on a fabricated heavy state stays in enumerated
    // territory (no tick descent, no V4 reverse-map).

    use crate::bot_core::divergence_probe::{derive_v4_pool_state_base, TrackedSlotKind};
    use crate::bot_core::{PoolTickCoverage, RegisterV4PoolParams, V4PoolKey};
    use degenbot_pools::TickInfo;
    use std::time::Instant;

    fn sn_anchor_word(
        anchor: &SimAnchorState,
        key: &(Address, U256),
    ) -> Option<(TrackedSlotKind, [u8; 32], u64)> {
        anchor
            .anchor_words
            .get(key)
            .map(|p| (p.kind, p.engine_word.into(), p.update_block))
    }

    const V4_TEST_PM: Address = Address::new([0x44; 20]);

    #[expect(clippy::cast_possible_truncation)] // fixture: i < 2 pools
    fn v3_addr(i: usize) -> Address {
        let mut a = [0x33u8; 20];
        a[0] = (i as u8) + 1;
        Address::new(a)
    }

    #[expect(clippy::cast_possible_truncation)] // fixture: i, j < 2 pools
    fn v4_id(i: usize) -> [u8; 32] {
        std::array::from_fn(|j| (i as u8).wrapping_add(j as u8))
    }

    #[expect(
        clippy::expect_used,
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap
    )]
    fn heavy_state(v3_pools: usize, v3_ticks: usize, v4_pools: usize) -> BotState {
        let mut core = BotState::new();
        core.register_v2_pool(&crate::bot_core::RegisterV2PoolParams {
            address: Address::new([0x22; 20]),
            token0: Address::new([0xbb; 20]),
            token1: Address::new([0xcc; 20]),
            reserve0: U256::from(1000u64).to::<alloy::primitives::aliases::U112>(),
            reserve1: U256::from(2000u64).to::<alloy::primitives::aliases::U112>(),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::new([0xdd; 20]),
            update_block: 42,
            variant: DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        })
        .expect("V2 registration");
        for i in 0..v3_pools {
            let mut params = crate::bot_core::RegisterV3PoolParams {
                address: v3_addr(i),
                token0: Address::ZERO,
                token1: Address::new([0xa0; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000,
                tick: 0,
                tick_data: hashbrown::HashMap::new(),
                update_block: 18_000_000,
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
                ..Default::default()
            };
            for t in 0..v3_ticks {
                params.tick_data.insert(
                    (t as i32) - (v3_ticks as i32) / 2,
                    TickInfo {
                        liquidity_gross: alloy::primitives::U128::from(1_000u64 + t as u64),
                        liquidity_net: 500 - (t as i128),
                        block: 0,
                    },
                );
            }
            core.register_v3_pool(&params).expect("V3 registration");
        }
        for i in 0..v4_pools {
            core.register_v4_pool(&RegisterV4PoolParams {
                pool_manager: V4_TEST_PM,
                pool_id: v4_id(i),
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::new([1u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: hashbrown::HashMap::new(),
                update_block: 17_999_999,
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
            })
            .expect("V4 registration");
        }
        core
    }

    #[test]
    #[expect(clippy::unwrap_used, clippy::panic, clippy::type_complexity)]
    fn snapshot_parity_with_query_semantics() {
        // Fixture: one pool of each family (Aerodrome/Curve/Balancer families
        // contribute no anchor words by probe semantics — pass-through None).
        let core = heavy_state(1, 64, 1);
        let snap = SimAnchorState::snapshot(&core);

        // Positive half: expected words via the live probe at per-family
        // VALID indices only (V2->[8], V3->[0,4], V4->[S_state, S_state+3]).
        let s_state = derive_v4_pool_state_base(&v4_id(0));
        let mut expected: Vec<((Address, U256), (TrackedSlotKind, [u8; 32], u64))> = Vec::new();
        for (addr, idx) in [
            (Address::new([0x22; 20]), U256::from(8u64)),
            (v3_addr(0), U256::ZERO),
            (v3_addr(0), U256::from(4u64)),
            (V4_TEST_PM, s_state),
            (V4_TEST_PM, s_state.checked_add(U256::from(3u64)).unwrap()),
        ] {
            if let Some(p) = core.probe_tracked_storage_slot(addr, idx) {
                expected.push(((addr, idx), (p.kind, p.engine_word.into(), p.update_block)));
            }
        }
        assert!(
            expected.len() >= 5,
            "fixture must produce words for all three families, got {expected:?}"
        );
        for (key, want) in &expected {
            let got = sn_anchor_word(&snap, key)
                .unwrap_or_else(|| panic!("anchor missing {key:?} — projection diverged"));
            assert_eq!(got.0, want.0, "kind mismatch at {key:?}");
            assert_eq!(got.1, want.1, "word mismatch at {key:?}");
            assert_eq!(got.2, want.2, "update_block mismatch at {key:?}");
        }

        // Negative half: out-of-family probes must be ABSENT. V2 at 0/4 and
        // V3 at slot 8 (and any literal index that is not a family slot) are
        // precisely the arbitrary-key misses the projection must not pay for.
        for key in [
            (Address::new([0x22; 20]), U256::ZERO),
            (Address::new([0x22; 20]), U256::from(4u64)),
            (v3_addr(0), U256::from(8u64)),
            (v3_addr(1), U256::from(8u64)),
        ] {
            assert!(
                snap.anchor_words.get(&key).is_none(),
                "out-of-family probe {key:?} leaked into the anchor"
            );
        }

        // tracked_pools parity: the tripwire set is the full pool-address map.
        // pool_addresses covers V2/V3 contract addresses; V4 is keyed by
        // (PoolManager, pool_id) and lives outside the address map.
        assert_eq!(snap.tracked_pools.len(), 2);
        assert_eq!(
            snap.tracked_pools.get(&Address::new([0x22; 20])).copied(),
            core.pool_id_by_address(&Address::new([0x22; 20])),
            "V2 tripwire entry matches"
        );
    }

    #[test]
    fn snapshot_is_enumerated_not_a_scan() {
        // The ADR-039 perf gate: the projection enumerates per-family scalars
        // and NEVER iterates tick maps (V3 arbitrary-index fallthrough) or the
        // V4 reverse-map (O(V^2) keccak). Fabricate a heavy state and bound
        // the snapshot wall time.
        let core = heavy_state(120, 1_800, 200);
        let t0 = Instant::now();
        let _snap = SimAnchorState::snapshot(&core);
        let elapsed = t0.elapsed();

        assert!(
            elapsed.as_millis() < 150,
            "snapshot must be an enumerated projection (ADR-039): took {}ms for \
             120 V3 pools x 1800 ticks + 200 V4 pools",
            elapsed.as_millis()
        );
    }

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

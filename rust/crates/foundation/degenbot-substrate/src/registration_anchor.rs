//! Registration-anchor fused lookups over the state lock (relocated with
//! `BotState` + `StateLock` per ADR-067; consumers live in the host
//! crate's registration lifecycle).

use alloy::primitives::Address;
use degenbot_decoders::v4_swap_decoder::V4PoolId;

use crate::state_lock::StateLock;
use crate::BotState;

///
/// Both chain the two-step lookup — V3 `address → pool_id → liquidity clock`,
/// V4 `(pool_manager, pool_id) → pool_id → clock` — under ONE
/// `read_at(LockSite::Registration)`, so a caller cannot hold read#1 while
/// requesting read#2 on the same lock. That nested shape is the
/// soak-2026-08-22 self-deadlock class (a writer queued between the two reads
/// cycles the lock); see the `NEVER NEST ACQUISITIONS` note in
/// [`crate::state_lock`]. **Single acquisition is the invariant:**
/// keep these fused rather than recomposing the single-step accessors at a
/// call site.
///
/// Returns `0` when the pool is unregistered — the same unset-clock sentinel
/// as [`BotState::pool_tick_data_block`] — so the caller's fallback to the
/// aggregate snapshot block `S` is unchanged.
impl StateLock<BotState> {
    /// V3: pool address → `pool_id` → `tick_data_block` under one read.
    #[must_use]
    pub fn pool_tick_data_block_by_address(&self, address: &Address) -> u64 {
        let state = self.read_at(crate::state_lock::LockSite::Registration);
        let Some(id) = state.pool_id_by_address(address) else {
            return 0;
        };
        state.pool_tick_data_block(id)
    }

    /// V3: pool address → immutable tick spacing under one read.
    #[must_use]
    pub fn pool_tick_spacing_by_address(&self, address: &Address) -> Option<i32> {
        let state = self.read_at(crate::state_lock::LockSite::Registration);
        let id = state.pool_id_by_address(address)?;
        state
            .get_v3_identity(id)
            .map(|identity| identity.tick_spacing)
    }

    /// V4: `(pool_manager, pool_id)` → immutable tick spacing under one read.
    #[must_use]
    pub fn pool_tick_spacing_by_v4_key(
        &self,
        pool_manager: Address,
        pool_id: &V4PoolId,
    ) -> Option<i32> {
        let state = self.read_at(crate::state_lock::LockSite::Registration);
        let id = state.v4_pool_id_by_key(pool_manager, pool_id)?;
        state
            .get_v4_identity(id)
            .map(|identity| identity.pool_key.tick_spacing)
    }

    /// V4: `(pool_manager, pool_id)` → `pool_id` → `tick_data_block`
    /// under one read.
    #[must_use]
    pub fn pool_tick_data_block_by_v4_key(&self, pool_manager: Address, pool_id: &V4PoolId) -> u64 {
        let state = self.read_at(crate::state_lock::LockSite::Registration);
        let Some(id) = state.v4_pool_id_by_key(pool_manager, pool_id) else {
            return 0;
        };
        state.pool_tick_data_block(id)
    }
}

#[expect(clippy::expect_used)]
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use alloy::primitives::{Address, U256};
    use hashbrown::HashMap;

    use super::{BotState, StateLock, V4PoolId};
    use crate::state_lock::LockSite;
    use crate::{PoolTickCoverage, RegisterV3PoolParams, RegisterV4PoolParams, V4PoolKey};

    fn new_core() -> Arc<StateLock<BotState>> {
        Arc::new(StateLock::new(BotState::new()))
    }

    // ── Fused single-acquisition lookups ────────────────────────
    //
    // The two seed-anchor sites used to compose
    // `pool_id_by_address`/`v4_pool_id_by_key` with `pool_tick_data_block`
    // in ONE expression, holding read#1 as the expression temporary while the
    // closure requested read#2 on the same `StateLock`. With a writer queued
    // between them that is a cycle (soak-2026-08-22: read#2 parks behind the
    // writer, the writer waits on read#1, and read#1 lives until the
    // expression completes). The fused accessors take
    // `read_at(Registration)` once; the tests below pin both the hazard and
    // the single-acquisition invariant.

    /// Register a V3 pool whose liquidity clock (`tick_data_block`) is
    /// `clock` — the value a fused lookup must return.
    fn reg_v3_with_clock(core: &mut BotState, address: Address, clock: u64) -> u64 {
        core.register_v3_pool(&RegisterV3PoolParams {
            address,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: clock + 1,
            tick_data_block: Some(clock),
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration with a liquidity clock")
    }

    /// V4 twin of [`reg_v3_with_clock`], keyed by `(pool_manager, pool_id)`.
    fn reg_v4_with_clock(core: &mut BotState, pm: Address, pid: V4PoolId, clock: u64) -> u64 {
        core.register_v4_pool(&RegisterV4PoolParams {
            pool_manager: pm,
            pool_id: pid,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: clock + 1,
            tick_data_block: Some(clock),
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        })
        .expect("test setup: V4 registration with a liquidity clock")
    }

    /// A fused lookup takes `read_at(LockSite::Registration)` EXACTLY ONCE.
    ///
    /// Chosen test design (the task allowed either this or reproducing the
    /// hang): the lock's test-only acquisition counter is the assertion,
    /// because an in-process hang cannot be asserted cleanly. The pre-fusion
    /// one-expression composition is measured alongside as calibration,
    /// proving the counter can actually see a second (nested) acquisition.
    #[test]
    fn fused_lookups_acquire_the_registration_lock_exactly_once() {
        let core = new_core();
        let addr = Address::from([0x81u8; 20]);
        let pm = Address::from([0x91u8; 20]);
        let pid = [0xd1u8; 32];
        {
            let mut c = core.write_at(LockSite::Registration);
            reg_v3_with_clock(&mut c, addr, 4242);
            reg_v4_with_clock(&mut c, pm, pid, 7777);
        }

        // Calibration: the pre-fusion composition (read#1 held as the
        // expression temporary, read#2 inside the `map_or` closure) takes
        // TWO acquisitions. Single-threaded here, so it does not deadlock —
        // the queued-writer cycle is pinned by the host crate's registration-lifecycle tests.
        let before = core.read_acquires_for_tests();
        let legacy = core
            .read_at(LockSite::Registration)
            .pool_id_by_address(&addr)
            .map_or(0, |id| {
                core.read_at(LockSite::Registration)
                    .pool_tick_data_block(id)
            });
        assert_eq!(legacy, 4242);
        assert_eq!(
            core.read_acquires_for_tests() - before,
            2,
            "the pre-fusion composition takes two reads (counter calibration)"
        );

        let before = core.read_acquires_for_tests();
        let v3 = core.pool_tick_data_block_by_address(&addr);
        assert_eq!(
            core.read_acquires_for_tests() - before,
            1,
            "the fused V3 lookup must take exactly one Registration read"
        );
        assert_eq!(
            v3, 4242,
            "fused V3 lookup returns the pool's liquidity clock"
        );

        let before = core.read_acquires_for_tests();
        let v4 = core.pool_tick_data_block_by_v4_key(pm, &pid);
        assert_eq!(
            core.read_acquires_for_tests() - before,
            1,
            "the fused V4 lookup must take exactly one Registration read"
        );
        assert_eq!(
            v4, 7777,
            "fused V4 lookup returns the pool's liquidity clock"
        );

        // Unregistered keys keep the unset-clock sentinel 0, still one read.
        let before = core.read_acquires_for_tests();
        assert_eq!(
            core.pool_tick_data_block_by_address(&Address::from([0xeeu8; 20])),
            0
        );
        assert_eq!(core.pool_tick_data_block_by_v4_key(pm, &[0xeeu8; 32]), 0);
        assert_eq!(
            core.read_acquires_for_tests() - before,
            2,
            "one read per fused lookup, registered or not"
        );
    }

    /// The fused accessors stay live and coherent under a concurrent writer
    /// stampede: a lookup that took two reads with a queued writer between
    /// them would park (the cycle above). `recv_timeout` is the hang
    /// watchdog — a regression fails the test instead of hanging the suite.
    #[test]
    fn fused_lookup_stays_live_under_concurrent_writer() {
        let core = new_core();
        let addr = Address::from([0x83u8; 20]);
        {
            let mut c = core.write_at(LockSite::Registration);
            reg_v3_with_clock(&mut c, addr, 4242);
        }

        let (observed_tx, observed_rx) = std::sync::mpsc::channel();
        let reader_core = Arc::clone(&core);
        let reader = std::thread::spawn(move || {
            let observed: Vec<u64> = (0..2_000)
                .map(|_| reader_core.pool_tick_data_block_by_address(&addr))
                .collect();
            observed_tx.send(observed).expect("send observations");
        });
        let writer_core = Arc::clone(&core);
        let writer = std::thread::spawn(move || {
            for block in 0..2_000 {
                writer_core
                    .write_at(LockSite::Registration)
                    .cl_mut()
                    .advance_pump_complete_cutoff(block);
            }
        });

        let observed = observed_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("fused lookup must complete under a concurrent writer (no nested-read cycle)");
        writer.join().expect("writer joins");
        reader.join().expect("reader joins");
        assert_eq!(observed.len(), 2_000);
        assert!(
            observed.iter().all(|&clock| clock == 4242),
            "every fused lookup observes the same registered liquidity clock"
        );
    }
}

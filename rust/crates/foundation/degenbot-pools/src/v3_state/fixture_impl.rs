//! The V3 adapter's fixture seam for the merged concentrated liquidity state test suites
//! (ADR-080 D4): construction + event application for [`V3PoolState`],
//! reached through the shared `ClStateFixture` trait. Everything the shared
//! batteries assert lives in the state modules' tests
//! (`families/concentrated_liquidity/state_{journal,seeding,walk}/tests.rs`);
//! this impl only knows how to build and drive the adapter.

use super::V3PoolState;
use crate::families::concentrated_liquidity::state_fixture::{ClStateFixture, LIQ};
use crate::int_v3_hop::IntV3TickRangeSequence;
use crate::registry::ConcentratedLiquidityPoolMut;
use crate::state_history::{ReorgJournal, V3BlockDelta};
use crate::TickInfo;
use alloy::primitives::{U128, U256};
use hashbrown::{HashMap, HashSet};

impl ClStateFixture for V3PoolState {
    fn with_position(liq: u128) -> Self {
        let sp_0 = U256::from(1u128) << 96;
        let liq_u128 = U256::from(liq).to::<U128>();
        let mut tick_data = HashMap::new();
        tick_data.insert(
            -60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: i128::try_from(liq).unwrap_or(0),
                block: 0,
            },
        );
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: -i128::try_from(liq).unwrap_or(0),
                block: 0,
            },
        );
        V3PoolState {
            sqrt_price_x96: sp_0,
            liquidity: liq,
            tick: 0,
            update_block: 0,
            tick_data_block: 0,
            initial_state_block: 0,
            state_nonce: 0,
            registration_lifecycle: super::RegistrationLifecycle::default(),
            tick_data,
            coverage: super::PoolTickCoverage::Tracked,
            known_bitmap_words: HashSet::new(),
            fetcher: None,
            journal: ReorgJournal::<V3BlockDelta>::new(8),
            snapshot_seed: None,
            post_drain_snapshot: None,
            cached_tick_ranges: parking_lot::Mutex::new(super::TickRangeCache::default()),
        }
    }

    fn pool_state() -> Self {
        // The rewind battery's registration-state pool: same shape, but an
        // empty depth-16 journal (the original battery's construction).
        let mut state = Self::with_position(LIQ);
        state.journal = ReorgJournal::<V3BlockDelta>::new(16);
        state
    }

    fn seed_genesis(&mut self, block: u64) {
        ConcentratedLiquidityPoolMut::seed_genesis(self, block);
    }

    fn apply_swap(
        &mut self,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block: u64,
        tick_priors: &[(i32, TickInfo)],
    ) {
        crate::registry::ConcentratedLiquidityPoolMut::apply_swap(
            self,
            sqrt_price_x96,
            liquidity,
            tick,
            block,
            tick_priors,
        );
    }

    fn apply_liquidity_update(&mut self, lower: i32, upper: i32, delta: i128, block: u64) {
        crate::registry::ConcentratedLiquidityPoolMut::apply_liquidity_update(
            self, lower, upper, delta, block,
        );
    }

    fn build_int_sequence(
        &self,
        tick_spacing: i32,
        fee: u32,
        zero_for_one: bool,
    ) -> Option<IntV3TickRangeSequence> {
        V3PoolState::build_int_v3_sequence(self, tick_spacing, fee, zero_for_one)
    }

    fn state_nonce(&self) -> u64 {
        self.state_nonce
    }

    fn cached_slot(
        &self,
        zero_for_one: bool,
    ) -> crate::families::concentrated_liquidity::state_walk::CachedTickRanges {
        let cache = self.cached_tick_ranges.lock();
        if zero_for_one {
            cache.zfo.clone()
        } else {
            cache.ofz.clone()
        }
    }

    fn set_tick(&mut self, tick: i32) {
        self.tick = tick;
    }

    fn set_sqrt_price(&mut self, sqrt_price_x96: U256) {
        self.sqrt_price_x96 = sqrt_price_x96;
    }

    fn set_initial_state_block(&mut self, block: u64) {
        self.initial_state_block = block;
    }
}

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]
use crate::arb_engine::delivery_policy::{
    compute_diff_and_send, deregister_path, set_profit_thresholds, set_result_channel,
};
use crate::arb_engine::lifecycle::{
    latest_results, path_count, path_dedups, register_and_solve_path, register_path,
    set_deferred_re_record, set_path_cap, solve_all_paths, v2_pool_count, v3_pool_count,
};
use crate::arb_engine::test_harness::{
    finalize_for_test, has_logs_this_block, hop_projection_count, last_solved_block,
    merge_detached_for_test, process_updates, run_test_cycle,
};
use crate::arb_engine::{ArbitrageEngine, BlockMetadata, PumpPhase};
use crate::bot_core::RegisterV3PoolParams;
use crate::bot_core::RegisterV4PoolParams;
use ::degenbot_solvers::mixed::{
    HopType, PoolHop, ResolvedHop, ResolvedMixedPath, SolidlyHopState, SolvePathResult, INT128_MAX,
};
use alloy::primitives::{aliases::U112, Address, U256};
use degenbot_uniswap::dex_identity::DexVariant;
use hashbrown::{HashMap, HashSet};
fn usdc(amount: u64) -> U112 {
    (U256::from(amount) * U256::from(10u64).pow(U256::from(6))).to::<U112>()
}
fn weth(amount: u64) -> U112 {
    (U256::from(amount) * U256::from(10u64).pow(U256::from(18))).to::<U112>()
}
const GAMMA_03: u64 = 997;
const FEE_DENOM_03: u64 = 1000;

/// Common scaffolding: a 3-path V2→V2 engine (same live-corpus-shaped
/// fixtures as the streaming test), with the slow path's hook injectable
/// per test.
fn detached_fixture(delay_ms: u64) -> (ArbitrageEngine, Vec<u64>, Vec<u64>) {
    let mut engine = ArbitrageEngine::new();
    let mut pool_ids = Vec::new();
    let mut path_ids = Vec::new();
    for i in 0u8..3 {
        let fwd = engine.register_v2_pool(
            Address::from([0x90 + i; 20]),
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let back = engine.register_v2_pool(
            Address::from([0xA0 + i; 20]),
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        pool_ids.push(fwd);
        pool_ids.push(back);
        path_ids.push(
            register_path(
                &mut engine,
                vec![
                    PoolHop {
                        pool_id: fwd,
                        zero_for_one: true,
                    },
                    PoolHop {
                        pool_id: back,
                        zero_for_one: true,
                    },
                ],
            )
            .unwrap(),
        );
    }
    // The first registered path's id owns the injected delay.
    let target = path_ids[0];
    engine
        .cycle
        .set_solve_delay_hook(std::sync::Arc::new(move |pid: u64| {
            if pid == target {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
        }));
    (engine, pool_ids, path_ids)
}

/// Build the minimal V3 tick-data (initialized +60/-60 ticks) used by
/// `inspect_path_returns_hop_details`.
fn inspect_test_v3_tick_data() -> HashMap<i32, crate::bot_core::TickInfo> {
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(300),
            liquidity_net: 150i128,
            block: 0,
        },
    );
    tick_data.insert(
        -60,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(200),
            liquidity_net: -100i128,
            block: 0,
        },
    );
    tick_data
}
mod admission;
mod arb_solving;
mod balancer_stable;
mod balancer_weighted;
mod curve_and_spans;
mod deferred_rerecord;
mod detached_merge;
mod lane_unify;
mod paths_reorg_and_locking;
mod registration_and_delivery;
mod solidly;

pub(crate) mod test_keys;

// =======================================================================
// the fixture-driven clamp/merge/worker tests, moved here from
// the deleted grab file's test island. They exercise the `SolveCycle`
// clamp + merge surfaces through a real engine, so they live with the
// engine-parity tests.
// =======================================================================
#[cfg(test)]
mod clamp_merge_worker_tests;

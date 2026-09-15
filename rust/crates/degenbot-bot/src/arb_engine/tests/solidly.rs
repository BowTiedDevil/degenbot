use super::*;

// -----------------------------------------------------------------
// HopType::SolidlyStable + ResolvedHop::SolidlyStable variant (Plan: Port
// Solidly solve into the Rust engine — task BFIWUG).
// -----------------------------------------------------------------
#[test]
fn solidly_hop_variant_is_not_v2_and_not_cl() {
    // The new variant must be excluded from the existing all-V2 and
    // all-CL dispatch branches — otherwise solve_path would mis-dispatch.
    assert!(!HopType::SolidlyStable.is_concentrated_liquidity());
}
#[test]
fn resolved_solidly_hop_round_trips_via_as_solidly_state() {
    let state = SolidlyHopState {
        reserves_0: U256::from(1_000_000u64),
        reserves_1: U256::from(1_000_000u64),
        decimals_0: U256::from(10u64).pow(U256::from(6u64)),
        decimals_1: U256::from(10u64).pow(U256::from(18u64)),
        token_in: 0,
        fee_numer: U256::from(3u64),
        fee_denom: U256::from(1000u64),
        stable: true,
        variant: DexVariant::AerodromeV2Stable,
    };
    let hop = ResolvedHop::SolidlyStable {
        state: state.clone(),
    };
    // The new accessor returns the state.
    let got = hop
        .as_solidly_state()
        .expect("Solidly hop should yield its state");
    assert_eq!(got.reserves_0, state.reserves_0);
    assert_eq!(got.variant, DexVariant::AerodromeV2Stable);
    assert!(got.stable);
    // hop_type() maps to the new variant.
    assert_eq!(hop.hop_type(), HopType::SolidlyStable);
    // The Solidly hop is excluded from the V2 + CL accessors — the
    // existing dispatch arms must not pick it up.
    assert!(hop.as_v2_state().is_none());
    assert!(hop.as_int_sequence().is_none());
}
// The per-family Solidly projection tests live in
// `crate::bot_core::resolve::solidly::tests` (moved in T3 of epic
// MKRKNB; they assert the `MissingHopReason` variants directly
// against `project_solidly`). This module keeps only the
// engine-level classifier test (`solidly_hop_variant_is_not_v2_and_not_cl`).
// -----------------------------------------------------------------
// solve_solidly_path_int (task DMPSNG) — the two-stage Möbius precheck +
// golden-section search. Tests cover all four AC cases: (1) all-Solidly
// 2-hop, (2) V2+Solidly mixed, (3) unprofitable → None (precheck),
// (4) Solidly+CL → None (scope rejection).
// -----------------------------------------------------------------
fn solidly_arb_engine() -> (ArbitrageEngine, u64, u64) {
    // Two Aerodrome-stable pools with the same token pair but divergent
    // reserves — a profitable arb cycle. Reserves use "wei magnitude"
    // (1e18 == 1 token of an 18-dec token) so the solidly math's
    // calc_d (which divides intermediate products by 1e18) does not
    // underflow to zero (small-magnitude reserves would panic on
    // divide-by-zero in get_y_solidly).
    use crate::bot_core::{BotState, RegisterAerodromeV2PoolParams};
    use std::sync::Arc;
    fn tokens(n: u64) -> U112 {
        (U256::from(n) * U256::from(10u64).pow(U256::from(18u64))).to::<U112>()
    }
    let core = Arc::new(crate::bot_core::state_lock::StateLock::new(BotState::new()));
    core.write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_token(
            Address::from([0x01u8; 20]),
            "Token0".into(),
            "T0".into(),
            18,
            1,
        );
    core.write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_token(
            Address::from([0x02u8; 20]),
            "Token1".into(),
            "T1".into(),
            18,
            1,
        );
    let aero_a = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
            token0_decimals: 18,
            token1_decimals: 18,
            address: Address::from([0xa1u8; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            factory: Address::from([0xfau8; 20]),
            variant: DexVariant::AerodromeV2Stable,
            stable: true,
            fee: (3, 1000),
            reserve0: tokens(1000),
            reserve1: tokens(100),
            update_block: 0,
        });
    let aero_b = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
            token0_decimals: 18,
            token1_decimals: 18,
            address: Address::from([0xa2u8; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            factory: Address::from([0xfau8; 20]),
            variant: DexVariant::AerodromeV2Stable,
            stable: true,
            fee: (3, 1000),
            // Pool B holds the SAME pair but with twice the token0 — its
            // token1→token0 price (reserve0 / reserve1) is 2x Pool A's, so a
            // token0→token1→token0 cycle is profitable (the V2-equivalent
            // Möbius optimal input is non-trivial).
            reserve0: tokens(2000),
            reserve1: tokens(100),
            update_block: 0,
        });
    let engine = ArbitrageEngine::with_core(Arc::clone(&core));
    (engine, aero_a, aero_b)
}
#[test]
fn solve_solidly_2hop_all_solidly_matches_grid_scan() {
    let (mut engine, aero_a, aero_b) = solidly_arb_engine();
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: aero_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: aero_b,
                zero_for_one: false,
            },
        ],
    )
    .expect("path registers");
    let resolved = engine.cycle.path_resolved.get(&path_id).expect("resolved");
    let result = ::degenbot_solvers::mixed::solve_path(
        resolved,
        &::degenbot_solvers::profit_envelope::GateDeps::offline(),
    )
    .result
    .expect("profitable path solves");
    assert!(!result.optimal_input.is_zero());
    assert!(!result.profit.is_zero());
    assert_eq!(result.hop_outputs.len(), 2);
    assert_eq!(result.consumed_inputs.len(), 2);
    assert_eq!(result.consumed_inputs[0], result.optimal_input);
    assert_eq!(result.consumed_inputs[1], result.hop_outputs[0]);
    // profit = final output − optimal_input.
    assert_eq!(
        result.profit,
        result.hop_outputs[1].saturating_sub(result.optimal_input)
    );
    // Golden-section must not miss the global optimum: scan a fine grid
    // (1-token steps) and assert the solver's profit is within one grid
    // step of the grid max (±3 verification radius tolerance).
    let max_reserve = U256::from(1000u64) * U256::from(10u64).pow(U256::from(18u64));
    let grid_step = U256::from(10u64).pow(U256::from(18u64)); // 1 token
    let mut grid_best_profit = U256::ZERO;
    let mut x = U256::from(1u64);
    while x <= max_reserve {
        let out = ::degenbot_solvers::mixed::simulate_solidly_path(x, &resolved.hops);
        let profit = out.saturating_sub(x);
        if profit > grid_best_profit {
            grid_best_profit = profit;
        }
        x += grid_step;
    }
    assert!(
        result.profit + grid_step >= grid_best_profit,
        "solver profit {} should be within one grid step of grid max {}",
        result.profit,
        grid_best_profit
    );
    assert!(
        result.profit >= grid_best_profit.saturating_sub(grid_step),
        "solver profit {} must not fall more than one grid step below grid max {}",
        result.profit,
        grid_best_profit
    );
}
#[test]
fn solve_solidly_mixed_v2_and_solidly_matches_grid_scan() {
    use crate::bot_core::{BotState, RegisterAerodromeV2PoolParams, RegisterV2PoolParams};
    use std::sync::Arc;
    fn tokens(n: u64) -> U112 {
        (U256::from(n) * U256::from(10u64).pow(U256::from(18u64))).to::<U112>()
    }
    let core = Arc::new(crate::bot_core::state_lock::StateLock::new(BotState::new()));
    core.write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_token(
            Address::from([0x01u8; 20]),
            "Token0".into(),
            "T0".into(),
            18,
            1,
        );
    core.write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_token(
            Address::from([0x02u8; 20]),
            "Token1".into(),
            "T1".into(),
            18,
            1,
        );
    // Mixed path: Solidly hop0 (token0→token1), V2 hop1 (token1→token0).
    // Mirrors the profitable all-Solidly fixture but with the second hop
    // as V2 constant-product (more slippage than Solidly, but the cycle
    // is still profitable because Solidly hop0 emits ample token1).
    let aero_id = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
            token0_decimals: 18,
            token1_decimals: 18,
            address: Address::from([0xb1u8; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            factory: Address::from([0xfau8; 20]),
            variant: DexVariant::AerodromeV2Stable,
            stable: true,
            fee: (3, 1000),
            reserve0: tokens(1000),
            reserve1: tokens(100),
            update_block: 0,
        });
    let v2_id = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_v2_pool(&RegisterV2PoolParams {
            address: Address::from([0xb2u8; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            reserve0: tokens(2000),
            reserve1: tokens(100),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::from([0xfbu8; 20]),
            update_block: 0,
            variant: DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        })
        .expect("test setup: V2 registration");
    let mut engine = ArbitrageEngine::with_core(Arc::clone(&core));
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: aero_id,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2_id,
                zero_for_one: false,
            },
        ],
    )
    .expect("mixed V2+Solidly path registers");
    let resolved = engine.cycle.path_resolved.get(&path_id).expect("resolved");
    let result = ::degenbot_solvers::mixed::solve_path(
        resolved,
        &::degenbot_solvers::profit_envelope::GateDeps::offline(),
    )
    .result
    .expect("profitable mixed path solves");
    assert!(!result.profit.is_zero());
    // Grid scan parity check (Solidly hop uses the integer leaf, V2 hop
    // uses IntHopState::swap).
    let max_reserve = tokens(1000).to::<U256>();
    let grid_step = tokens(1).to::<U256>();
    let mut grid_best = U256::ZERO;
    let mut x = U256::from(1u64);
    while x <= max_reserve {
        let profit =
            ::degenbot_solvers::mixed::simulate_solidly_path(x, &resolved.hops).saturating_sub(x);
        if profit > grid_best {
            grid_best = profit;
        }
        x += grid_step;
    }
    assert!(
        result.profit + grid_step >= grid_best,
        "mixed-path profit {} within one grid step of grid max {}",
        result.profit,
        grid_best
    );
}
#[test]
fn solve_solidly_unprofitable_path_returns_none() {
    let (mut engine, aero_a, _aero_b) = solidly_arb_engine();
    // A round-trip through the SAME pool (token0→token1 then token1→token0)
    // is always unprofitable after fees — the Möbius precheck must early-out.
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: aero_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: aero_a,
                zero_for_one: false,
            },
        ],
    )
    .expect("path registers");
    let resolved = engine.cycle.path_resolved.get(&path_id).expect("resolved");
    assert!(
        ::degenbot_solvers::mixed::solve_path(
            resolved,
            &::degenbot_solvers::profit_envelope::GateDeps::offline()
        )
        .result
        .is_none(),
        "round-trip through one pool is unprofitable"
    );
}
#[test]
fn solve_solidly_plus_cl_path_rejected_by_scope() {
    use crate::bot_core::{BotState, RegisterAerodromeV2PoolParams, RegisterV3PoolParams};
    use std::sync::Arc;
    let core = Arc::new(crate::bot_core::state_lock::StateLock::new(BotState::new()));
    core.write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_token(
            Address::from([0x01u8; 20]),
            "Token0".into(),
            "T0".into(),
            18,
            1,
        );
    core.write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_token(
            Address::from([0x02u8; 20]),
            "Token1".into(),
            "T1".into(),
            18,
            1,
        );
    let aero = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
            token0_decimals: 18,
            token1_decimals: 18,
            address: Address::from([0xa1u8; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            factory: Address::from([0xfau8; 20]),
            variant: DexVariant::AerodromeV2Stable,
            stable: true,
            fee: (3, 1000),
            reserve0: (U256::from(1000u64) * U256::from(10u64).pow(U256::from(18u64))).to::<U112>(),
            reserve1: (U256::from(100u64) * U256::from(10u64).pow(U256::from(18u64))).to::<U112>(),
            update_block: 0,
        });
    // Register a minimal V3 pool for the second hop using the same
    // ..Default::default() pattern as the existing V3 tests.
    let v3_id = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_v3_pool(&RegisterV3PoolParams {
            address: Address::from([0xc1u8; 20]),
            token0: Address::from([0x02u8; 20]),
            token1: Address::from([0x01u8; 20]),
            fee: 500,
            tick_spacing: 10,
            sqrt_price_x96: U256::from(1u64) << 96,
            tick: 0,
            liquidity: 1_000_000,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");
    let mut engine = ArbitrageEngine::with_core(Arc::clone(&core));
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: aero,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v3_id,
                zero_for_one: false,
            },
        ],
    )
    .expect("path registers (resolve is per-arm)");
    let resolved = engine.cycle.path_resolved.get(&path_id).expect("resolved");
    // Solidly + CL is out of scope (p): solve_path returns None.
    assert!(::degenbot_solvers::mixed::solve_path(
        resolved,
        &::degenbot_solvers::profit_envelope::GateDeps::offline()
    )
    .result
    .is_none());
}

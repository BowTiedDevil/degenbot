//! Consumer probe (ii) — `cargo add degenbot-pools`: solve a path through the
//! solve door with door items only (ADR-084 D1/C5). This example doubles as
//! the catalog's documented snippet for the pools-only consumer.

#![expect(
    clippy::print_stdout,
    clippy::panic,
    reason = "probe: stdout is the deliverable and a failed solve must fail the probe loudly"
)]

use alloy::primitives::U256;
use degenbot_math::v2::IntHopState;
use degenbot_pools::solve::gate::GateDeps;
use degenbot_pools::solve::runtime::SolveRuntimeConfig;
use degenbot_pools::solve::{solve_path_with_min_profit, ResolvedHop, ResolvedMixedPath};

fn main() {
    // A two-hop constant-product path with a mispriced middle: the solver
    // should find a profitable input through the root door.
    let hops = vec![
        ResolvedHop::V2 {
            state: IntHopState::new(
                U256::from(1_000_000_000_000_000_000u128),
                U256::from(5_000_000_000_000_000_000u128),
                997,
                1000,
            ),
        },
        ResolvedHop::V2 {
            state: IntHopState::new(
                U256::from(4_900_000_000_000_000_000u128),
                U256::from(1_010_000_000_000_000_000u128),
                997,
                1000,
            ),
        },
    ];
    let path = ResolvedMixedPath {
        hops,
        valid: true,
        state_nonces: vec![1, 1],
        max_update_block: 0,
    };

    let outcome = solve_path_with_min_profit(
        &path,
        U256::ZERO,
        &GateDeps::offline_with_runtime(SolveRuntimeConfig::default()),
    );

    println!("solved: {:?}", outcome.result.is_some());
    if let Some(result) = outcome.result {
        println!(
            "optimal input in: {}, profit: {}",
            result.optimal_input, result.profit
        );
        assert!(result.profit > 0, "the probe path must be profitable");
    } else {
        panic!("consumer probe: the door returned no solution for a profitable path");
    }
}

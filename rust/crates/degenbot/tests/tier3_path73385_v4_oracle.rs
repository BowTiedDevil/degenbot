//! Tier-3 path-73385 V4 oracle byte-exactness guard (block 25706469).
//!
//! Pins the byte-exact V4 concentrated-liquidity twin for the exact topology
//! that crashed the bot (pool `0x8aa4e11c` USDC/USDT, fee=10, tick_spacing=1,
//! tiny 1:1 63-tick band). The live solver `hop_outputs[1]` over-predicted the
//! V4 output by 3 wei (85097884 vs the twin 85097881), which over-took the pool
//! and stranding a residual the settle repaid via a failing `USDT.transfer`
//! (0xfe halt). This guard asserts the tier-3-proven twin
//! (`v4_simulate_swap`) is byte-exact to the recorded on-chain actual
//! (85097881) for this topology — the authoritative bound the solver's
//! `clamp_cl_hop_capacity` now aligns `hop_outputs[i]`/`consumed_inputs[i+1]`
//! to (so the take can never exceed the pool's actual yield).
//!
//! No RPC / toolchain — feeds the committed
//! `tests/fixtures/path73385_v4_block25706469.json` capture.

#![allow(clippy::doc_markdown)] // pool identifiers

use alloy::primitives::aliases::I256;
use alloy::primitives::U256;
use degenbot::investigation::{build_v4_state, PathFixture};
use degenbot_cl_math::cl_lib::tick_math::MIN_SQRT_RATIO;
use degenbot_pools::v4_state::v4_simulate_swap;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path73385_v4_block25706469.json"
);

/// The recorded on-chain V4 output (`[sim-revert-swap] actual_out`) — the
/// byte-exact twin target the solver over-predicted by 3 wei.
const RECORDED_ACTUAL: u128 = 85_097_881;
/// The live solver's over-predicted `hop_outputs[1]` that caused the 3-wei
/// over-take (and whose drift `clamp_cl_hop_capacity` now corrects at the seam).
const SOLVER_PREDICTED: u128 = 85_097_884;
/// The recorded V4 exact-in (previous hop's output, `consumed_inputs[1]`).
const RECORDED_V4_INPUT: u128 = 85_060_245;

#[test]
fn tier3_path73385_v4_twin_is_byte_exact_to_onchain() {
    let fx = PathFixture::load(FIXTURE_PATH).unwrap_or_else(|e| panic!("{e}"));
    let state = build_v4_state(&fx.pools["v4"]);

    let neg = I256::ZERO
        .checked_sub(I256::try_from(RECORDED_V4_INPUT).expect("input fits i256"))
        .expect("no underflow");
    // zfo=true (sell USDC/currency0 for USDT/currency1) — the buyer's extreme
    // price limit is the floor (the approximate iterated twin).
    let limit = U256::from(MIN_SQRT_RATIO)
        .checked_add(U256::from(1u64))
        .unwrap();
    let sim = v4_simulate_swap(
        &state,
        fx.pools["v4"].fee_currency0.unwrap(),
        fx.pools["v4"].tick_spacing.unwrap(),
        true, // zfo
        neg,
        limit,
    )
    .expect("twin simulates");

    // The output-token amount for zfo=true is amount1 (USDT).
    let out: u128 = u128::try_from(sim.amount1).expect("amount1 fits");
    assert_eq!(
        out, RECORDED_ACTUAL,
        "path-73385 V4 twin must be byte-exact to the on-chain actual"
    );
    // And it is 3 wei BELOW the solver's over-prediction — the exact drift the
    // clamp now absorbs, so the take can never over-take the pool.
    assert_eq!(out.saturating_sub(RECORDED_ACTUAL), 0);
    assert_eq!(SOLVER_PREDICTED - RECORDED_ACTUAL, 3);
}

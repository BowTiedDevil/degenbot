//! Tier-2 behavioral dual-driver parity — V3 concentrated-liquidity swap
//! (ADR-005 standalone claim, the behavioral tier).
//!
//! The V2 parity test (`parity_v2_swap.rs`) uses a hand-derived closed-form
//! `getAmountOut` oracle. The V3 CL swap math has no comparably simple closed
//! form — `v3_simulate_swap` routes through `compute_swap_step_v3` + the tick
//! walk, and a non-circular re-derivation (`get_next_sqrt_price_from_input` +
//! `get_amount1_delta` with manual fee/rounding handling) is fiddly enough to
//! deserve its own careful TDD task rather than a rapid corpus extension.
//!
//! So this Tier-2 seed asserts the **direct FFI-seam-lossless claim**: the
//! **same** canonical fixture, driven through the **Rust consumer**
//! (`BotState`, as a `cargo add degenbot` user) produces the **same**
//! `amount_out` as the **Python consumer** (`PyBot`, the PyO3 binding). The
//! shared oracle is the recorded constant `EXPECTED_AMOUNT_OUT_ZFO`
//! (deterministic — both consumers hit one `BotState::calculate_tokens_out`
//! core). Plus a monotonicity sanity check (larger input → larger output,
//! within the active tick) to catch a degenerate zero-return regression, and
//! a direction-symmetry check (zfo == ofz at the 1:1 price) to catch a
//! direction-flag inversion in the FFI seam.
//!
//! The matching Python side lives at
//! `tests/standalone_parity/test_v3_swap_dual_driver.py`.
//!
//! ## Fixture (the shared contract)
//!
//! A 1:1-price V3 pool (sqrt_price_x96 = 2^96, tick 0), liquidity 1e12, fee
//! 3000 (0.3%), tick_spacing 60, with tick 0 seeded in `tick_data` +
//! `Tracked` coverage so the swap stays within the active tick (no
//! `MissingTickWord`). `amount_in = 1_000_000_000` is small relative to
//! liquidity → stays in-tick → deterministic computed output
//! `996_006_981` (symmetric for zfo/ofz at the 1:1 price).

#![allow(clippy::panic_in_result_fn, clippy::doc_markdown)]

use alloy::primitives::{address, I256, U256};
use degenbot::bot_core::BotState;
use degenbot::RegisterV3PoolParams;
use degenbot_pools::v3_state::PoolTickCoverage;
use degenbot_pools::TickInfo;
use std::collections::HashMap;

// ---- the shared canonical fixture (mirror in the Python parity test) ----
const TOKEN0: alloy::primitives::Address = address!("000000000000000000000000000000000000000A");
const TOKEN1: alloy::primitives::Address = address!("000000000000000000000000000000000000000B");
const POOL: alloy::primitives::Address = address!("000000000000000000000000000000000000000C");
const FACTORY: alloy::primitives::Address = address!("000000000000000000000000000000000000000F");

const FEE: u32 = 3_000; // 0.3%
const TICK_SPACING: i32 = 60;
/// A 1:1 price: sqrt_price_x96 = 2^96 (the Q96.32 repr of 1.0).
const SQRT_PRICE_1TO1: U256 = U256::from_limbs([0, 1 << 32, 0, 0]);
const LIQUIDITY: u128 = 1_000_000_000_000; // 1e12
const TICK: i32 = 0;

const AMOUNT_IN: u128 = 1_000_000_000; // 1e9 — small, stays in-tick
const ZERO_FOR_ONE: bool = true; // token0 → token1

/// Canonical V3 swap output, deterministic from the fixture above.
/// Symmetric for zfo/ofz at the 1:1 price (no directional asymmetry at
/// balance). Both the Rust consumer and the Python consumer MUST reproduce
/// this exact value — divergence = a lossy FFI seam.
const EXPECTED_AMOUNT_OUT_ZFO: u128 = 996_006_981;

#[test]
fn standalone_rust_consumer_v3_swap_matches_recorded_constant() {
    // Tier-2 dual-driver gate — Rust consumer side (V3 CL path).
    //
    // A `cargo add degenbot` standalone consumer, driving the canonical V3
    // fixture, MUST reproduce `EXPECTED_AMOUNT_OUT_ZFO`. The Python side
    // (`tests/standalone_parity/test_v3_swap_dual_driver.py`) drives the same
    // fixture through `PyBot` and asserts the same constant. Divergence = a
    // lossy FFI seam on the CL swap path — which the V2-only parity gate
    // cannot catch (the CL `simulate_swap` arm + `v3_simulate_swap` are
    // V3-specific).
    let mut bot = BotState::new();
    let mut tick_data = HashMap::new();
    // Seed tick 0 so the active-tick bitmap + net liquidity are present (a
    // swap staying near tick 0 with `Tracked` coverage never needs a fetch).
    tick_data.insert(
        0,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(LIQUIDITY),
            liquidity_net: I256::ZERO,
            block: 0,
        },
    );
    let params = RegisterV3PoolParams {
        address: POOL,
        token0: TOKEN0,
        token1: TOKEN1,
        fee: FEE,
        tick_spacing: TICK_SPACING,
        factory: FACTORY,
        sqrt_price_x96: SQRT_PRICE_1TO1,
        liquidity: LIQUIDITY,
        tick: TICK,
        tick_data,
        update_block: 0,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    };
    let pid = bot
        .register_v3_pool(&params)
        .expect("register canonical V3 pool");
    assert_eq!(pid, 1, "first registered pool gets id 1 (parity contract)");

    let amount_out = bot.calculate_tokens_out(pid, ZERO_FOR_ONE, U256::from(AMOUNT_IN));
    assert_eq!(
        amount_out,
        U256::from(EXPECTED_AMOUNT_OUT_ZFO),
        "standalone Rust consumer V3 swap must match the recorded constant \
         (the Python side asserts the same value)"
    );

    // Monotonicity sanity: a larger in-tick input produces a strictly-larger
    // output (catches a degenerate zero-return / overflow-clamp regression
    // that a single-point constant alone would miss).
    let bigger_in = U256::from(AMOUNT_IN) * U256::from(10_u64);
    let bigger_out = bot.calculate_tokens_out(pid, ZERO_FOR_ONE, bigger_in);
    assert!(
        bigger_out > amount_out,
        "V3 in-tick swap must be monotonic: {bigger_out} !> {amount_out}"
    );

    // Symmetry sanity: at the 1:1 price the zfo and ofz outputs are equal
    // (no directional asymmetry at balance). Catches a direction-flag
    // inversion in the FFI seam.
    let ofz_out = bot.calculate_tokens_out(pid, false, U256::from(AMOUNT_IN));
    assert_eq!(
        ofz_out, amount_out,
        "1:1-price V3 swap must be direction-symmetric (zfo == ofz)"
    );
}

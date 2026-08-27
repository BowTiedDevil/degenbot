#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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
//! shared oracle is the recorded constant in the shared fixture file. Plus a
//! monotonicity sanity check (larger input → larger output, within the active
//! tick) to catch a degenerate zero-return regression, and a
//! direction-symmetry check (zfo == ofz at the 1:1 price) to catch a
//! direction-flag inversion in the FFI seam.
//!
//! ## Fixture (single source of truth — HRT356)
//!
//! The fixture + expected output are loaded from the SHARED file
//! `tests/standalone_parity/fixtures/v3_swap.json`, which the Python
//! dual-driver test (`tests/standalone_parity/test_v3_swap_dual_driver.py`)
//! ALSO loads. A fixture edit that drifts the expected output fails BOTH
//! sides mechanically — closing the V3/V4 fixture-drift gap documented in
//! AGENTS.md "Known gap — V3/V4 fixture drift" (the constants were previously
//! copied between the two sides with no mechanical link, so a one-sided edit
//! left both tests green but testing different fixtures).
//!
//! A 1:1-price V3 pool (sqrt_price_x96 = 2^96, tick 0), liquidity 1e12, fee
//! 3000 (0.3%), tick_spacing 60, with tick 0 seeded in `tick_data` +
//! `Tracked` coverage so the swap stays within the active tick (no
//! `MissingTickWord`). `amount_in = 1_000_000_000` is small relative to
//! liquidity → stays in-tick → deterministic computed output
//! `996_006_981` (symmetric for zfo/ofz at the 1:1 price).

#![expect(clippy::doc_markdown)]

use alloy::primitives::{I256, U256};
use degenbot::bot_core::swap_simulation::{SwapRead, SwapRequest};
use degenbot::bot_core::BotState;
use degenbot::RegisterV3PoolParams;
use degenbot_pools::v3_state::PoolTickCoverage;
use degenbot_pools::TickInfo;
use hashbrown::HashMap;

/// Exact-input read through the swap-simulation gate (ADR-037), replacing
/// the former `calculate_tokens_out_miss_aware` seam for this suite.
fn tokens_out(bot: &mut BotState, pool_id: u64, zero_for_one: bool, amount_in: U256) -> U256 {
    match bot.swap_simulation(
        0,
        pool_id,
        SwapRequest {
            zero_for_one,
            amount_specified: -I256::try_from(amount_in).unwrap(),
            sqrt_price_limit: None,
        },
    ) {
        SwapRead::Computed(outcome) => outcome.delivered_unsigned(),
        f => panic!("in-tick fixture calc must not miss or overflow: {f:?}"),
    }
}

/// Path to the shared V3 fixture (loaded by both this Rust test and the
/// Python dual-driver test — HRT356, the single source of truth).
const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/standalone_parity/fixtures/v3_swap.json"
);

/// The shared V3 fixture, deserialized once per test.
#[derive(Debug, serde::Deserialize)]
struct V3FixtureFile {
    fixture: V3FixtureInputs,
    probe: V3Probe,
    expected: V3Expected,
}

#[derive(Debug, serde::Deserialize)]
#[expect(dead_code)]
struct V3FixtureInputs {
    token0: String,
    token1: String,
    pool: String,
    factory: String,
    fee: u32,
    tick_spacing: i32,
    /// Decimal-string big integers (JSON has no u128/u256).
    sqrt_price_x96: String,
    liquidity: String,
    tick: i32,
    tick_0_net_liquidity: String,
    coverage: String,
    update_block: u64,
}

#[derive(Debug, serde::Deserialize)]
struct V3Probe {
    amount_in: String,
    zero_for_one: bool,
}

#[derive(Debug, serde::Deserialize)]
struct V3Expected {
    amount_out_zfo: String,
}

/// Load + parse the shared fixture. Panics on any parse/IO failure (a corrupt
/// fixture file is a test-infrastructure failure, not a test skip).
fn load_shared_v3_fixture() -> V3FixtureFile {
    let text = std::fs::read_to_string(FIXTURE_PATH)
        .unwrap_or_else(|e| panic!("read shared V3 fixture {FIXTURE_PATH}: {e}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parse shared V3 fixture {FIXTURE_PATH}: {e}"))
}

#[test]
fn standalone_rust_consumer_v3_swap_matches_recorded_constant() {
    // Tier-2 dual-driver gate — Rust consumer side (V3 CL path).
    //
    // A `cargo add degenbot` standalone consumer, driving the canonical V3
    // fixture (loaded from the shared file), MUST reproduce the recorded
    // `amount_out_zfo`. The Python side drives the SAME file through `PyBot`
    // and asserts the same constant. Divergence = a lossy FFI seam on the CL
    // swap path — which the V2-only parity gate cannot catch (the CL
    // `simulate_swap` arm + `v3_simulate_swap` are V3-specific).
    let fx = load_shared_v3_fixture();

    let token0: alloy::primitives::Address = fx.fixture.token0.parse().unwrap();
    let token1: alloy::primitives::Address = fx.fixture.token1.parse().unwrap();
    let pool: alloy::primitives::Address = fx.fixture.pool.parse().unwrap();
    let factory: alloy::primitives::Address = fx.fixture.factory.parse().unwrap();
    let sqrt_price_x96: U256 = fx.fixture.sqrt_price_x96.parse().unwrap();
    let liquidity: u128 = fx.fixture.liquidity.parse().unwrap();
    let amount_in: u128 = fx.probe.amount_in.parse().unwrap();
    let expected_amount_out: u128 = fx.expected.amount_out_zfo.parse().unwrap();
    let zero_for_one = fx.probe.zero_for_one;

    let mut bot = BotState::new();
    let mut tick_data = HashMap::new();
    // Seed tick 0 so the active-tick bitmap + net liquidity are present (a
    // swap staying near tick 0 with `Tracked` coverage never needs a fetch).
    tick_data.insert(
        fx.fixture.tick,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(liquidity),
            liquidity_net: I256::ZERO,
            block: 0,
        },
    );
    let params = RegisterV3PoolParams {
        address: pool,
        token0,
        token1,
        fee: fx.fixture.fee,
        tick_spacing: fx.fixture.tick_spacing,
        factory,
        sqrt_price_x96,
        liquidity,
        tick: fx.fixture.tick,
        tick_data,
        update_block: fx.fixture.update_block,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    };
    let pid = bot
        .register_v3_pool(&params)
        .expect("register canonical V3 pool");
    assert_eq!(pid, 1, "first registered pool gets id 1 (parity contract)");

    let amount_out = tokens_out(&mut bot, pid, zero_for_one, U256::from(amount_in));
    assert_eq!(
        amount_out,
        U256::from(expected_amount_out),
        "standalone Rust consumer V3 swap must match the recorded constant in the \
         shared fixture file (the Python side asserts the same value)"
    );

    // Monotonicity sanity: a larger in-tick input produces a strictly-larger
    // output (catches a degenerate zero-return / overflow-clamp regression
    // that a single-point constant alone would miss).
    let bigger_in = U256::from(amount_in) * U256::from(10_u64);
    let bigger_out = tokens_out(&mut bot, pid, zero_for_one, bigger_in);
    assert!(
        bigger_out > amount_out,
        "V3 in-tick swap must be monotonic: {bigger_out} !> {amount_out}"
    );

    // Symmetry sanity: at the 1:1 price the zfo and ofz outputs are equal
    // (no directional asymmetry at balance). Catches a direction-flag
    // inversion in the FFI seam.
    let ofz_out = tokens_out(&mut bot, pid, false, U256::from(amount_in));
    assert_eq!(
        ofz_out, amount_out,
        "1:1-price V3 swap must be direction-symmetric (zfo == ofz)"
    );
}

//! Tier-2 behavioral dual-driver parity — V4 concentrated-liquidity swap
//! (ADR-005 standalone claim, the behavioral tier).
//!
//! The V4 dual-driver seed. V4 uses the **same CL math** as V3 but the
//! `amountSpecified` sign convention flips (V4 exact-input is *negative*,
//! opposite to V3's positive — see `simulate_swap`'s `PoolEntry::V4` arm).
//! So this test exercises a distinct FFI-seam path from the V3 seed: the
//! Rust core's sign flip in `simulate_swap` + the V4 `register_v4_pool`
//! admission (pool_manager + pool_id + pool_key + hook_flags). Divergence
//! between the Rust and Python consumers = a lossy seam on the V4 path.
//!
//! Like the V3 seed, the CL swap has no simple closed form, so the shared
//! oracle is the recorded constant in the shared fixture file. Plus
//! monotonicity + direction-symmetry sanity checks.
//!
//! ## Fixture (single source of truth — HRT356)
//!
//! The fixture + expected output are loaded from the SHARED file
//! `tests/standalone_parity/fixtures/v4_swap.json`, which the Python
//! dual-driver test (`tests/standalone_parity/test_v4_swap_dual_driver.py`)
//! ALSO loads. A fixture edit that drifts the expected output fails BOTH
//! sides mechanically — closing the V3/V4 fixture-drift gap documented in
//! AGENTS.md "Known gap — V3/V4 fixture drift".
//!
//! A 1:1-price V4 pool (sqrt_price_x96 = 2^96, tick 0), liquidity 1e12, fee
//! 500 (0.05%), tick_spacing 10, no hooks (`hook_flags = 0`), with tick 0
//! seeded + `Tracked` coverage. `amount_in = 1_000_000_000` → deterministic
//! output `998_501_997` (symmetric for zfo/ofz at the 1:1 price).
//!
//! Note the fee differs from the V3 seed (500 vs 3000) — V4's lower default
//! fee tier produces a distinct output, ruling out an accidental
//! hardcoding of the V3 constant.

#![allow(clippy::panic_in_result_fn, clippy::doc_markdown)]

use alloy::primitives::{I256, U256};
use degenbot::bot_core::BotState;
use degenbot::{RegisterV4PoolParams, V4PoolKey};
use degenbot_pools::v3_state::PoolTickCoverage;
use degenbot_pools::TickInfo;
use std::collections::HashMap;

/// Path to the shared V4 fixture (loaded by both this Rust test and the
/// Python dual-driver test — HRT356, the single source of truth).
const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/standalone_parity/fixtures/v4_swap.json"
);

/// The shared V4 fixture, deserialized once per test.
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct V4FixtureFile {
    fixture: V4FixtureInputs,
    probe: V4Probe,
    expected: V4Expected,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct V4FixtureInputs {
    pool_manager: String,
    currency0: String,
    currency1: String,
    hooks: String,
    pool_id_hex: String,
    fee: u32,
    tick_spacing: i32,
    sqrt_price_x96: String,
    liquidity: String,
    tick: i32,
    coverage: String,
    update_block: u64,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct V4Probe {
    amount_in: String,
    zero_for_one: bool,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct V4Expected {
    amount_out_zfo: String,
}

/// Parse a 32-byte pool_id from its `0x`-prefixed hex string (64 hex chars).
fn parse_pool_id(hex: &str) -> [u8; 32] {
    let stripped = hex.strip_prefix("0x").unwrap_or(hex);
    let bytes = alloy::hex::decode(stripped)
        .unwrap_or_else(|e| panic!("parse pool_id_hex {stripped}: {e}"));
    assert_eq!(
        bytes.len(),
        32,
        "pool_id_hex must be 32 bytes, got {}",
        bytes.len()
    );
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

/// Load + parse the shared fixture. Panics on any parse/IO failure (a corrupt
/// fixture file is a test-infrastructure failure, not a test skip).
#[allow(dead_code)]
fn load_shared_v4_fixture() -> V4FixtureFile {
    let text = std::fs::read_to_string(FIXTURE_PATH)
        .unwrap_or_else(|e| panic!("read shared V4 fixture {FIXTURE_PATH}: {e}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parse shared V4 fixture {FIXTURE_PATH}: {e}"))
}

#[test]
fn standalone_rust_consumer_v4_swap_matches_recorded_constant() {
    // Tier-2 dual-driver gate — Rust consumer side (V4 CL path).
    //
    // V4 uses the same CL math as V3 but the `amountSpecified` sign flips
    // (V4 exact-input is negative — see `simulate_swap`'s `PoolEntry::V4`
    // arm's `-spec`). So this exercises a distinct FFI-seam path from the
    // V3 seed: the Rust core's sign flip + the V4 `register_v4_pool`
    // admission (pool_manager + pool_id + pool_key + hook_flags).
    let fx = load_shared_v4_fixture();

    let pool_manager: alloy::primitives::Address = fx.fixture.pool_manager.parse().unwrap();
    let currency0: alloy::primitives::Address = fx.fixture.currency0.parse().unwrap();
    let currency1: alloy::primitives::Address = fx.fixture.currency1.parse().unwrap();
    let hooks: alloy::primitives::Address = fx.fixture.hooks.parse().unwrap();
    let pool_id = parse_pool_id(&fx.fixture.pool_id_hex);
    let sqrt_price_x96: U256 = fx.fixture.sqrt_price_x96.parse().unwrap();
    let liquidity: u128 = fx.fixture.liquidity.parse().unwrap();
    let amount_in: u128 = fx.probe.amount_in.parse().unwrap();
    let expected_amount_out: u128 = fx.expected.amount_out_zfo.parse().unwrap();
    let zero_for_one = fx.probe.zero_for_one;

    let mut bot = BotState::new();
    let mut tick_data = HashMap::new();
    tick_data.insert(
        fx.fixture.tick,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(liquidity),
            liquidity_net: I256::ZERO,
            block: 0,
        },
    );
    let params = RegisterV4PoolParams {
        pool_manager,
        pool_id,
        pool_key: V4PoolKey {
            currency0,
            currency1,
            fee: fx.fixture.fee,
            tick_spacing: fx.fixture.tick_spacing,
            hooks,
        },
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96,
        liquidity,
        tick: fx.fixture.tick,
        tick_data,
        update_block: fx.fixture.update_block,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    let pid = bot
        .register_v4_pool(&params)
        .expect("register canonical V4 pool");
    assert_eq!(pid, 1, "first registered pool gets id 1 (parity contract)");

    let amount_out = bot
        .calculate_tokens_out_miss_aware(pid, zero_for_one, U256::from(amount_in))
        .expect(
            "small in-tick amount with pre-populated tick data; V4 calc must not miss or overflow",
        );
    assert_eq!(
        amount_out,
        U256::from(expected_amount_out),
        "standalone Rust consumer V4 swap must match the recorded constant in the \
         shared fixture file (the Python side asserts the same value)"
    );

    // Monotonicity (in-tick).
    let bigger_in = U256::from(amount_in) * U256::from(10_u64);
    let bigger_out = bot
        .calculate_tokens_out_miss_aware(pid, zero_for_one, bigger_in)
        .expect(
            "in-tick 10x amount with pre-populated tick data; V4 calc must not miss or overflow",
        );
    assert!(
        bigger_out > amount_out,
        "V4 in-tick swap must be monotonic: {bigger_out} !> {amount_out}"
    );

    // Direction symmetry at the 1:1 price (catches a V4 sign-flip regression).
    let ofz_out = bot
        .calculate_tokens_out_miss_aware(pid, false, U256::from(amount_in))
        .expect(
            "small in-tick amount with pre-populated tick data; V4 calc must not miss or overflow",
        );
    assert_eq!(
        ofz_out, amount_out,
        "1:1-price V4 swap must be direction-symmetric (zfo == ofz)"
    );
}

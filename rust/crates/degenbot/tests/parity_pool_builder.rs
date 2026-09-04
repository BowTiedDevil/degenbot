#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Tier-2 behavioral dual-driver parity — Rust pool builder identity+state
//! (ADR-005 standalone claim, the behavioral tier; task A2QRWO).
//!
//! The swap parities (`parity_v2_swap.rs` / `parity_v3_swap.rs`) prove the
//! FFI seam on the *math* is lossless (same swap output across consumers).
//! This builder-parity proves the FFI seam on the *pool a `PoolBuilder` emits*
//! is lossless: the **same** canonical V3 identity+state fixture, driven
//! through the **Rust consumer** (`BotState`/`register_v3_pool` — the exact
//! `RegisterV3PoolParams` `build_v3` produces after its on-chain/DB I/O),
//! registers pool id 1 and reports IDENTICAL identity (token0/token1/fee/
//! tick_spacing) + state (sqrt_price_x96/liquidity/tick) + coverage as the
//! fixture records.
//!
//! The matching Python side lives at
//! `tests/standalone_parity/test_pool_builder_dual_driver.py` — it drives the
//! SAME fixture (loaded from the SAME shared JSON file, never copied) through
//! the Python consumer (`PyBot`). Both consumers must agree on the pool id AND
//! every identity/state field; divergence = a lossy FFI seam on the
//! registration/identity path that a swap-only parity gate cannot catch.

#![expect(clippy::doc_markdown)]

use alloy::primitives::{Address, U256};
use degenbot::bot_core::BotState;
use degenbot::PoolEntry;
use degenbot::RegisterV3PoolParams;
use degenbot_pools::{v3_state::PoolTickCoverage, TickInfo};
use hashbrown::HashMap;

/// Path to the shared builder fixture (loaded by both this Rust test and the
/// Python dual-driver test — the single source of truth).
const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/standalone_parity/fixtures/pool_builder.json"
);

/// The shared builder fixture, deserialized once per test.
#[derive(Debug, serde::Deserialize)]
struct PoolBuilderFixtureFile {
    fixture: PoolBuilderFixtureInputs,
    expected: PoolBuilderExpected,
}

#[derive(Debug, serde::Deserialize)]
struct PoolBuilderFixtureInputs {
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
    coverage: String,
    update_block: u64,
}

#[derive(Debug, serde::Deserialize)]
struct PoolBuilderExpected {
    pool_id: u64,
}

/// Load + parse the shared fixture. Panics on IO/parse failure (a corrupt
/// fixture is a test-infra failure, not a skip).
fn load_shared_fixture() -> PoolBuilderFixtureFile {
    let text = std::fs::read_to_string(FIXTURE_PATH)
        .unwrap_or_else(|e| panic!("read shared PoolBuilder fixture {FIXTURE_PATH}: {e}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parse shared PoolBuilder fixture {FIXTURE_PATH}: {e}"))
}

#[test]
fn standalone_rust_consumer_pool_builder_identity_state_matches_fixture() {
    // Tier-2 dual-driver gate — Rust consumer side (pool builder identity +
    // state). Both consumers must register the SAME fixture under the SAME
    // pool id and report IDENTICAL identity + state accessors.
    let fx = load_shared_fixture();

    let token0: Address = fx.fixture.token0.parse().unwrap();
    let token1: Address = fx.fixture.token1.parse().unwrap();
    let pool: Address = fx.fixture.pool.parse().unwrap();
    let factory: Address = fx.fixture.factory.parse().unwrap();
    let sqrt_price_x96: U256 = fx.fixture.sqrt_price_x96.parse().unwrap();
    let liquidity: u128 = fx.fixture.liquidity.parse().unwrap();
    assert_eq!(
        fx.fixture.coverage, "tracked",
        "fixture must declare Tracked coverage for the in-tick identity+state parity"
    );

    let mut bot = BotState::new();
    let mut tick_data = HashMap::new();
    tick_data.insert(
        fx.fixture.tick,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(liquidity),
            liquidity_net: 0,
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
    assert_eq!(
        pid, fx.expected.pool_id,
        "pool id must match the shared fixture"
    );

    // Read the registered pool back through the identity/state projection and
    // assert the full identity + state matches the fixture (the Python side
    // asserts the same values from the same file).
    let entry: &PoolEntry = bot
        .pool_entry(pid)
        .expect("registered pool must be retrievable by id");
    let (v3_id, v3_state) = entry
        .v3()
        .expect("registered pool must be a V3 concentrated-liquidity entry");
    assert_eq!(v3_id.token0, token0, "token0 identity diverged");
    assert_eq!(v3_id.token1, token1, "token1 identity diverged");
    assert_eq!(v3_id.fee, fx.fixture.fee, "fee identity diverged");
    assert_eq!(
        v3_id.tick_spacing, fx.fixture.tick_spacing,
        "tick_spacing identity diverged"
    );
    assert_eq!(
        v3_state.sqrt_price_x96, sqrt_price_x96,
        "sqrt_price state diverged"
    );
    assert_eq!(v3_state.liquidity, liquidity, "liquidity state diverged");
    assert_eq!(v3_state.tick, fx.fixture.tick, "tick state diverged");
    assert_eq!(
        v3_state.coverage,
        PoolTickCoverage::Tracked,
        "registered pool must be Tracked coverage (the builder's DB-tracked decision)"
    );
}
